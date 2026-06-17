#![no_main]

use libfuzzer_sys::fuzz_target;
use omni_meta::__fuzzing::decode_xmp;
use omni_meta_fuzz::{FuzzAlloc, FUZZ_LIMITS};

#[global_allocator]
static ALLOC: FuzzAlloc = FuzzAlloc::new();

fuzz_target!(|data: &[u8]| {
    let (props, _warns) = decode_xmp(data, &FUZZ_LIMITS);
    assert!(props.len() <= FUZZ_LIMITS.max_tags);
});
