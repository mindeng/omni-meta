//! omni-meta：batteries-included facade。
//! 重导出 omni-meta-core 的全部公开面，并在 std 下提供 I/O 适配器。
#![forbid(unsafe_code)]

pub use omni_meta_core::*;

#[cfg(feature = "std")]
mod adapters;
#[cfg(feature = "std")]
pub use adapters::blocking::read_blocking;
#[cfg(feature = "std")]
pub use adapters::seek::read_seek;
#[cfg(feature = "std")]
pub use adapters::strip_blocking::strip_blocking;
