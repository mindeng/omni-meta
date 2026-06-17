//! blocking / seek 适配器端到端 + 与 read_slice 一致。

use omni_meta::{Options, Orientation, read_blocking, read_seek, read_slice};
use std::io::Cursor;

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
fn blocking_extracts_fields() {
    let j = jpeg_with_exif();
    let meta = read_blocking(&j[..], Options::default()).expect("parse");
    assert_eq!(meta.unified.camera_make.as_deref(), Some("Acme"));
    assert_eq!(meta.unified.orientation, Some(Orientation::Rotate90));
}

#[test]
fn seek_matches_slice() {
    let j = jpeg_with_exif();
    let want = read_slice(&j, Options::default()).unwrap();
    let got = read_seek(Cursor::new(&j), Options::default()).unwrap();
    assert_eq!(got, want);
}

#[test]
fn blocking_unrecognized_errors() {
    assert!(read_blocking(&[0u8, 1, 2][..], Options::default()).is_err());
}
