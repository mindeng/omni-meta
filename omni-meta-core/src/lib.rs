#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

// 内部模块一律 pub(crate)：公开 API 仅通过下方精选的 `pub use` 暴露，
// 避免内部路径（如 omni_meta::driver::drive_slice）固化成 semver 稳定面。
pub(crate) mod adapters;
pub(crate) mod codecs;
pub(crate) mod formats;
pub(crate) mod cursor;
pub(crate) mod demand;
pub(crate) mod error;
pub(crate) mod limits;
pub(crate) mod model;
pub(crate) mod normalize;
pub(crate) mod probe;
pub(crate) mod driver;

pub use adapters::push::PushParser;
pub use adapters::slice::{read_slice, Options};
pub use driver::Outcome;
pub use error::Error;
pub use limits::Limits;
pub use model::{
    ExifTag, FileFormat, Metadata, Orientation, RawTags, Unified, Value, WarnKind, Warning,
    XmpProperty,
};

#[cfg(test)]
mod smoke {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
