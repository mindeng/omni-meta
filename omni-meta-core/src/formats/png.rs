//! PNG chunk 遍历（增量状态机）：8 字节签名后逐 chunk 推进。
//! IHDR 发 Width/Height；eXIf 发 Exif 载荷；iTXt(XML:com.adobe.xmp，未压缩)发 Xmp 载荷；
//! 压缩文本块（flag=1）告警并跳过；IEND 发 Done；其余 chunk Skip(len+crc)。

use alloc::vec::Vec;

use crate::demand::{Demand, Event, MetaParser, PayloadKind, PullResult};
use crate::model::{Field, WarnKind, Warning};

const SIG: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

#[derive(Debug, Default)]
pub struct PngParser {
    saw_sig: bool,
    done: bool,
}

impl PngParser {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MetaParser for PngParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        let mut events: Vec<Event<'a>> = Vec::new();
        if self.done {
            return PullResult { demand: Demand::Done, consumed: 0, events };
        }
        let mut pos = 0usize;
        if !self.saw_sig {
            if input.len() < 8 {
                return PullResult { demand: Demand::NeedBytes(8), consumed: 0, events };
            }
            if input[..8] != SIG {
                self.done = true;
                return PullResult { demand: Demand::Done, consumed: 0, events };
            }
            self.saw_sig = true;
            pos = 8;
        }

        loop {
            let rest = &input[pos..];
            if rest.len() < 8 {
                return PullResult { demand: Demand::NeedBytes(8), consumed: pos, events };
            }
            let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            let ctype = &rest[4..8];

            if ctype == b"IEND" {
                self.done = true;
                return PullResult { demand: Demand::Done, consumed: pos + 8, events };
            }

            let is_meta = ctype == b"IHDR" || ctype == b"eXIf" || ctype == b"iTXt";
            if is_meta {
                // 须整读 header(8)+data(len)+crc(4)
                let need = match 8usize.checked_add(len).and_then(|v| v.checked_add(4)) {
                    Some(v) => v,
                    None => {
                        // 长度溢出 → 当作不可读，跳过数据+crc
                        self.done = true;
                        return PullResult { demand: Demand::Done, consumed: pos, events };
                    }
                };
                if rest.len() < need {
                    return PullResult { demand: Demand::NeedBytes(need), consumed: pos, events };
                }
                let data = &rest[8..8 + len];
                match ctype {
                    b"IHDR" => {
                        if data.len() >= 8 {
                            let w = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                            let h = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
                            events.push(Event::Field(Field::Width(w)));
                            events.push(Event::Field(Field::Height(h)));
                        }
                    }
                    b"eXIf" => {
                        events.push(Event::Payload { kind: PayloadKind::Exif, data });
                    }
                    b"iTXt" => {
                        handle_itxt(data, pos, &mut events);
                    }
                    _ => {}
                }
                pos += need; // 跳过 crc 一并消费
                continue;
            }

            // 可跳过 chunk：消费 8 字节头，Skip(data + crc)
            let skip = (len as u64).saturating_add(4);
            return PullResult { demand: Demand::Skip(skip), consumed: pos + 8, events };
        }
    }
}

/// 解析 iTXt 数据；仅当 keyword 为 XMP 且未压缩时发 Xmp 载荷，压缩则告警。
fn handle_itxt<'a>(data: &'a [u8], chunk_pos: usize, events: &mut Vec<Event<'a>>) {
    // keyword\0 compflag(1) compmethod(1) lang\0 transkw\0 text
    let kw_end = match data.iter().position(|&b| b == 0) {
        Some(p) => p,
        None => return,
    };
    if &data[..kw_end] != b"XML:com.adobe.xmp" {
        return;
    }
    let after_kw = &data[kw_end + 1..];
    if after_kw.len() < 2 {
        return;
    }
    let compressed = after_kw[0] != 0;
    if compressed {
        events.push(Event::Warning(Warning {
            offset: chunk_pos as u64,
            kind: WarnKind::CompressedChunkSkipped,
        }));
        return;
    }
    // 跳过 compflag(1)+compmethod(1)，再跳过 lang\0 与 transkw\0
    let rest = &after_kw[2..];
    let lang_end = match rest.iter().position(|&b| b == 0) {
        Some(p) => p,
        None => return,
    };
    let rest2 = &rest[lang_end + 1..];
    let tk_end = match rest2.iter().position(|&b| b == 0) {
        Some(p) => p,
        None => return,
    };
    let text = &rest2[tk_end + 1..];
    events.push(Event::Payload { kind: PayloadKind::Xmp, data: text });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 拼一个 PNG chunk：len(4 BE) + type(4) + data + crc(4，置 0)。
    fn chunk(ctype: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend_from_slice(&(data.len() as u32).to_be_bytes());
        c.extend_from_slice(ctype);
        c.extend_from_slice(data);
        c.extend_from_slice(&[0, 0, 0, 0]); // crc 不校验
        c
    }

    fn ihdr(w: u32, h: u32) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&w.to_be_bytes());
        d.extend_from_slice(&h.to_be_bytes());
        d.extend_from_slice(&[8, 6, 0, 0, 0]); // bitdepth/colortype/...
        chunk(b"IHDR", &d)
    }

    fn itxt_xmp(packet: &[u8], compressed: bool) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(b"XML:com.adobe.xmp");
        d.push(0); // keyword NUL
        d.push(if compressed { 1 } else { 0 }); // compression flag
        d.push(0); // compression method
        d.push(0); // language tag NUL
        d.push(0); // translated keyword NUL
        d.extend_from_slice(packet);
        chunk(b"iTXt", &d)
    }

    fn full_png() -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(1920, 1080));
        p.extend_from_slice(&chunk(b"eXIf", &[0xAA, 0xBB, 0xCC])); // 占位 TIFF
        p.extend_from_slice(&itxt_xmp(br#"<rdf:Description tiff:Make="Acme"/>"#, false));
        p.extend_from_slice(&chunk(b"IDAT", &[1, 2, 3, 4]));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        p
    }

    fn collect(buf: &[u8]) -> crate::driver::Collector {
        let mut p = PngParser::new();
        crate::driver::drive_slice(buf, &mut p, crate::limits::Limits::default())
    }

    #[test]
    fn extracts_dimensions_exif_xmp() {
        let col = collect(&full_png());
        assert!(col.warnings.iter().all(|w| w.kind == WarnKind::BadExifHeader), "warnings: {:?}", col.warnings);
        // eXIf 载荷被送入 exif::decode（占位 TIFF → BadExifHeader? 不，3 字节非 II/MM → 告警）
        // 为避免 EXIF 解码噪声，这里只断言 XMP 与维度经由 finalize。
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
        assert_eq!(meta.unified.width, Some(1920));
        assert_eq!(meta.unified.height, Some(1080));
        assert!(meta.raw.xmp.iter().any(|x| x.prefix == "tiff" && x.name == "Make" && x.value == "Acme"));
    }

    #[test]
    fn compressed_itxt_warns_and_skips() {
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(2, 2));
        p.extend_from_slice(&itxt_xmp(b"ignored", true)); // compressed
        p.extend_from_slice(&chunk(b"IEND", &[]));
        let col = collect(&p);
        assert!(col.warnings.iter().any(|w| w.kind == WarnKind::CompressedChunkSkipped));
        assert!(col.xmp.is_empty());
    }

    #[test]
    fn non_png_signature_done_no_events() {
        let mut p = PngParser::new();
        let res = p.pull(&[0u8; 8]);
        assert_eq!(res.demand, Demand::Done);
        assert!(res.events.is_empty());
    }

    #[test]
    fn truncated_chunk_requests_more() {
        // 签名 + 声称 len=100 的 eXIf，但数据不足
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&100u32.to_be_bytes());
        p.extend_from_slice(b"eXIf");
        p.extend_from_slice(&[1, 2, 3]); // 远不足 100
        let mut parser = PngParser::new();
        let res = parser.pull(&p);
        assert!(matches!(res.demand, Demand::NeedBytes(_)));
    }
}
