//! PNG chunk 遍历（增量状态机）：8 字节签名后逐 chunk 推进。
//! IHDR 发 Width/Height；eXIf 发 Exif 载荷；iTXt(XML:com.adobe.xmp，未压缩)发 Xmp 载荷；
//! 压缩文本块（flag=1）告警并跳过；IEND 发 Done；其余 chunk Skip(len+crc)。

use alloc::string::String;
use alloc::vec::Vec;

use crate::demand::{Demand, Event, MetaParser, PayloadKind, PullResult};
use crate::model::{Field, TextTag, TextValue, WarnKind, Warning};

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
            return PullResult {
                demand: Demand::Done,
                consumed: 0,
                events,
            };
        }
        let mut pos = 0usize;
        if !self.saw_sig {
            if input.len() < 8 {
                return PullResult {
                    demand: Demand::NeedBytes(8),
                    consumed: 0,
                    events,
                };
            }
            if input[..8] != SIG {
                self.done = true;
                return PullResult {
                    demand: Demand::Done,
                    consumed: 0,
                    events,
                };
            }
            self.saw_sig = true;
            pos = 8;
        }

        loop {
            let rest = &input[pos..];
            if rest.len() < 8 {
                return PullResult {
                    demand: Demand::NeedBytes(8),
                    consumed: pos,
                    events,
                };
            }
            let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            let ctype = &rest[4..8];

            if ctype == b"IEND" {
                self.done = true;
                return PullResult {
                    demand: Demand::Done,
                    consumed: pos + 8,
                    events,
                };
            }

            let is_meta = ctype == b"IHDR"
                || ctype == b"eXIf"
                || ctype == b"iTXt"
                || ctype == b"tEXt"
                || ctype == b"zTXt";
            if is_meta {
                // 须整读 header(8)+data(len)+crc(4)
                let need = match 8usize.checked_add(len).and_then(|v| v.checked_add(4)) {
                    Some(v) => v,
                    None => {
                        // 长度溢出 → 当作不可读，跳过数据+crc
                        self.done = true;
                        return PullResult {
                            demand: Demand::Done,
                            consumed: pos,
                            events,
                        };
                    }
                };
                if rest.len() < need {
                    return PullResult {
                        demand: Demand::NeedBytes(need),
                        consumed: pos,
                        events,
                    };
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
                        events.push(Event::Payload {
                            kind: PayloadKind::Exif,
                            data,
                        });
                    }
                    b"iTXt" => {
                        handle_itxt(data, pos as u64 + 8, &mut events);
                    }
                    b"tEXt" => {
                        handle_text(data, pos as u64 + 8, &mut events);
                    }
                    b"zTXt" => {
                        handle_ztxt(data, &mut events);
                    }
                    _ => {}
                }
                pos += need; // 跳过 crc 一并消费
                continue;
            }

            // 可跳过 chunk：消费 8 字节头，Skip(data + crc)
            let skip = (len as u64).saturating_add(4);
            return PullResult {
                demand: Demand::Skip(skip),
                consumed: pos + 8,
                events,
            };
        }
    }
}

/// 解析 iTXt 数据。
/// keyword==XML:com.adobe.xmp 且未压缩 → 发 Xmp 载荷（不变）。
/// 其它 keyword：未压缩合法 UTF-8 → Text(Utf8)；非法 UTF-8 → UnrecognizedValue；
/// 压缩 → Text(CompressedUtf8)，不报 warning。`offset` 为 chunk 数据起点。
fn handle_itxt<'a>(data: &'a [u8], offset: u64, events: &mut Vec<Event<'a>>) {
    // 布局：keyword\0 compflag(1) compmethod(1) lang\0 transkw\0 text
    let (kw, after_kw) = match split_keyword(data) {
        KwSplit::Ok(kw, rest) => (kw, rest),
        KwSplit::Malformed => return,
        KwSplit::TooLong => {
            events.push(Event::Warning(Warning {
                offset,
                kind: WarnKind::UnrecognizedValue,
            }));
            return;
        }
    };
    if after_kw.len() < 2 {
        return;
    }
    let compressed = after_kw[0] != 0;
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

    let is_xmp = kw == b"XML:com.adobe.xmp";
    if is_xmp && !compressed {
        events.push(Event::Payload {
            kind: PayloadKind::Xmp,
            data: text,
        });
        return;
    }
    if compressed {
        events.push(Event::Text(TextTag {
            keyword: latin1_to_string(kw),
            value: TextValue::CompressedUtf8(text.to_vec()),
        }));
        return;
    }
    match core::str::from_utf8(text) {
        Ok(s) => events.push(Event::Text(TextTag {
            keyword: latin1_to_string(kw),
            value: TextValue::Utf8(String::from(s)),
        })),
        Err(_) => events.push(Event::Warning(Warning {
            offset,
            kind: WarnKind::UnrecognizedValue,
        })),
    }
}

/// keyword 切分结果。
enum KwSplit<'a> {
    /// (keyword, keyword 之后的余下字节)
    Ok(&'a [u8], &'a [u8]),
    /// 无 \0 分隔 或 空 keyword —— 静默丢弃。
    Malformed,
    /// keyword > 79 字节（违反 PNG 规范）—— 调用方应发 UnrecognizedValue。
    TooLong,
}

/// 按首个 \0 切分 keyword；强制 1..=79 字节（PNG 规范）。
fn split_keyword(data: &[u8]) -> KwSplit<'_> {
    let nul = match data.iter().position(|&b| b == 0) {
        Some(p) => p,
        None => return KwSplit::Malformed,
    };
    if nul == 0 {
        return KwSplit::Malformed; // 空 keyword
    }
    if nul > 79 {
        return KwSplit::TooLong;
    }
    KwSplit::Ok(&data[..nul], &data[nul + 1..])
}

/// Latin-1 字节逐个无损映射为 UTF-8 String（永不失败、零依赖）。
fn latin1_to_string(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// 解析 tEXt：keyword\0value，全 Latin-1。`offset` 为该 chunk 数据起点（供 warning）。
fn handle_text<'a>(data: &'a [u8], offset: u64, events: &mut Vec<Event<'a>>) {
    match split_keyword(data) {
        KwSplit::Ok(kw, val) => {
            events.push(Event::Text(TextTag {
                keyword: latin1_to_string(kw),
                value: TextValue::Latin1(latin1_to_string(val)),
            }));
        }
        KwSplit::Malformed => {}
        KwSplit::TooLong => events.push(Event::Warning(Warning {
            offset,
            kind: WarnKind::UnrecognizedValue,
        })),
    }
}

/// 解析 zTXt：keyword\0 compmethod(1) <zlib 压缩字节>。
/// 保留压缩字节为 CompressedLatin1（本库不解压），不报 warning。
fn handle_ztxt<'a>(data: &'a [u8], events: &mut Vec<Event<'a>>) {
    let (kw, after_kw) = match split_keyword(data) {
        KwSplit::Ok(kw, rest) => (kw, rest),
        // zTXt 的 keyword 同受 1..=79 约束；畸形/超长均直接丢弃（不投影、无价值）。
        KwSplit::Malformed | KwSplit::TooLong => return,
    };
    if after_kw.is_empty() {
        return; // 缺 compression method 字节
    }
    let zdata = &after_kw[1..]; // 跳过 compmethod
    events.push(Event::Text(TextTag {
        keyword: latin1_to_string(kw),
        value: TextValue::CompressedLatin1(zdata.to_vec()),
    }));
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

    fn text_chunk(kw: &[u8], val: &[u8]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(kw);
        d.push(0);
        d.extend_from_slice(val);
        chunk(b"tEXt", &d)
    }

    #[test]
    fn text_chunk_parses_into_rawtext_latin1() {
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(2, 2));
        p.extend_from_slice(&text_chunk(b"Author", b"Ada Lovelace"));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        let col = collect(&p);
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
        assert!(meta.raw.text.iter().any(|t| t.keyword == "Author"
            && t.value == crate::model::TextValue::Latin1("Ada Lovelace".into())));
    }

    #[test]
    fn text_chunk_empty_keyword_or_no_nul_is_dropped_silently() {
        for data in [&b"\0value"[..], &b"noseparator"[..]] {
            let mut p = Vec::new();
            p.extend_from_slice(&SIG);
            p.extend_from_slice(&ihdr(2, 2));
            p.extend_from_slice(&chunk(b"tEXt", data));
            p.extend_from_slice(&chunk(b"IEND", &[]));
            let col = collect(&p);
            assert!(col.warnings.is_empty(), "畸形 tEXt 应静默丢弃: {data:?}");
            let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
            assert!(meta.raw.text.is_empty());
        }
    }

    #[test]
    fn text_chunk_keyword_too_long_warns_unrecognized() {
        let long_kw = [b'K'; 80]; // >79
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(2, 2));
        p.extend_from_slice(&text_chunk(&long_kw, b"v"));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        let col = collect(&p);
        assert!(
            col.warnings
                .iter()
                .any(|w| w.kind == WarnKind::UnrecognizedValue)
        );
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
        assert!(meta.raw.text.is_empty());
    }

    #[test]
    fn extracts_dimensions_exif_xmp() {
        let col = collect(&full_png());
        assert!(
            col.warnings
                .iter()
                .all(|w| w.kind == WarnKind::BadExifHeader),
            "warnings: {:?}",
            col.warnings
        );
        // eXIf 载荷被送入 exif::decode（占位 TIFF → BadExifHeader? 不，3 字节非 II/MM → 告警）
        // 为避免 EXIF 解码噪声，这里只断言 XMP 与维度经由 finalize。
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
        assert_eq!(meta.unified.width, Some(1920));
        assert_eq!(meta.unified.height, Some(1080));
        assert!(
            meta.raw
                .xmp
                .iter()
                .any(|x| x.prefix == "tiff" && x.name == "Make" && x.value == "Acme")
        );
    }

    fn itxt(keyword: &[u8], compressed: bool, text: &[u8]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(keyword);
        d.push(0); // keyword NUL
        d.push(if compressed { 1 } else { 0 }); // compflag
        d.push(0); // compmethod
        d.push(0); // lang NUL
        d.push(0); // transkw NUL
        d.extend_from_slice(text);
        chunk(b"iTXt", &d)
    }

    #[test]
    fn itxt_non_xmp_uncompressed_parses_utf8() {
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(2, 2));
        p.extend_from_slice(&itxt(b"Description", false, "héllo".as_bytes()));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        let meta = crate::driver::finalize(collect(&p), crate::model::FileFormat::Png);
        assert!(meta.raw.text.iter().any(|t| t.keyword == "Description"
            && t.value == crate::model::TextValue::Utf8("héllo".into())));
    }

    #[test]
    fn itxt_invalid_utf8_warns_unrecognized() {
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(2, 2));
        p.extend_from_slice(&itxt(b"Comment", false, &[0xFF, 0xFE]));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        let col = collect(&p);
        assert!(
            col.warnings
                .iter()
                .any(|w| w.kind == WarnKind::UnrecognizedValue)
        );
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
        assert!(meta.raw.text.is_empty());
    }

    #[test]
    fn itxt_compressed_keeps_bytes_no_warning() {
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(2, 2));
        p.extend_from_slice(&itxt(b"Description", true, &[0x78, 0x9c, 1, 2, 3]));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        let col = collect(&p);
        assert!(
            !col.warnings
                .iter()
                .any(|w| w.kind == WarnKind::CompressedChunkSkipped)
        );
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
        assert!(meta.raw.text.iter().any(|t| t.keyword == "Description"
            && matches!(t.value, crate::model::TextValue::CompressedUtf8(_))));
    }

    #[test]
    fn itxt_xmp_still_routes_to_xmp_unchanged() {
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(2, 2));
        p.extend_from_slice(&itxt_xmp(br#"<rdf:Description tiff:Make="Acme"/>"#, false));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        let meta = crate::driver::finalize(collect(&p), crate::model::FileFormat::Png);
        assert!(meta.raw.text.is_empty());
        assert!(meta.raw.xmp.iter().any(|x| x.name == "Make"));
    }

    fn ztxt(keyword: &[u8], zdata: &[u8]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(keyword);
        d.push(0); // keyword NUL
        d.push(0); // compression method
        d.extend_from_slice(zdata);
        chunk(b"zTXt", &d)
    }

    #[test]
    fn ztxt_keeps_compressed_latin1_no_warning() {
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(2, 2));
        p.extend_from_slice(&ztxt(b"Comment", &[0x78, 0x9c, 9, 8, 7]));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        let col = collect(&p);
        assert!(
            !col.warnings
                .iter()
                .any(|w| w.kind == WarnKind::CompressedChunkSkipped)
        );
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
        assert!(meta.raw.text.iter().any(|t| t.keyword == "Comment"
            && matches!(t.value, crate::model::TextValue::CompressedLatin1(_))));
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
