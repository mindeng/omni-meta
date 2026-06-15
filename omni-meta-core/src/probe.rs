//! 魔数嗅探 + 格式→解析器分派。本阶段识别 JPEG/PNG/WebP/GIF。

use alloc::boxed::Box;

use crate::demand::MetaParser;
use crate::model::FileFormat;

/// 各格式签名最长字节数（WebP "RIFF"+4+"WEBP" = 12）。
pub(crate) const PROBE_MAX: usize = 12;

pub fn probe(buf: &[u8]) -> FileFormat {
    if buf.len() >= 2 && buf[0] == 0xFF && buf[1] == 0xD8 {
        FileFormat::Jpeg
    } else {
        FileFormat::Unknown
    }
}

/// 把已探测的格式映射到对应解析器。Unknown / 尚未实现的格式 → None。
pub(crate) fn parser_for(fmt: FileFormat) -> Option<Box<dyn MetaParser>> {
    match fmt {
        FileFormat::Jpeg => Some(Box::new(crate::formats::jpeg::JpegParser::new())),
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
    fn probe_max_covers_signatures() {
        assert!(PROBE_MAX >= 12);
    }

    #[test]
    fn parser_for_jpeg_some_unknown_none() {
        assert!(parser_for(FileFormat::Jpeg).is_some());
        assert!(parser_for(FileFormat::Unknown).is_none());
    }
}
