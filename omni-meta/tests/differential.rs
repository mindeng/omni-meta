//! 差分测试：read_slice / read_blocking / read_seek / push 对同一输入逐字段一致。

use omni_meta_fixtures::*;
use omni_meta::{read_slice, Options};

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

#[test]
fn differential_sof_dimensions() {
    assert_all_equal(&fixture_with_sof());
}

#[test]
fn differential_png() {
    assert_all_equal(&fixture_png());
}

#[test]
fn differential_png_skip_past_eof() {
    // 回归：非元数据 chunk 声明长度越过 EOF，四适配器须一致（UnreachableSection）。
    assert_all_equal(&png_skip_past_eof());
}

#[test]
fn differential_webp() {
    assert_all_equal(&fixture_webp());
}

#[test]
fn differential_webp_vp8l() {
    assert_all_equal(&fixture_webp_vp8l());
}

#[test]
fn differential_gif() {
    assert_all_equal(&fixture_gif());
}

#[test]
fn differential_png_compressed_itxt() {
    assert_all_equal(&fixture_png_compressed_itxt());
}

#[test]
fn differential_exif_subifd() {
    assert_all_equal(&fixture_exif_subifd());
}

#[test]
fn differential_gps_list() {
    assert_all_equal(&wrap_jpeg_tiff(&make_tiff_gps_list()));
}

#[test]
fn differential_thumbnail_ifd() {
    assert_all_equal(&wrap_jpeg_tiff(&make_tiff_thumbnail()));
}

#[test]
fn differential_bmff_heic() {
    // 四适配器对 SeekTo 抽取（meta 在前、数据在 mdat）逐字段一致。
    assert_all_equal(&fixture_bmff_heic());
}

#[test]
fn differential_bmff_mp4() {
    assert_all_equal(&fixture_bmff_mp4());
}

#[test]
fn differential_bmff_mp4_moov_after_mdat() {
    assert_all_equal(&fixture_bmff_mp4_moov_after_mdat());
}

#[test]
fn differential_bmff_mp4_v1_mvhd() {
    assert_all_equal(&fixture_bmff_mp4_v1());
}

#[test]
fn differential_jpeg_exif_created() {
    assert_all_equal(&wrap_jpeg_tiff(&make_tiff_datetime_original()));
}

#[test]
fn differential_webm() {
    assert_all_equal(&fixture_ebml(b"webm"));
}

#[test]
fn differential_mkv() {
    assert_all_equal(&fixture_ebml(b"matroska"));
}

#[test]
fn differential_ebml_unknown_size_segment() {
    assert_all_equal(&fixture_ebml_unknown_size_segment());
}

// ---- GPS 差分测试 ----

#[test]
fn gps_mov_mdta_consistent_across_adapters() {
    let bytes = build_mov_with_gps_and_mdta();
    let m = read_slice(&bytes, Options::default()).unwrap();
    assert_eq!(m.unified.gps.map(|g| g.lat_e7), Some(350_000_000));
    assert_eq!(m.unified.camera_make.as_deref(), Some("Apple"));
    assert_all_equal(&bytes);
}

#[test]
fn gps_jpeg_exif_consistent_across_adapters() {
    let bytes = build_jpeg_with_gps_ifd();
    let m = read_slice(&bytes, Options::default()).unwrap();
    let g = m.unified.gps.expect("gps 应被投影");
    assert_eq!(g.lat_e7, 350_000_000);
    assert_eq!(g.lon_e7, 1_390_000_000);
    assert_all_equal(&bytes);
}

#[test]
fn differential_bmff_mp4_container_tags() {
    assert_all_equal(&fixture_bmff_mp4_container_tags());
}

#[test]
fn mov_container_projects_software_and_creator() {
    let bytes = fixture_bmff_mp4_container_tags();
    let m = read_slice(&bytes, Options::default()).expect("metadata");
    assert_eq!(m.unified.software.as_deref(), Some("13.5.1"));
    assert_eq!(m.unified.creator.as_deref(), Some("Jane"));
}
