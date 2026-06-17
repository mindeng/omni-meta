//! strip_slice：全内存剥离适配器。

use alloc::vec::Vec;

use crate::error::Error;
use crate::probe::probe;
use crate::strip::{drive_strip_slice, planner_for, StripOptions, StripReport};

/// 从一整块内存缓冲剥离元数据，返回（干净字节, 报告）。
/// 无法识别格式 → `UnrecognizedFormat`；已识别但不支持 → `Unsupported`。
pub fn strip_slice(buf: &[u8], opts: StripOptions) -> Result<(Vec<u8>, StripReport), Error> {
    let fmt = probe(buf);
    let mut planner = planner_for(&fmt, opts)?;
    Ok(drive_strip_slice(buf, planner.as_mut(), fmt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strip::{RemovedKind, StripOptions};

    fn minimal_jpeg_with_exif() -> alloc::vec::Vec<u8> {
        let mut j = alloc::vec::Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]);
        let mut body = alloc::vec::Vec::new();
        body.extend_from_slice(b"Exif\0\0");
        body.extend_from_slice(&crate::strip::exif_synth::orientation_tiff(1));
        j.extend_from_slice(&[0xFF, 0xE1]);
        j.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        j.extend_from_slice(&body);
        j.extend_from_slice(&[0xFF, 0xDA, 0, 4, 1, 0, 0]); // SOS
        j.extend_from_slice(&[0xFF, 0xD9]);
        j
    }

    #[test]
    fn strips_jpeg_via_probe() {
        let j = minimal_jpeg_with_exif();
        let (out, report) = strip_slice(&j, StripOptions::aggressive()).unwrap();
        assert_eq!(report.format, crate::model::FileFormat::Jpeg);
        assert!(report.removed.contains(RemovedKind::Exif));
        let meta = crate::read_slice(&out, crate::Options::default()).unwrap();
        assert!(meta.raw.exif.is_empty());
    }

    #[test]
    fn unsupported_format_errors() {
        let gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00";
        let err = strip_slice(gif, StripOptions::default()).unwrap_err();
        assert_eq!(err, crate::Error::Unsupported);
    }

    #[test]
    fn unrecognized_format_errors() {
        let junk = [0u8, 1, 2, 3, 4, 5];
        let err = strip_slice(&junk, StripOptions::default()).unwrap_err();
        assert_eq!(err, crate::Error::UnrecognizedFormat);
    }
}
