//! 魔数嗅探。本计划只识别 JPEG，其余归 Unknown（后续计划扩展）。

use crate::model::FileFormat;

pub fn probe(buf: &[u8]) -> FileFormat {
    if buf.len() >= 2 && buf[0] == 0xFF && buf[1] == 0xD8 {
        FileFormat::Jpeg
    } else {
        FileFormat::Unknown
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
}
