//! 差分测试：read_slice / read_blocking / read_seek / push 对同一输入逐字段一致。

use omni_meta::{read_blocking, read_seek, read_slice, Metadata, Options, Outcome, PushParser};
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

fn wrap_jpeg(pre_segments: &[u8], with_exif: bool, eoi: bool) -> Vec<u8> {
    let mut j: Vec<u8> = Vec::new();
    j.extend_from_slice(&[0xFF, 0xD8]); // SOI
    j.extend_from_slice(pre_segments);
    if with_exif {
        let tiff = make_tiff();
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(b"Exif\0\0");
        body.extend_from_slice(&tiff);
        let len = (body.len() + 2) as u16;
        j.extend_from_slice(&[0xFF, 0xE1]);
        j.extend_from_slice(&len.to_be_bytes());
        j.extend_from_slice(&body);
    }
    if eoi {
        j.extend_from_slice(&[0xFF, 0xD9]);
    }
    j
}

/// EXIF-first 的常规 JPEG。
fn fixture_plain() -> Vec<u8> {
    wrap_jpeg(&[], true, true)
}

/// APP1 之前有大的非元数据段（行使 Skip）。
fn fixture_large_nonmeta() -> Vec<u8> {
    let mut app0: Vec<u8> = Vec::new();
    app0.extend_from_slice(&[0xFF, 0xE0]);
    app0.extend_from_slice(&202u16.to_be_bytes()); // body 200
    app0.extend_from_slice(&[0u8; 200]);
    wrap_jpeg(&app0, true, true)
}

/// APP0 段体 9000 字节，超过 read_seek 的 8192 字节读取块。
/// 第一次 read() 只能消费 8192 字节，剩余跳过字节仍由驱动持有，
/// 驱动因此向上层返回 SkipHint，触发 read_seek 的原生 seek 路径。
fn fixture_huge_nonmeta() -> Vec<u8> {
    let mut app0: Vec<u8> = Vec::new();
    app0.extend_from_slice(&[0xFF, 0xE0]);
    app0.extend_from_slice(&9002u16.to_be_bytes()); // length field = body(9000) + 2
    app0.extend_from_slice(&[0u8; 9000]);
    wrap_jpeg(&app0, true, true)
}

/// 截断在 APP1 段体中间（声明 len 远大于实际）。
fn fixture_truncated() -> Vec<u8> {
    let mut j: Vec<u8> = Vec::new();
    j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
    j.extend_from_slice(&200u16.to_be_bytes());
    j.extend_from_slice(b"Exif\0\0");
    j.extend_from_slice(&[0xAA, 0xBB]); // body 严重不足
    j
}

fn push_drive(bytes: &[u8], opts: Options, chunk: usize) -> Result<Metadata, omni_meta::Error> {
    let mut p = PushParser::new(opts);
    let mut i = 0;
    while i < bytes.len() {
        let end = (i + chunk).min(bytes.len());
        if let Outcome::Done = p.feed(&bytes[i..end])? {
            return p.finish();
        }
        i = end;
    }
    p.finish()
}

fn assert_all_equal(bytes: &[u8]) {
    let want = read_slice(bytes, Options::default());
    let blocking = read_blocking(bytes, Options::default());
    let seek = read_seek(Cursor::new(bytes), Options::default());
    match &want {
        Ok(w) => {
            assert_eq!(blocking.as_ref().unwrap(), w, "blocking vs slice");
            assert_eq!(seek.as_ref().unwrap(), w, "seek vs slice");
            for chunk in [1usize, 3, 7, bytes.len().max(1)] {
                let got = push_drive(bytes, Options::default(), chunk).unwrap();
                assert_eq!(&got, w, "push chunk={chunk} vs slice");
            }
        }
        Err(_) => {
            assert!(blocking.is_err(), "blocking should also err");
            assert!(seek.is_err(), "seek should also err");
            assert!(push_drive(bytes, Options::default(), 1).is_err(), "push should also err");
        }
    }
}

#[test]
fn differential_plain() {
    assert_all_equal(&fixture_plain());
}

#[test]
fn differential_large_nonmeta() {
    assert_all_equal(&fixture_large_nonmeta());
}

#[test]
fn differential_truncated() {
    assert_all_equal(&fixture_truncated());
}

#[test]
fn differential_unrecognized() {
    assert_all_equal(&[0x00, 0x01, 0x02, 0x03]);
}

#[test]
fn differential_huge_nonmeta() {
    // 段体 9000 字节 > 8192（read_seek 块大小），强制驱动在首次 read 后
    // 返回 SkipHint，从而覆盖 read_seek 的原生 seek 分支。
    assert_all_equal(&fixture_huge_nonmeta());
}

fn fixture_with_sof() -> Vec<u8> {
    let mut j: Vec<u8> = Vec::new();
    j.extend_from_slice(&[0xFF, 0xD8]);           // SOI
    j.extend_from_slice(&[0xFF, 0xC0]);           // SOF0
    j.extend_from_slice(&10u16.to_be_bytes());    // len = 2 + 8 body bytes
    j.push(8);                                    // precision
    j.extend_from_slice(&1080u16.to_be_bytes());  // height
    j.extend_from_slice(&1920u16.to_be_bytes());  // width
    j.extend_from_slice(&[1, 0x11, 0]);           // 1 component
    let tiff = make_tiff();
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(b"Exif\0\0");
    body.extend_from_slice(&tiff);
    let len = (body.len() + 2) as u16;
    j.extend_from_slice(&[0xFF, 0xE1]);
    j.extend_from_slice(&len.to_be_bytes());
    j.extend_from_slice(&body);
    j.extend_from_slice(&[0xFF, 0xD9]);           // EOI
    j
}

#[test]
fn differential_sof_dimensions() {
    assert_all_equal(&fixture_with_sof());
}

fn png_chunk(ctype: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(&(data.len() as u32).to_be_bytes());
    c.extend_from_slice(ctype);
    c.extend_from_slice(data);
    c.extend_from_slice(&[0, 0, 0, 0]);
    c
}

fn fixture_png() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
    // IHDR 1920x1080
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&1920u32.to_be_bytes());
    ihdr.extend_from_slice(&1080u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    p.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
    // eXIf：完整 TIFF（复用 make_tiff）
    p.extend_from_slice(&png_chunk(b"eXIf", &make_tiff()));
    // iTXt XMP（未压缩）
    let mut itxt = Vec::new();
    itxt.extend_from_slice(b"XML:com.adobe.xmp");
    itxt.push(0);
    itxt.push(0);
    itxt.push(0);
    itxt.push(0);
    itxt.push(0);
    itxt.extend_from_slice(br#"<rdf:Description tiff:Make="Acme"/>"#);
    p.extend_from_slice(&png_chunk(b"iTXt", &itxt));
    p.extend_from_slice(&png_chunk(b"IDAT", &[1, 2, 3, 4]));
    p.extend_from_slice(&png_chunk(b"IEND", &[]));
    p
}

#[test]
fn differential_png() {
    assert_all_equal(&fixture_png());
}

fn riff_chunk(fourcc: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(fourcc);
    c.extend_from_slice(&(data.len() as u32).to_le_bytes());
    c.extend_from_slice(data);
    if data.len() % 2 == 1 {
        c.push(0);
    }
    c
}

fn fixture_webp() -> Vec<u8> {
    // VP8X 640x480
    let mut vp8x = vec![0u8; 10];
    let (wm1, hm1) = (639u32, 479u32);
    vp8x[4] = (wm1 & 0xFF) as u8;
    vp8x[5] = ((wm1 >> 8) & 0xFF) as u8;
    vp8x[6] = ((wm1 >> 16) & 0xFF) as u8;
    vp8x[7] = (hm1 & 0xFF) as u8;
    vp8x[8] = ((hm1 >> 8) & 0xFF) as u8;
    vp8x[9] = ((hm1 >> 16) & 0xFF) as u8;

    let mut body = Vec::new();
    body.extend_from_slice(b"WEBP");
    body.extend_from_slice(&riff_chunk(b"VP8X", &vp8x));
    body.extend_from_slice(&riff_chunk(b"EXIF", &make_tiff()));
    body.extend_from_slice(&riff_chunk(b"XMP ", br#"<rdf:Description tiff:Make="Acme"/>"#));

    let mut f = Vec::new();
    f.extend_from_slice(b"RIFF");
    f.extend_from_slice(&(body.len() as u32).to_le_bytes());
    f.extend_from_slice(&body);
    f
}

#[test]
fn differential_webp() {
    assert_all_equal(&fixture_webp());
}

fn fixture_webp_vp8l() -> Vec<u8> {
    // VP8L lossless chunk: w=100, h=80.
    // bits = (w-1) | ((h-1) << 14) = 99 | (79 << 14)
    let (w, h): (u32, u32) = (100, 80);
    let bits: u32 = (w - 1) | ((h - 1) << 14);
    let mut vp8l_data = vec![0u8; 5];
    vp8l_data[0] = 0x2f;
    vp8l_data[1..5].copy_from_slice(&bits.to_le_bytes());

    let xmp = riff_chunk(b"XMP ", br#"<rdf:Description tiff:Make="Acme"/>"#);

    let mut body = Vec::new();
    body.extend_from_slice(b"WEBP");
    body.extend_from_slice(&riff_chunk(b"VP8L", &vp8l_data));
    body.extend_from_slice(&xmp);

    let mut f = Vec::new();
    f.extend_from_slice(b"RIFF");
    f.extend_from_slice(&(body.len() as u32).to_le_bytes());
    f.extend_from_slice(&body);
    f
}

#[test]
fn differential_webp_vp8l() {
    assert_all_equal(&fixture_webp_vp8l());
}

fn fixture_gif() -> Vec<u8> {
    let mut g = Vec::new();
    g.extend_from_slice(b"GIF89a");
    g.extend_from_slice(&800u16.to_le_bytes());
    g.extend_from_slice(&600u16.to_le_bytes());
    g.push(0x00); // 无 GCT
    g.push(0);
    g.push(0);
    // XMP Application Extension
    g.push(0x21);
    g.push(0xFF);
    g.push(0x0B);
    g.extend_from_slice(b"XMP DataXMP");
    g.extend_from_slice(br#"<rdf:Description tiff:Make="Acme"/>"#);
    g.push(0x01);
    for v in (0u8..=0xFFu8).rev() {
        g.push(v);
    }
    g.push(0x3B); // trailer
    g
}

#[test]
fn differential_gif() {
    assert_all_equal(&fixture_gif());
}
