//! 魔数嗅探 + 格式→解析器分派。本阶段识别 JPEG/PNG/WebP/GIF。

use alloc::boxed::Box;

use crate::demand::MetaParser;
use crate::model::FileFormat;

/// 各格式签名最长字节数（WebP "RIFF"+4+"WEBP" = 12）。
pub(crate) const PROBE_MAX: usize = 12;
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
    FileFormat::Unknown
}

/// 把已探测的格式映射到对应解析器。Unknown / 尚未实现的格式 → None。
pub(crate) fn parser_for(fmt: FileFormat) -> Option<Box<dyn MetaParser>> {
    match fmt {
        FileFormat::Jpeg => Some(Box::new(crate::formats::jpeg::JpegParser::new())),
        FileFormat::Png => Some(Box::new(crate::formats::png::PngParser::new())),
        FileFormat::Webp => Some(Box::new(crate::formats::webp::WebpParser::new())),
        FileFormat::Gif => Some(Box::new(crate::formats::gif::GifParser::new())),
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
        assert!(parser_for(FileFormat::Jpeg).is_some());
        assert!(parser_for(FileFormat::Unknown).is_none());
    }

    #[test]
    fn detects_png_signature() {
        let sig = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        assert_eq!(probe(&sig), FileFormat::Png);
        assert!(parser_for(FileFormat::Png).is_some());
    }

    #[test]
    fn detects_webp_signature() {
        use alloc::vec::Vec;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(b"WEBP");
        assert_eq!(probe(&b), FileFormat::Webp);
        assert!(parser_for(FileFormat::Webp).is_some());
    }

    #[test]
    fn detects_gif_signature() {
        assert_eq!(probe(b"GIF89a\0\0\0\0\0\0\0"), FileFormat::Gif);
        assert_eq!(probe(b"GIF87a\0\0\0\0\0\0\0"), FileFormat::Gif);
        assert!(parser_for(FileFormat::Gif).is_some());
    }
}
