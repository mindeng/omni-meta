#![no_main]

use libfuzzer_sys::fuzz_target;
use omni_meta_fixtures::{adapters_outcome, Agreement};
use omni_meta_fuzz::FuzzAlloc;

#[global_allocator]
static ALLOC: FuzzAlloc = FuzzAlloc::new();

fuzz_target!(|data: &[u8]| {
    // 任意字节经真实 probe→driver 路径过四适配器：分歧即违反核心契约 → panic。
    if let Agreement::Disagree(why) = adapters_outcome(data) {
        panic!("adapter disagreement on {} bytes: {why}", data.len());
    }
});
