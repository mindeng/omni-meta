#![no_main]

use libfuzzer_sys::fuzz_target;
use omni_meta::{read_slice, Options};
use omni_meta_fuzz::{FuzzAlloc, FUZZ_LIMITS};

#[global_allocator]
static ALLOC: FuzzAlloc = FuzzAlloc::new();

fuzz_target!(|data: &[u8]| {
    let opts = Options { limits: FUZZ_LIMITS };
    if let Ok(meta) = read_slice(data, opts) {
        // 容器标签数受 max_tags 显式封顶——产物计数必须落在 Limits 内。
        assert!(
            meta.raw.container.len() <= FUZZ_LIMITS.max_tags,
            "container tags {} 超过 max_tags {}",
            meta.raw.container.len(),
            FUZZ_LIMITS.max_tags
        );
    }
    // 分配上界由 FuzzAlloc 守护：越顶即 abort。
});
