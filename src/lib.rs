#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod adapters;
pub mod codecs;
pub mod formats;
pub mod cursor;
pub mod demand;
pub mod error;
pub mod limits;
pub mod model;
pub mod normalize;
pub mod probe;
pub mod driver;

pub use adapters::slice::{read_slice, Options};
pub use error::Error;
pub use limits::Limits;
pub use model::{
    ExifTag, FileFormat, Metadata, Orientation, RawTags, Unified, Value, WarnKind, Warning,
};

#[cfg(test)]
mod smoke {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
