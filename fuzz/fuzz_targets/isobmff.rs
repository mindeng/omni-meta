#![no_main]

use libfuzzer_sys::fuzz_target;
use omni_meta::__fuzzing::drive_bmff;
use omni_meta_fuzz::{FuzzAlloc, FUZZ_LIMITS};

#[global_allocator]
static ALLOC: FuzzAlloc = FuzzAlloc::new();

fuzz_target!(|data: &[u8]| {
    let meta = drive_bmff(data, FUZZ_LIMITS);
    assert!(meta.raw.container.len() <= FUZZ_LIMITS.max_tags);
});
