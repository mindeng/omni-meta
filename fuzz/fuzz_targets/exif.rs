#![no_main]

use libfuzzer_sys::fuzz_target;
use omni_meta::__fuzzing::decode_exif;
use omni_meta_fuzz::{FuzzAlloc, FUZZ_LIMITS};

#[global_allocator]
static ALLOC: FuzzAlloc = FuzzAlloc::new();

fuzz_target!(|data: &[u8]| {
    let (tags, _warns) = decode_exif(data, &FUZZ_LIMITS);
    assert!(tags.len() <= FUZZ_LIMITS.max_tags);
});
