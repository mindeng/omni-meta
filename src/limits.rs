//! 解析不可信输入时的分配上界，防 OOM / 解压炸弹 / 深递归。

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_payload_bytes: usize,
    pub max_retained_bytes: usize,
    pub max_depth: u16,
    pub max_tags: usize,
    pub max_total_alloc: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_payload_bytes: 64 * 1024 * 1024,
            max_retained_bytes: 16 * 1024 * 1024,
            max_depth: 32,
            max_tags: 8192,
            max_total_alloc: 128 * 1024 * 1024,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let l = Limits::default();
        assert_eq!(l.max_tags, 8192);
        assert!(l.max_retained_bytes < l.max_total_alloc);
    }
}
