//! 元数据剥离（写路径）。sans-io 核心：planner 只发 StripCmd，引擎组装输出。
//! 默认隐私模式（剥离 EXIF/XMP/IPTC，保留 ICC/orientation）；aggressive 全删。

// 本模块的类型在后续 strip 任务（引擎/各格式 walker/适配器）中逐步被消费；
// 在构建过程中暂允许 dead_code，待 T11 全部接入后移除本属性。
#![allow(dead_code)]

use alloc::vec::Vec;

use crate::limits::Limits;
use crate::model::{FileFormat, Warning};

pub mod crc32;
pub mod exif_synth;
pub mod jpeg;
pub mod png;
pub mod webp;

/// 单类被删元数据的归类，用于报告统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovedKind {
    Exif,
    Xmp,
    Iptc,
    Icc,
    Other,
}

/// 被删类别的集合（位标记）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemovedKinds {
    bits: u8,
}

impl RemovedKinds {
    fn bit(kind: RemovedKind) -> u8 {
        match kind {
            RemovedKind::Exif => 1 << 0,
            RemovedKind::Xmp => 1 << 1,
            RemovedKind::Iptc => 1 << 2,
            RemovedKind::Icc => 1 << 3,
            RemovedKind::Other => 1 << 4,
        }
    }
    pub fn insert(&mut self, kind: RemovedKind) {
        self.bits |= Self::bit(kind);
    }
    pub fn contains(&self, kind: RemovedKind) -> bool {
        self.bits & Self::bit(kind) != 0
    }
    pub fn is_empty(&self) -> bool {
        self.bits == 0
    }
}

/// 剥离选项。默认隐私模式：保留 ICC / orientation。
#[derive(Clone, Copy, Debug)]
pub struct StripOptions {
    pub limits: Limits,
    /// true：保留 ICC 色彩配置（避免偏色）。默认 true。
    pub keep_icc: bool,
    /// true：保留方向（避免显示翻车，经最小 EXIF 合成）。默认 true。
    pub keep_orientation: bool,
}

impl Default for StripOptions {
    fn default() -> Self {
        Self { limits: Limits::default(), keep_icc: true, keep_orientation: true }
    }
}

impl StripOptions {
    /// 隐私极端模式：连 ICC/orientation 一并删除（可能偏色/翻车）。
    pub fn aggressive() -> Self {
        Self { keep_icc: false, keep_orientation: false, ..Self::default() }
    }
}

/// 剥离报告。
#[derive(Debug, Clone)]
pub struct StripReport {
    pub format: FileFormat,
    pub bytes_removed: u64,
    pub removed: RemovedKinds,
    pub warnings: Vec<Warning>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_default_is_privacy_mode() {
        let o = StripOptions::default();
        assert!(o.keep_icc);
        assert!(o.keep_orientation);
    }

    #[test]
    fn options_aggressive_strips_everything() {
        let o = StripOptions::aggressive();
        assert!(!o.keep_icc);
        assert!(!o.keep_orientation);
    }

    #[test]
    fn removed_kinds_sets_and_queries_flags() {
        let mut r = RemovedKinds::default();
        assert!(!r.contains(RemovedKind::Exif));
        r.insert(RemovedKind::Exif);
        r.insert(RemovedKind::Xmp);
        assert!(r.contains(RemovedKind::Exif));
        assert!(r.contains(RemovedKind::Xmp));
        assert!(!r.contains(RemovedKind::Icc));
    }

    #[test]
    fn report_constructs_with_defaults() {
        let rep = StripReport {
            format: crate::model::FileFormat::Jpeg,
            bytes_removed: 0,
            removed: RemovedKinds::default(),
            warnings: alloc::vec::Vec::new(),
        };
        assert_eq!(rep.bytes_removed, 0);
        assert_eq!(rep.format, crate::model::FileFormat::Jpeg);
    }
}
