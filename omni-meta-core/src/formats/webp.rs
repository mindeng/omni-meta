//! WebP（RIFF）chunk 遍历：RIFF/WEBP 头后逐 chunk 推进。
//! VP8X/VP8/VP8L 发维度；EXIF 发 Exif 载荷；"XMP " 发 Xmp 载荷；其余 Skip。
//! 每 chunk 前进 size + (size & 1)（RIFF 偶数对齐）。

use alloc::vec::Vec;

use crate::demand::{Demand, Event, MetaParser, PayloadKind, PullResult};
use crate::model::Field;

#[derive(Debug, Default)]
pub struct WebpParser {
    saw_header: bool,
    done: bool,
    /// Remaining chunk bytes within the RIFF container. Set after header parsed.
    /// Decremented as chunks are consumed/skipped.
    riff_remaining: Option<u64>,
}

impl WebpParser {
    pub fn new() -> Self {
        Self::default()
    }
}

fn u24_le(b: &[u8]) -> u32 {
    (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16)
}

impl MetaParser for WebpParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        let mut events: Vec<Event<'a>> = Vec::new();
        if self.done {
            return PullResult { demand: Demand::Done, consumed: 0, events };
        }
        let mut pos = 0usize;
        if !self.saw_header {
            if input.len() < 12 {
                return PullResult { demand: Demand::NeedBytes(12), consumed: 0, events };
            }
            if &input[0..4] != b"RIFF" || &input[8..12] != b"WEBP" {
                self.done = true;
                return PullResult { demand: Demand::Done, consumed: 0, events };
            }
            let filesize = u32::from_le_bytes([input[4], input[5], input[6], input[7]]) as u64;
            // RIFF filesize covers "WEBP" (4) + all chunks.
            // After consuming the 12-byte header, remaining = filesize - 4.
            self.riff_remaining = Some(filesize.saturating_sub(4));
            self.saw_header = true;
            pos = 12;
        }

        loop {
            // Stop when we've consumed all chunks within the RIFF container.
            if let Some(0) = self.riff_remaining {
                self.done = true;
                return PullResult { demand: Demand::Done, consumed: pos, events };
            }

            let rest = &input[pos..];
            if rest.len() < 8 {
                return PullResult { demand: Demand::NeedBytes(8), consumed: pos, events };
            }
            let fourcc = &rest[0..4];
            let size = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
            let pad = size & 1;
            // Total bytes this chunk occupies: 8-byte header + size + pad.
            let chunk_total = (8u64).saturating_add(size as u64).saturating_add(pad as u64);

            // 维度 chunk：只需小前缀即可读出，读后 Skip 整个 data+pad。
            let dim_prefix = match fourcc {
                b"VP8X" => Some(10usize),
                b"VP8 " => Some(10usize),
                b"VP8L" => Some(5usize),
                _ => None,
            };
            if let Some(prefix) = dim_prefix {
                let need = 8 + prefix.min(size);
                if rest.len() < need {
                    return PullResult { demand: Demand::NeedBytes(need), consumed: pos, events };
                }
                let data = &rest[8..8 + prefix.min(size)];
                read_dimensions(fourcc, data, &mut events);
                let skip = (size as u64).saturating_add(pad as u64);
                // Consumed header (8) + skipped data+pad = chunk_total bytes used.
                if let Some(rem) = self.riff_remaining.as_mut() {
                    *rem = rem.saturating_sub(chunk_total);
                }
                return PullResult { demand: Demand::Skip(skip), consumed: pos + 8, events };
            }

            // 元数据 chunk：须整读 data。
            if fourcc == b"EXIF" || fourcc == b"XMP " {
                let need = match 8usize.checked_add(size) {
                    Some(v) => v,
                    None => {
                        self.done = true;
                        return PullResult { demand: Demand::Done, consumed: pos, events };
                    }
                };
                if rest.len() < need {
                    return PullResult { demand: Demand::NeedBytes(need), consumed: pos, events };
                }
                let mut data = &rest[8..8 + size];
                let kind = if fourcc == b"EXIF" {
                    // 容错可选 "Exif\0\0" 前缀
                    if data.starts_with(b"Exif\0\0") {
                        data = &data[6..];
                    }
                    PayloadKind::Exif
                } else {
                    PayloadKind::Xmp
                };
                events.push(Event::Payload { kind, data });
                // 跳过对齐填充（pad 为 0 或 1）
                let skip = pad as u64;
                if let Some(rem) = self.riff_remaining.as_mut() {
                    *rem = rem.saturating_sub(chunk_total);
                }
                if skip > 0 {
                    return PullResult { demand: Demand::Skip(skip), consumed: pos + need, events };
                }
                pos += need;
                continue;
            }

            // 其他 chunk：消费 8 字节头，Skip(data + pad)。
            let skip = (size as u64).saturating_add(pad as u64);
            if let Some(rem) = self.riff_remaining.as_mut() {
                *rem = rem.saturating_sub(chunk_total);
            }
            return PullResult { demand: Demand::Skip(skip), consumed: pos + 8, events };
        }
    }
}

fn read_dimensions<'a>(fourcc: &[u8], data: &[u8], events: &mut Vec<Event<'a>>) {
    match fourcc {
        b"VP8X" if data.len() >= 10 => {
            let w = u24_le(&data[4..7]) + 1;
            let h = u24_le(&data[7..10]) + 1;
            events.push(Event::Field(Field::Width(w)));
            events.push(Event::Field(Field::Height(h)));
        }
        b"VP8 " if data.len() >= 10 => {
            // 关键帧起始码 0x9d 0x01 0x2a 在 data[3..6]
            if data[3] == 0x9d && data[4] == 0x01 && data[5] == 0x2a {
                let w = (u16::from_le_bytes([data[6], data[7]]) & 0x3FFF) as u32;
                let h = (u16::from_le_bytes([data[8], data[9]]) & 0x3FFF) as u32;
                events.push(Event::Field(Field::Width(w)));
                events.push(Event::Field(Field::Height(h)));
            }
        }
        b"VP8L" if data.len() >= 5 && data[0] == 0x2f => {
            let bits = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
            let w = (bits & 0x3FFF) + 1;
            let h = ((bits >> 14) & 0x3FFF) + 1;
            events.push(Event::Field(Field::Width(w)));
            events.push(Event::Field(Field::Height(h)));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn riff_chunk(fourcc: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend_from_slice(fourcc);
        c.extend_from_slice(&(data.len() as u32).to_le_bytes());
        c.extend_from_slice(data);
        if data.len() % 2 == 1 {
            c.push(0); // 偶数对齐
        }
        c
    }

    fn vp8x_data(w: u32, h: u32) -> Vec<u8> {
        let mut d = vec![0u8; 10];
        // d[0]=flags，d[1..4]=reserved；width-1 @4..7，height-1 @7..10（u24 LE）
        let wm1 = w - 1;
        let hm1 = h - 1;
        d[4] = (wm1 & 0xFF) as u8;
        d[5] = ((wm1 >> 8) & 0xFF) as u8;
        d[6] = ((wm1 >> 16) & 0xFF) as u8;
        d[7] = (hm1 & 0xFF) as u8;
        d[8] = ((hm1 >> 8) & 0xFF) as u8;
        d[9] = ((hm1 >> 16) & 0xFF) as u8;
        d
    }

    fn fixture(extra: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(b"WEBP");
        body.extend_from_slice(&riff_chunk(b"VP8X", &vp8x_data(640, 480)));
        body.extend_from_slice(extra);
        let mut f = Vec::new();
        f.extend_from_slice(b"RIFF");
        f.extend_from_slice(&(body.len() as u32).to_le_bytes());
        f.extend_from_slice(&body);
        f
    }

    fn collect(buf: &[u8]) -> crate::driver::Collector {
        let mut p = WebpParser::new();
        crate::driver::drive_slice(buf, &mut p, crate::limits::Limits::default())
    }

    #[test]
    fn vp8x_dimensions_and_xmp() {
        let xmp = riff_chunk(b"XMP ", br#"<rdf:Description tiff:Make="Acme"/>"#);
        let buf = fixture(&xmp);
        let col = collect(&buf);
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Webp);
        assert_eq!(meta.unified.width, Some(640));
        assert_eq!(meta.unified.height, Some(480));
        assert!(meta.raw.xmp.iter().any(|x| x.name == "Make" && x.value == "Acme"));
    }

    #[test]
    fn exif_chunk_emitted() {
        // EXIF chunk 带完整 TIFF
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&0u16.to_le_bytes()); // 0 entries
        tiff.extend_from_slice(&0u32.to_le_bytes());
        let exif = riff_chunk(b"EXIF", &tiff);
        let buf = fixture(&exif);
        let col = collect(&buf);
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
    }

    #[test]
    fn non_webp_done_no_events() {
        let mut p = WebpParser::new();
        let res = p.pull(b"RIFF\0\0\0\0XXXX");
        assert_eq!(res.demand, Demand::Done);
        assert!(res.events.is_empty());
    }
}
