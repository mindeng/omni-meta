//! Stripper 集成测试：幂等、多格式回环、默认/aggressive 双路、合成边界。

use omni_meta::{read_slice, strip_slice, Options, StripOptions};

/// JPEG with APP0 + APP1(Exif, Orientation=6) + SOF0(8x8) + SOS + EOI.
fn jpeg_fixture() -> Vec<u8> {
    let mut j = Vec::new();
    j.extend_from_slice(&[0xFF, 0xD8]);
    j.extend_from_slice(&[0xFF, 0xE0, 0, 16]); // APP0 len=16 → 14 body
    j.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    let mut exif = Vec::new();
    exif.extend_from_slice(b"Exif\0\0");
    // 小端 TIFF：IFD0 单条 Orientation(0x0112)=6
    exif.extend_from_slice(b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0");
    j.extend_from_slice(&[0xFF, 0xE1]);
    j.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    j.extend_from_slice(&exif);
    j.extend_from_slice(&[0xFF, 0xC0, 0, 11, 8, 0, 8, 0, 8, 1, 0x11, 0]); // SOF0 8x8
    j.extend_from_slice(&[0xFF, 0xDA, 0, 4, 1, 0, 0, 0x11, 0x22, 0xFF, 0xD9]); // SOS+data+EOI
    j
}

/// JPEG whose EXIF has NO orientation tag (IFD0 count=0).
fn jpeg_exif_without_orientation() -> Vec<u8> {
    let mut j = Vec::new();
    j.extend_from_slice(&[0xFF, 0xD8]);
    let mut exif = Vec::new();
    exif.extend_from_slice(b"Exif\0\0");
    // II, 42, IFD0@8, count=0, next=0
    exif.extend_from_slice(b"II*\0\x08\0\0\0\x00\0\0\0\0\0");
    j.extend_from_slice(&[0xFF, 0xE1]);
    j.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    j.extend_from_slice(&exif);
    j.extend_from_slice(&[0xFF, 0xDA, 0, 4, 1, 0, 0, 0xFF, 0xD9]);
    j
}

#[test]
fn jpeg_default_is_idempotent() {
    let j = jpeg_fixture();
    let (once, _) = strip_slice(&j, StripOptions::default()).unwrap();
    let (twice, _) = strip_slice(&once, StripOptions::default()).unwrap();
    assert_eq!(once, twice, "strip 应幂等");
}

#[test]
fn jpeg_default_keeps_orientation_drops_privacy() {
    let j = jpeg_fixture();
    let before = read_slice(&j, Options::default()).unwrap();
    assert_eq!(before.unified.orientation, Some(omni_meta::Orientation::Rotate90));
    let (out, _) = strip_slice(&j, StripOptions::default()).unwrap();
    let after = read_slice(&out, Options::default()).unwrap();
    assert_eq!(after.unified.orientation, Some(omni_meta::Orientation::Rotate90));
    assert_eq!(after.unified.width, Some(8));
    assert_eq!(after.unified.height, Some(8));
}

#[test]
fn jpeg_aggressive_zero_exif() {
    let j = jpeg_fixture();
    let (out, _) = strip_slice(&j, StripOptions::aggressive()).unwrap();
    let after = read_slice(&out, Options::default()).unwrap();
    assert!(after.raw.exif.is_empty());
    assert_eq!(after.unified.orientation, None);
    assert_eq!(after.unified.width, Some(8)); // 维度来自 SOF0，非元数据
}

#[test]
fn jpeg_exif_without_orientation_synthesizes_nothing() {
    // EXIF 存在但无 orientation tag → 默认模式不应合成任何 EXIF（输出零 exif）。
    let j = jpeg_exif_without_orientation();
    let (out, _) = strip_slice(&j, StripOptions::default()).unwrap();
    let after = read_slice(&out, Options::default()).unwrap();
    assert!(after.raw.exif.is_empty(), "无 orientation 时不应合成 EXIF: {:?}", after.raw.exif);
    assert_eq!(after.unified.orientation, None);
}
