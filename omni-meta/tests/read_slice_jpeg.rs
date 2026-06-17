//! 端到端：从带 EXIF 的 JPEG 字节经公开 API 读出统一字段。

use omni_meta::{Error, FileFormat, Options, Orientation, read_slice};

/// 构造小端 TIFF：Make="Acme"(0x010F) + Orientation=6(0x0112)。
fn make_tiff() -> Vec<u8> {
    let mut t: Vec<u8> = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    t.extend_from_slice(&2u16.to_le_bytes());
    t.extend_from_slice(&0x010Fu16.to_le_bytes());
    t.extend_from_slice(&2u16.to_le_bytes());
    t.extend_from_slice(&5u32.to_le_bytes());
    t.extend_from_slice(&38u32.to_le_bytes());
    t.extend_from_slice(&0x0112u16.to_le_bytes());
    t.extend_from_slice(&3u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&6u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    t.extend_from_slice(b"Acme\0");
    t
}

fn jpeg_with_exif() -> Vec<u8> {
    let tiff = make_tiff();
    let mut seg_body: Vec<u8> = Vec::new();
    seg_body.extend_from_slice(b"Exif\0\0");
    seg_body.extend_from_slice(&tiff);
    let len = (seg_body.len() + 2) as u16;
    let mut j: Vec<u8> = Vec::new();
    j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
    j.extend_from_slice(&len.to_be_bytes());
    j.extend_from_slice(&seg_body);
    j.extend_from_slice(&[0xFF, 0xD9]);
    j
}

#[test]
fn extracts_unified_fields_from_jpeg() {
    let j = jpeg_with_exif();
    let meta = read_slice(&j, Options::default()).expect("should parse");
    assert_eq!(meta.format, FileFormat::Jpeg);
    assert_eq!(meta.unified.camera_make.as_deref(), Some("Acme"));
    assert_eq!(meta.unified.camera_model, None);
    assert_eq!(meta.unified.orientation, Some(Orientation::Rotate90));
    assert!(meta.warnings.is_empty(), "warnings: {:?}", meta.warnings);
    assert_eq!(meta.raw.exif.len(), 2);
}

#[test]
fn unrecognized_format_errors() {
    let err = read_slice(&[0x00, 0x01, 0x02], Options::default());
    assert_eq!(err, Err(Error::UnrecognizedFormat));
}
