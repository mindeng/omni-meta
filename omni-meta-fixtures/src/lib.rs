//! omni-meta 测试/模糊共享 fixtures：纯字节构造器 + 四适配器一致性 oracle。
//! 差分集成测试与 fuzz 种子生成器共用，单一真相源（DRY）。

use std::vec::Vec;

use omni_meta::{read_blocking, read_seek, read_slice, Error, Metadata, Options, Outcome, PushParser};
use std::io::Cursor;

// ---- Pure byte-builder functions ----

pub fn make_tiff() -> Vec<u8> {
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

pub fn wrap_jpeg(pre_segments: &[u8], with_exif: bool, eoi: bool) -> Vec<u8> {
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
pub fn fixture_plain() -> Vec<u8> {
    wrap_jpeg(&[], true, true)
}

/// APP1 之前有大的非元数据段（行使 Skip）。
pub fn fixture_large_nonmeta() -> Vec<u8> {
    let mut app0: Vec<u8> = Vec::new();
    app0.extend_from_slice(&[0xFF, 0xE0]);
    app0.extend_from_slice(&202u16.to_be_bytes()); // body 200
    app0.extend_from_slice(&[0u8; 200]);
    wrap_jpeg(&app0, true, true)
}

/// APP0 段体 9000 字节，超过 read_seek 的 8192 字节读取块。
/// 第一次 read() 只能消费 8192 字节，剩余跳过字节仍由驱动持有，
/// 驱动因此向上层返回 SkipHint，触发 read_seek 的原生 seek 路径。
pub fn fixture_huge_nonmeta() -> Vec<u8> {
    let mut app0: Vec<u8> = Vec::new();
    app0.extend_from_slice(&[0xFF, 0xE0]);
    app0.extend_from_slice(&9002u16.to_be_bytes()); // length field = body(9000) + 2
    app0.extend_from_slice(&[0u8; 9000]);
    wrap_jpeg(&app0, true, true)
}

/// 截断在 APP1 段体中间（声明 len 远大于实际）。
pub fn fixture_truncated() -> Vec<u8> {
    let mut j: Vec<u8> = Vec::new();
    j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
    j.extend_from_slice(&200u16.to_be_bytes());
    j.extend_from_slice(b"Exif\0\0");
    j.extend_from_slice(&[0xAA, 0xBB]); // body 严重不足
    j
}

pub fn fixture_with_sof() -> Vec<u8> {
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

pub fn png_chunk(ctype: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(&(data.len() as u32).to_be_bytes());
    c.extend_from_slice(ctype);
    c.extend_from_slice(data);
    c.extend_from_slice(&[0, 0, 0, 0]);
    c
}

pub fn fixture_png() -> Vec<u8> {
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

pub fn riff_chunk(fourcc: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(fourcc);
    c.extend_from_slice(&(data.len() as u32).to_le_bytes());
    c.extend_from_slice(data);
    if data.len() % 2 == 1 {
        c.push(0);
    }
    c
}

pub fn fixture_webp() -> Vec<u8> {
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

pub fn fixture_webp_vp8l() -> Vec<u8> {
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

pub fn fixture_gif() -> Vec<u8> {
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

pub fn fixture_png_compressed_itxt() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
    // IHDR 4x4
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&4u32.to_be_bytes());
    ihdr.extend_from_slice(&4u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    p.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
    // iTXt with keyword XML:com.adobe.xmp, compression flag = 1 (compressed → warn & skip)
    let mut itxt = Vec::new();
    itxt.extend_from_slice(b"XML:com.adobe.xmp");
    itxt.push(0x00); // keyword NUL
    itxt.push(0x01); // compression flag = 1
    itxt.push(0x00); // compression method
    itxt.push(0x00); // lang NUL
    itxt.push(0x00); // translated-keyword NUL
    itxt.extend_from_slice(b"fake-compressed-payload");
    p.extend_from_slice(&png_chunk(b"iTXt", &itxt));
    p.extend_from_slice(&png_chunk(b"IEND", &[]));
    p
}

pub fn make_tiff_subifd() -> Vec<u8> {
    let mut t: Vec<u8> = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    // IFD0 @8: 仅一个 Exif 指针
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x8769u16.to_le_bytes());
    t.extend_from_slice(&4u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&26u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    // Exif IFD @26: FNumber RATIONAL cnt1 @44
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x829Du16.to_le_bytes());
    t.extend_from_slice(&5u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&44u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    // @44 数据
    t.extend_from_slice(&4u32.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t
}

pub fn fixture_exif_subifd() -> Vec<u8> {
    let tiff = make_tiff_subifd();
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(b"Exif\0\0");
    body.extend_from_slice(&tiff);
    let len = (body.len() + 2) as u16;
    let mut j: Vec<u8> = Vec::new();
    j.extend_from_slice(&[0xFF, 0xD8]); // SOI
    j.extend_from_slice(&[0xFF, 0xE1]); // APP1
    j.extend_from_slice(&len.to_be_bytes());
    j.extend_from_slice(&body);
    j.extend_from_slice(&[0xFF, 0xD9]); // EOI
    j
}

pub fn make_tiff_gps_list() -> Vec<u8> {
    // IFD0 → GPS sub-IFD(0x8825) → GPSLatitude(0x0002) RATIONAL cnt=3 → Value::List
    let mut t: Vec<u8> = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x8825u16.to_le_bytes());
    t.extend_from_slice(&4u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&26u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x0002u16.to_le_bytes());
    t.extend_from_slice(&5u16.to_le_bytes());
    t.extend_from_slice(&3u32.to_le_bytes());
    t.extend_from_slice(&44u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    for n in [12u32, 34, 56] {
        t.extend_from_slice(&n.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
    }
    t
}

pub fn make_tiff_thumbnail() -> Vec<u8> {
    // IFD0(Orientation=1) → next → IFD1/Thumbnail(Orientation=6)
    let mut t: Vec<u8> = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x0112u16.to_le_bytes());
    t.extend_from_slice(&3u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&26u32.to_le_bytes()); // next = IFD1 @26
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x0112u16.to_le_bytes());
    t.extend_from_slice(&3u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&6u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    t
}

pub fn wrap_jpeg_tiff(tiff: &[u8]) -> Vec<u8> {
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(b"Exif\0\0");
    body.extend_from_slice(tiff);
    let len = (body.len() + 2) as u16;
    let mut j: Vec<u8> = Vec::new();
    j.extend_from_slice(&[0xFF, 0xD8]);
    j.extend_from_slice(&[0xFF, 0xE1]);
    j.extend_from_slice(&len.to_be_bytes());
    j.extend_from_slice(&body);
    j.extend_from_slice(&[0xFF, 0xD9]);
    j
}

pub fn bmff_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    b.extend_from_slice(kind);
    b.extend_from_slice(payload);
    b
}

pub fn bmff_infe(id: u16, typ: &[u8; 4], content_type: Option<&[u8]>) -> Vec<u8> {
    let mut p = vec![2u8, 0, 0, 0];
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes());
    p.extend_from_slice(typ);
    p.push(0); // item_name = "" (spec 要求 v2/3 存在)
    if let Some(ct) = content_type {
        p.extend_from_slice(ct);
        p.push(0);
    }
    bmff_box(b"infe", &p)
}

pub fn bmff_ispe(w: u32, h: u32) -> Vec<u8> {
    let mut p = vec![0u8, 0, 0, 0];
    p.extend_from_slice(&w.to_be_bytes());
    p.extend_from_slice(&h.to_be_bytes());
    bmff_box(b"ispe", &p)
}

pub fn bmff_meta(exif_off: u64, exif_len: u64, xmp_off: u64, xmp_len: u64) -> Vec<u8> {
    let mut pitm_p = vec![0u8, 0, 0, 0];
    pitm_p.extend_from_slice(&1u16.to_be_bytes());
    let pitm = bmff_box(b"pitm", &pitm_p);

    let mut iinf_p = vec![0u8, 0, 0, 0];
    iinf_p.extend_from_slice(&2u16.to_be_bytes());
    iinf_p.extend_from_slice(&bmff_infe(1, b"Exif", None));
    iinf_p.extend_from_slice(&bmff_infe(2, b"mime", Some(b"application/rdf+xml")));
    let iinf = bmff_box(b"iinf", &iinf_p);

    let ipco = bmff_box(b"ipco", &bmff_ispe(4032, 3024));
    let mut ipma_p = vec![0u8, 0, 0, 0];
    ipma_p.extend_from_slice(&1u32.to_be_bytes());
    ipma_p.extend_from_slice(&1u16.to_be_bytes());
    ipma_p.push(1);
    ipma_p.push(1);
    let ipma = bmff_box(b"ipma", &ipma_p);
    let mut iprp_p = Vec::new();
    iprp_p.extend_from_slice(&ipco);
    iprp_p.extend_from_slice(&ipma);
    let iprp = bmff_box(b"iprp", &iprp_p);

    let mut iloc_p = vec![0u8, 0, 0, 0];
    iloc_p.push(0x44);
    iloc_p.push(0x00);
    iloc_p.extend_from_slice(&2u16.to_be_bytes());
    for (id, off, len) in [(1u16, exif_off, exif_len), (2u16, xmp_off, xmp_len)] {
        iloc_p.extend_from_slice(&id.to_be_bytes());
        iloc_p.extend_from_slice(&0u16.to_be_bytes());
        iloc_p.extend_from_slice(&1u16.to_be_bytes());
        iloc_p.extend_from_slice(&(off as u32).to_be_bytes());
        iloc_p.extend_from_slice(&(len as u32).to_be_bytes());
    }
    let iloc = bmff_box(b"iloc", &iloc_p);

    let mut meta_p = vec![0u8, 0, 0, 0];
    meta_p.extend_from_slice(&pitm);
    meta_p.extend_from_slice(&iinf);
    meta_p.extend_from_slice(&iprp);
    meta_p.extend_from_slice(&iloc);
    bmff_box(b"meta", &meta_p)
}

/// 完整 HEIC：ftyp + meta + mdat(exif, xmp)，method 0 绝对偏移指向 mdat。
pub fn fixture_bmff_heic() -> Vec<u8> {
    let mut exif = vec![0u8, 0, 0, 0]; // tiff_header_offset = 0
    exif.extend_from_slice(&make_tiff());
    let xmp = br#"<rdf:Description tiff:Make="Acme"/>"#.to_vec();

    let mut ftyp_p = Vec::new();
    ftyp_p.extend_from_slice(b"heic");
    ftyp_p.extend_from_slice(&0u32.to_be_bytes());
    ftyp_p.extend_from_slice(b"mif1");
    let ftyp = bmff_box(b"ftyp", &ftyp_p);

    let meta_probe = bmff_meta(0, exif.len() as u64, 0, xmp.len() as u64);
    let base = ftyp.len() as u64 + meta_probe.len() as u64 + 8;
    let meta = bmff_meta(base, exif.len() as u64, base + exif.len() as u64, xmp.len() as u64);
    assert_eq!(meta.len(), meta_probe.len());

    let mut mdat_payload = Vec::new();
    mdat_payload.extend_from_slice(&exif);
    mdat_payload.extend_from_slice(&xmp);
    let mdat = bmff_box(b"mdat", &mdat_payload);

    let mut f = Vec::new();
    f.extend_from_slice(&ftyp);
    f.extend_from_slice(&meta);
    f.extend_from_slice(&mdat);
    f
}

pub fn mp4_mvhd_v0(creation: u32, timescale: u32, duration: u32) -> Vec<u8> {
    let mut p = vec![0u8, 0, 0, 0];
    p.extend_from_slice(&creation.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&timescale.to_be_bytes());
    p.extend_from_slice(&duration.to_be_bytes());
    p
}

pub fn mp4_tkhd_v0(w: u32, h: u32) -> Vec<u8> {
    let mut p = vec![0u8, 0, 0, 7];
    p.extend_from_slice(&0u32.to_be_bytes()); // creation
    p.extend_from_slice(&0u32.to_be_bytes()); // modification
    p.extend_from_slice(&1u32.to_be_bytes()); // track_ID
    p.extend_from_slice(&0u32.to_be_bytes()); // reserved
    p.extend_from_slice(&0u32.to_be_bytes()); // duration
    p.extend_from_slice(&[0u8; 8]);           // reserved[2]
    p.extend_from_slice(&[0u8; 8]);           // layer/alt/volume/reserved
    p.extend_from_slice(&[0u8; 36]);          // matrix
    p.extend_from_slice(&(w << 16).to_be_bytes());
    p.extend_from_slice(&(h << 16).to_be_bytes());
    p
}

pub fn fixture_bmff_mp4() -> Vec<u8> {
    let mut ftyp_p = Vec::new();
    ftyp_p.extend_from_slice(b"isom");
    ftyp_p.extend_from_slice(&0u32.to_be_bytes());
    ftyp_p.extend_from_slice(b"mp42");
    let ftyp = bmff_box(b"ftyp", &ftyp_p);

    let mut moov_p = Vec::new();
    moov_p.extend_from_slice(&bmff_box(b"mvhd", &mp4_mvhd_v0(2_082_844_800, 600, 900_900)));
    moov_p.extend_from_slice(&bmff_box(b"trak", &bmff_box(b"tkhd", &mp4_tkhd_v0(1920, 1080))));
    let moov = bmff_box(b"moov", &moov_p);

    let mut f = Vec::new();
    f.extend_from_slice(&ftyp);
    f.extend_from_slice(&moov);
    f
}

/// moov 在 mdat 之后：行使 read_seek 的 Skip/seek 路径。
pub fn fixture_bmff_mp4_moov_after_mdat() -> Vec<u8> {
    let mut ftyp_p = Vec::new();
    ftyp_p.extend_from_slice(b"isom");
    ftyp_p.extend_from_slice(&0u32.to_be_bytes());
    ftyp_p.extend_from_slice(b"mp42");
    let ftyp = bmff_box(b"ftyp", &ftyp_p);

    let mut moov_p = Vec::new();
    moov_p.extend_from_slice(&bmff_box(b"mvhd", &mp4_mvhd_v0(0, 1000, 5000)));
    moov_p.extend_from_slice(&bmff_box(b"trak", &bmff_box(b"tkhd", &mp4_tkhd_v0(640, 480))));
    let moov = bmff_box(b"moov", &moov_p);

    let mdat = bmff_box(b"mdat", &[0u8; 10_000]); // >8192 读块，强制 seek 路径

    let mut f = Vec::new();
    f.extend_from_slice(&ftyp);
    f.extend_from_slice(&mdat);
    f.extend_from_slice(&moov);
    f
}

pub fn mp4_mvhd_v1(creation: u64, timescale: u32, duration: u64) -> Vec<u8> {
    let mut p = vec![1u8, 0, 0, 0]; // version 1, flags 0
    p.extend_from_slice(&creation.to_be_bytes());
    p.extend_from_slice(&0u64.to_be_bytes()); // modification_time
    p.extend_from_slice(&timescale.to_be_bytes());
    p.extend_from_slice(&duration.to_be_bytes());
    p
}

pub fn fixture_bmff_mp4_v1() -> Vec<u8> {
    let mut ftyp_p = Vec::new();
    ftyp_p.extend_from_slice(b"isom");
    ftyp_p.extend_from_slice(&0u32.to_be_bytes());
    ftyp_p.extend_from_slice(b"mp42");
    let ftyp = bmff_box(b"ftyp", &ftyp_p);

    let mut moov_p = Vec::new();
    // creation 2_082_844_800 → 1970-01-01; timescale 1000, duration 5000 → 5000 ms
    moov_p.extend_from_slice(&bmff_box(b"mvhd", &mp4_mvhd_v1(2_082_844_800, 1000, 5000)));
    moov_p.extend_from_slice(&bmff_box(b"trak", &bmff_box(b"tkhd", &mp4_tkhd_v0(1280, 720))));
    let moov = bmff_box(b"moov", &moov_p);

    let mut f = Vec::new();
    f.extend_from_slice(&ftyp);
    f.extend_from_slice(&moov);
    f
}

pub fn make_tiff_datetime_original() -> Vec<u8> {
    // little-endian TIFF; IFD0 @8 → ExifIFDPointer; Exif sub-IFD @26 → DateTimeOriginal @44
    let mut t: Vec<u8> = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    // IFD0 @8: 1 entry (ExifIFDPointer)
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x8769u16.to_le_bytes()); // ExifIFDPointer
    t.extend_from_slice(&4u16.to_le_bytes());      // LONG
    t.extend_from_slice(&1u32.to_le_bytes());      // count
    t.extend_from_slice(&26u32.to_le_bytes());     // → Exif sub-IFD @26
    t.extend_from_slice(&0u32.to_le_bytes());      // next IFD = 0
    // Exif sub-IFD @26: 1 entry (DateTimeOriginal)
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x9003u16.to_le_bytes()); // DateTimeOriginal
    t.extend_from_slice(&2u16.to_le_bytes());      // ASCII
    t.extend_from_slice(&20u32.to_le_bytes());     // count = 19 chars + NUL
    t.extend_from_slice(&44u32.to_le_bytes());     // value offset @44
    t.extend_from_slice(&0u32.to_le_bytes());      // next IFD = 0
    // @44: the string
    t.extend_from_slice(b"2003:01:24 09:20:00\0");
    t
}

// ---- EBML（Matroska/WebM）----

pub fn ebml_elem(id: &[u8], payload: &[u8]) -> Vec<u8> {
    // 8 字节 vint size 编码
    let mut e = Vec::new();
    e.extend_from_slice(id);
    e.push(0x01);
    e.extend_from_slice(&(payload.len() as u64).to_be_bytes()[1..]);
    e.extend_from_slice(payload);
    e
}

pub fn ebml_video_track(w: u32, h: u32) -> Vec<u8> {
    let mut vid = Vec::new();
    vid.extend_from_slice(&ebml_elem(&[0xB0], &w.to_be_bytes())); // PixelWidth
    vid.extend_from_slice(&ebml_elem(&[0xBA], &h.to_be_bytes())); // PixelHeight
    let video = ebml_elem(&[0xE0], &vid);
    ebml_elem(&[0xAE], &video) // TrackEntry { Video }
}

pub fn ebml_info() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&ebml_elem(&[0x2A, 0xD7, 0xB1], &1_000_000u64.to_be_bytes())); // TimestampScale
    p.extend_from_slice(&ebml_elem(&[0x44, 0x89], &5000.0f64.to_be_bytes()));          // Duration
    p.extend_from_slice(&ebml_elem(&[0x44, 0x61], &0i64.to_be_bytes()));               // DateUTC=0 → 2001
    ebml_elem(&[0x15, 0x49, 0xA9, 0x66], &p)
}

pub fn ebml_header(doctype: &[u8]) -> Vec<u8> {
    let dt = ebml_elem(&[0x42, 0x82], doctype);
    ebml_elem(&[0x1A, 0x45, 0xDF, 0xA3], &dt)
}

/// EBML头 + Segment{ Info, Void(大), Tracks, Cluster }。
/// 大 Void 在 Tracks 之前被 Skip（>8192 → 行使 read_seek 原生 seek 路径）。
pub fn fixture_ebml(doctype: &[u8]) -> Vec<u8> {
    let void = ebml_elem(&[0xEC], &vec![0u8; 10_000]); // 大 Void，跳过
    let tracks = ebml_elem(&[0x16, 0x54, 0xAE, 0x6B], &ebml_video_track(1280, 720));
    let cluster = ebml_elem(&[0x1F, 0x43, 0xB6, 0x75], &[0u8; 16]);
    let mut seg_children = Vec::new();
    seg_children.extend_from_slice(&ebml_info());
    seg_children.extend_from_slice(&void);
    seg_children.extend_from_slice(&tracks);
    seg_children.extend_from_slice(&cluster);
    let segment = ebml_elem(&[0x18, 0x53, 0x80, 0x67], &seg_children);
    let mut f = ebml_header(doctype);
    f.extend_from_slice(&segment);
    f
}

/// Segment 用「未知大小」编码（直播常见）；下钻不依赖 Segment size。
pub fn fixture_ebml_unknown_size_segment() -> Vec<u8> {
    let tracks = ebml_elem(&[0x16, 0x54, 0xAE, 0x6B], &ebml_video_track(640, 480));
    let mut seg_children = Vec::new();
    seg_children.extend_from_slice(&ebml_info());
    seg_children.extend_from_slice(&tracks);
    let mut segment = Vec::new();
    segment.extend_from_slice(&[0x18, 0x53, 0x80, 0x67]); // Segment id
    segment.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]); // 未知大小
    segment.extend_from_slice(&seg_children);
    let mut f = ebml_header(b"webm");
    f.extend_from_slice(&segment);
    f
}

/// 构造 QuickTime mdta meta 载荷：hdlr(mdta) + keys + ilst。
/// 移植自 omni-meta-core/src/formats/bmff.rs 的同名测试辅助函数。
pub fn qt_meta_with_keys_local(keys_and_vals: &[(&str, &[u8])]) -> Vec<u8> {
    let mut hdlr = Vec::new();
    hdlr.extend_from_slice(&[0u8; 8]); // version/flags + pre_defined
    hdlr.extend_from_slice(b"mdta");   // handler_type
    hdlr.extend_from_slice(&[0u8; 12]); // reserved(3*4)
    hdlr.push(0);                       // name 空

    let mut keys_payload = Vec::new();
    keys_payload.extend_from_slice(&[0u8; 4]); // version/flags
    keys_payload.extend_from_slice(&(keys_and_vals.len() as u32).to_be_bytes()); // entry_count
    for (k, _) in keys_and_vals {
        let entry_size = 8 + k.len();
        keys_payload.extend_from_slice(&(entry_size as u32).to_be_bytes());
        keys_payload.extend_from_slice(b"mdta"); // namespace
        keys_payload.extend_from_slice(k.as_bytes());
    }

    let mut ilst = Vec::new();
    for (i, (_, v)) in keys_and_vals.iter().enumerate() {
        let idx = (i as u32) + 1;
        let mut data_payload = Vec::new();
        data_payload.extend_from_slice(&[0u8; 4]); // type
        data_payload.extend_from_slice(&[0u8; 4]); // locale
        data_payload.extend_from_slice(v);
        let data_box = bmff_box(b"data", &data_payload);
        let item_inner_size = 8 + data_box.len();
        let mut item_box = Vec::new();
        item_box.extend_from_slice(&(item_inner_size as u32).to_be_bytes());
        item_box.extend_from_slice(&idx.to_be_bytes()); // box "kind" = 索引
        item_box.extend_from_slice(&data_box);
        ilst.extend_from_slice(&item_box);
    }

    let mut meta = Vec::new();
    meta.extend_from_slice(&bmff_box(b"hdlr", &hdlr));
    meta.extend_from_slice(&bmff_box(b"keys", &keys_payload));
    meta.extend_from_slice(&bmff_box(b"ilst", &ilst));
    meta
}

/// 构造 .MOV 文件（ftyp qt + moov{ mvhd, udta{©xyz}, meta{mdta make/model} }）。
/// ©xyz lat=+35.0000 lon=+139.0000 → lat_e7 应为 350_000_000。
pub fn build_mov_with_gps_and_mdta() -> Vec<u8> {
    // ftyp: brand=qt
    let mut ftyp_p = Vec::new();
    ftyp_p.extend_from_slice(b"qt  ");
    ftyp_p.extend_from_slice(&0u32.to_be_bytes()); // minor_version
    let ftyp = bmff_box(b"ftyp", &ftyp_p);

    // ©xyz payload: u16 size + u16 lang + ISO6709 text
    let xyz_text = b"+35.0000+139.0000/";
    let mut xyz_payload = Vec::new();
    xyz_payload.extend_from_slice(&(xyz_text.len() as u16).to_be_bytes());
    xyz_payload.extend_from_slice(&0u16.to_be_bytes()); // lang
    xyz_payload.extend_from_slice(xyz_text);

    let mut udta = Vec::new();
    udta.extend_from_slice(&bmff_box(b"\xA9xyz", &xyz_payload));

    // mdta meta: make=Apple, model=iPhone 15
    let meta_payload = qt_meta_with_keys_local(&[
        ("com.apple.quicktime.make", b"Apple"),
        ("com.apple.quicktime.model", b"iPhone 15"),
    ]);

    // mvhd v0: creation=2_082_844_800, timescale=600, duration=600
    let mvhd_p = mp4_mvhd_v0(2_082_844_800, 600, 600);

    let mut moov_p = Vec::new();
    moov_p.extend_from_slice(&bmff_box(b"mvhd", &mvhd_p));
    moov_p.extend_from_slice(&bmff_box(b"udta", &udta));
    moov_p.extend_from_slice(&bmff_box(b"meta", &meta_payload));
    let moov = bmff_box(b"moov", &moov_p);

    let mut f = Vec::new();
    f.extend_from_slice(&ftyp);
    f.extend_from_slice(&moov);
    f
}

/// 构造含 GPS IFD 的 JPEG/EXIF TIFF。
/// IFD0 → GPS sub-IFD(0x8825) → lat_ref="N", lat=35°, lon_ref="E", lon=139°。
pub fn build_jpeg_with_gps_ifd() -> Vec<u8> {
    // Little-endian TIFF layout:
    // 0x00: "II" + 42 + IFD0_offset(8)
    // IFD0 @8: 1 entry (GPS IFD pointer 0x8825 → 26)
    // GPS IFD @26: 4 entries (lat_ref, lat, lon_ref, lon)
    //   Data at 80: lat 3×RATIONAL (24 bytes), lon 3×RATIONAL (24 bytes)
    let mut t: Vec<u8> = Vec::new();
    // TIFF header
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes()); // IFD0 at 8

    // IFD0 @8: 1 entry
    t.extend_from_slice(&1u16.to_le_bytes()); // entry count
    // Entry: tag=0x8825(GPS IFD), type=4(LONG), count=1, value=26(GPS IFD offset)
    t.extend_from_slice(&0x8825u16.to_le_bytes());
    t.extend_from_slice(&4u16.to_le_bytes());  // LONG
    t.extend_from_slice(&1u32.to_le_bytes());  // count
    t.extend_from_slice(&26u32.to_le_bytes()); // → GPS IFD at 26
    t.extend_from_slice(&0u32.to_le_bytes());  // next IFD = 0
    // IFD0 ends at 8 + 2 + 12 + 4 = 26 ✓

    // GPS IFD @26: 4 entries
    t.extend_from_slice(&4u16.to_le_bytes()); // entry count
    // Data for large values (RATIONAL×3 = 24 bytes each) starts after GPS IFD:
    // GPS IFD size = 2 + 4×12 + 4 = 54 bytes → data starts at 26+54=80
    let lat_data_offset: u32 = 80;
    let lon_data_offset: u32 = 80 + 24; // = 104

    // Entry 1: GPSLatitudeRef (0x0001), ASCII, count=2, inline value "N\0"
    t.extend_from_slice(&0x0001u16.to_le_bytes());
    t.extend_from_slice(&2u16.to_le_bytes());  // ASCII
    t.extend_from_slice(&2u32.to_le_bytes());  // count (includes NUL)
    t.extend_from_slice(b"N\0\0\0");           // inline (LE, padded to 4)

    // Entry 2: GPSLatitude (0x0002), RATIONAL, count=3, data at lat_data_offset
    t.extend_from_slice(&0x0002u16.to_le_bytes());
    t.extend_from_slice(&5u16.to_le_bytes());              // RATIONAL
    t.extend_from_slice(&3u32.to_le_bytes());              // count
    t.extend_from_slice(&lat_data_offset.to_le_bytes());   // offset

    // Entry 3: GPSLongitudeRef (0x0003), ASCII, count=2, inline value "E\0"
    t.extend_from_slice(&0x0003u16.to_le_bytes());
    t.extend_from_slice(&2u16.to_le_bytes());  // ASCII
    t.extend_from_slice(&2u32.to_le_bytes());  // count
    t.extend_from_slice(b"E\0\0\0");           // inline (LE, padded to 4)

    // Entry 4: GPSLongitude (0x0004), RATIONAL, count=3, data at lon_data_offset
    t.extend_from_slice(&0x0004u16.to_le_bytes());
    t.extend_from_slice(&5u16.to_le_bytes());              // RATIONAL
    t.extend_from_slice(&3u32.to_le_bytes());              // count
    t.extend_from_slice(&lon_data_offset.to_le_bytes());   // offset

    t.extend_from_slice(&0u32.to_le_bytes()); // GPS IFD next = 0
    // GPS IFD ends at 26 + 2 + 48 + 4 = 80 ✓

    debug_assert_eq!(t.len(), 80, "data section should start at offset 80");

    // Data @80: lat = 35°0'0" (3 RATIONALs: 35/1, 0/1, 0/1)
    t.extend_from_slice(&35u32.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    // Data @104: lon = 139°0'0" (3 RATIONALs: 139/1, 0/1, 0/1)
    t.extend_from_slice(&139u32.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());

    wrap_jpeg_tiff(&t)
}

pub fn qt_meta_typed(items: &[(&str, u32, &[u8])]) -> Vec<u8> {
    let mut hdlr = Vec::new();
    hdlr.extend_from_slice(&[0u8; 8]);
    hdlr.extend_from_slice(b"mdta");
    hdlr.extend_from_slice(&[0u8; 12]);
    hdlr.push(0);
    let mut keys = Vec::new();
    keys.extend_from_slice(&[0u8; 4]);
    keys.extend_from_slice(&(items.len() as u32).to_be_bytes());
    for (k, _, _) in items {
        keys.extend_from_slice(&((8 + k.len()) as u32).to_be_bytes());
        keys.extend_from_slice(b"mdta");
        keys.extend_from_slice(k.as_bytes());
    }
    let mut ilst = Vec::new();
    for (i, (_, ty, v)) in items.iter().enumerate() {
        let idx = (i as u32) + 1;
        let mut data = Vec::new();
        data.extend_from_slice(&ty.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(v);
        let data_box = bmff_box(b"data", &data);
        let mut item = Vec::new();
        item.extend_from_slice(&((8 + data_box.len()) as u32).to_be_bytes());
        item.extend_from_slice(&idx.to_be_bytes());
        item.extend_from_slice(&data_box);
        ilst.extend_from_slice(&item);
    }
    let mut meta = Vec::new();
    meta.extend_from_slice(&bmff_box(b"hdlr", &hdlr));
    meta.extend_from_slice(&bmff_box(b"keys", &keys));
    meta.extend_from_slice(&bmff_box(b"ilst", &ilst));
    meta
}

pub fn fixture_bmff_mp4_container_tags() -> Vec<u8> {
    let mut ftyp_p = Vec::new();
    ftyp_p.extend_from_slice(b"isom");
    ftyp_p.extend_from_slice(&0u32.to_be_bytes());
    ftyp_p.extend_from_slice(b"mp42");
    let ftyp = bmff_box(b"ftyp", &ftyp_p);

    // udta { ©swr }
    let swr_text = b"MyCam 1.0";
    let mut swr_payload = Vec::new();
    swr_payload.extend_from_slice(&(swr_text.len() as u16).to_be_bytes());
    swr_payload.extend_from_slice(&0u16.to_be_bytes());
    swr_payload.extend_from_slice(swr_text);
    let udta = bmff_box(b"\xA9swr", &swr_payload);

    let meta = qt_meta_typed(&[
        ("com.apple.quicktime.software", 1, b"13.5.1"),
        ("com.apple.quicktime.author", 1, b"Jane"),
    ]);

    let mut moov_p = Vec::new();
    moov_p.extend_from_slice(&bmff_box(b"mvhd", &mp4_mvhd_v0(2_082_844_800, 600, 900_900)));
    moov_p.extend_from_slice(&bmff_box(b"udta", &udta));
    moov_p.extend_from_slice(&bmff_box(b"meta", &meta));
    let moov = bmff_box(b"moov", &moov_p);

    let mut f = ftyp;
    f.extend_from_slice(&moov);
    f
}

// ---- Four-adapter consistency oracle ----

/// 四适配器对同一输入的裁决。
///
/// `Agree` 携带完整 `Metadata`（按设计如此——这是诊断 oracle，非热点路径），
/// 故变体尺寸差异属预期。
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Agreement {
    /// 全部 Ok 且 Metadata 逐字段相等。
    Agree(Metadata),
    /// 全部 Err（格式不可识别等）。
    AllErr,
    /// 适配器间出现分歧——附人类可读原因（违反核心契约）。
    Disagree(String),
}

/// 把 bytes 喂给 push 适配器（分块 chunk），返回最终结果。
pub fn push_drive(bytes: &[u8], opts: Options, chunk: usize) -> Result<Metadata, Error> {
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

/// 跑全部四适配器（push 用多种分块），判定一致性。永不 panic——分歧以 Disagree 返回。
pub fn adapters_outcome(bytes: &[u8]) -> Agreement {
    let slice = read_slice(bytes, Options::default());
    let blocking = read_blocking(bytes, Options::default());
    let seek = read_seek(Cursor::new(bytes), Options::default());
    match &slice {
        Ok(w) => {
            if blocking.as_ref() != Ok(w) {
                return Agreement::Disagree(format!("blocking vs slice: {blocking:?}"));
            }
            if seek.as_ref() != Ok(w) {
                return Agreement::Disagree(format!("seek vs slice: {seek:?}"));
            }
            for chunk in [1usize, 3, 7, bytes.len().max(1)] {
                match push_drive(bytes, Options::default(), chunk) {
                    Ok(got) if &got == w => {}
                    other => {
                        return Agreement::Disagree(format!("push chunk={chunk}: {other:?}"));
                    }
                }
            }
            Agreement::Agree(w.clone())
        }
        Err(_) => {
            if blocking.is_err()
                && seek.is_err()
                && push_drive(bytes, Options::default(), 1).is_err()
            {
                Agreement::AllErr
            } else {
                Agreement::Disagree(format!(
                    "slice Err 但他者非全 Err: blocking={blocking:?} seek={seek:?}"
                ))
            }
        }
    }
}

/// 现有差分测试的断言入口：分歧即 panic（保持原行为）。
pub fn assert_all_equal(bytes: &[u8]) {
    if let Agreement::Disagree(why) = adapters_outcome(bytes) {
        panic!("adapter disagreement: {why}");
    }
}

// ---- Seed corpus functions ----

/// 完整文件级种子：喂 differential / read_slice_bounded / probe 全链路。
pub fn file_corpus() -> Vec<(&'static str, Vec<u8>)> {
    let mut v = vec![
        ("jpeg_plain", fixture_plain()),
        ("jpeg_sof", fixture_with_sof()),
        ("jpeg_truncated", fixture_truncated()),
        ("jpeg_gps_ifd", build_jpeg_with_gps_ifd()),
        ("png", fixture_png()),
        ("png_itxt", fixture_png_compressed_itxt()),
        ("webp", fixture_webp()),
        ("webp_vp8l", fixture_webp_vp8l()),
        ("gif", fixture_gif()),
    ];
    for (n, b) in bmff_corpus() {
        v.push((n, b));
    }
    for (n, b) in ebml_corpus() {
        v.push((n, b));
    }
    v
}

/// 容器（BMFF）种子。
pub fn bmff_corpus() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("heic", fixture_bmff_heic()),
        ("mp4", fixture_bmff_mp4()),
        ("mp4_v1", fixture_bmff_mp4_v1()),
        ("mp4_moov_after_mdat", fixture_bmff_mp4_moov_after_mdat()),
        ("mp4_container_tags", fixture_bmff_mp4_container_tags()),
        ("mov_gps_mdta", build_mov_with_gps_and_mdta()),
    ]
}

/// 容器（EBML）种子。
pub fn ebml_corpus() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("webm", fixture_ebml(b"webm")),
        ("matroska", fixture_ebml(b"matroska")),
        ("unknown_size_segment", fixture_ebml_unknown_size_segment()),
    ]
}

/// EXIF codec 种子（裸 TIFF 字节，即 "Exif\0\0" 之后的内容）。
pub fn tiff_corpus() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("tiff_basic", make_tiff()),
        ("tiff_subifd", make_tiff_subifd()),
        ("tiff_gps_list", make_tiff_gps_list()),
        ("tiff_thumbnail", make_tiff_thumbnail()),
        ("tiff_datetime_original", make_tiff_datetime_original()),
    ]
}

/// XMP codec 种子（RDF/XML 包字节）。
pub fn xmp_corpus() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "xmp_attr",
            br#"<?xpacket?><rdf:Description xmlns:tiff="ns" tiff:Make="Acme" tiff:Model="X1"/>"#
                .to_vec(),
        ),
        (
            "xmp_elem",
            br#"<rdf:Description><dc:creator><rdf:Seq><rdf:li>Jane</rdf:li></rdf:Seq></dc:creator></rdf:Description>"#
                .to_vec(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
