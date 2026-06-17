//! omni-meta 测试/模糊共享 fixtures：纯字节构造器 + 四适配器一致性 oracle。
//! 差分集成测试与 fuzz 种子生成器共用，单一真相源（DRY）。

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
