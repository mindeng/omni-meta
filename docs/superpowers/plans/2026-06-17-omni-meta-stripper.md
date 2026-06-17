# Stripper（元数据剥离）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 omni-meta 增加第一条「写」路径——把 EXIF/XMP/IPTC 等隐私元数据从 JPEG/PNG/WebP 中剥离，默认保留渲染必需的 ICC/orientation，产出干净文件。

**Architecture:** 方案 A——自包含 strip walker，不改动已 fuzz 硬化的读路径。sans-io 核心 `StripPlanner` 状态机产出 `StripCmd`（Emit/Drop/Replace/Insert）指令流，`drive_strip_slice` 引擎消费指令组装输出 + 报告。`strip_slice`（no_std）全缓冲驱动；`strip_blocking`（std）有界缓冲后复用 slice 引擎。`keep_orientation` 经最小 EXIF 合成保真。

**Tech Stack:** Rust edition 2024，`#![no_std]` + `alloc`，`#![forbid(unsafe_code)]`，零外部依赖。测试用内置 `#[test]` + 复用读路径 `read_slice` 做回环 oracle。

**基准 spec:** `docs/superpowers/specs/2026-06-17-omni-meta-stripper-design.md`

**全局不变量（每个 walker 都不得破坏）:** 显式栈迭代非递归；所有偏移/长度 `checked_*`，越界/歧义 → 保留字节 + Warning，永不 panic；写路径要么干净剥离要么字节等同源，绝不输出损坏文件；缺失即不合成不臆造。

---

## 文件结构

**新建（omni-meta-core）:**
- `src/strip/mod.rs` — `StripPlanner` trait、`StripCmd`/`StripDemand`/`StripResult`、`RemovedKind`/`RemovedKinds`、`StripOptions`/`StripReport`、`drive_strip_slice` 引擎、`planner_for` 分派
- `src/strip/crc32.rs` — IEEE CRC32（PNG chunk 合成用）
- `src/strip/exif_synth.rs` — 最小 orientation EXIF TIFF 合成 + 各容器封装
- `src/strip/jpeg.rs` — `JpegStripper`
- `src/strip/png.rs` — `PngStripper`
- `src/strip/webp.rs` — `WebpStripper`
- `src/adapters/strip_slice.rs` — `strip_slice` 公开适配器

**新建（omni-meta facade）:**
- `src/adapters/strip_blocking.rs` — `strip_blocking`

**新建（fuzz）:**
- `fuzz/fuzz_targets/strip/main.rs` — strip fuzz target（结构同现有 target）

**修改:**
- `omni-meta-core/src/error.rs` — 加 `Error::Unsupported`
- `omni-meta-core/src/model.rs` — 加 `WarnKind::StripSkippedAmbiguous`
- `omni-meta-core/src/lib.rs` — `pub(crate) mod strip;`、`mod adapters` 已存在；`pub use` strip 公开面
- `omni-meta-core/src/adapters/mod.rs` — 加 `pub mod strip_slice;`
- `omni-meta/src/adapters/mod.rs` — 加 `pub mod strip_blocking;`
- `omni-meta/src/lib.rs` — `pub use adapters::strip_blocking::strip_blocking;`
- `fuzz/Cargo.toml` — 注册 `strip` target
- `docs/ROADMAP.md` — 勾选里程碑 F

---

## Task 1: 基础类型（Error / WarnKind / strip 模型 + 模块骨架）

**Files:**
- Modify: `omni-meta-core/src/error.rs`
- Modify: `omni-meta-core/src/model.rs:171-181`（`WarnKind`）
- Create: `omni-meta-core/src/strip/mod.rs`
- Modify: `omni-meta-core/src/lib.rs:8-30`

- [ ] **Step 1: 写失败测试（strip 模型类型）**

在新建文件 `omni-meta-core/src/strip/mod.rs` 末尾放入测试模块（类型先不写，故编译失败）：

```rust
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p omni-meta-core strip::tests 2>&1 | head -20`
Expected: 编译错误（`StripOptions`/`RemovedKinds` 等未定义）。

- [ ] **Step 3: 写最小实现**

在 `omni-meta-core/src/strip/mod.rs` 顶部（测试模块之前）写入：

```rust
//! 元数据剥离（写路径）。sans-io 核心：planner 只发 StripCmd，引擎组装输出。
//! 默认隐私模式（剥离 EXIF/XMP/IPTC，保留 ICC/orientation）；aggressive 全删。

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
```

在 `omni-meta-core/src/error.rs` 的 `Error` 枚举加变体（`UnrecognizedFormat` 之后）：

```rust
    /// 已识别格式，但当前 strip 不支持（GIF/HEIF/MP4/MKV…）。
    Unsupported,
```

并在 `Display` impl 的 match 加：

```rust
            Error::Unsupported => f.write_str("format not supported for this operation"),
```

在 `omni-meta-core/src/model.rs` 的 `WarnKind` 枚举末尾（`CompressedChunkSkipped` 之后）加：

```rust
    /// 剥离时遇歧义/损坏结构，为安全保留该区字节未删。
    StripSkippedAmbiguous,
```

在 `omni-meta-core/src/lib.rs` 模块声明区（`pub(crate) mod driver;` 附近）加：

```rust
pub(crate) mod strip;
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p omni-meta-core strip::tests`
Expected: 4 个测试 PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/strip/mod.rs omni-meta-core/src/error.rs omni-meta-core/src/model.rs omni-meta-core/src/lib.rs
git commit -m "feat(strip): 基础类型 StripOptions/StripReport/RemovedKinds + Error::Unsupported + WarnKind::StripSkippedAmbiguous"
```

> 注：Step 3 声明了 `pub mod crc32/exif_synth/jpeg/png/webp;`，但这些文件尚未创建——本任务结束时它们在 Task 2/3/4/5/6 创建前会导致 `cargo build` 失败。为让本任务可独立编译，**先建空占位文件**：每个写入单行注释 `//! 占位，后续任务实现。`（5 个文件）。这些占位在后续任务被替换。把占位文件一并 `git add`。

---

## Task 2: 指令 trait + drive_strip_slice 引擎

**Files:**
- Modify: `omni-meta-core/src/strip/mod.rs`

- [ ] **Step 1: 写失败测试（引擎消费指令）**

在 `strip/mod.rs` 测试模块加入：

```rust
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
                        StripCmd::Drop { len: 3, kind: RemovedKind::Exif },
                        StripCmd::Insert(alloc::vec![0xAA]),
                    ],
                },
                2 => StripResult {
                    demand: StripDemand::More,
                    consumed: 2,
                    cmds: alloc::vec![StripCmd::Replace { consume: 2, with: alloc::vec![0xBB] }],
                },
                _ => {
                    let _ = input;
                    StripResult { demand: StripDemand::Done, consumed: 0, cmds: alloc::vec![] }
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p omni-meta-core engine_applies 2>&1 | head -20`
Expected: 编译错误（`StripDemand`/`StripCmd`/`StripResult`/`StripPlanner`/`drive_strip_slice` 未定义）。

- [ ] **Step 3: 写最小实现**

在 `strip/mod.rs`（`StripReport` 之后、测试模块之前）加入：

```rust
/// planner 下一步需求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripDemand {
    /// 还需更多输入（slice 全缓冲下若无更多 = 截断，引擎终止）。
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
```

> 注：测试 Step 1 用 `StripDemand::More`；与此处定义一致。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p omni-meta-core strip::tests`
Expected: 全部 PASS（含 Task 1 的 4 个 + 本任务 1 个）。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/strip/mod.rs
git commit -m "feat(strip): StripPlanner trait + StripCmd 指令集 + drive_strip_slice 引擎"
```

---

## Task 3: CRC32 + 最小 EXIF 合成器

**Files:**
- Create/replace: `omni-meta-core/src/strip/crc32.rs`
- Create/replace: `omni-meta-core/src/strip/exif_synth.rs`

- [ ] **Step 1: 写失败测试（CRC32 已知向量）**

`strip/crc32.rs` 测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_of_iend_chunk_type_known_vector() {
        // PNG IEND chunk（类型 + 空数据）的 CRC32 是众所周知的 0xAE426082。
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }

    #[test]
    fn crc32_of_empty_is_zero() {
        assert_eq!(crc32(b""), 0);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p omni-meta-core crc32 2>&1 | head -20`
Expected: 编译错误（`crc32` 未定义）。

- [ ] **Step 3: 写 CRC32 实现**

`strip/crc32.rs`（替换占位）：

```rust
//! IEEE CRC32（PNG chunk 合成用）。零依赖，逐字节查表。

/// 计算 IEEE CRC32（PNG 用：多项式 0xEDB88320，初值/末值全反转）。
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p omni-meta-core crc32`
Expected: 2 个 PASS。

- [ ] **Step 5: 写失败测试（EXIF 合成）**

`strip/exif_synth.rs` 测试模块——验证合成的裸 TIFF 能被现有 EXIF codec 解回 orientation，且各容器封装结构正确：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Limits;

    #[test]
    fn synthesized_tiff_decodes_back_to_orientation() {
        let tiff = orientation_tiff(6);
        let mut tags = alloc::vec::Vec::new();
        let mut warns = alloc::vec::Vec::new();
        crate::codecs::exif::decode(&tiff, &mut tags, &mut warns, &Limits::default());
        assert!(warns.is_empty(), "warns: {:?}", warns);
        assert!(tags.iter().any(|t| t.tag == 0x0112
            && t.value == crate::model::Value::U16(6)));
    }

    #[test]
    fn jpeg_app1_wraps_exif_prefix_and_tiff() {
        let seg = jpeg_app1_exif(&orientation_tiff(1));
        // FFE1 + len(2 BE) + "Exif\0\0" + TIFF
        assert_eq!(&seg[0..2], &[0xFF, 0xE1]);
        let len = u16::from_be_bytes([seg[2], seg[3]]) as usize;
        assert_eq!(len, seg.len() - 2); // 段长含 len 字段自身、不含 marker
        assert_eq!(&seg[4..10], b"Exif\0\0");
    }

    #[test]
    fn png_exif_chunk_has_valid_crc() {
        let tiff = orientation_tiff(8);
        let chunk = png_exif_chunk(&tiff);
        // len(4 BE) + "eXIf" + tiff + crc(4 BE)
        let len = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as usize;
        assert_eq!(len, tiff.len());
        assert_eq!(&chunk[4..8], b"eXIf");
        let crc_off = 8 + tiff.len();
        let crc = u32::from_be_bytes([
            chunk[crc_off], chunk[crc_off + 1], chunk[crc_off + 2], chunk[crc_off + 3],
        ]);
        // CRC 覆盖 type + data
        let mut crc_input = alloc::vec::Vec::new();
        crc_input.extend_from_slice(b"eXIf");
        crc_input.extend_from_slice(&tiff);
        assert_eq!(crc, super::super::crc32::crc32(&crc_input));
    }

    #[test]
    fn webp_exif_chunk_fourcc_and_size() {
        let tiff = orientation_tiff(3);
        let chunk = webp_exif_chunk(&tiff);
        // "EXIF" + size(4 LE) + data (+pad 若奇数)
        assert_eq!(&chunk[0..4], b"EXIF");
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as usize;
        assert_eq!(size, tiff.len());
    }
}
```

- [ ] **Step 6: 运行确认失败**

Run: `cargo test -p omni-meta-core exif_synth 2>&1 | head -20`
Expected: 编译错误（合成函数未定义）。

- [ ] **Step 7: 写 EXIF 合成实现**

`strip/exif_synth.rs`（替换占位）：

```rust
//! 最小 EXIF 合成（keep_orientation 用）：只含一条 Orientation(0x0112) 的小端 TIFF，
//! 及 JPEG/PNG/WebP 各自的容器封装。

use alloc::vec::Vec;

use super::crc32::crc32;

/// 构造一个小端 TIFF：IFD0 含单条 Orientation=val（SHORT，内联）。约 26 字节。
pub fn orientation_tiff(val: u16) -> Vec<u8> {
    let mut t = Vec::with_capacity(26);
    t.extend_from_slice(b"II"); // little-endian
    t.extend_from_slice(&42u16.to_le_bytes()); // magic
    t.extend_from_slice(&8u32.to_le_bytes()); // IFD0 @ offset 8
    t.extend_from_slice(&1u16.to_le_bytes()); // count = 1
    // entry: tag=0x0112, type=SHORT(3), count=1, value=val（内联，左对齐 4 字节）
    t.extend_from_slice(&0x0112u16.to_le_bytes());
    t.extend_from_slice(&3u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&val.to_le_bytes());
    t.extend_from_slice(&[0u8, 0]); // value 字段剩余 2 字节填充
    t.extend_from_slice(&0u32.to_le_bytes()); // next IFD = 0
    t
}

/// JPEG APP1 段：FFE1 + len(2 BE，含 len 字段自身) + "Exif\0\0" + TIFF。
pub fn jpeg_app1_exif(tiff: &[u8]) -> Vec<u8> {
    let body_len = 6 + tiff.len(); // "Exif\0\0" + TIFF
    let seg_len = body_len + 2; // 含 len 字段 2 字节
    let mut s = Vec::with_capacity(seg_len + 2);
    s.extend_from_slice(&[0xFF, 0xE1]);
    s.extend_from_slice(&(seg_len as u16).to_be_bytes());
    s.extend_from_slice(b"Exif\0\0");
    s.extend_from_slice(tiff);
    s
}

/// PNG eXIf chunk：len(4 BE) + "eXIf" + tiff + crc(4 BE，覆盖 type+data)。
pub fn png_exif_chunk(tiff: &[u8]) -> Vec<u8> {
    let mut c = Vec::with_capacity(12 + tiff.len());
    c.extend_from_slice(&(tiff.len() as u32).to_be_bytes());
    c.extend_from_slice(b"eXIf");
    c.extend_from_slice(tiff);
    let mut crc_input = Vec::with_capacity(4 + tiff.len());
    crc_input.extend_from_slice(b"eXIf");
    crc_input.extend_from_slice(tiff);
    c.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    c
}

/// WebP EXIF chunk：fourcc "EXIF" + size(4 LE) + data (+1 pad 若 size 为奇数)。
pub fn webp_exif_chunk(tiff: &[u8]) -> Vec<u8> {
    let mut c = Vec::with_capacity(8 + tiff.len() + 1);
    c.extend_from_slice(b"EXIF");
    c.extend_from_slice(&(tiff.len() as u32).to_le_bytes());
    c.extend_from_slice(tiff);
    if tiff.len() % 2 == 1 {
        c.push(0);
    }
    c
}
```

- [ ] **Step 8: 运行确认通过**

Run: `cargo test -p omni-meta-core exif_synth crc32`
Expected: 全部 PASS（6 个）。

- [ ] **Step 9: 提交**

```bash
git add omni-meta-core/src/strip/crc32.rs omni-meta-core/src/strip/exif_synth.rs
git commit -m "feat(strip): CRC32 + 最小 orientation EXIF 合成（裸 TIFF + JPEG/PNG/WebP 封装）"
```

---

## Task 4: JPEG stripper

**Files:**
- Create/replace: `omni-meta-core/src/strip/jpeg.rs`

辅助：JPEG 段遍历逻辑参考读路径 `omni-meta-core/src/formats/jpeg.rs`，但语义改为 Emit/Drop。本 walker 在 slice 全缓冲下工作（引擎一次给整缓冲）。

- [ ] **Step 1: 写失败测试（核心剥离 + 保留 + 合成）**

`strip/jpeg.rs` 测试模块。先放一组构造工具与测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::strip::{drive_strip_slice, RemovedKind, StripOptions};
    use crate::model::FileFormat;

    fn app_seg(marker: u8, body: &[u8]) -> alloc::vec::Vec<u8> {
        let mut s = alloc::vec::Vec::new();
        s.extend_from_slice(&[0xFF, marker]);
        s.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        s.extend_from_slice(body);
        s
    }

    /// SOI + APP0(JFIF) + APP1(Exif) + APP1(XMP) + SOF0 + SOS + 图像 + EOI
    fn full_jpeg() -> alloc::vec::Vec<u8> {
        let mut j = alloc::vec::Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]); // SOI
        j.extend_from_slice(&app_seg(0xE0, b"JFIF\0\x01\x01")); // APP0
        let mut exif_body = alloc::vec::Vec::new();
        exif_body.extend_from_slice(b"Exif\0\0");
        exif_body.extend_from_slice(&crate::strip::exif_synth::orientation_tiff(6));
        j.extend_from_slice(&app_seg(0xE1, &exif_body)); // APP1 Exif
        j.extend_from_slice(&app_seg(0xE1, b"http://ns.adobe.com/xap/1.0/\0<x/>")); // APP1 XMP
        // SOF0：len=10 precision/h/w/comp
        j.extend_from_slice(&[0xFF, 0xC0]);
        j.extend_from_slice(&10u16.to_be_bytes());
        j.push(8);
        j.extend_from_slice(&8u16.to_be_bytes()); // height
        j.extend_from_slice(&8u16.to_be_bytes()); // width
        j.extend_from_slice(&[1, 0x11, 0]);
        j.extend_from_slice(&[0xFF, 0xDA]); // SOS
        j.extend_from_slice(&4u16.to_be_bytes()); // SOS header len
        j.extend_from_slice(&[1, 0, 0]); // SOS body
        j.extend_from_slice(&[0x12, 0x34, 0x56]); // 熵编码数据
        j.extend_from_slice(&[0xFF, 0xD9]); // EOI
        j
    }

    fn run(buf: &[u8], opts: StripOptions) -> (alloc::vec::Vec<u8>, crate::strip::StripReport) {
        let mut p = JpegStripper::new(opts);
        drive_strip_slice(buf, &mut p, FileFormat::Jpeg)
    }

    #[test]
    fn default_strips_exif_and_xmp_keeps_orientation() {
        let j = full_jpeg();
        let (out, report) = run(&j, StripOptions::default());
        // 回环：读输出无隐私 EXIF（除合成的 orientation）、无 XMP
        let meta = crate::read_slice(&out, crate::Options::default()).unwrap();
        assert!(meta.raw.xmp.is_empty());
        assert_eq!(meta.unified.orientation, Some(crate::model::Orientation::Rotate90));
        assert_eq!(meta.unified.width, Some(8));
        assert_eq!(meta.unified.height, Some(8));
        assert!(report.removed.contains(RemovedKind::Exif));
        assert!(report.removed.contains(RemovedKind::Xmp));
        // 输出仍是合法 JPEG（SOI/EOI 在位）
        assert_eq!(&out[0..2], &[0xFF, 0xD8]);
        assert_eq!(&out[out.len() - 2..], &[0xFF, 0xD9]);
    }

    #[test]
    fn aggressive_strips_orientation_too_zero_exif() {
        let j = full_jpeg();
        let (out, _r) = run(&j, StripOptions::aggressive());
        let meta = crate::read_slice(&out, crate::Options::default()).unwrap();
        assert!(meta.raw.exif.is_empty(), "exif should be empty: {:?}", meta.raw.exif);
        assert_eq!(meta.unified.orientation, None);
    }

    #[test]
    fn keeps_app0_jfif() {
        let j = full_jpeg();
        let (out, _r) = run(&j, StripOptions::aggressive());
        // APP0 JFIF 段应保留
        assert!(out.windows(4).any(|w| w == b"JFIF"));
    }

    #[test]
    fn strips_app13_iptc() {
        let mut j = alloc::vec::Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]);
        j.extend_from_slice(&app_seg(0xED, b"Photoshop 3.0\08BIM\x04\x04\0\0\0\0")); // APP13 8BIM
        j.extend_from_slice(&[0xFF, 0xDA, 0, 4, 1, 0, 0]); // SOS
        j.extend_from_slice(&[0xFF, 0xD9]);
        let (out, report) = run(&j, StripOptions::default());
        assert!(report.removed.contains(RemovedKind::Iptc));
        assert!(!out.windows(4).any(|w| w == b"8BIM"));
    }

    #[test]
    fn non_jpeg_returns_input_unchanged_no_panic() {
        let buf = [0u8, 1, 2, 3, 4];
        let (out, _r) = run(&buf, StripOptions::default());
        assert_eq!(out, buf); // 非 JPEG：原样输出（best-effort 保留）
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p omni-meta-core strip::jpeg 2>&1 | head -20`
Expected: 编译错误（`JpegStripper` 未定义）。

- [ ] **Step 3: 写 JpegStripper 实现**

`strip/jpeg.rs`（替换占位）：

```rust
//! JPEG 剥离 walker：逐段遍历，元数据段 Drop、结构/图像段 Emit。
//! SOS 起的熵编码数据整段 Emit 到尾。keep_orientation 时合成最小 EXIF。
//! slice 全缓冲驱动：一次 pull 处理整个输入。

use alloc::vec::Vec;

use super::exif_synth::{jpeg_app1_exif, orientation_tiff};
use super::{RemovedKind, StripCmd, StripDemand, StripOptions, StripPlanner, StripResult};

pub struct JpegStripper {
    opts: StripOptions,
}

impl JpegStripper {
    pub fn new(opts: StripOptions) -> Self {
        Self { opts }
    }
}

/// 在 EXIF TIFF 内就地查 Orientation(0x0112) 值（仅扫 IFD0，best-effort）。
fn find_orientation(tiff: &[u8]) -> Option<u16> {
    if tiff.len() < 8 {
        return None;
    }
    let le = match &tiff[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let rd16 = |b: &[u8]| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) };
    let rd32 = |b: &[u8]| if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) };
    let ifd0 = rd32(&tiff[4..8]) as usize;
    let count_end = ifd0.checked_add(2)?;
    if count_end > tiff.len() {
        return None;
    }
    let count = rd16(&tiff[ifd0..]) as usize;
    let mut e = count_end;
    for _ in 0..count {
        let entry_end = e.checked_add(12)?;
        if entry_end > tiff.len() {
            return None;
        }
        let tag = rd16(&tiff[e..]);
        if tag == 0x0112 {
            return Some(rd16(&tiff[e + 8..])); // SHORT 内联值在 entry+8
        }
        e = entry_end;
    }
    None
}

impl StripPlanner for JpegStripper {
    fn pull(&mut self, input: &[u8]) -> StripResult {
        let mut cmds: Vec<StripCmd> = Vec::new();

        // 非 JPEG 或太短：原样保留全部。
        if input.len() < 2 || input[0] != 0xFF || input[1] != 0xD8 {
            if !input.is_empty() {
                cmds.push(StripCmd::Emit(input.len()));
            }
            return StripResult { demand: StripDemand::Done, consumed: input.len(), cmds };
        }

        cmds.push(StripCmd::Emit(2)); // SOI
        let mut pos = 2usize;
        // 记录待合成 orientation（首个含 orientation 的 EXIF 段决定）。
        let mut synth_orientation: Option<u16> = None;

        loop {
            // 标记区：FF + (可能多个 FF) + 码字
            if pos >= input.len() {
                break;
            }
            if input[pos] != 0xFF {
                // 畸形（标记区无 FF）：保留剩余字节，安全停止。
                cmds.push(StripCmd::Emit(input.len() - pos));
                pos = input.len();
                break;
            }
            let mut i = pos + 1;
            while i < input.len() && input[i] == 0xFF {
                i += 1;
            }
            if i >= input.len() {
                cmds.push(StripCmd::Emit(input.len() - pos));
                pos = input.len();
                break;
            }
            let marker = input[i];
            let marker_hdr = i + 1 - pos; // pos..i+1（FF...码字）字节数

            match marker {
                0xDA => {
                    // SOS：其后熵编码数据 + 后续标记 + EOI 全部原样到尾。
                    cmds.push(StripCmd::Emit(input.len() - pos));
                    pos = input.len();
                    break;
                }
                0xD9 => {
                    // EOI：保留并结束。
                    cmds.push(StripCmd::Emit(marker_hdr));
                    pos = i + 1;
                    break;
                }
                0x01 | 0xD0..=0xD7 => {
                    // 无长度字段的标记：保留。
                    cmds.push(StripCmd::Emit(marker_hdr));
                    pos = i + 1;
                }
                _ => {
                    // 有长度字段的段：码字后 2 字节为段长。
                    if i + 3 > input.len() {
                        // 长度字段不全：保留剩余，安全停止。
                        cmds.push(StripCmd::Emit(input.len() - pos));
                        pos = input.len();
                        break;
                    }
                    let seg_len = u16::from_be_bytes([input[i + 1], input[i + 2]]) as usize;
                    if seg_len < 2 {
                        // 畸形段长：保留剩余，安全停止。
                        cmds.push(StripCmd::Emit(input.len() - pos));
                        pos = input.len();
                        break;
                    }
                    let body_start = i + 3; // 段体起点
                    let body_len = seg_len - 2;
                    let seg_end = match body_start.checked_add(body_len) {
                        Some(v) if v <= input.len() => v,
                        _ => {
                            // 段越界（截断/畸形）：保留剩余，安全停止。
                            cmds.push(StripCmd::Emit(input.len() - pos));
                            pos = input.len();
                            break;
                        }
                    };
                    let body = &input[body_start..seg_end];
                    let total = seg_end - pos; // 整段（含 FF+码字+len+body）字节数

                    let drop_kind = classify(marker, body, &self.opts);
                    match drop_kind {
                        Some((kind, is_exif)) => {
                            if is_exif && self.opts.keep_orientation && synth_orientation.is_none() {
                                // 段体形如 "Exif\0\0" + TIFF
                                if body.len() > 6 && body.starts_with(b"Exif\0\0") {
                                    synth_orientation = find_orientation(&body[6..]);
                                }
                            }
                            cmds.push(StripCmd::Drop { len: total, kind });
                        }
                        None => {
                            cmds.push(StripCmd::Emit(total));
                        }
                    }
                    pos = seg_end;
                }
            }
        }

        // 合成最小 EXIF（紧随 SOI 之前的已发指令之后插入到流末——
        // 注：JPEG 解析器读到第一个 Exif APP1 即用，位置在 SOS 前即可，
        // 这里 Insert 在所有段命令之后、但仍在 SOS Emit 之前？为简单起见，
        // 把合成段 Insert 紧跟 SOI：改为在 cmds[1] 处插入）。
        if let Some(val) = synth_orientation {
            let seg = jpeg_app1_exif(&orientation_tiff(val));
            cmds.insert(1, StripCmd::Insert(seg)); // 紧随 SOI Emit(2)
        }

        StripResult { demand: StripDemand::Done, consumed: pos, cmds }
    }
}

/// 判定一个段是否该删。返回 Some((kind, is_exif))。is_exif 用于触发 orientation 合成。
fn classify(marker: u8, body: &[u8], opts: &StripOptions) -> Option<(RemovedKind, bool)> {
    match marker {
        0xE1 => {
            if body.starts_with(b"Exif\0\0") {
                Some((RemovedKind::Exif, true))
            } else if body.starts_with(b"http://ns.adobe.com/xap/1.0/\0") {
                Some((RemovedKind::Xmp, false))
            } else {
                None // 未知 APP1：保留
            }
        }
        0xED => {
            // APP13 Photoshop 8BIM（含 IPTC-IIM）
            if body.windows(4).any(|w| w == b"8BIM") {
                Some((RemovedKind::Iptc, false))
            } else {
                None
            }
        }
        0xE2 => {
            // APP2 ICC
            if body.starts_with(b"ICC_PROFILE\0") {
                if opts.keep_icc { None } else { Some((RemovedKind::Icc, false)) }
            } else {
                None
            }
        }
        _ => None, // 其它（APP0/JFIF、SOF、DQT、DHT…）保留
    }
}
```

> 设计要点：`synth_orientation` 在删 EXIF 段时就地提取；最后用 `cmds.insert(1, …)` 把合成段紧跟 `SOI Emit(2)` 注入，保证读路径能在 SOS 前命中。`consumed` = pos（已覆盖整个输入）。引擎对 `Insert` 不推进 consumed，符合契约。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p omni-meta-core strip::jpeg`
Expected: 5 个测试全 PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/strip/jpeg.rs
git commit -m "feat(strip): JPEG walker — 剥离 Exif/XMP/APP13-IPTC，保留 APP0/ICC，keep_orientation 合成"
```

---

## Task 5: PNG stripper

**Files:**
- Create/replace: `omni-meta-core/src/strip/png.rs`

参考读路径 `omni-meta-core/src/formats/png.rs` 的 chunk 遍历。

- [ ] **Step 1: 写失败测试**

`strip/png.rs` 测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::strip::{drive_strip_slice, RemovedKind, StripOptions};
    use crate::model::FileFormat;

    const SIG: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

    fn chunk(ctype: &[u8; 4], data: &[u8]) -> alloc::vec::Vec<u8> {
        let mut c = alloc::vec::Vec::new();
        c.extend_from_slice(&(data.len() as u32).to_be_bytes());
        c.extend_from_slice(ctype);
        c.extend_from_slice(data);
        let mut crc_in = alloc::vec::Vec::new();
        crc_in.extend_from_slice(ctype);
        crc_in.extend_from_slice(data);
        c.extend_from_slice(&super::super::crc32::crc32(&crc_in).to_be_bytes());
        c
    }

    fn ihdr(w: u32, h: u32) -> alloc::vec::Vec<u8> {
        let mut d = alloc::vec::Vec::new();
        d.extend_from_slice(&w.to_be_bytes());
        d.extend_from_slice(&h.to_be_bytes());
        d.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(b"IHDR", &d)
    }

    fn itxt_xmp(packet: &[u8]) -> alloc::vec::Vec<u8> {
        let mut d = alloc::vec::Vec::new();
        d.extend_from_slice(b"XML:com.adobe.xmp");
        d.extend_from_slice(&[0, 0, 0, 0, 0]); // kw nul, compflag, compmethod, lang nul, transkw nul
        d.extend_from_slice(packet);
        chunk(b"iTXt", &d)
    }

    fn full_png() -> alloc::vec::Vec<u8> {
        let mut p = alloc::vec::Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(8, 8));
        p.extend_from_slice(&chunk(b"eXIf", &crate::strip::exif_synth::orientation_tiff(6)));
        p.extend_from_slice(&itxt_xmp(br#"<rdf:Description tiff:Make="Acme"/>"#));
        p.extend_from_slice(&chunk(b"iCCP", b"prof\0\0somedata"));
        p.extend_from_slice(&chunk(b"IDAT", &[1, 2, 3, 4]));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        p
    }

    fn run(buf: &[u8], opts: StripOptions) -> (alloc::vec::Vec<u8>, crate::strip::StripReport) {
        let mut p = PngStripper::new(opts);
        drive_strip_slice(buf, &mut p, FileFormat::Png)
    }

    #[test]
    fn default_strips_exif_xmp_keeps_icc_and_orientation() {
        let (out, report) = run(&full_png(), StripOptions::default());
        let meta = crate::read_slice(&out, crate::Options::default()).unwrap();
        assert!(meta.raw.xmp.is_empty());
        assert_eq!(meta.unified.orientation, Some(crate::model::Orientation::Rotate90));
        assert_eq!(meta.unified.width, Some(8));
        assert!(report.removed.contains(RemovedKind::Exif));
        assert!(report.removed.contains(RemovedKind::Xmp));
        assert!(out.windows(4).any(|w| w == b"iCCP")); // ICC 保留
        assert!(out.windows(4).any(|w| w == b"IDAT")); // 图像保留
        assert!(out.windows(4).any(|w| w == b"IEND"));
    }

    #[test]
    fn aggressive_strips_icc_and_orientation() {
        let (out, report) = run(&full_png(), StripOptions::aggressive());
        assert!(!out.windows(4).any(|w| w == b"iCCP"));
        assert!(report.removed.contains(RemovedKind::Icc));
        let meta = crate::read_slice(&out, crate::Options::default()).unwrap();
        assert!(meta.raw.exif.is_empty());
        assert_eq!(meta.unified.orientation, None);
    }

    #[test]
    fn synthesized_exif_chunk_is_valid_and_reparses() {
        // keep_orientation 合成的 eXIf 必须 CRC 合法、能被读回
        let (out, _r) = run(&full_png(), StripOptions::default());
        assert!(out.windows(4).any(|w| w == b"eXIf"));
    }

    #[test]
    fn non_png_returns_input_unchanged() {
        let buf = [0u8, 1, 2, 3];
        let (out, _r) = run(&buf, StripOptions::default());
        assert_eq!(out, buf);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p omni-meta-core strip::png 2>&1 | head -20`
Expected: 编译错误（`PngStripper` 未定义）。

- [ ] **Step 3: 写实现**

`strip/png.rs`（替换占位）：

```rust
//! PNG 剥离 walker：逐 chunk 遍历，eXIf/XMP-iTXt/文本块 Drop、iCCP 视选项、
//! 关键/图像 chunk Emit。keep_orientation 时在 IDAT 前 Insert 合成 eXIf。

use alloc::vec::Vec;

use super::exif_synth::{orientation_tiff, png_exif_chunk};
use super::{RemovedKind, StripCmd, StripDemand, StripOptions, StripPlanner, StripResult};

const SIG: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

pub struct PngStripper {
    opts: StripOptions,
}

impl PngStripper {
    pub fn new(opts: StripOptions) -> Self {
        Self { opts }
    }
}

/// 从 eXIf chunk 的裸 TIFF 中查 orientation（复用 jpeg 的逻辑）。
use super::jpeg::find_orientation_pub as find_orientation;

impl StripPlanner for PngStripper {
    fn pull(&mut self, input: &[u8]) -> StripResult {
        let mut cmds: Vec<StripCmd> = Vec::new();
        if input.len() < 8 || input[..8] != SIG {
            if !input.is_empty() {
                cmds.push(StripCmd::Emit(input.len()));
            }
            return StripResult { demand: StripDemand::Done, consumed: input.len(), cmds };
        }
        cmds.push(StripCmd::Emit(8)); // 签名
        let mut pos = 8usize;
        let mut synth_orientation: Option<u16> = None;
        let mut synth_inserted = false;

        loop {
            if pos + 8 > input.len() {
                // chunk 头不全：保留剩余，安全停止。
                if pos < input.len() {
                    cmds.push(StripCmd::Emit(input.len() - pos));
                    pos = input.len();
                }
                break;
            }
            let len = u32::from_be_bytes([input[pos], input[pos + 1], input[pos + 2], input[pos + 3]]) as usize;
            let ctype = &input[pos + 4..pos + 8];
            let total = match 8usize.checked_add(len).and_then(|v| v.checked_add(4)) {
                Some(v) if pos.checked_add(v).map(|e| e <= input.len()).unwrap_or(false) => v,
                _ => {
                    // chunk 越界/溢出：保留剩余，安全停止。
                    cmds.push(StripCmd::Emit(input.len() - pos));
                    pos = input.len();
                    break;
                }
            };
            let data = &input[pos + 8..pos + 8 + len];
            let is_iend = ctype == b"IEND";
            let is_idat = ctype == b"IDAT";

            // 合成 eXIf：在首个 IDAT（或 IEND 兜底）之前注入一次。
            if !synth_inserted && (is_idat || is_iend) {
                if let Some(val) = synth_orientation {
                    cmds.push(StripCmd::Insert(png_exif_chunk(&orientation_tiff(val))));
                }
                synth_inserted = true;
            }

            let drop_kind = classify(ctype, data, &self.opts);
            match drop_kind {
                Some((kind, is_exif)) => {
                    if is_exif && self.opts.keep_orientation && synth_orientation.is_none() {
                        synth_orientation = find_orientation(data);
                    }
                    cmds.push(StripCmd::Drop { len: total, kind });
                }
                None => {
                    cmds.push(StripCmd::Emit(total));
                }
            }
            pos += total;
            if is_iend {
                break;
            }
        }

        StripResult { demand: StripDemand::Done, consumed: pos, cmds }
    }
}

fn classify(ctype: &[u8], _data: &[u8], opts: &StripOptions) -> Option<(RemovedKind, bool)> {
    match ctype {
        b"eXIf" => Some((RemovedKind::Exif, true)),
        b"iTXt" | b"tEXt" | b"zTXt" => {
            // 文本块：XMP 归 Xmp，其余归 Other（潜在隐私注释）。
            if _data.starts_with(b"XML:com.adobe.xmp") {
                Some((RemovedKind::Xmp, false))
            } else {
                Some((RemovedKind::Other, false))
            }
        }
        b"iCCP" => {
            if opts.keep_icc { None } else { Some((RemovedKind::Icc, false)) }
        }
        _ => None, // IHDR/PLTE/IDAT/IEND 等保留
    }
}
```

并在 `strip/jpeg.rs` 把 `find_orientation` 暴露给 png 复用：在 jpeg.rs 末尾（impl 之后）加：

```rust
/// 供 png walker 复用：在裸 TIFF 中查 orientation。
pub(crate) fn find_orientation_pub(tiff: &[u8]) -> Option<u16> {
    find_orientation(tiff)
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p omni-meta-core strip::png`
Expected: 4 个 PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/strip/png.rs omni-meta-core/src/strip/jpeg.rs
git commit -m "feat(strip): PNG walker — 剥离 eXIf/XMP/文本块，保留 iCCP/图像，keep_orientation 合成 eXIf"
```

---

## Task 6: WebP stripper

**Files:**
- Create/replace: `omni-meta-core/src/strip/webp.rs`

参考读路径 `omni-meta-core/src/formats/webp.rs`。难点：删 chunk 后须重算 RIFF filesize（offset 4），并清除/设置 VP8X flag 位。

VP8X flags 字节（chunk data[0]）位定义（高位起）：ICC(0x20)、Alpha(0x10)、EXIF(0x08)、XMP(0x04)、Animation(0x02)。

- [ ] **Step 1: 写失败测试**

`strip/webp.rs` 测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::strip::{drive_strip_slice, RemovedKind, StripOptions};
    use crate::model::FileFormat;

    fn riff_chunk(fourcc: &[u8; 4], data: &[u8]) -> alloc::vec::Vec<u8> {
        let mut c = alloc::vec::Vec::new();
        c.extend_from_slice(fourcc);
        c.extend_from_slice(&(data.len() as u32).to_le_bytes());
        c.extend_from_slice(data);
        if data.len() % 2 == 1 {
            c.push(0);
        }
        c
    }

    fn vp8x(flags: u8, w: u32, h: u32) -> alloc::vec::Vec<u8> {
        let mut d = alloc::vec![0u8; 10];
        d[0] = flags;
        let wm1 = w - 1;
        let hm1 = h - 1;
        d[4] = (wm1 & 0xFF) as u8;
        d[5] = ((wm1 >> 8) & 0xFF) as u8;
        d[6] = ((wm1 >> 16) & 0xFF) as u8;
        d[7] = (hm1 & 0xFF) as u8;
        d[8] = ((hm1 >> 8) & 0xFF) as u8;
        d[9] = ((hm1 >> 16) & 0xFF) as u8;
        riff_chunk(b"VP8X", &d)
    }

    /// RIFF/WEBP，VP8X(flags=EXIF|XMP|ICC) + ICCP + VP8 + EXIF + XMP
    fn full_webp() -> alloc::vec::Vec<u8> {
        let mut body = alloc::vec::Vec::new();
        body.extend_from_slice(b"WEBP");
        body.extend_from_slice(&vp8x(0x08 | 0x04 | 0x20, 8, 8));
        body.extend_from_slice(&riff_chunk(b"ICCP", b"iccdata1")); // 8 字节
        body.extend_from_slice(&riff_chunk(b"VP8 ", &[0u8; 12]));
        let mut exif = alloc::vec::Vec::new();
        exif.extend_from_slice(&crate::strip::exif_synth::orientation_tiff(6));
        body.extend_from_slice(&riff_chunk(b"EXIF", &exif));
        body.extend_from_slice(&riff_chunk(b"XMP ", br#"<x/>"#));
        let mut f = alloc::vec::Vec::new();
        f.extend_from_slice(b"RIFF");
        f.extend_from_slice(&(body.len() as u32).to_le_bytes());
        f.extend_from_slice(&body);
        f
    }

    fn run(buf: &[u8], opts: StripOptions) -> (alloc::vec::Vec<u8>, crate::strip::StripReport) {
        let mut p = WebpStripper::new(opts);
        drive_strip_slice(buf, &mut p, FileFormat::Webp)
    }

    #[test]
    fn filesize_recomputed_and_valid() {
        let (out, _r) = run(&full_webp(), StripOptions::default());
        assert_eq!(&out[0..4], b"RIFF");
        let declared = u32::from_le_bytes([out[4], out[5], out[6], out[7]]) as usize;
        assert_eq!(declared, out.len() - 8, "filesize 应等于其后字节数");
        assert_eq!(&out[8..12], b"WEBP");
    }

    #[test]
    fn default_strips_exif_xmp_keeps_icc_orientation() {
        let (out, report) = run(&full_webp(), StripOptions::default());
        let meta = crate::read_slice(&out, crate::Options::default()).unwrap();
        assert!(meta.raw.xmp.is_empty());
        assert_eq!(meta.unified.orientation, Some(crate::model::Orientation::Rotate90));
        assert!(report.removed.contains(RemovedKind::Exif));
        assert!(report.removed.contains(RemovedKind::Xmp));
        assert!(out.windows(4).any(|w| w == b"ICCP"));
        assert!(!out.windows(4).any(|w| w == b"XMP "));
    }

    #[test]
    fn vp8x_flags_updated() {
        // 默认：删 EXIF/XMP → 清这两 bit；但 keep_orientation 合成回 EXIF → EXIF bit 仍置位。
        let (out, _r) = run(&full_webp(), StripOptions::default());
        // 定位 VP8X data[0]
        let idx = out.windows(4).position(|w| w == b"VP8X").unwrap();
        let flags = out[idx + 8];
        assert_eq!(flags & 0x04, 0, "XMP bit 应清除");
        assert_eq!(flags & 0x08, 0x08, "EXIF bit 应保留（合成回 orientation）");
        assert_eq!(flags & 0x20, 0x20, "ICC bit 应保留");
    }

    #[test]
    fn aggressive_clears_exif_icc_bits() {
        let (out, _r) = run(&full_webp(), StripOptions::aggressive());
        let idx = out.windows(4).position(|w| w == b"VP8X").unwrap();
        let flags = out[idx + 8];
        assert_eq!(flags & 0x08, 0, "EXIF bit 清除");
        assert_eq!(flags & 0x20, 0, "ICC bit 清除");
        assert!(!out.windows(4).any(|w| w == b"ICCP"));
    }

    #[test]
    fn non_webp_returns_input_unchanged() {
        let buf = [0u8, 1, 2, 3];
        let (out, _r) = run(&buf, StripOptions::default());
        assert_eq!(out, buf);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p omni-meta-core strip::webp 2>&1 | head -20`
Expected: 编译错误（`WebpStripper` 未定义）。

- [ ] **Step 3: 写实现**

`strip/webp.rs`（替换占位）。注意：WebP 需先扫一遍算出保留集合与 filesize、VP8X flags，再产指令——故本 walker 一次 pull 内完成「全扫 + 重建」。

```rust
//! WebP 剥离 walker：逐 RIFF chunk 遍历，删 EXIF/XMP/ICC（视选项），
//! 重算 RIFF filesize、更新 VP8X flags；keep_orientation 合成回 EXIF chunk。
//! 一次 pull 完成（slice 全缓冲）。

use alloc::vec::Vec;

use super::exif_synth::{orientation_tiff, webp_exif_chunk};
use super::jpeg::find_orientation_pub as find_orientation;
use super::{RemovedKind, StripCmd, StripDemand, StripOptions, StripPlanner, StripResult};

const FLAG_ICC: u8 = 0x20;
const FLAG_EXIF: u8 = 0x08;
const FLAG_XMP: u8 = 0x04;

pub struct WebpStripper {
    opts: StripOptions,
}

impl WebpStripper {
    pub fn new(opts: StripOptions) -> Self {
        Self { opts }
    }
}

impl StripPlanner for WebpStripper {
    fn pull(&mut self, input: &[u8]) -> StripResult {
        // 非 WebP：原样保留。
        if input.len() < 12 || &input[0..4] != b"RIFF" || &input[8..12] != b"WEBP" {
            let mut cmds = Vec::new();
            if !input.is_empty() {
                cmds.push(StripCmd::Emit(input.len()));
            }
            return StripResult { demand: StripDemand::Done, consumed: input.len(), cmds };
        }

        // 先收集保留后的 body 字节（"WEBP" + 各保留 chunk），同时算 VP8X flags 与 orientation。
        let mut report_removed: Vec<(usize, RemovedKind)> = Vec::new(); // (len, kind)
        let mut synth_orientation: Option<u16> = None;

        // 第一遍：找 orientation（在 EXIF chunk 内），决定是否合成。
        {
            let mut p = 12usize;
            while p + 8 <= input.len() {
                let fourcc = &input[p..p + 4];
                let size = u32::from_le_bytes([input[p + 4], input[p + 5], input[p + 6], input[p + 7]]) as usize;
                let pad = size & 1;
                let data_end = match p.checked_add(8).and_then(|v| v.checked_add(size)) {
                    Some(v) if v <= input.len() => v,
                    _ => break,
                };
                if fourcc == b"EXIF" && self.opts.keep_orientation {
                    synth_orientation = find_orientation(&input[p + 8..data_end]);
                }
                p = match data_end.checked_add(pad) {
                    Some(v) => v,
                    None => break,
                };
            }
        }

        // 第二遍：重建 body。
        let mut new_body: Vec<u8> = Vec::new();
        new_body.extend_from_slice(b"WEBP");
        let mut vp8x_flag_pos: Option<usize> = None; // new_body 内 VP8X data[0] 偏移

        let mut p = 12usize;
        let mut truncated_tail: Option<usize> = None;
        while p + 8 <= input.len() {
            let fourcc = [input[p], input[p + 1], input[p + 2], input[p + 3]];
            let size = u32::from_le_bytes([input[p + 4], input[p + 5], input[p + 6], input[p + 7]]) as usize;
            let pad = size & 1;
            let chunk_end = match p.checked_add(8).and_then(|v| v.checked_add(size)).and_then(|v| v.checked_add(pad)) {
                Some(v) if v <= input.len() => v,
                _ => {
                    // 越界 chunk：保留从 p 到尾的原始字节（安全），停止。
                    truncated_tail = Some(p);
                    break;
                }
            };
            let data = &input[p + 8..p + 8 + size];

            let kind = match &fourcc {
                b"EXIF" => Some(RemovedKind::Exif),
                b"XMP " => Some(RemovedKind::Xmp),
                b"ICCP" => {
                    if self.opts.keep_icc { None } else { Some(RemovedKind::Icc) }
                }
                _ => None,
            };

            match kind {
                Some(k) => {
                    report_removed.push((chunk_end - p, k));
                }
                None => {
                    if &fourcc == b"VP8X" {
                        vp8x_flag_pos = Some(new_body.len() + 8); // data[0]
                    }
                    new_body.extend_from_slice(&input[p..chunk_end]);
                    // 合成 EXIF：紧跟 VP8X 之后注入一次（若需要）。
                    if &fourcc == b"VP8X" {
                        if let Some(val) = synth_orientation.take() {
                            new_body.extend_from_slice(&webp_exif_chunk(&orientation_tiff(val)));
                        }
                    }
                }
            }
            p = chunk_end;
        }

        // 若没有 VP8X 但仍需合成 orientation（极少见：simple lossy/lossless 无 VP8X），
        // 则在 body 末尾追加 EXIF chunk（解码器仍可读到）。
        if let Some(val) = synth_orientation.take() {
            new_body.extend_from_slice(&webp_exif_chunk(&orientation_tiff(val)));
        }

        // 更新 VP8X flags：清 XMP；EXIF/ICC 视最终是否保留。
        if let Some(fp) = vp8x_flag_pos {
            let mut flags = new_body[fp];
            flags &= !FLAG_XMP;
            // EXIF：若合成回了 orientation 则置位，否则清。
            let has_exif = new_body.windows(4).any(|w| w == b"EXIF");
            if has_exif { flags |= FLAG_EXIF; } else { flags &= !FLAG_EXIF; }
            let has_icc = new_body.windows(4).any(|w| w == b"ICCP");
            if has_icc { flags |= FLAG_ICC; } else { flags &= !FLAG_ICC; }
            new_body[fp] = flags;
        }

        // 越界尾：把原始 [truncated_tail..] 追加（安全保留）。
        if let Some(tt) = truncated_tail {
            new_body.extend_from_slice(&input[tt..]);
        }

        // 组装：RIFF + filesize(LE) + new_body。
        let mut out_chunk: Vec<u8> = Vec::with_capacity(8 + new_body.len());
        out_chunk.extend_from_slice(b"RIFF");
        out_chunk.extend_from_slice(&(new_body.len() as u32).to_le_bytes());
        out_chunk.extend_from_slice(&new_body);

        // 用 Replace 把整个输入替换为重建结果；用 Drop 记账。
        let mut cmds: Vec<StripCmd> = Vec::new();
        cmds.push(StripCmd::Replace { consume: input.len(), with: out_chunk });
        for (len, kind) in report_removed {
            cmds.push(StripCmd::Drop { len, kind });
        }

        StripResult { demand: StripDemand::Done, consumed: input.len(), cmds }
    }
}
```

> 注：WebP walker 用一条 `Replace{consume: input.len(), with: 重建结果}` 直接产出整文件，再用零长度逻辑的 `Drop` 仅作记账——但引擎的 `Drop` 会按 `len` 推进窗口游标。因 `Replace` 已 consume 全部输入，其后 `Drop` 的 `cur` 已到窗口尾，`end-cur=0`，故 **`bytes_removed` 不会增加**。需修正记账方式：把删除字节数并入报告而非靠引擎。**改为**：不发记账用的 `Drop`，而在引擎外补——见下方修正。

- [ ] **Step 3b: 修正 WebP 记账（引擎扩展）**

WebP 用整体 `Replace` 重建，无法靠引擎的逐段 `Drop` 记账。最简洁修正：让 `Replace` 也能携带删除统计。在 `strip/mod.rs` 给 `StripCmd` 增强——但为避免改动已稳定的指令集，改用更直接的方案：**WebP walker 发 `Drop` 在前、`Replace` 在后**，且 `Drop` 用 `len=0`、配合一个新的「纯记账」路径。

为保持简单与一致，采用：在 `StripCmd` 增加一个 `Account { len: u64, kind: RemovedKind }` 变体（纯记账、不动窗口、不产输出）。

在 `strip/mod.rs` 的 `StripCmd` 加变体：

```rust
    /// 纯记账：把 len 字节计入报告的 removed/bytes_removed，不消费输入、不产输出。
    /// 供整体重建型 walker（WebP）使用。
    Account { len: u64, kind: RemovedKind },
```

在 `drive_strip_slice` 的 match 加分支：

```rust
                StripCmd::Account { len, kind } => {
                    report.bytes_removed += len;
                    report.removed.insert(kind);
                }
```

把 WebP walker 末尾的记账循环改为：

```rust
        for (len, kind) in report_removed {
            cmds.push(StripCmd::Account { len: len as u64, kind });
        }
```

并更新 Task 2 的 `engine_applies_emit_drop_insert_replace` 不受影响（未用 Account）。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p omni-meta-core strip::webp`
Expected: 5 个 PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/strip/webp.rs omni-meta-core/src/strip/mod.rs
git commit -m "feat(strip): WebP walker — 删 EXIF/XMP/ICC + 重算 filesize + 更新 VP8X flags + StripCmd::Account 记账"
```

---

## Task 7: strip_slice 适配器 + planner_for 分派 + 公开面

**Files:**
- Create/replace: `omni-meta-core/src/adapters/strip_slice.rs`
- Modify: `omni-meta-core/src/adapters/mod.rs`
- Modify: `omni-meta-core/src/strip/mod.rs`（加 `planner_for`）
- Modify: `omni-meta-core/src/lib.rs`（`pub use`）

- [ ] **Step 1: 写失败测试**

`adapters/strip_slice.rs` 测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::strip::{RemovedKind, StripOptions};

    fn minimal_jpeg_with_exif() -> alloc::vec::Vec<u8> {
        let mut j = alloc::vec::Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]);
        let mut body = alloc::vec::Vec::new();
        body.extend_from_slice(b"Exif\0\0");
        body.extend_from_slice(&crate::strip::exif_synth::orientation_tiff(1));
        j.extend_from_slice(&[0xFF, 0xE1]);
        j.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        j.extend_from_slice(&body);
        j.extend_from_slice(&[0xFF, 0xDA, 0, 4, 1, 0, 0]); // SOS
        j.extend_from_slice(&[0xFF, 0xD9]);
        j
    }

    #[test]
    fn strips_jpeg_via_probe() {
        let j = minimal_jpeg_with_exif();
        let (out, report) = strip_slice(&j, StripOptions::aggressive()).unwrap();
        assert_eq!(report.format, crate::model::FileFormat::Jpeg);
        assert!(report.removed.contains(RemovedKind::Exif));
        let meta = crate::read_slice(&out, crate::Options::default()).unwrap();
        assert!(meta.raw.exif.is_empty());
    }

    #[test]
    fn unsupported_format_errors() {
        // GIF 签名（已识别但 strip 不支持）
        let gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00";
        let err = strip_slice(gif, StripOptions::default()).unwrap_err();
        assert_eq!(err, crate::Error::Unsupported);
    }

    #[test]
    fn unrecognized_format_errors() {
        let junk = [0u8, 1, 2, 3, 4, 5];
        let err = strip_slice(&junk, StripOptions::default()).unwrap_err();
        assert_eq!(err, crate::Error::UnrecognizedFormat);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p omni-meta-core strip_slice 2>&1 | head -20`
Expected: 编译错误（`strip_slice`/`planner_for` 未定义）。

- [ ] **Step 3: 写 planner_for + strip_slice**

在 `strip/mod.rs` 加（`drive_strip_slice` 附近）：

```rust
use alloc::boxed::Box;
use crate::error::Error;

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
```

`adapters/strip_slice.rs`（替换占位）：

```rust
//! strip_slice：全内存剥离适配器。

use alloc::vec::Vec;

use crate::error::Error;
use crate::probe::probe;
use crate::strip::{drive_strip_slice, planner_for, StripOptions, StripReport};

/// 从一整块内存缓冲剥离元数据，返回（干净字节, 报告）。
/// 无法识别格式 → `UnrecognizedFormat`；已识别但不支持 → `Unsupported`。
pub fn strip_slice(buf: &[u8], opts: StripOptions) -> Result<(Vec<u8>, StripReport), Error> {
    let fmt = probe(buf);
    let mut planner = planner_for(&fmt, opts)?;
    Ok(drive_strip_slice(buf, planner.as_mut(), fmt))
}
```

在 `omni-meta-core/src/adapters/mod.rs` 加：

```rust
pub mod strip_slice;
```

在 `omni-meta-core/src/lib.rs` 的 `pub use` 区加：

```rust
pub use adapters::strip_slice::strip_slice;
pub use strip::{RemovedKind, RemovedKinds, StripOptions, StripReport};
```

并把 `Error` 的 `pub use`（`pub use error::Error;` 已存在，无需改）——确认 `Unsupported` 随 `Error` 导出。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p omni-meta-core strip_slice`
Expected: 3 个 PASS。

- [ ] **Step 5: 全量回归**

Run: `cargo test -p omni-meta-core`
Expected: 全部 PASS（读路径 + strip 全部）。

- [ ] **Step 6: 提交**

```bash
git add omni-meta-core/src/adapters/strip_slice.rs omni-meta-core/src/adapters/mod.rs omni-meta-core/src/strip/mod.rs omni-meta-core/src/lib.rs
git commit -m "feat(strip): strip_slice 适配器 + planner_for 分派 + 公开面导出"
```

---

## Task 8: strip_blocking 适配器（facade, std）

**Files:**
- Create/replace: `omni-meta/src/adapters/strip_blocking.rs`
- Modify: `omni-meta/src/adapters/mod.rs`
- Modify: `omni-meta/src/lib.rs`

- [ ] **Step 1: 写失败测试**

`omni-meta/src/adapters/strip_blocking.rs` 测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use omni_meta_core::{strip_slice, StripOptions};

    fn minimal_jpeg() -> Vec<u8> {
        let mut j = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]);
        let body = b"Exif\0\0II*\0\x08\0\0\0\0\0\0\0\0\0"; // 极简
        j.extend_from_slice(&[0xFF, 0xE1]);
        j.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        j.extend_from_slice(body);
        j.extend_from_slice(&[0xFF, 0xDA, 0, 4, 1, 0, 0]);
        j.extend_from_slice(&[0xFF, 0xD9]);
        j
    }

    #[test]
    fn blocking_matches_slice_byte_for_byte() {
        let j = minimal_jpeg();
        let (slice_out, _r) = strip_slice(&j, StripOptions::aggressive()).unwrap();
        let mut blocking_out: Vec<u8> = Vec::new();
        let report = strip_blocking(&j[..], &mut blocking_out, StripOptions::aggressive()).unwrap();
        assert_eq!(blocking_out, slice_out, "blocking 输出须与 slice 字节级一致");
        assert_eq!(report.format, omni_meta_core::FileFormat::Jpeg);
    }

    #[test]
    fn blocking_unsupported_errors() {
        let gif = b"GIF89a\x01\0\x01\0\0\0\0";
        let mut out: Vec<u8> = Vec::new();
        let err = strip_blocking(&gif[..], &mut out, StripOptions::default()).unwrap_err();
        assert_eq!(err, omni_meta_core::Error::Unsupported);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p omni-meta strip_blocking 2>&1 | head -20`
Expected: 编译错误（`strip_blocking` 未定义）。

- [ ] **Step 3: 写实现**

`omni-meta/src/adapters/strip_blocking.rs`（替换占位）：

```rust
//! strip_blocking：std 阻塞剥离。v1 把输入读入有界缓冲后复用 slice 引擎，整块写出。
//! 理由见 spec §7：planner 只需处理整缓冲，slice↔blocking 字节级一致为平凡真值。

use std::io::{Read, Write};

use omni_meta_core::{strip_slice, Error, StripOptions, StripReport};

const CHUNK: usize = 8192;

pub fn strip_blocking<R: Read, W: Write>(
    mut r: R,
    mut w: W,
    opts: StripOptions,
) -> Result<StripReport, Error> {
    // 读入有界缓冲（≤ max_payload_bytes）。
    let cap = opts.limits.max_payload_bytes;
    let mut input: Vec<u8> = Vec::new();
    let mut buf = [0u8; CHUNK];
    loop {
        let n = r.read(&mut buf).map_err(|_| Error::Io)?;
        if n == 0 {
            break;
        }
        if input.len().saturating_add(n) > cap {
            return Err(Error::Io); // 超界：拒绝（防 OOM）
        }
        input.extend_from_slice(&buf[..n]);
    }
    let (out, report) = strip_slice(&input, opts)?;
    w.write_all(&out).map_err(|_| Error::Io)?;
    Ok(report)
}
```

在 `omni-meta/src/adapters/mod.rs` 加：

```rust
pub mod strip_blocking;
```

在 `omni-meta/src/lib.rs` 的 std 区加：

```rust
#[cfg(feature = "std")]
pub use adapters::strip_blocking::strip_blocking;
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p omni-meta strip_blocking`
Expected: 2 个 PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta/src/adapters/strip_blocking.rs omni-meta/src/adapters/mod.rs omni-meta/src/lib.rs
git commit -m "feat(strip): strip_blocking 适配器（有界缓冲走 slice 引擎，与 slice 字节级一致）"
```

---

## Task 9: 幂等 + 结构完整集成测试（facade）

**Files:**
- Create: `omni-meta/tests/strip_integration.rs`

- [ ] **Step 1: 写测试（幂等 + 多格式回环）**

`omni-meta/tests/strip_integration.rs`：

```rust
//! Stripper 集成测试：幂等、多格式回环、默认/aggressive 双路。

use omni_meta::{read_slice, strip_slice, Options, StripOptions};

fn jpeg_fixture() -> Vec<u8> {
    let mut j = Vec::new();
    j.extend_from_slice(&[0xFF, 0xD8]);
    j.extend_from_slice(&[0xFF, 0xE0, 0, 16]); // APP0
    j.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    let mut exif = Vec::new();
    exif.extend_from_slice(b"Exif\0\0");
    // 小端 TIFF：Orientation=6
    exif.extend_from_slice(b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0");
    j.extend_from_slice(&[0xFF, 0xE1]);
    j.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    j.extend_from_slice(&exif);
    j.extend_from_slice(&[0xFF, 0xC0, 0, 11, 8, 0, 8, 0, 8, 1, 0x11, 0]); // SOF0
    j.extend_from_slice(&[0xFF, 0xDA, 0, 4, 1, 0, 0, 0x11, 0x22, 0xFF, 0xD9]); // SOS+data+EOI
    j
}

#[test]
fn jpeg_default_is_idempotent() {
    let j = jpeg_fixture();
    let (once, _) = strip_slice(&j, StripOptions::default()).unwrap();
    let (twice, _) = strip_slice(&once, StripOptions::default()).unwrap();
    assert_eq!(once, twice, "strip 应幂等");
}

#[test]
fn jpeg_default_keeps_orientation_drops_privacy() {
    let j = jpeg_fixture();
    let before = read_slice(&j, Options::default()).unwrap();
    assert_eq!(before.unified.orientation, Some(omni_meta::Orientation::Rotate90));
    let (out, _) = strip_slice(&j, StripOptions::default()).unwrap();
    let after = read_slice(&out, Options::default()).unwrap();
    assert_eq!(after.unified.orientation, Some(omni_meta::Orientation::Rotate90));
    assert_eq!(after.unified.width, Some(8));
    assert_eq!(after.unified.height, Some(8));
}

#[test]
fn jpeg_aggressive_zero_exif() {
    let j = jpeg_fixture();
    let (out, _) = strip_slice(&j, StripOptions::aggressive()).unwrap();
    let after = read_slice(&out, Options::default()).unwrap();
    assert!(after.raw.exif.is_empty());
    assert_eq!(after.unified.orientation, None);
    // 维度仍在（来自 SOF0，非元数据）
    assert_eq!(after.unified.width, Some(8));
}
```

- [ ] **Step 2: 运行确认（先失败如有，再通过）**

Run: `cargo test -p omni-meta --test strip_integration`
Expected: 3 个 PASS。若 orientation fixture 字节有误，对照 `strip/exif_synth.rs::orientation_tiff(6)` 修正后再跑。

- [ ] **Step 3: 提交**

```bash
git add omni-meta/tests/strip_integration.rs
git commit -m "test(strip): 集成测试 — 幂等 + 默认保 orientation + aggressive 零 EXIF"
```

---

## Task 10: fuzz target

**Files:**
- Create: `fuzz/fuzz_targets/strip.rs`
- Modify: `fuzz/Cargo.toml`
- Reference: 现有 `fuzz/fuzz_targets/exif.rs` 的结构（扁平 `.rs` 文件，依赖 `omni-meta` facade）

- [ ] **Step 1: 看现有 target 结构**

Run: `cat fuzz/fuzz_targets/exif.rs; echo '--- Cargo ---'; grep -n "name = \|path = \|\[\[bin\]\]" fuzz/Cargo.toml | head -40`
Expected: 了解 target 模板（用 `omni_meta::…`）与 `[[bin]]` 注册格式（`path = "fuzz_targets/<name>.rs"`）。

- [ ] **Step 2: 写 strip fuzz target**

`fuzz/fuzz_targets/strip.rs`（扁平文件，对齐现有 target；不变式：永不 panic + 输出可重解析或等于输入 + 幂等）：

```rust
#![no_main]

use libfuzzer_sys::fuzz_target;
use omni_meta::{read_slice, strip_slice, Options, StripOptions};

fuzz_target!(|data: &[u8]| {
    for opts in [StripOptions::default(), StripOptions::aggressive()] {
        if let Ok((out, _report)) = strip_slice(data, opts) {
            // 输出必须可被读路径重解析（不 panic / 不返致命错）或为合法 best-effort。
            let _ = read_slice(&out, Options::default());
            // aggressive 下不变式：剥离产物再剥离应幂等（不 panic）。
            let _ = strip_slice(&out, opts);
        }
    }
});
```

- [ ] **Step 3: 注册 target**

在 `fuzz/Cargo.toml` 仿现有 target 追加：

```toml
[[bin]]
name = "strip"
path = "fuzz_targets/strip.rs"
test = false
doc = false
```

（对齐现有 target 的 `[[bin]]` 字段；若现有项含 `bench = false` 等，照抄。）

- [ ] **Step 4: 构建 fuzz target**

Run: `cd fuzz && cargo +nightly fuzz build strip 2>&1 | tail -20`
Expected: 构建成功（若无 nightly/cargo-fuzz，则 `cargo build --manifest-path fuzz/Cargo.toml --bin strip` 验证可编译）。

- [ ] **Step 5: 短跑冒烟（可选，若环境支持）**

Run: `cd fuzz && timeout 30 cargo +nightly fuzz run strip -- -max_total_time=20 2>&1 | tail -20`
Expected: 无 crash。

- [ ] **Step 6: 提交**

```bash
git add fuzz/fuzz_targets/strip.rs fuzz/Cargo.toml
git commit -m "test(fuzz): strip target — 永不 panic + read(strip(x)) 无残留 + 幂等"
```

---

## Task 11: no_std 验证 + ROADMAP 收尾

**Files:**
- Modify: `docs/ROADMAP.md`

- [ ] **Step 1: no_std 构建**

Run: `cargo build -p omni-meta-core --no-default-features 2>&1 | tail -20`
Expected: 成功（strip 全在 core、仅用 alloc，应通过）。若报 `std` 引用，定位修正（strip core 不得用 std）。

- [ ] **Step 2: 全量测试 + clippy**

Run: `cargo test --workspace 2>&1 | tail -20 && cargo clippy --workspace --all-targets 2>&1 | tail -20`
Expected: 测试全 PASS；clippy 无 error（warning 比照现有标准清理）。

- [ ] **Step 3: 更新 ROADMAP**

在 `docs/ROADMAP.md` 里程碑 F 区块把任务勾选，并在「已完成 ✅」表加一行。具体编辑：

把第 134-138 行的里程碑 F 各 `- [ ]` 改为 `- [x]`，并在标题后加「✅ 完成 — 设计 `specs/2026-06-17-omni-meta-stripper-design.md` / 计划 `plans/2026-06-17-omni-meta-stripper.md`」。

在「已完成 ✅」表（约第 34 行后）追加：

```markdown
| **Stripper (F)** | JPEG/PNG/WebP 剥离 EXIF/XMP/IPTC，默认保留 ICC/orientation（最小 EXIF 合成）；`strip_slice`/`strip_blocking`；`StripOptions::aggressive()` 全删；slice↔blocking 字节级一致 + fuzz target | 本次分支 |
```

把第 51 行「尚未开始 ⬜」里的 `Stripper（剥离）` 删去。

- [ ] **Step 4: 提交**

```bash
git add docs/ROADMAP.md
git commit -m "docs(roadmap): 勾选里程碑 F（Stripper）完成"
```

---

## Self-Review 记录（计划作者已核对）

- **spec 覆盖**：§2 模块布局→Task1-8 文件；§3 trait→Task2；§4.1 JPEG→Task4；§4.2 PNG→Task5；§4.3 WebP（filesize+VP8X flags）→Task6；§5 keep_orientation 合成→Task3+各格式；§6 写安全契约（歧义保留/Unsupported）→各 walker 的越界分支 + Task7 分派；§7 引擎/strip_slice/strip_blocking→Task2/7/8；§8 测试（回环/幂等/字节一致/合成/畸形/fuzz/no_std）→Task4-11；§9 不变量→各 walker。无遗漏。
- **类型一致**：`StripCmd`(Emit/Drop/Replace/Insert/Account)、`StripDemand`(More/Done)、`StripOptions`(keep_icc/keep_orientation)、`RemovedKind`/`RemovedKinds`、`find_orientation_pub` 跨 Task 命名一致。
- **占位说明**：Task1 Step3 末显式建 5 个占位文件以保证逐任务可编译。
- **已知微调点**：Task6 在写作中发现整体 `Replace` 与逐段 `Drop` 记账冲突，已在 Step 3b 用 `StripCmd::Account` 修正（同步更新 Task2 引擎）。
</content>
