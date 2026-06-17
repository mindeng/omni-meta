//! fuzz 共享：可证伪的分配上界（计数分配器）+ 测试用小 Limits。

use std::sync::atomic::{AtomicUsize, Ordering};

use omni_meta::Limits;

/// 模糊用收紧 Limits：远低于 Default，使分配上界在合理时间内可达。
pub const FUZZ_LIMITS: Limits = Limits {
    max_payload_bytes: 1 << 20,
    max_retained_bytes: 1 << 20,
    max_depth: 16,
    max_tags: 256,
    max_ifds: 8,
    max_total_alloc: 8 << 20,
};

/// 全局分配上界（字节）。远高于 FUZZ_LIMITS.max_total_alloc：合法解析通过，
/// 失控分配触发——返回 null 触发 Rust alloc 错误处理 → abort（libfuzzer 捕获）。
pub const ALLOC_CEILING: usize = 256 * 1024 * 1024;

/// 纯计数逻辑（不真正分配，便于单测，不会 abort）。
pub struct AllocCounter {
    live: AtomicUsize,
    ceiling: usize,
}

impl AllocCounter {
    pub const fn new(ceiling: usize) -> Self {
        Self { live: AtomicUsize::new(0), ceiling }
    }

    /// 预约 n 字节：若会越顶返回 false（不计入），否则计入返回 true。
    pub fn try_add(&self, n: usize) -> bool {
        let mut cur = self.live.load(Ordering::Relaxed);
        loop {
            let next = match cur.checked_add(n) {
                Some(v) if v <= self.ceiling => v,
                _ => return false,
            };
            match self.live.compare_exchange_weak(cur, next, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return true,
                Err(actual) => cur = actual,
            }
        }
    }

    pub fn sub(&self, n: usize) {
        self.live.fetch_sub(n, Ordering::Relaxed);
    }

    pub fn live(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }
}

use std::alloc::{GlobalAlloc, Layout, System};

/// 全局计数分配器：委托 System，按 ALLOC_CEILING 守上界。越顶 → 返回 null →
/// Rust alloc 错误处理 abort（libfuzzer 记为可复现 crash，带分配栈）。
pub struct FuzzAlloc {
    counter: AllocCounter,
}

impl FuzzAlloc {
    pub const fn new() -> Self {
        Self { counter: AllocCounter::new(ALLOC_CEILING) }
    }
}

impl Default for FuzzAlloc {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl GlobalAlloc for FuzzAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !self.counter.try_add(layout.size()) {
            return core::ptr::null_mut();
        }
        let p = unsafe { System.alloc(layout) };
        if p.is_null() {
            self.counter.sub(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        self.counter.sub(layout.size());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_caps_without_allocating() {
        let c = AllocCounter::new(100);
        assert!(c.try_add(60));
        assert_eq!(c.live(), 60);
        assert!(!c.try_add(60), "越顶预约必须被拒且不计入");
        assert_eq!(c.live(), 60, "被拒预约不得改变 live");
        c.sub(60);
        assert_eq!(c.live(), 0);
        assert!(c.try_add(60), "释放后可再预约");
    }

    #[test]
    fn add_overflow_is_rejected() {
        let c = AllocCounter::new(usize::MAX);
        assert!(c.try_add(usize::MAX));
        assert!(!c.try_add(1), "溢出（checked_add 失败）必须被拒");
    }
}
