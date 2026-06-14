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
pub fn drive_slice(buf: &[u8], parser: &mut dyn MetaParser, limits: Limits) -> Collector {
    let mut col = Collector { exif: Vec::new(), warnings: Vec::new(), limits };
    let mut pos: usize = 0;
    loop {
        let start = pos.min(buf.len());
        let res = parser.pull(&buf[start..]);
        for ev in res.events {
            col.handle(ev);
        }
        match res.demand {
            Demand::Done => break,
            Demand::NeedBytes(_) => {
                // slice 不会再增长 → 截断。
                col.warnings.push(Warning { offset: start as u64, kind: WarnKind::Truncated });
                break;
            }
            Demand::Skip(n) => {
                pos = start.saturating_add(res.consumed).saturating_add(n as usize);
                if pos > buf.len() {
                    col.warnings.push(Warning { offset: pos as u64, kind: WarnKind::UnreachableSection });
                    break;
                }
            }
            Demand::SeekTo(p) => {
                let p = p as usize;
                if p > buf.len() {
                    col.warnings.push(Warning { offset: p as u64, kind: WarnKind::UnreachableSection });
                    break;
                }
                pos = p;
            }
        }
    }
    col
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let mut parser = crate::formats::jpeg::JpegParser;
        let col = drive_slice(&j, &mut parser, Limits::default());
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
        assert_eq!(col.exif.len(), 2);
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
