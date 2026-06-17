//! 元数据剥离（写路径）。sans-io 核心：planner 只发 StripCmd，引擎组装输出。
//! 默认隐私模式（剥离 EXIF/XMP/IPTC，保留 ICC/orientation）；aggressive 全删。

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::error::Error;
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
        Self {
            limits: Limits::default(),
            keep_icc: true,
            keep_orientation: true,
        }
    }
}

impl StripOptions {
    /// 隐私极端模式：连 ICC/orientation 一并删除（可能偏色/翻车）。
    pub fn aggressive() -> Self {
        Self {
            keep_icc: false,
            keep_orientation: false,
            ..Self::default()
        }
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

/// planner 下一步需求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripDemand {
    /// 还需更多输入（slice 全缓冲下若无更多 = 截断，引擎终止）。
    /// 当前所有 walker 全缓冲一次性完成；此变体保留用于流式扩展，引擎已处理。
    #[allow(dead_code)]
    More,
    /// 完成。
    Done,
}

/// 一步指令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StripCmd {
    /// 把输入窗口接下来的 n 字节原样拷到输出。
    Emit(usize),
    /// 丢弃输入窗口接下来的 n 字节（被剥离），计入报告。
    Drop { len: usize, kind: RemovedKind },
    /// 消费 consume 字节、改写为 `with` 写入输出。
    Replace { consume: usize, with: Vec<u8> },
    /// 不消费输入，注入 `with`（合成段）。
    Insert(Vec<u8>),
    /// 纯记账：把 len 字节计入报告的 removed/bytes_removed，不消费输入、不产输出。
    /// 供整体重建型 walker（WebP）使用。
    Account { len: u64, kind: RemovedKind },
}

/// 一次 pull 的结果。`consumed` = 本步覆盖的输入字节（Emit+Drop+Replace.consume 之和）。
pub struct StripResult {
    pub demand: StripDemand,
    pub consumed: usize,
    pub cmds: Vec<StripCmd>,
}

/// 格式 strip walker 的唯一 trait——纯状态机。
pub trait StripPlanner {
    fn pull(&mut self, input: &[u8]) -> StripResult;
}

/// slice 引擎：把整缓冲反复喂给 planner，按指令组装输出 + 报告。
pub(crate) fn drive_strip_slice(
    buf: &[u8],
    planner: &mut dyn StripPlanner,
    format: FileFormat,
) -> (Vec<u8>, StripReport) {
    let mut out: Vec<u8> = Vec::new();
    let mut report = StripReport {
        format,
        bytes_removed: 0,
        removed: RemovedKinds::default(),
        warnings: Vec::new(),
    };
    let mut pos = 0usize;
    loop {
        let window = &buf[pos.min(buf.len())..];
        let res = planner.pull(window);
        // 应用指令；cur 跟踪窗口内消费位置（仅消费型指令推进）。
        let mut cur = 0usize;
        for cmd in res.cmds {
            match cmd {
                StripCmd::Emit(n) => {
                    let end = cur.saturating_add(n).min(window.len());
                    out.extend_from_slice(&window[cur..end]);
                    cur = end;
                }
                StripCmd::Drop { len, kind } => {
                    let end = cur.saturating_add(len).min(window.len());
                    report.bytes_removed += (end - cur) as u64;
                    report.removed.insert(kind);
                    cur = end;
                }
                StripCmd::Replace { consume, with } => {
                    let end = cur.saturating_add(consume).min(window.len());
                    out.extend_from_slice(&with);
                    cur = end;
                }
                StripCmd::Insert(with) => {
                    out.extend_from_slice(&with);
                }
                StripCmd::Account { len, kind } => {
                    report.bytes_removed += len;
                    report.removed.insert(kind);
                }
            }
        }
        pos = pos.saturating_add(res.consumed).min(buf.len());
        match res.demand {
            StripDemand::Done => break,
            StripDemand::More => {
                // slice 全缓冲：若已到尾且 planner 仍要更多 = 截断，安全终止。
                if pos >= buf.len() {
                    break;
                }
            }
        }
    }
    (out, report)
}

/// 按格式选 walker。仅 JPEG/PNG/WebP 支持；其余已识别格式 → Unsupported。
pub(crate) fn planner_for(
    fmt: &FileFormat,
    opts: StripOptions,
) -> Result<Box<dyn StripPlanner>, Error> {
    match fmt {
        FileFormat::Jpeg => Ok(Box::new(jpeg::JpegStripper::new(opts))),
        FileFormat::Png => Ok(Box::new(png::PngStripper::new(opts))),
        FileFormat::Webp => Ok(Box::new(webp::WebpStripper::new(opts))),
        FileFormat::Unknown => Err(Error::UnrecognizedFormat),
        _ => Err(Error::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 假 planner：第一拉 Emit(2)+Drop(3,Exif)+Insert([0xAA])，第二拉 Replace(2->[0xBB])，第三拉 Done。
    struct FakePlanner {
        step: u8,
    }
    impl StripPlanner for FakePlanner {
        fn pull(&mut self, input: &[u8]) -> StripResult {
            self.step += 1;
            match self.step {
                1 => StripResult {
                    demand: StripDemand::More,
                    consumed: 5,
                    cmds: alloc::vec![
                        StripCmd::Emit(2),
                        StripCmd::Drop {
                            len: 3,
                            kind: RemovedKind::Exif
                        },
                        StripCmd::Insert(alloc::vec![0xAA]),
                    ],
                },
                2 => StripResult {
                    demand: StripDemand::More,
                    consumed: 2,
                    cmds: alloc::vec![StripCmd::Replace {
                        consume: 2,
                        with: alloc::vec![0xBB]
                    }],
                },
                _ => {
                    let _ = input;
                    StripResult {
                        demand: StripDemand::Done,
                        consumed: 0,
                        cmds: alloc::vec![],
                    }
                }
            }
        }
    }

    #[test]
    fn engine_applies_emit_drop_insert_replace() {
        // 输入 7 字节：前 5 给第一拉（Emit 2 + Drop 3），后 2 给第二拉（Replace）。
        let buf = [1u8, 2, 3, 4, 5, 6, 7];
        let mut p = FakePlanner { step: 0 };
        let (out, report) = drive_strip_slice(&buf, &mut p, FileFormat::Jpeg);
        // 输出：Emit[1,2] + Insert[0xAA] + Replace[0xBB] = [1,2,0xAA,0xBB]
        assert_eq!(out, alloc::vec![1, 2, 0xAA, 0xBB]);
        assert_eq!(report.bytes_removed, 3);
        assert!(report.removed.contains(RemovedKind::Exif));
        assert_eq!(report.format, FileFormat::Jpeg);
    }

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
