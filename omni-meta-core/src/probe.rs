//! 魔数嗅探 + 格式→解析器分派。本阶段识别 JPEG/PNG/WebP/GIF。

use alloc::boxed::Box;

use crate::demand::MetaParser;
use crate::limits::Limits;
use crate::model::FileFormat;

/// 探测窗口上界：EBML DocType（区分 MKV/WebM）可能落在头部数十字节内。
pub(crate) const PROBE_MAX: usize = 64;
// 编译期断言：PROBE_MAX 必须覆盖最长签名（WebP = 12 字节）。
const _: () = assert!(PROBE_MAX >= 12);

pub fn probe(buf: &[u8]) -> FileFormat {
    if buf.len() >= 2 && buf[0] == 0xFF && buf[1] == 0xD8 {
        return FileFormat::Jpeg;
    }
    if buf.len() >= 8
        && buf[..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
    {
        return FileFormat::Png;
    }
    if buf.len() >= 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"WEBP" {
        return FileFormat::Webp;
    }
    if buf.len() >= 6 && (&buf[0..6] == b"GIF87a" || &buf[0..6] == b"GIF89a") {
        return FileFormat::Gif;
    }
    // ISO-BMFF：偏移 4 处的 "ftyp" box，major brand 在 [8..12]。
    if buf.len() >= 12 && &buf[4..8] == b"ftyp" {
        return brand_to_format(&buf[8..12]);
    }
    // EBML（Matroska/WebM）：魔数 1A45DFA3 在偏移 0。
    if buf.len() >= 4 && buf[0..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        return ebml_format(buf);
    }
    FileFormat::Unknown
}

/// 把 ftyp major brand 映射到 FileFormat。未知品牌但确为 ftyp → Mp4（ISO-BMFF 兜底）。
fn brand_to_format(brand: &[u8]) -> FileFormat {
    match brand {
        b"avif" | b"avis" => FileFormat::Avif,
        b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"mif1"
        | b"msf1" => FileFormat::Heif,
        b"qt  " => FileFormat::Mov,
        // isom/iso2/mp41/mp42/M4V /M4A /dash/avc1… 及其余未知 ISO-BMFF
        _ => FileFormat::Mp4,
    }
}

/// 魔数已匹配。在已缓冲头部内定位 DocType → Mkv/Webm；尚不可见且未达 PROBE_MAX
/// → Unknown（请求更多字节）；达 PROBE_MAX 仍无 → 默认 Mkv（给出确定答案）。
fn ebml_format(buf: &[u8]) -> FileFormat {
    if let Some(dt) = find_doctype(buf) {
        return if dt == b"webm" { FileFormat::Webm } else { FileFormat::Mkv };
    }
    if buf.len() >= PROBE_MAX {
        return FileFormat::Mkv;
    }
    FileFormat::Unknown
}

/// 在前 PROBE_MAX 字节内、按 EBML 结构定位 DocType（id 0x4282）并读取其字符串值。
/// 头部/字符串尚未完整缓冲 → None（继续等待）。结构化解析，避免裸字节误配。
fn find_doctype(buf: &[u8]) -> Option<&[u8]> {
    let scan = &buf[..buf.len().min(PROBE_MAX)];
    let hdr = crate::containers::ebml::read_element_header(scan)?;
    if hdr.id != 0x1A45_DFA3 {
        return None; // 非 EBML 头
    }
    let hdr_len = usize::try_from(hdr.header_len).ok()?;
    let payload = scan.get(hdr_len..)?;
    for (child, p) in crate::containers::ebml::iter_child_elements(payload) {
        if child.id == 0x4282 {
            return Some(p);
        }
    }
    None
}

/// 把已探测的格式映射到对应解析器。Unknown / 尚未实现的格式 → None。
pub(crate) fn parser_for(fmt: FileFormat, limits: Limits) -> Option<Box<dyn MetaParser>> {
    match fmt {
        FileFormat::Jpeg => Some(Box::new(crate::formats::jpeg::JpegParser::new())),
        FileFormat::Png => Some(Box::new(crate::formats::png::PngParser::new())),
        FileFormat::Webp => Some(Box::new(crate::formats::webp::WebpParser::new())),
        FileFormat::Gif => Some(Box::new(crate::formats::gif::GifParser::new())),
        FileFormat::Heif | FileFormat::Avif | FileFormat::Mp4 | FileFormat::Mov => {
            Some(Box::new(crate::formats::bmff::BmffParser::with_limits(limits)))
        }
        FileFormat::Mkv | FileFormat::Webm => {
            Some(Box::new(crate::formats::ebml::EbmlParser::new()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_jpeg_soi() {
        assert_eq!(probe(&[0xFF, 0xD8, 0xFF, 0xE0]), FileFormat::Jpeg);
    }

    #[test]
    fn unknown_for_others_and_short_input() {
        assert_eq!(probe(&[0x89, 0x50]), FileFormat::Unknown);
        assert_eq!(probe(&[0xFF]), FileFormat::Unknown);
        assert_eq!(probe(&[]), FileFormat::Unknown);
    }

    #[test]
    fn parser_for_jpeg_some_unknown_none() {
        assert!(parser_for(FileFormat::Jpeg, Limits::default()).is_some());
        assert!(parser_for(FileFormat::Unknown, Limits::default()).is_none());
    }

    #[test]
    fn detects_png_signature() {
        let sig = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        assert_eq!(probe(&sig), FileFormat::Png);
        assert!(parser_for(FileFormat::Png, Limits::default()).is_some());
    }

    #[test]
    fn detects_webp_signature() {
        use alloc::vec::Vec;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(b"WEBP");
        assert_eq!(probe(&b), FileFormat::Webp);
        assert!(parser_for(FileFormat::Webp, Limits::default()).is_some());
    }

    #[test]
    fn detects_gif_signature() {
        assert_eq!(probe(b"GIF89a\0\0\0\0\0\0\0"), FileFormat::Gif);
        assert_eq!(probe(b"GIF87a\0\0\0\0\0\0\0"), FileFormat::Gif);
        assert!(parser_for(FileFormat::Gif, Limits::default()).is_some());
    }

    fn ftyp(major: &[u8; 4]) -> alloc::vec::Vec<u8> {
        let mut b = alloc::vec::Vec::new();
        b.extend_from_slice(&20u32.to_be_bytes()); // size
        b.extend_from_slice(b"ftyp");
        b.extend_from_slice(major);
        b.extend_from_slice(&0u32.to_be_bytes());   // minor version
        b.extend_from_slice(b"mif1");               // 一个兼容品牌
        b
    }

    #[test]
    fn detects_bmff_brands() {
        assert_eq!(probe(&ftyp(b"heic")), FileFormat::Heif);
        assert_eq!(probe(&ftyp(b"mif1")), FileFormat::Heif);
        assert_eq!(probe(&ftyp(b"avif")), FileFormat::Avif);
        assert_eq!(probe(&ftyp(b"qt  ")), FileFormat::Mov);
        assert_eq!(probe(&ftyp(b"isom")), FileFormat::Mp4);
        // 未知品牌但确为 ftyp → 归类 Mp4（ISO-BMFF 兜底）
        assert_eq!(probe(&ftyp(b"zzzz")), FileFormat::Mp4);
    }

    #[test]
    fn bmff_parsers_wired() {
        assert!(parser_for(FileFormat::Heif, Limits::default()).is_some());
        assert!(parser_for(FileFormat::Avif, Limits::default()).is_some());
        assert!(parser_for(FileFormat::Mp4, Limits::default()).is_some());
        assert!(parser_for(FileFormat::Mov, Limits::default()).is_some());
    }

    fn ebml(doctype: &[u8]) -> alloc::vec::Vec<u8> {
        // EBML 头 { DocType } —— 用 8 字节 vint size 编码
        let mut dt = alloc::vec::Vec::new();
        dt.extend_from_slice(&[0x42, 0x82, 0x01]);
        dt.extend_from_slice(&(doctype.len() as u64).to_be_bytes()[1..]);
        dt.extend_from_slice(doctype);
        let mut hdr = alloc::vec::Vec::new();
        hdr.extend_from_slice(&[0x1A, 0x45, 0xDF, 0xA3, 0x01]);
        hdr.extend_from_slice(&(dt.len() as u64).to_be_bytes()[1..]);
        hdr.extend_from_slice(&dt);
        hdr
    }

    #[test]
    fn detects_mkv_and_webm_via_doctype() {
        assert_eq!(probe(&ebml(b"webm")), FileFormat::Webm);
        assert_eq!(probe(&ebml(b"matroska")), FileFormat::Mkv);
        assert!(parser_for(FileFormat::Mkv, Limits::default()).is_some());
        assert!(parser_for(FileFormat::Webm, Limits::default()).is_some());
    }

    #[test]
    fn read_slice_recognizes_heic_empty_meta() {
        use crate::adapters::slice::{read_slice, Options};
        let buf = ftyp(b"heic");
        let meta = read_slice(&buf, Options::default()).unwrap();
        assert_eq!(meta.format, FileFormat::Heif);
        // A1 不抽取任何字段，但必须干净返回、无警告。
        assert!(meta.warnings.is_empty(), "warnings: {:?}", meta.warnings);
        assert_eq!(meta.unified.width, None);
        assert!(meta.raw.exif.is_empty());
    }
}
