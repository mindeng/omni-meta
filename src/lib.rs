#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod codecs;
pub mod cursor;
pub mod demand;
pub mod error;
pub mod limits;
pub mod model;
pub mod normalize;

#[cfg(test)]
mod smoke {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
