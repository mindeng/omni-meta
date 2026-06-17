//! 最小 EXIF 合成（keep_orientation 用）：只含一条 Orientation(0x0112) 的小端 TIFF，
//! 及 JPEG/PNG/WebP 各自的容器封装。

use alloc::vec::Vec;

use super::crc32::crc32;

/// 构造一个小端 TIFF：IFD0 含单条 Orientation=val（SHORT，内联）。约 26 字节。
pub fn orientation_tiff(val: u16) -> Vec<u8> {
    let mut t = Vec::with_capacity(26);
    t.extend_from_slice(b"II"); // little-endian
    t.extend_from_slice(&42u16.to_le_bytes()); // magic
    t.extend_from_slice(&8u32.to_le_bytes()); // IFD0 @ offset 8
    t.extend_from_slice(&1u16.to_le_bytes()); // count = 1
    // entry: tag=0x0112, type=SHORT(3), count=1, value=val（内联，左对齐 4 字节）
    t.extend_from_slice(&0x0112u16.to_le_bytes());
    t.extend_from_slice(&3u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&val.to_le_bytes());
    t.extend_from_slice(&[0u8, 0]); // value 字段剩余 2 字节填充
    t.extend_from_slice(&0u32.to_le_bytes()); // next IFD = 0
    t
}

/// JPEG APP1 段：FFE1 + len(2 BE，含 len 字段自身) + "Exif\0\0" + TIFF。
pub fn jpeg_app1_exif(tiff: &[u8]) -> Vec<u8> {
    let body_len = 6 + tiff.len(); // "Exif\0\0" + TIFF
    let seg_len = body_len + 2; // 含 len 字段 2 字节
    let mut s = Vec::with_capacity(seg_len + 2);
    s.extend_from_slice(&[0xFF, 0xE1]);
    s.extend_from_slice(&(seg_len as u16).to_be_bytes());
    s.extend_from_slice(b"Exif\0\0");
    s.extend_from_slice(tiff);
    s
}

/// PNG eXIf chunk：len(4 BE) + "eXIf" + tiff + crc(4 BE，覆盖 type+data)。
pub fn png_exif_chunk(tiff: &[u8]) -> Vec<u8> {
    let mut c = Vec::with_capacity(12 + tiff.len());
    c.extend_from_slice(&(tiff.len() as u32).to_be_bytes());
    c.extend_from_slice(b"eXIf");
    c.extend_from_slice(tiff);
    let mut crc_input = Vec::with_capacity(4 + tiff.len());
    crc_input.extend_from_slice(b"eXIf");
    crc_input.extend_from_slice(tiff);
    c.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    c
}

/// WebP EXIF chunk：fourcc "EXIF" + size(4 LE) + data (+1 pad 若 size 为奇数)。
pub fn webp_exif_chunk(tiff: &[u8]) -> Vec<u8> {
    let mut c = Vec::with_capacity(8 + tiff.len() + 1);
    c.extend_from_slice(b"EXIF");
    c.extend_from_slice(&(tiff.len() as u32).to_le_bytes());
    c.extend_from_slice(tiff);
    if tiff.len() % 2 == 1 {
        c.push(0);
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Limits;

    #[test]
    fn synthesized_tiff_decodes_back_to_orientation() {
        let tiff = orientation_tiff(6);
        let mut tags = alloc::vec::Vec::new();
        let mut warns = alloc::vec::Vec::new();
        crate::codecs::exif::decode(&tiff, &mut tags, &mut warns, &Limits::default());
        assert!(warns.is_empty(), "warns: {:?}", warns);
        assert!(tags.iter().any(|t| t.tag == 0x0112
            && t.value == crate::model::Value::U16(6)));
    }

    #[test]
    fn jpeg_app1_wraps_exif_prefix_and_tiff() {
        let seg = jpeg_app1_exif(&orientation_tiff(1));
        // FFE1 + len(2 BE) + "Exif\0\0" + TIFF
        assert_eq!(&seg[0..2], &[0xFF, 0xE1]);
        let len = u16::from_be_bytes([seg[2], seg[3]]) as usize;
        assert_eq!(len, seg.len() - 2); // 段长含 len 字段自身、不含 marker
        assert_eq!(&seg[4..10], b"Exif\0\0");
    }

    #[test]
    fn png_exif_chunk_has_valid_crc() {
        let tiff = orientation_tiff(8);
        let chunk = png_exif_chunk(&tiff);
        // len(4 BE) + "eXIf" + tiff + crc(4 BE)
        let len = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as usize;
        assert_eq!(len, tiff.len());
        assert_eq!(&chunk[4..8], b"eXIf");
        let crc_off = 8 + tiff.len();
        let crc = u32::from_be_bytes([
            chunk[crc_off], chunk[crc_off + 1], chunk[crc_off + 2], chunk[crc_off + 3],
        ]);
        // CRC 覆盖 type + data
        let mut crc_input = alloc::vec::Vec::new();
        crc_input.extend_from_slice(b"eXIf");
        crc_input.extend_from_slice(&tiff);
        assert_eq!(crc, super::super::crc32::crc32(&crc_input));
    }

    #[test]
    fn webp_exif_chunk_fourcc_and_size() {
        let tiff = orientation_tiff(3);
        let chunk = webp_exif_chunk(&tiff);
        // "EXIF" + size(4 LE) + data (+pad 若奇数)
        assert_eq!(&chunk[0..4], b"EXIF");
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as usize;
        assert_eq!(size, tiff.len());
    }
}
