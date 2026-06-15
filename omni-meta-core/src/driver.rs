//! slice 驱动循环：把整块缓冲反复喂给 MetaParser，按 Demand 推进逻辑位置，
//! 并把 Payload 事件分派给对应 codec。这是 read_slice 的引擎。

use alloc::vec::Vec;

use crate::codecs;
use crate::demand::{Demand, Event, MetaParser, PayloadKind};
use crate::limits::Limits;
use crate::model::{ExifTag, WarnKind, Warning};

/// 解析过程中累积的产物。
pub struct Collector {
    pub exif: Vec<ExifTag>,
    pub warnings: Vec<Warning>,
    limits: Limits,
}

impl Collector {
    fn handle(&mut self, ev: Event<'_>) {
        match ev {
            Event::Payload { kind: PayloadKind::Exif, data } => {
                codecs::exif::decode(data, &mut self.exif, &mut self.warnings, &self.limits);
            }
            Event::Warning(w) => self.warnings.push(w),
        }
    }
}

/// 在一整块内存缓冲上驱动 parser 跑到 Done。
///
/// 终止保证：每次迭代要么前进、要么 break；并设有迭代预算兜底，
/// 因此任何（含畸形/恶意）解析器都不会让它死循环。
pub fn drive_slice(buf: &[u8], parser: &mut dyn MetaParser, limits: Limits) -> Collector {
    let mut col = Collector { exif: Vec::new(), warnings: Vec::new(), limits };
    let mut pos: usize = 0;
    // 防卡死预算：正常解析的 pull 次数远小于此（约等于段/box 数量）；
    // 仅用于解析器反复 SeekTo 同一位置等零前进情形的兜底。
    let max_iters = buf.len().saturating_mul(2).saturating_add(1024);
    let mut iters: usize = 0;
    loop {
        if iters >= max_iters {
            col.warnings.push(Warning { offset: pos as u64, kind: WarnKind::UnreachableSection });
            break;
        }
        iters += 1;

        let start = pos.min(buf.len());
        let res = parser.pull(&buf[start..]);
        for ev in res.events {
            col.handle(ev);
        }
        match res.demand {
            Demand::Done => break,
            Demand::NeedBytes(n) => {
                // 截断点 = 解析器卡住的绝对位置（slice 永不丢弃前缀 → start 即绝对）。
                let stuck = start.saturating_add(res.consumed);
                let avail = buf.len().saturating_sub(stuck);
                // `stuck > start`（即 consumed > 0）是零前进守卫：
                // 在全量 slice 上，契约合规的解析器只在窗口确实不足时返回 NeedBytes；
                // 若 consumed==0 却已有足够字节，说明解析器行为异常，停止以防自旋。
                if avail >= n && stuck > start {
                    // 已有足够字节且有推进 → 续跑（增量 parser 的正常路径）。
                    pos = stuck;
                } else {
                    // 字节确实不够（slice 给的是全量剩余）→ 截断。
                    col.warnings.push(Warning { offset: stuck as u64, kind: WarnKind::Truncated });
                    break;
                }
            }
            Demand::Skip(n) => {
                // 用 u64 计算目标偏移（供诊断保真）；转回 usize 溢出即按越界处理。
                let target = (start as u64)
                    .saturating_add(res.consumed as u64)
                    .saturating_add(n);
                match usize::try_from(target) {
                    Ok(p) if p <= buf.len() => {
                        if p == start {
                            // 零前进（consumed==0 且 n==0）→ 防卡死，按截断收尾。
                            col.warnings.push(Warning { offset: start as u64, kind: WarnKind::Truncated });
                            break;
                        }
                        pos = p;
                    }
                    _ => {
                        col.warnings.push(Warning { offset: target, kind: WarnKind::UnreachableSection });
                        break;
                    }
                }
            }
            Demand::SeekTo(p) => {
                match usize::try_from(p) {
                    Ok(up) if up <= buf.len() => {
                        if up == start {
                            // 零前进（SeekTo 回到当前位置）→ 防卡死，按截断收尾。
                            col.warnings.push(Warning { offset: start as u64, kind: WarnKind::Truncated });
                            break;
                        }
                        pos = up;
                    }
                    _ => {
                        col.warnings.push(Warning { offset: p, kind: WarnKind::UnreachableSection });
                        break;
                    }
                }
            }
        }
    }
    col
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demand::PullResult;
    use alloc::vec;
    use alloc::vec::Vec;

    /// 按脚本依次返回 Demand（consumed=0，无事件）；脚本耗尽后返回 Done。
    struct Script {
        steps: Vec<Demand>,
        i: usize,
    }
    impl MetaParser for Script {
        fn pull<'a>(&mut self, _input: &'a [u8]) -> PullResult<'a> {
            let demand = self.steps.get(self.i).cloned().unwrap_or(Demand::Done);
            self.i += 1;
            PullResult { demand, consumed: 0, events: Vec::new() }
        }
    }

    /// 永远返回 Skip(0)（零前进）的恶意解析器。
    struct AlwaysSkipZero;
    impl MetaParser for AlwaysSkipZero {
        fn pull<'a>(&mut self, _input: &'a [u8]) -> PullResult<'a> {
            PullResult { demand: Demand::Skip(0), consumed: 0, events: Vec::new() }
        }
    }

    /// 永远 SeekTo(0)（反复回到同一位置）的恶意解析器。
    struct AlwaysSeekZero;
    impl MetaParser for AlwaysSeekZero {
        fn pull<'a>(&mut self, _input: &'a [u8]) -> PullResult<'a> {
            PullResult { demand: Demand::SeekTo(0), consumed: 0, events: Vec::new() }
        }
    }

    #[test]
    fn skip_advances_then_done() {
        let buf = [0u8; 20];
        let mut p = Script { steps: vec![Demand::Skip(10)], i: 0 };
        let col = drive_slice(&buf, &mut p, Limits::default());
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
    }

    #[test]
    fn seek_within_bounds_then_done() {
        let buf = [0u8; 20];
        let mut p = Script { steps: vec![Demand::SeekTo(5)], i: 0 };
        let col = drive_slice(&buf, &mut p, Limits::default());
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
    }

    #[test]
    fn seek_past_end_warns_unreachable() {
        let buf = [0u8; 4];
        let mut p = Script { steps: vec![Demand::SeekTo(9999)], i: 0 };
        let col = drive_slice(&buf, &mut p, Limits::default());
        assert_eq!(col.warnings.len(), 1);
        assert_eq!(col.warnings[0].kind, WarnKind::UnreachableSection);
    }

    #[test]
    fn need_bytes_yields_truncated_warning() {
        let buf = [0u8; 4];
        let mut p = Script { steps: vec![Demand::NeedBytes(99)], i: 0 };
        let col = drive_slice(&buf, &mut p, Limits::default());
        assert_eq!(col.warnings.len(), 1);
        assert_eq!(col.warnings[0].kind, WarnKind::Truncated);
    }

    #[test]
    fn zero_progress_skip_terminates() {
        // 必须返回（不得死循环），并留下一条警告。
        let buf = [0u8; 8];
        let mut p = AlwaysSkipZero;
        let col = drive_slice(&buf, &mut p, Limits::default());
        assert!(!col.warnings.is_empty());
    }

    #[test]
    fn zero_progress_seek_terminates() {
        // SeekTo 回到当前位置（零前进）→ 防卡死，必须立即返回并留警告。
        let buf = [0u8; 8];
        let mut p = AlwaysSeekZero;
        let col = drive_slice(&buf, &mut p, Limits::default());
        assert!(!col.warnings.is_empty());
    }

    /// 用真实 JPEG 解析器 + 真实 EXIF 走一遍，验证驱动把载荷送进了 codec。
    #[test]
    fn drives_jpeg_into_exif_collector() {
        // 复用 EXIF 与 JPEG 的 fixture 思路：构造 JPEG(含完整 TIFF)。
        let tiff = make_tiff();
        let mut seg_body: Vec<u8> = Vec::new();
        seg_body.extend_from_slice(b"Exif\0\0");
        seg_body.extend_from_slice(&tiff);
        let len = (seg_body.len() + 2) as u16;
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
        j.extend_from_slice(&len.to_be_bytes());
        j.extend_from_slice(&seg_body);
        j.extend_from_slice(&[0xFF, 0xD9]);

        let mut parser = crate::formats::jpeg::JpegParser::new();
        let col = drive_slice(&j, &mut parser, Limits::default());
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
        assert_eq!(col.exif.len(), 2);
    }

    #[test]
    fn slice_truncated_app1_warns_with_offset() {
        // SOI + APP1(声明 len=20) 但 body 截断
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
        j.extend_from_slice(&20u16.to_be_bytes());
        j.extend_from_slice(&[0xAA, 0xBB]); // body 不足
        let mut parser = crate::formats::jpeg::JpegParser::new();
        let col = drive_slice(&j, &mut parser, Limits::default());
        assert_eq!(col.warnings.len(), 1);
        assert_eq!(col.warnings[0].kind, WarnKind::Truncated);
        // 卡在 APP1 段起点（SOI 之后）= 偏移 2
        assert_eq!(col.warnings[0].offset, 2);
    }

    fn make_tiff() -> Vec<u8> {
        let mut t: Vec<u8> = Vec::new();
        t.extend_from_slice(b"II");
        t.extend_from_slice(&42u16.to_le_bytes());
        t.extend_from_slice(&8u32.to_le_bytes());
        t.extend_from_slice(&2u16.to_le_bytes());
        t.extend_from_slice(&0x010Fu16.to_le_bytes());
        t.extend_from_slice(&2u16.to_le_bytes());
        t.extend_from_slice(&5u32.to_le_bytes());
        t.extend_from_slice(&38u32.to_le_bytes());
        t.extend_from_slice(&0x0112u16.to_le_bytes());
        t.extend_from_slice(&3u16.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&6u32.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(b"Acme\0");
        t
    }
}
