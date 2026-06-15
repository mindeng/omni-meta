//! slice 驱动循环：把整块缓冲反复喂给 MetaParser，按 Demand 推进逻辑位置，
//! 并把 Payload 事件分派给对应 codec。这是 read_slice 的引擎。

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::codecs;
use crate::demand::{Demand, Event, MetaParser, PayloadKind};
use crate::limits::Limits;
use crate::model::{ExifTag, Field, FileFormat, Metadata, RawTags, WarnKind, Warning, XmpProperty};
use crate::normalize::normalize;

/// 解析过程中累积的产物。
pub struct Collector {
    pub exif: Vec<ExifTag>,
    pub xmp: Vec<XmpProperty>,
    pub warnings: Vec<Warning>,
    width: Option<u32>,
    height: Option<u32>,
    limits: Limits,
}

impl Collector {
    fn handle(&mut self, ev: Event<'_>) {
        match ev {
            Event::Payload { kind: PayloadKind::Exif, data } => {
                codecs::exif::decode(data, &mut self.exif, &mut self.warnings, &self.limits);
            }
            Event::Payload { kind: PayloadKind::Xmp, data } => {
                codecs::xmp::decode(data, &mut self.xmp, &mut self.warnings, &self.limits);
            }
            Event::Field(Field::Width(w)) => {
                if self.width.is_none() {
                    self.width = Some(w);
                }
            }
            Event::Field(Field::Height(h)) => {
                if self.height.is_none() {
                    self.height = Some(h);
                }
            }
            Event::Warning(w) => self.warnings.push(w),
        }
    }
}

/// 流式适配器与解析引擎之间的结果。`Need`/`SkipHint` 的数值都是"还需多少字节"
/// / "还需向前跳多少字节"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 需要再补至少 n 字节才能继续。
    Need(usize),
    /// 建议向前跳过 n 字节：能 seek 就 seek + skip_external(n)，不能就照常 feed（driver 吞掉）。
    SkipHint(u64),
    /// 解析完成。
    Done,
}

/// 收尾：把 Collector 投影为统一模型，组装 Metadata。read_slice 与 push 路径共用。
pub(crate) fn finalize(col: Collector, format: FileFormat) -> Metadata {
    let (width, height) = (col.width, col.height);
    let raw = RawTags { exif: col.exif, xmp: col.xmp };
    let mut warnings = col.warnings;
    let mut unified = normalize(&raw, &mut warnings);
    if let Some(w) = width {
        unified.width = Some(w);
    }
    if let Some(h) = height {
        unified.height = Some(h);
    }
    Metadata { unified, raw, warnings, format }
}

/// 流式驱动：自有增长缓冲 + parser + Collector。被 PushParser/blocking/seek 复用。
pub(crate) struct StreamDriver {
    buf: Vec<u8>,
    cursor: usize, // buf 内已消费偏移
    parser: Box<dyn MetaParser>,
    collector: Collector,
    skip_remaining: u64,
    pos_base: u64, // buf[0] 的绝对文件偏移
    done: bool,
    eof: bool,
    max_retained: usize,
}

impl StreamDriver {
    pub(crate) fn new(parser: Box<dyn MetaParser>, limits: Limits) -> Self {
        let max_retained = limits.max_retained_bytes;
        Self {
            buf: Vec::new(),
            cursor: 0,
            parser,
            collector: Collector {
                exif: Vec::new(),
                xmp: Vec::new(),
                warnings: Vec::new(),
                width: None,
                height: None,
                limits,
            },
            skip_remaining: 0,
            pos_base: 0,
            done: false,
            eof: false,
            max_retained,
        }
    }

    /// 追加一块字节并推进，返回下一步 Outcome。chunk 可为空（仅推进）。
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Outcome {
        if !chunk.is_empty() {
            self.buf.extend_from_slice(chunk);
        }
        self.drive()
    }

    /// 调用者已自行向前跳 n 字节（源级 seek）后，扣减逻辑待跳量。
    pub(crate) fn skip_external(&mut self, n: u64) {
        let take = n.min(self.skip_remaining);
        self.skip_remaining -= take;
        self.pos_base = self.pos_base.saturating_add(take);
    }

    /// 收尾：若未 Done，置 eof 再驱动一次以记录截断/不可达；返回 Collector。
    pub(crate) fn finish(mut self) -> Collector {
        if !self.done {
            self.eof = true;
            let _ = self.drive();
        }
        self.collector
    }

    fn drop_consumed(&mut self) {
        if self.cursor > 0 {
            self.buf.drain(..self.cursor);
            self.pos_base = self.pos_base.saturating_add(self.cursor as u64);
            self.cursor = 0;
        }
    }

    fn drive(&mut self) -> Outcome {
        if self.done {
            return Outcome::Done;
        }
        // 防卡死：单次 drive 内的循环上界（远大于正常段数）。
        let mut budget = self.buf.len().saturating_mul(2).saturating_add(1024);
        loop {
            if budget == 0 {
                self.collector.warnings.push(Warning {
                    offset: self.pos_base + self.cursor as u64,
                    kind: WarnKind::UnreachableSection,
                });
                self.done = true;
                return Outcome::Done;
            }
            budget -= 1;

            // 1) 先用缓冲字节抵扣在途 skip。
            if self.skip_remaining > 0 {
                let avail = (self.buf.len() - self.cursor) as u64;
                let take = avail.min(self.skip_remaining);
                self.cursor += take as usize;
                self.skip_remaining -= take;
                self.drop_consumed();
                if self.skip_remaining > 0 {
                    if self.eof {
                        // 跳越文件尾：该段不可达（与 drive_slice Skip 越界对齐）。
                        self.collector.warnings.push(Warning {
                            offset: self.pos_base + self.cursor as u64 + self.skip_remaining,
                            kind: WarnKind::UnreachableSection,
                        });
                        self.done = true;
                        return Outcome::Done;
                    }
                    return Outcome::SkipHint(self.skip_remaining);
                }
            }

            // DoS 上界：等待巨型段体导致缓冲超限。
            if self.buf.len() - self.cursor > self.max_retained {
                self.collector.warnings.push(Warning {
                    offset: self.pos_base + self.cursor as u64,
                    kind: WarnKind::UnreachableSection,
                });
                self.done = true;
                return Outcome::Done;
            }

            // 2) 拉解析器（拆分字段借用：parser &mut 与 buf & 互不相干）。
            let (demand, consumed) = {
                let Self { buf, cursor, parser, collector, .. } = self;
                let window = &buf[*cursor..];
                let res = parser.pull(window);
                for ev in res.events {
                    collector.handle(ev);
                }
                (res.demand, res.consumed)
            };

            match demand {
                Demand::Done => {
                    self.cursor += consumed;
                    self.drop_consumed();
                    self.done = true;
                    return Outcome::Done;
                }
                Demand::NeedBytes(n) => {
                    self.cursor += consumed;
                    self.drop_consumed();
                    let avail = self.buf.len() - self.cursor;
                    if avail >= n {
                        if consumed == 0 {
                            // 零前进且已有足够字节 → 解析器违约，防卡死收尾。
                            self.collector.warnings.push(Warning {
                                offset: self.pos_base + self.cursor as u64,
                                kind: WarnKind::Truncated,
                            });
                            self.done = true;
                            return Outcome::Done;
                        }
                        continue; // 已够，续跑
                    }
                    if self.eof {
                        self.collector.warnings.push(Warning {
                            offset: self.pos_base + self.cursor as u64,
                            kind: WarnKind::Truncated,
                        });
                        self.done = true;
                        return Outcome::Done;
                    }
                    return Outcome::Need(n - avail);
                }
                Demand::Skip(k) => {
                    self.cursor += consumed;
                    self.drop_consumed();
                    self.skip_remaining = k;
                    if k == 0 && consumed == 0 {
                        // 零前进 Skip(0) → 防卡死。
                        self.collector.warnings.push(Warning {
                            offset: self.pos_base + self.cursor as u64,
                            kind: WarnKind::Truncated,
                        });
                        self.done = true;
                        return Outcome::Done;
                    }
                    continue; // 回到顶部抵扣 skip
                }
                Demand::SeekTo(p) => {
                    self.cursor += consumed;
                    let abs = self.pos_base + self.cursor as u64;
                    if p >= abs {
                        self.skip_remaining = p - abs;
                        self.drop_consumed();
                        if self.skip_remaining == 0 {
                            // 零前进 SeekTo 当前位置 → 防卡死。
                            self.collector.warnings.push(Warning {
                                offset: abs,
                                kind: WarnKind::Truncated,
                            });
                            self.done = true;
                            return Outcome::Done;
                        }
                        continue;
                    } else if p >= self.pos_base {
                        // 落在保留缓冲内 → cursor 回移。
                        self.cursor = (p - self.pos_base) as usize;
                        continue;
                    } else {
                        // 早于保留下界且字节已弃 → 不可达。
                        self.collector.warnings.push(Warning {
                            offset: p,
                            kind: WarnKind::UnreachableSection,
                        });
                        self.done = true;
                        return Outcome::Done;
                    }
                }
            }
        }
    }
}

/// 在一整块内存缓冲上驱动 parser 跑到 Done。
///
/// 终止保证：每次迭代要么前进、要么 break；并设有迭代预算兜底，
/// 因此任何（含畸形/恶意）解析器都不会让它死循环。
pub fn drive_slice(buf: &[u8], parser: &mut dyn MetaParser, limits: Limits) -> Collector {
    let mut col = Collector {
        exif: Vec::new(),
        xmp: Vec::new(),
        warnings: Vec::new(),
        width: None,
        height: None,
        limits,
    };
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
    use alloc::string::String;
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

    use crate::model::FileFormat;

    /// 把若干 chunk 依次 feed 进 StreamDriver，返回最终 Collector。
    fn run_stream(chunks: &[&[u8]], parser: alloc::boxed::Box<dyn MetaParser>) -> Collector {
        let mut d = StreamDriver::new(parser, Limits::default());
        for c in chunks {
            let _ = d.feed(c);
        }
        d.finish()
    }

    #[test]
    fn stream_drives_jpeg_in_one_chunk() {
        let j = make_jpeg_with_exif();
        let col = run_stream(&[&j], alloc::boxed::Box::new(crate::formats::jpeg::JpegParser::new()));
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
        assert_eq!(col.exif.len(), 2);
    }

    #[test]
    fn stream_drives_jpeg_byte_by_byte() {
        let j = make_jpeg_with_exif();
        let chunks: Vec<&[u8]> = j.chunks(1).collect();
        let col = run_stream(&chunks, alloc::boxed::Box::new(crate::formats::jpeg::JpegParser::new()));
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
        assert_eq!(col.exif.len(), 2);
    }

    #[test]
    fn stream_skip_outcome_then_seek_external() {
        // 用 Script：先 Skip(100)，再 Done。模拟可 Seek 适配器：feed 少量后用 skip_external 抵扣。
        let mut d = StreamDriver::new(
            alloc::boxed::Box::new(Script { steps: vec![Demand::Skip(100)], i: 0 }),
            Limits::default(),
        );
        // 喂 4 字节触发首个 pull → Script 立即 Skip(100)，driver 吞掉这 4 字节，剩余 skip。
        match d.feed(&[0u8; 4]) {
            Outcome::SkipHint(k) => assert!(k > 0 && k <= 100),
            other => panic!("expected SkipHint, got {other:?}"),
        }
        // 适配器自行 seek 了剩余 k 字节：
        if let Outcome::SkipHint(k) = d.feed(&[]) {
            d.skip_external(k);
        }
        let col = d.finish();
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
    }

    #[test]
    fn stream_truncated_app1_warns_truncated() {
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
        j.extend_from_slice(&20u16.to_be_bytes());
        j.extend_from_slice(&[0xAA, 0xBB]);
        let chunks: Vec<&[u8]> = j.chunks(1).collect();
        let col = run_stream(&chunks, alloc::boxed::Box::new(crate::formats::jpeg::JpegParser::new()));
        assert_eq!(col.warnings.len(), 1);
        assert_eq!(col.warnings[0].kind, WarnKind::Truncated);
        assert_eq!(col.warnings[0].offset, 2);
    }

    #[test]
    fn stream_seekto_backward_beyond_retained_warns() {
        // SeekTo(0) 在丢弃前缀后属于"早于保留下界"→ UnreachableSection。
        let mut d = StreamDriver::new(
            alloc::boxed::Box::new(Script { steps: vec![Demand::Skip(4), Demand::SeekTo(0)], i: 0 }),
            Limits::default(),
        );
        let _ = d.feed(&[0u8; 8]);
        let _ = d.feed(&[]);
        let col = d.finish();
        assert!(col.warnings.iter().any(|w| w.kind == WarnKind::UnreachableSection));
    }

    fn make_jpeg_with_exif() -> Vec<u8> {
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
        j
    }

    #[test]
    fn finalize_projects_unified() {
        let j = make_jpeg_with_exif();
        let mut parser = crate::formats::jpeg::JpegParser::new();
        let col = drive_slice(&j, &mut parser, Limits::default());
        let meta = finalize(col, FileFormat::Jpeg);
        assert_eq!(meta.format, FileFormat::Jpeg);
        assert_eq!(meta.unified.orientation, Some(crate::model::Orientation::Rotate90));
        assert_eq!(meta.unified.camera_make.as_deref(), Some("Acme"));
        assert_eq!(meta.raw.exif.len(), 2);
    }

    use crate::model::{Field, XmpProperty};
    use crate::demand::PayloadKind;

    /// 一次性发出 Width/Height Field + 一个 XMP 载荷后 Done 的假解析器。
    struct FieldXmpEmitter;
    impl MetaParser for FieldXmpEmitter {
        fn pull<'a>(&mut self, input: &'a [u8]) -> crate::demand::PullResult<'a> {
            use crate::demand::PullResult;
            let events = vec![
                Event::Field(Field::Width(1920)),
                Event::Field(Field::Height(1080)),
                Event::Payload {
                    kind: PayloadKind::Xmp,
                    data: br#"<rdf:Description tiff:Make="Acme"/>"#,
                },
            ];
            PullResult { demand: Demand::Done, consumed: input.len(), events }
        }
    }

    #[test]
    fn collector_records_fields_and_xmp() {
        let buf = [0u8; 4];
        let mut p = FieldXmpEmitter;
        let col = drive_slice(&buf, &mut p, Limits::default());
        let meta = finalize(col, FileFormat::Png);
        assert_eq!(meta.unified.width, Some(1920));
        assert_eq!(meta.unified.height, Some(1080));
        assert_eq!(
            meta.raw.xmp,
            vec![XmpProperty {
                prefix: String::from("tiff"),
                name: String::from("Make"),
                value: String::from("Acme"),
            }]
        );
    }

    /// 发 容器维度 Field + 含冲突 tiff:ImageWidth/Length 的 XMP，验证容器维度胜出。
    struct DimConflictEmitter;
    impl MetaParser for DimConflictEmitter {
        fn pull<'a>(&mut self, input: &'a [u8]) -> crate::demand::PullResult<'a> {
            use crate::demand::PullResult;
            let events = vec![
                Event::Field(Field::Width(1920)),
                Event::Field(Field::Height(1080)),
                Event::Payload {
                    kind: PayloadKind::Xmp,
                    data: br#"<rdf:Description tiff:ImageWidth="999" tiff:ImageLength="888"/>"#,
                },
            ];
            PullResult { demand: Demand::Done, consumed: input.len(), events }
        }
    }

    #[test]
    fn container_dims_beat_xmp_dims() {
        let buf = [0u8; 4];
        let mut p = DimConflictEmitter;
        let col = drive_slice(&buf, &mut p, Limits::default());
        let meta = finalize(col, FileFormat::Png);
        assert_eq!(meta.unified.width, Some(1920));  // 容器值胜出，非 XMP 的 999
        assert_eq!(meta.unified.height, Some(1080));
        // XMP 仍保留在 raw 层
        assert!(meta.raw.xmp.iter().any(|x| x.name == "ImageWidth" && x.value == "999"));
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
