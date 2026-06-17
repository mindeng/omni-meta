#![no_main]

use libfuzzer_sys::fuzz_target;
use omni_meta::{read_slice, strip_slice, Options, StripOptions};
use omni_meta_fuzz::FuzzAlloc;

#[global_allocator]
static ALLOC: FuzzAlloc = FuzzAlloc::new();

fuzz_target!(|data: &[u8]| {
    for opts in [StripOptions::default(), StripOptions::aggressive()] {
        if let Ok((out, _report)) = strip_slice(data, opts) {
            // 输出必须能被读路径处理（不 panic）。
            let _ = read_slice(&out, Options::default());
            // 幂等：剥离产物再剥离不 panic。
            let _ = strip_slice(&out, opts);
        }
    }
});
