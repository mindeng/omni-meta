//! strip_blocking：std 阻塞剥离。v1 把输入读入有界缓冲后复用 slice 引擎，整块写出。
//! 理由见 spec §7：planner 只需处理整缓冲，slice↔blocking 字节级一致为平凡真值。

use std::io::{Read, Write};

use omni_meta_core::{strip_slice, Error, StripOptions, StripReport};

const CHUNK: usize = 8192;

pub fn strip_blocking<R: Read, W: Write>(
    mut r: R,
    mut w: W,
    opts: StripOptions,
) -> Result<StripReport, Error> {
    // 读入有界缓冲（≤ max_payload_bytes）。
    let cap = opts.limits.max_payload_bytes;
    let mut input: Vec<u8> = Vec::new();
    let mut buf = [0u8; CHUNK];
    loop {
        let n = r.read(&mut buf).map_err(|_| Error::Io)?;
        if n == 0 {
            break;
        }
        if input.len().saturating_add(n) > cap {
            return Err(Error::Io); // 超界：拒绝（防 OOM）
        }
        input.extend_from_slice(&buf[..n]);
    }
    let (out, report) = strip_slice(&input, opts)?;
    w.write_all(&out).map_err(|_| Error::Io)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use omni_meta_core::{strip_slice, StripOptions};

    fn minimal_jpeg() -> Vec<u8> {
        let mut j = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]);
        let body = b"Exif\0\0II*\0\x08\0\0\0\0\0\0\0\0\0"; // 极简
        j.extend_from_slice(&[0xFF, 0xE1]);
        j.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        j.extend_from_slice(body);
        j.extend_from_slice(&[0xFF, 0xDA, 0, 4, 1, 0, 0]);
        j.extend_from_slice(&[0xFF, 0xD9]);
        j
    }

    #[test]
    fn blocking_matches_slice_byte_for_byte() {
        let j = minimal_jpeg();
        let (slice_out, _r) = strip_slice(&j, StripOptions::aggressive()).unwrap();
        let mut blocking_out: Vec<u8> = Vec::new();
        let report = strip_blocking(&j[..], &mut blocking_out, StripOptions::aggressive()).unwrap();
        assert_eq!(blocking_out, slice_out, "blocking 输出须与 slice 字节级一致");
        assert_eq!(report.format, omni_meta_core::FileFormat::Jpeg);
    }

    #[test]
    fn blocking_unsupported_errors() {
        let gif = b"GIF89a\x01\0\x01\0\0\0\0";
        let mut out: Vec<u8> = Vec::new();
        let err = strip_blocking(&gif[..], &mut out, StripOptions::default()).unwrap_err();
        assert_eq!(err, omni_meta_core::Error::Unsupported);
    }
}
