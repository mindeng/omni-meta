//! JPEG 段遍历：SOI 起，逐段扫描，遇 APP1 "Exif\0\0" 发出 Exif 载荷；
//! 遇 SOS/EOI 停止（后面是熵编码数据，无元数据）。单遍处理整块缓冲。

use alloc::vec::Vec;

use crate::cursor::ByteCursor;
use crate::demand::{Demand, Event, MetaParser, PayloadKind, PullResult};

pub struct JpegParser;

impl MetaParser for JpegParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        let mut events = Vec::new();
        // best-effort：截断/畸形直接停，已收集的照常返回。
        let _result = walk(input, &mut events);
        PullResult {
            demand: Demand::Done,
            consumed: input.len(),
            events,
        }
    }
}

fn walk<'a>(input: &'a [u8], events: &mut Vec<Event<'a>>) -> Option<()> {
    let mut cur = ByteCursor::new(input);
    if cur.u16_be()? != 0xFFD8 {
        return None; // 非 JPEG
    }
    loop {
        // 标记以 0xFF 开头，后跟非 0x00/0xFF 的码字；0xFF 填充字节可重复。
        let lead = cur.u8()?;
        if lead != 0xFF {
            return None;
        }
        let mut marker = cur.u8()?;
        while marker == 0xFF {
            marker = cur.u8()?;
        }
        match marker {
            0xD9 | 0xDA => return Some(()), // EOI / SOS：到此为止
            0x01 | 0xD0..=0xD7 => continue, // TEM / RSTn：无长度字段
            _ => {
                let len = cur.u16_be()?;
                if len < 2 {
                    return None;
                }
                let body = cur.take(len as usize - 2)?;
                if marker == 0xE1 && body.starts_with(b"Exif\0\0") {
                    events.push(Event::Payload {
                        kind: PayloadKind::Exif,
                        data: &body[6..],
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最小 JPEG：SOI + APP1(Exif + 3 字节假 TIFF) + EOI。
    fn jpeg_with_exif() -> Vec<u8> {
        let tiff = [0xAAu8, 0xBB, 0xCC]; // 占位 TIFF 内容
        let mut seg_body: Vec<u8> = Vec::new();
        seg_body.extend_from_slice(b"Exif\0\0");
        seg_body.extend_from_slice(&tiff);
        let len = (seg_body.len() + 2) as u16;

        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]); // SOI
        j.extend_from_slice(&[0xFF, 0xE1]); // APP1
        j.extend_from_slice(&len.to_be_bytes());
        j.extend_from_slice(&seg_body);
        j.extend_from_slice(&[0xFF, 0xD9]); // EOI
        j
    }

    #[test]
    fn emits_exif_payload() {
        let j = jpeg_with_exif();
        let mut p = JpegParser;
        let res = p.pull(&j);
        assert_eq!(res.demand, Demand::Done);
        assert_eq!(res.events.len(), 1);
        match &res.events[0] {
            Event::Payload { kind, data } => {
                assert_eq!(*kind, PayloadKind::Exif);
                assert_eq!(*data, &[0xAA, 0xBB, 0xCC][..]); // "Exif\0\0" 已剥离
            }
            _ => panic!("expected payload"),
        }
    }

    #[test]
    fn non_jpeg_emits_nothing() {
        let mut p = JpegParser;
        let res = p.pull(&[0x00, 0x01, 0x02, 0x03]);
        assert_eq!(res.demand, Demand::Done);
        assert!(res.events.is_empty());
    }
}
