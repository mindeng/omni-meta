#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

// 内部模块一律 pub(crate)：公开 API 仅通过下方精选的 `pub use` 暴露，
// 避免内部路径（如 omni_meta::driver::drive_slice）固化成 semver 稳定面。
pub(crate) mod adapters;
pub(crate) mod codecs;
pub(crate) mod containers;
pub(crate) mod formats;
pub(crate) mod civil;
pub(crate) mod cursor;
pub(crate) mod demand;
pub(crate) mod error;
pub(crate) mod limits;
pub(crate) mod model;
pub(crate) mod normalize;
pub(crate) mod probe;
pub(crate) mod driver;
pub(crate) mod strip;

pub use adapters::push::PushParser;
pub use adapters::slice::{read_slice, Options};
pub use driver::Outcome;
pub use error::Error;
pub use limits::Limits;
pub use model::{
    DateTimeParts, ExifTag, FileFormat, IfdKind, Metadata, Orientation, RawTags, Unified, Value,
    WarnKind, Warning, XmpProperty,
};

/// 模糊专用入口（薄包装）。仅在 `__fuzzing` 特性下编译；`#[doc(hidden)]` 且
/// 双下划线命名，明确「内部、非 semver 稳定」。绕过 probe，强制走指定解析路径。
#[cfg(feature = "__fuzzing")]
#[doc(hidden)]
pub mod __fuzzing {
    use alloc::vec::Vec;

    use crate::limits::Limits;
    use crate::model::{ExifTag, FileFormat, Metadata, Warning, XmpProperty};

    /// 直接在裸 TIFF 字节上跑 EXIF codec。
    pub fn decode_exif(tiff: &[u8], limits: &Limits) -> (Vec<ExifTag>, Vec<Warning>) {
        let mut out = Vec::new();
        let mut warnings = Vec::new();
        crate::codecs::exif::decode(tiff, &mut out, &mut warnings, limits);
        (out, warnings)
    }

    /// 直接在 XMP 包字节上跑 XMP codec。
    pub fn decode_xmp(packet: &[u8], limits: &Limits) -> (Vec<XmpProperty>, Vec<Warning>) {
        let mut out = Vec::new();
        let mut warnings = Vec::new();
        crate::codecs::xmp::decode(packet, &mut out, &mut warnings, limits);
        (out, warnings)
    }

    /// 强制以 BMFF 解析器在 slice 上驱动到底，投影为 Metadata。
    pub fn drive_bmff(data: &[u8], limits: Limits) -> Metadata {
        let mut parser = crate::formats::bmff::BmffParser::with_limits(limits);
        let col = crate::driver::drive_slice(data, &mut parser, limits);
        crate::driver::finalize(col, FileFormat::Mp4)
    }

    /// 强制以 EBML 解析器在 slice 上驱动到底，投影为 Metadata。
    pub fn drive_ebml(data: &[u8], limits: Limits) -> Metadata {
        let mut parser = crate::formats::ebml::EbmlParser::new();
        let col = crate::driver::drive_slice(data, &mut parser, limits);
        crate::driver::finalize(col, FileFormat::Mkv)
    }
}

#[cfg(test)]
mod smoke {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}

#[cfg(all(test, feature = "__fuzzing"))]
mod fuzzing_api_tests {
    use crate::limits::Limits;

    #[test]
    fn decode_exif_wrapper_runs_and_is_bounded() {
        // 裸 TIFF：II + 42 + IFD0@8，count=1，一条 Make(0x010F) ASCII="A\0"
        let tiff: &[u8] = &[
            b'I', b'I', 42, 0, 8, 0, 0, 0, // header
            1, 0, // IFD0 count=1
            0x0F, 0x01, 2, 0, 2, 0, 0, 0, b'A', 0, 0, 0, // Make ASCII cnt=2 inline "A\0"
            0, 0, 0, 0, // next IFD = 0
        ];
        let (tags, warns) = crate::__fuzzing::decode_exif(tiff, &Limits::default());
        assert!(tags.len() <= Limits::default().max_tags);
        let _ = warns;
    }

    #[test]
    fn decode_xmp_wrapper_runs() {
        let (props, _w) = crate::__fuzzing::decode_xmp(
            br#"<rdf:Description xmlns:tiff="n" tiff:Make="Acme"/>"#,
            &Limits::default(),
        );
        assert!(props.iter().any(|p| p.name == "Make"));
    }

    #[test]
    fn drive_bmff_wrapper_runs_on_garbage_without_panic() {
        let m = crate::__fuzzing::drive_bmff(&[0u8; 32], Limits::default());
        assert!(m.raw.container.len() <= Limits::default().max_tags);
    }

    #[test]
    fn drive_ebml_wrapper_runs_on_garbage_without_panic() {
        let _ = crate::__fuzzing::drive_ebml(&[0u8; 32], Limits::default());
    }
}
