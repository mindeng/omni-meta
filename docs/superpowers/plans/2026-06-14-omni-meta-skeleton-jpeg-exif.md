# omni-meta 走骨架 + JPEG/EXIF 提取 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 走通 sans-io 核心架构，并端到端实现"从带 EXIF 的 JPEG 经 `read_slice` 提取 orientation / camera_make / camera_model"。

**Architecture:** 纯 sans-io：格式解析器实现 `MetaParser::pull(&[u8]) -> PullResult`（只发 `Demand`、产出 `Event`，绝不碰 I/O）。`read_slice` 适配器把整块缓冲喂给一个极薄的 slice 驱动循环，`Payload` 事件交给 EXIF codec 解码成原始标签，再由 normalization 投影成统一模型。核心 `no_std + alloc`、零依赖、`#![forbid(unsafe_code)]`。

**Tech Stack:** Rust edition 2024，`no_std + alloc`（默认开 `std` feature），仅 std 测试工具，无第三方依赖。

**规范来源:** `docs/superpowers/specs/2026-06-14-omni-meta-design.md`（§3 架构、§4 指令机、§6 数据模型、§9 安全、§11 阶段 1–2）。

**本计划不含**（留待后续计划）：PNG/WebP/GIF/HEIF/AVIF/MP4/MKV、XMP/IPTC/ICC codec、ISO-BMFF/RIFF/EBML 容器、blocking/seek/async/push 适配器、Stripper、跨适配器差分测试（需 ≥2 个适配器，故随 push 适配器计划一起做）。本计划 EXIF 仅解 IFD0 的 ASCII / SHORT 标签，够覆盖目标三字段。

---

## 文件结构

| 文件 | 职责 |
|---|---|
| `Cargo.toml` | 改为 library，声明 feature flags |
| `src/lib.rs` | crate 根：`no_std` 开关、`forbid(unsafe)`、模块声明、公开 re-export |
| `src/cursor.rs` | `ByteCursor` + `Endian`：边界安全、`checked` 的原语读取 |
| `src/limits.rs` | `Limits`：分配上界（防 DoS） |
| `src/error.rs` | `Error`：仅顶层致命错误 |
| `src/model.rs` | `Metadata`/`Unified`/`RawTags`/`ExifTag`/`Value`/`Orientation`/`Warning`/`WarnKind`/`FileFormat` |
| `src/demand.rs` | `Demand`/`Event`/`PayloadKind`/`PullResult`/`MetaParser` |
| `src/codecs/mod.rs` + `src/codecs/exif.rs` | EXIF TIFF/IFD0 解码 |
| `src/normalize.rs` | `RawTags` → `Unified` 投影 |
| `src/formats/mod.rs` + `src/formats/jpeg.rs` | JPEG 段遍历，定位 APP1/Exif 载荷 |
| `src/probe.rs` | 魔数嗅探 → `FileFormat` |
| `src/driver.rs` | `Collector` + `drive_slice`：slice 驱动循环 |
| `src/adapters/mod.rs` + `src/adapters/slice.rs` | `Options` + `read_slice` 公开 API |

旧的 `src/main.rs` 删除（本 crate 是库）。

---

## Task 1: 转为 library crate + feature flags + lib 骨架

**Files:**
- Modify: `Cargo.toml`
- Delete: `src/main.rs`
- Create: `src/lib.rs`

- [ ] **Step 1: 写失败测试**

Create `src/lib.rs`：

```rust
#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

#[cfg(test)]
mod smoke {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
```

- [ ] **Step 2: 改 Cargo.toml 为库 + features**

Replace `Cargo.toml` 内容：

```toml
[package]
name = "omni-meta"
version = "0.1.0"
edition = "2024"

[lib]
name = "omni_meta"
path = "src/lib.rs"

[features]
default = ["std"]
std = []

[dependencies]
```

然后删除二进制入口：

```bash
git rm src/main.rs
```

- [ ] **Step 3: 运行测试验证通过**

Run: `cargo test --lib`
Expected: PASS（`smoke::crate_builds`），无警告级错误。

- [ ] **Step 4: 验证 no_std 构建**

Run: `cargo build --no-default-features`
Expected: 编译成功（仅 `extern crate alloc`，无 std）。

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/lib.rs
git commit -m "chore: 转为 library crate 并配置 no_std/std feature"
```

---

## Task 2: `ByteCursor` + `Endian` 原语读取

**Files:**
- Create: `src/cursor.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 写失败测试**

Create `src/cursor.rs`：

```rust
//! 边界安全的字节游标。所有读取在越界时返回 `None`，绝不 panic。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endian {
    Big,
    Little,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_primitives_and_respects_bounds() {
        let buf = [0x12u8, 0x34, 0x56, 0x78, 0xAB];
        let mut c = ByteCursor::new(&buf);
        assert_eq!(c.u8(), Some(0x12));
        assert_eq!(c.u16_be(), Some(0x3456));
        assert_eq!(c.position(), 3);
        assert_eq!(c.u16(Endian::Little), Some(0xAB78));
        // 只剩 0 字节
        assert_eq!(c.u8(), None);
    }

    #[test]
    fn take_past_end_returns_none_without_advancing() {
        let buf = [1u8, 2, 3];
        let mut c = ByteCursor::new(&buf);
        assert_eq!(c.take(4), None);
        assert_eq!(c.position(), 0);
        assert_eq!(c.take(2), Some(&buf[0..2]));
    }

    #[test]
    fn seek_and_skip() {
        let buf = [0u8; 10];
        let mut c = ByteCursor::new(&buf);
        assert_eq!(c.skip(4), Some(()));
        assert_eq!(c.position(), 4);
        assert_eq!(c.seek(9), Some(()));
        assert_eq!(c.seek(11), None);
        assert_eq!(c.position(), 9);
    }
}
```

- [ ] **Step 2: 运行测试验证失败**

Run: `cargo test --lib cursor`
Expected: FAIL，`cannot find type ByteCursor`（编译错误）。

- [ ] **Step 3: 实现 `ByteCursor`**

在 `src/cursor.rs` 顶部（`Endian` 之后、`#[cfg(test)]` 之前）加入：

```rust
pub struct ByteCursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> ByteCursor<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// 取 n 字节并前进；越界返回 None 且不改变位置。
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        if end > self.buf.len() {
            return None;
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Some(s)
    }

    pub fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }

    pub fn u16_be(&mut self) -> Option<u16> {
        self.take(2).map(|s| u16::from_be_bytes([s[0], s[1]]))
    }

    pub fn u16(&mut self, e: Endian) -> Option<u16> {
        let s = self.take(2)?;
        Some(match e {
            Endian::Big => u16::from_be_bytes([s[0], s[1]]),
            Endian::Little => u16::from_le_bytes([s[0], s[1]]),
        })
    }

    pub fn u32(&mut self, e: Endian) -> Option<u32> {
        let s = self.take(4)?;
        Some(match e {
            Endian::Big => u32::from_be_bytes([s[0], s[1], s[2], s[3]]),
            Endian::Little => u32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        })
    }

    pub fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }

    pub fn seek(&mut self, pos: usize) -> Option<()> {
        if pos > self.buf.len() {
            return None;
        }
        self.pos = pos;
        Some(())
    }
}
```

在 `src/lib.rs` 的 `extern crate alloc;` 下面加：

```rust
pub mod cursor;
```

- [ ] **Step 4: 运行测试验证通过**

Run: `cargo test --lib cursor`
Expected: PASS（3 个测试）。

- [ ] **Step 5: Commit**

```bash
git add src/cursor.rs src/lib.rs
git commit -m "feat: 边界安全的 ByteCursor 原语读取"
```

---

## Task 3: `Limits` + `Error`

**Files:**
- Create: `src/limits.rs`, `src/error.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 写失败测试**

Create `src/limits.rs`：

```rust
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
```

Create `src/error.rs`：

```rust
//! 顶层致命错误。格式内的局部损坏走 Warning，不进 Error。

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// 连容器格式都无法识别。
    UnrecognizedFormat,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::UnrecognizedFormat => f.write_str("unrecognized file format"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_renders() {
        assert_eq!(
            alloc::format!("{}", Error::UnrecognizedFormat),
            "unrecognized file format"
        );
    }
}
```

- [ ] **Step 2: 声明模块并运行测试验证失败**

在 `src/lib.rs` 加：

```rust
pub mod error;
pub mod limits;
```

Run: `cargo test --lib limits error`
Expected: 若顺序错先 FAIL；此处代码完整，应直接编译。运行后 Expected: PASS（2 个测试）。若 `alloc::format!` 报未导入，确认 `src/error.rs` 测试模块顶部能见到 `alloc`（crate 根已 `extern crate alloc;`，`alloc::format!` 全路径可用）。

- [ ] **Step 3: Commit**

```bash
git add src/limits.rs src/error.rs src/lib.rs
git commit -m "feat: Limits 上界与顶层 Error"
```

---

## Task 4: 数据模型（统一层 + 原始层）

**Files:**
- Create: `src/model.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 写失败测试**

Create `src/model.rs`：

```rust
//! 双层数据模型：原始标签 (RawTags) + 统一规范字段 (Unified)。
//! Unified 字段在后续计划中受控增长，每个字段需 >=2 种格式来源才纳入。

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileFormat {
    Jpeg,
    Unknown,
}

/// EXIF 方向（标准值 1..=8）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Normal,     // 1
    FlipH,      // 2
    Rotate180,  // 3
    FlipV,      // 4
    Transpose,  // 5
    Rotate90,   // 6
    Transverse, // 7
    Rotate270,  // 8
}

impl Orientation {
    pub fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            1 => Orientation::Normal,
            2 => Orientation::FlipH,
            3 => Orientation::Rotate180,
            4 => Orientation::FlipV,
            5 => Orientation::Transpose,
            6 => Orientation::Rotate90,
            7 => Orientation::Transverse,
            8 => Orientation::Rotate270,
            _ => return None,
        })
    }
}

/// 类型化的标签值（本计划只用到 U16 / Text，后续扩展 Rational/Bytes 等）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    U16(u16),
    Text(String),
}

/// 一条原始 EXIF 标签。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExifTag {
    pub ifd: u8,
    pub tag: u16,
    pub value: Value,
}

/// 原始标签层，按命名空间分类（本计划只有 exif，后续加 xmp/iptc/icc/container）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawTags {
    pub exif: Vec<ExifTag>,
}

/// 统一规范层。全部 Option —— 缺失即 None，绝不臆造。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unified {
    pub orientation: Option<Orientation>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarnKind {
    Truncated,
    BadExifHeader,
    UnreachableSection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub offset: u64,
    pub kind: WarnKind,
}

/// 顶层解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub unified: Unified,
    pub raw: RawTags,
    pub warnings: Vec<Warning>,
    pub format: FileFormat,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_maps_known_values() {
        assert_eq!(Orientation::from_u16(6), Some(Orientation::Rotate90));
        assert_eq!(Orientation::from_u16(1), Some(Orientation::Normal));
        assert_eq!(Orientation::from_u16(9), None);
        assert_eq!(Orientation::from_u16(0), None);
    }

    #[test]
    fn unified_defaults_to_all_none() {
        let u = Unified::default();
        assert_eq!(u.orientation, None);
        assert_eq!(u.camera_make, None);
        assert_eq!(u.camera_model, None);
    }
}
```

- [ ] **Step 2: 声明模块并运行测试验证失败→实现已含→通过**

在 `src/lib.rs` 加：

```rust
pub mod model;
```

Run: `cargo test --lib model`
Expected: PASS（2 个测试）。

- [ ] **Step 3: Commit**

```bash
git add src/model.rs src/lib.rs
git commit -m "feat: 双层数据模型 (RawTags + Unified)"
```

---

## Task 5: 核心指令机类型（`Demand` / `Event` / `MetaParser`）

**Files:**
- Create: `src/demand.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 写失败测试**

Create `src/demand.rs`：

```rust
//! sans-io 核心：解析器只发 Demand、产出 Event，绝不碰 I/O。

use alloc::vec::Vec;

use crate::model::Warning;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Demand {
    /// 需要至少 n 字节才能继续。
    NeedBytes(usize),
    /// 从当前位置向前跳过 n 字节。
    Skip(u64),
    /// 跳到绝对偏移（兜底）。
    SeekTo(u64),
    /// 解析完成。
    Done,
}

/// 已定位的元数据载荷种类（本计划只有 Exif，后续加 Xmp/Iptc/Icc）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadKind {
    Exif,
}

/// 解析过程中增量产出的事件。Payload 借用驱动缓冲，零拷贝。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<'a> {
    Payload { kind: PayloadKind, data: &'a [u8] },
    Warning(Warning),
}

/// 一次 pull 的结果：下一步需求 + 本步消耗字节数 + 产出事件。
pub struct PullResult<'a> {
    pub demand: Demand,
    pub consumed: usize,
    pub events: Vec<Event<'a>>,
}

/// 格式解析器实现的唯一 trait —— 纯状态机。
pub trait MetaParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个立刻完成、发一条 Exif 载荷的假解析器，验证 trait 形状可用。
    struct Dummy;
    impl MetaParser for Dummy {
        fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
            let mut events = Vec::new();
            events.push(Event::Payload {
                kind: PayloadKind::Exif,
                data: input,
            });
            PullResult {
                demand: Demand::Done,
                consumed: input.len(),
                events,
            }
        }
    }

    #[test]
    fn parser_can_emit_payload_and_finish() {
        let buf = [1u8, 2, 3];
        let mut p = Dummy;
        let res = p.pull(&buf);
        assert_eq!(res.demand, Demand::Done);
        assert_eq!(res.consumed, 3);
        assert_eq!(res.events.len(), 1);
        match &res.events[0] {
            Event::Payload { kind, data } => {
                assert_eq!(*kind, PayloadKind::Exif);
                assert_eq!(*data, &buf[..]);
            }
            _ => panic!("expected payload"),
        }
    }
}
```

- [ ] **Step 2: 声明模块并运行测试验证**

在 `src/lib.rs` 加：

```rust
pub mod demand;
```

Run: `cargo test --lib demand`
Expected: PASS（1 个测试）。

- [ ] **Step 3: Commit**

```bash
git add src/demand.rs src/lib.rs
git commit -m "feat: sans-io 核心指令机类型 Demand/Event/MetaParser"
```

---

## Task 6: EXIF codec（TIFF 头 + IFD0，ASCII/SHORT）

**Files:**
- Create: `src/codecs/mod.rs`, `src/codecs/exif.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 写失败测试**

Create `src/codecs/exif.rs`：

```rust
//! EXIF 解码：TIFF 头 (II/MM + 42 + IFD0 偏移) → IFD0 标签。
//! 本计划只解 ASCII (type 2) 与 SHORT (type 3) 标签，足够 Make/Model/Orientation。

use alloc::string::String;
use alloc::vec::Vec;

use crate::cursor::{ByteCursor, Endian};
use crate::limits::Limits;
use crate::model::{ExifTag, Value, WarnKind, Warning};

/// 解码一段 TIFF 字节（即 APP1 "Exif\0\0" 之后的内容）。
pub fn decode(
    tiff: &[u8],
    out: &mut Vec<ExifTag>,
    warnings: &mut Vec<Warning>,
    limits: &Limits,
) {
    let mut cur = ByteCursor::new(tiff);
    let endian = match cur.take(2) {
        Some(s) if s == b"II" => Endian::Little,
        Some(s) if s == b"MM" => Endian::Big,
        _ => {
            warnings.push(Warning { offset: 0, kind: WarnKind::BadExifHeader });
            return;
        }
    };
    if cur.u16(endian) != Some(42) {
        warnings.push(Warning { offset: 2, kind: WarnKind::BadExifHeader });
        return;
    }
    let ifd0 = match cur.u32(endian) {
        Some(v) => v as usize,
        None => {
            warnings.push(Warning { offset: 4, kind: WarnKind::BadExifHeader });
            return;
        }
    };
    parse_ifd(tiff, endian, ifd0, out, warnings, limits);
}

fn parse_ifd(
    tiff: &[u8],
    e: Endian,
    off: usize,
    out: &mut Vec<ExifTag>,
    warnings: &mut Vec<Warning>,
    limits: &Limits,
) {
    let mut cur = ByteCursor::new(tiff);
    if cur.seek(off).is_none() {
        warnings.push(Warning { offset: off as u64, kind: WarnKind::BadExifHeader });
        return;
    }
    let count = match cur.u16(e) {
        Some(c) => c,
        None => {
            warnings.push(Warning { offset: off as u64, kind: WarnKind::Truncated });
            return;
        }
    };
    for _ in 0..count {
        if out.len() >= limits.max_tags {
            break;
        }
        let tag = match cur.u16(e) {
            Some(v) => v,
            None => break,
        };
        let typ = match cur.u16(e) {
            Some(v) => v,
            None => break,
        };
        let cnt = match cur.u32(e) {
            Some(v) => v,
            None => break,
        };
        let valoff = match cur.take(4) {
            Some(s) => s,
            None => break,
        };
        if let Some(val) = read_value(tiff, e, typ, cnt, valoff) {
            out.push(ExifTag { ifd: 0, tag, value: val });
        }
    }
}

fn read_value(tiff: &[u8], e: Endian, typ: u16, cnt: u32, valoff: &[u8]) -> Option<Value> {
    match typ {
        // SHORT：本计划只取 cnt==1。
        3 => {
            if cnt != 1 || valoff.len() < 2 {
                return None;
            }
            let v = match e {
                Endian::Little => u16::from_le_bytes([valoff[0], valoff[1]]),
                Endian::Big => u16::from_be_bytes([valoff[0], valoff[1]]),
            };
            Some(Value::U16(v))
        }
        // ASCII：<=4 字节内联，否则按偏移取。
        2 => {
            let total = cnt as usize;
            let bytes: &[u8] = if total <= 4 {
                &valoff[..total.min(valoff.len())]
            } else {
                let off = match e {
                    Endian::Little => {
                        u32::from_le_bytes([valoff[0], valoff[1], valoff[2], valoff[3]])
                    }
                    Endian::Big => {
                        u32::from_be_bytes([valoff[0], valoff[1], valoff[2], valoff[3]])
                    }
                } as usize;
                let end = off.checked_add(total)?;
                tiff.get(off..end)?
            };
            let nul = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            let s = core::str::from_utf8(&bytes[..nul]).ok()?;
            Some(Value::Text(String::from(s)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个小端 TIFF：IFD0 含 Make="Acme"(0x010F) 与 Orientation=6(0x0112)。
    /// 布局：II,42,IFD0@8 | count=2 | Make 条目(偏移指向 38) | Orientation 条目(内联 6) | next=0 | "Acme\0"@38
    fn tiff_fixture() -> Vec<u8> {
        let mut t: Vec<u8> = Vec::new();
        t.extend_from_slice(b"II"); // 0..2 小端
        t.extend_from_slice(&42u16.to_le_bytes()); // 2..4
        t.extend_from_slice(&8u32.to_le_bytes()); // 4..8 IFD0 偏移
        // IFD0 @ 8
        t.extend_from_slice(&2u16.to_le_bytes()); // entry count
        // 条目 1: Make, ASCII, count=5, 偏移=38
        t.extend_from_slice(&0x010Fu16.to_le_bytes());
        t.extend_from_slice(&2u16.to_le_bytes());
        t.extend_from_slice(&5u32.to_le_bytes());
        t.extend_from_slice(&38u32.to_le_bytes());
        // 条目 2: Orientation, SHORT, count=1, 内联值=6
        t.extend_from_slice(&0x0112u16.to_le_bytes());
        t.extend_from_slice(&3u16.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&6u32.to_le_bytes());
        // next IFD = 0
        t.extend_from_slice(&0u32.to_le_bytes());
        // "Acme\0" @ 38
        debug_assert_eq!(t.len(), 38);
        t.extend_from_slice(b"Acme\0");
        t
    }

    #[test]
    fn decodes_make_and_orientation() {
        let tiff = tiff_fixture();
        let mut out = Vec::new();
        let mut warns = Vec::new();
        decode(&tiff, &mut out, &mut warns, &Limits::default());
        assert!(warns.is_empty(), "unexpected warnings: {:?}", warns);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], ExifTag { ifd: 0, tag: 0x010F, value: Value::Text(String::from("Acme")) });
        assert_eq!(out[1], ExifTag { ifd: 0, tag: 0x0112, value: Value::U16(6) });
    }

    #[test]
    fn bad_header_yields_warning_not_panic() {
        let mut out = Vec::new();
        let mut warns = Vec::new();
        decode(b"XX", &mut out, &mut warns, &Limits::default());
        assert!(out.is_empty());
        assert_eq!(warns.len(), 1);
        assert_eq!(warns[0].kind, WarnKind::BadExifHeader);
    }
}
```

Create `src/codecs/mod.rs`：

```rust
pub mod exif;
```

- [ ] **Step 2: 声明模块并运行测试验证失败**

在 `src/lib.rs` 加：

```rust
pub mod codecs;
```

Run: `cargo test --lib codecs`
Expected: 首次若有笔误则 FAIL；代码完整时 Expected: PASS（2 个测试）。重点验证 `decodes_make_and_orientation` 通过（证明偏移 38、内联 SHORT 都正确）。

- [ ] **Step 3: Commit**

```bash
git add src/codecs/mod.rs src/codecs/exif.rs src/lib.rs
git commit -m "feat: EXIF codec 解 IFD0 的 ASCII/SHORT 标签"
```

---

## Task 7: normalization（`RawTags` → `Unified`）

**Files:**
- Create: `src/normalize.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 写失败测试**

Create `src/normalize.rs`：

```rust
//! 把原始标签投影成统一规范字段。映射规则集中在此，便于测试。

use crate::model::{Orientation, RawTags, Unified, Value};

const TAG_MAKE: u16 = 0x010F;
const TAG_MODEL: u16 = 0x0110;
const TAG_ORIENTATION: u16 = 0x0112;

pub fn normalize(raw: &RawTags) -> Unified {
    let mut u = Unified::default();
    for t in &raw.exif {
        match (t.tag, &t.value) {
            (TAG_MAKE, Value::Text(s)) => u.camera_make = Some(s.clone()),
            (TAG_MODEL, Value::Text(s)) => u.camera_model = Some(s.clone()),
            (TAG_ORIENTATION, Value::U16(v)) => u.orientation = Orientation::from_u16(*v),
            _ => {}
        }
    }
    u
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ExifTag;
    use alloc::string::String;
    use alloc::vec::Vec;

    #[test]
    fn projects_exif_tags_to_unified() {
        let raw = RawTags {
            exif: Vec::from([
                ExifTag { ifd: 0, tag: 0x010F, value: Value::Text(String::from("Acme")) },
                ExifTag { ifd: 0, tag: 0x0112, value: Value::U16(6) },
            ]),
        };
        let u = normalize(&raw);
        assert_eq!(u.camera_make.as_deref(), Some("Acme"));
        assert_eq!(u.camera_model, None);
        assert_eq!(u.orientation, Some(Orientation::Rotate90));
    }

    #[test]
    fn unknown_orientation_value_is_dropped() {
        let raw = RawTags {
            exif: Vec::from([ExifTag { ifd: 0, tag: 0x0112, value: Value::U16(99) }]),
        };
        assert_eq!(normalize(&raw).orientation, None);
    }
}
```

- [ ] **Step 2: 声明模块并运行测试验证**

在 `src/lib.rs` 加：

```rust
pub mod normalize;
```

Run: `cargo test --lib normalize`
Expected: PASS（2 个测试）。

- [ ] **Step 3: Commit**

```bash
git add src/normalize.rs src/lib.rs
git commit -m "feat: normalization 将 EXIF 投影为统一模型"
```

---

## Task 8: JPEG 段遍历（定位 APP1/Exif 载荷）

**Files:**
- Create: `src/formats/mod.rs`, `src/formats/jpeg.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 写失败测试**

Create `src/formats/jpeg.rs`：

```rust
//! JPEG 段遍历：SOI 起，逐段扫描，遇 APP1 "Exif\0\0" 发出 Exif 载荷；
//! 遇 SOS/EOI 停止（后面是熵编码数据，无元数据）。单遍处理整块缓冲。

use alloc::vec::Vec;

use crate::cursor::ByteCursor;
use crate::demand::{Demand, Event, MetaParser, PayloadKind, PullResult};

pub struct JpegParser;

impl MetaParser for JpegParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        let mut events = Vec::new();
        // best-effort：截断/畸形直接停，已收集的照常返回。
        let _ = walk(input, &mut events);
        PullResult {
            demand: Demand::Done,
            consumed: input.len(),
            events,
        }
    }
}

fn walk<'a>(input: &'a [u8], events: &mut Vec<Event<'a>>) -> Option<()> {
    let mut cur = ByteCursor::new(input);
    if cur.u16_be()? != 0xFFD8 {
        return None; // 非 JPEG
    }
    loop {
        // 标记以 0xFF 开头，后跟非 0x00/0xFF 的码字；0xFF 填充字节可重复。
        let lead = cur.u8()?;
        if lead != 0xFF {
            return None;
        }
        let mut marker = cur.u8()?;
        while marker == 0xFF {
            marker = cur.u8()?;
        }
        match marker {
            0xD9 | 0xDA => return Some(()), // EOI / SOS：到此为止
            0x01 | 0xD0..=0xD7 => continue, // TEM / RSTn：无长度字段
            _ => {
                let len = cur.u16_be()?;
                if len < 2 {
                    return None;
                }
                let body = cur.take(len as usize - 2)?;
                if marker == 0xE1 && body.starts_with(b"Exif\0\0") {
                    events.push(Event::Payload {
                        kind: PayloadKind::Exif,
                        data: &body[6..],
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最小 JPEG：SOI + APP1(Exif + 3 字节假 TIFF) + EOI。
    fn jpeg_with_exif() -> Vec<u8> {
        let tiff = [0xAAu8, 0xBB, 0xCC]; // 占位 TIFF 内容
        let mut seg_body: Vec<u8> = Vec::new();
        seg_body.extend_from_slice(b"Exif\0\0");
        seg_body.extend_from_slice(&tiff);
        let len = (seg_body.len() + 2) as u16;

        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]); // SOI
        j.extend_from_slice(&[0xFF, 0xE1]); // APP1
        j.extend_from_slice(&len.to_be_bytes());
        j.extend_from_slice(&seg_body);
        j.extend_from_slice(&[0xFF, 0xD9]); // EOI
        j
    }

    #[test]
    fn emits_exif_payload() {
        let j = jpeg_with_exif();
        let mut p = JpegParser;
        let res = p.pull(&j);
        assert_eq!(res.demand, Demand::Done);
        assert_eq!(res.events.len(), 1);
        match &res.events[0] {
            Event::Payload { kind, data } => {
                assert_eq!(*kind, PayloadKind::Exif);
                assert_eq!(*data, &[0xAA, 0xBB, 0xCC][..]); // "Exif\0\0" 已剥离
            }
            _ => panic!("expected payload"),
        }
    }

    #[test]
    fn non_jpeg_emits_nothing() {
        let mut p = JpegParser;
        let res = p.pull(&[0x00, 0x01, 0x02, 0x03]);
        assert_eq!(res.demand, Demand::Done);
        assert!(res.events.is_empty());
    }
}
```

Create `src/formats/mod.rs`：

```rust
pub mod jpeg;
```

- [ ] **Step 2: 声明模块并运行测试验证**

在 `src/lib.rs` 加：

```rust
pub mod formats;
```

Run: `cargo test --lib formats`
Expected: PASS（2 个测试）。`emits_exif_payload` 证明段长度计算与 "Exif\0\0" 剥离正确。

- [ ] **Step 3: Commit**

```bash
git add src/formats/mod.rs src/formats/jpeg.rs src/lib.rs
git commit -m "feat: JPEG 段遍历定位 APP1/Exif 载荷"
```

---

## Task 9: probe（魔数嗅探）

**Files:**
- Create: `src/probe.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 写失败测试**

Create `src/probe.rs`：

```rust
//! 魔数嗅探。本计划只识别 JPEG，其余归 Unknown（后续计划扩展）。

use crate::model::FileFormat;

pub fn probe(buf: &[u8]) -> FileFormat {
    if buf.len() >= 2 && buf[0] == 0xFF && buf[1] == 0xD8 {
        FileFormat::Jpeg
    } else {
        FileFormat::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_jpeg_soi() {
        assert_eq!(probe(&[0xFF, 0xD8, 0xFF, 0xE0]), FileFormat::Jpeg);
    }

    #[test]
    fn unknown_for_others_and_short_input() {
        assert_eq!(probe(&[0x89, 0x50]), FileFormat::Unknown);
        assert_eq!(probe(&[0xFF]), FileFormat::Unknown);
        assert_eq!(probe(&[]), FileFormat::Unknown);
    }
}
```

- [ ] **Step 2: 声明模块并运行测试验证**

在 `src/lib.rs` 加：

```rust
pub mod probe;
```

Run: `cargo test --lib probe`
Expected: PASS（2 个测试）。

- [ ] **Step 3: Commit**

```bash
git add src/probe.rs src/lib.rs
git commit -m "feat: 魔数嗅探 probe (JPEG)"
```

---

## Task 10: slice 驱动循环（`Collector` + `drive_slice`）

**Files:**
- Create: `src/driver.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 写失败测试**

Create `src/driver.rs`：

```rust
//! slice 驱动循环：把整块缓冲反复喂给 MetaParser，按 Demand 推进逻辑位置，
//! 并把 Payload 事件分派给对应 codec。这是 read_slice 的引擎。

use alloc::vec::Vec;

use crate::codecs;
use crate::demand::{Demand, Event, MetaParser, PayloadKind};
use crate::limits::Limits;
use crate::model::{ExifTag, WarnKind, Warning};

/// 解析过程中累积的产物。
pub struct Collector {
    pub exif: Vec<ExifTag>,
    pub warnings: Vec<Warning>,
    limits: Limits,
}

impl Collector {
    fn handle(&mut self, ev: Event<'_>) {
        match ev {
            Event::Payload { kind: PayloadKind::Exif, data } => {
                codecs::exif::decode(data, &mut self.exif, &mut self.warnings, &self.limits);
            }
            Event::Warning(w) => self.warnings.push(w),
        }
    }
}

/// 在一整块内存缓冲上驱动 parser 跑到 Done。
pub fn drive_slice(buf: &[u8], parser: &mut dyn MetaParser, limits: Limits) -> Collector {
    let mut col = Collector { exif: Vec::new(), warnings: Vec::new(), limits };
    let mut pos: usize = 0;
    loop {
        let start = pos.min(buf.len());
        let res = parser.pull(&buf[start..]);
        for ev in res.events {
            col.handle(ev);
        }
        match res.demand {
            Demand::Done => break,
            Demand::NeedBytes(_) => {
                // slice 不会再增长 → 截断。
                col.warnings.push(Warning { offset: start as u64, kind: WarnKind::Truncated });
                break;
            }
            Demand::Skip(n) => {
                pos = start.saturating_add(res.consumed).saturating_add(n as usize);
                if pos > buf.len() {
                    col.warnings.push(Warning { offset: pos as u64, kind: WarnKind::UnreachableSection });
                    break;
                }
            }
            Demand::SeekTo(p) => {
                let p = p as usize;
                if p > buf.len() {
                    col.warnings.push(Warning { offset: p as u64, kind: WarnKind::UnreachableSection });
                    break;
                }
                pos = p;
            }
        }
    }
    col
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demand::{PullResult, PayloadKind};

    /// 发一条 Exif 载荷然后 Done 的假解析器。
    struct OneShot<'b>(&'b [u8]);
    impl<'b> MetaParser for OneShot<'b> {
        fn pull<'a>(&mut self, _input: &'a [u8]) -> PullResult<'a> {
            // 注意：这里返回的 Payload 借用的是 OneShot 自带的缓冲，
            // 不是 input —— 仅用于驱动逻辑测试。
            unreachable!()
        }
    }

    /// 用真实 JPEG 解析器 + 真实 EXIF 走一遍，验证驱动把载荷送进了 codec。
    #[test]
    fn drives_jpeg_into_exif_collector() {
        // 复用 EXIF 与 JPEG 的 fixture 思路：构造 JPEG(含完整 TIFF)。
        let tiff = make_tiff();
        let mut seg_body: Vec<u8> = Vec::new();
        seg_body.extend_from_slice(b"Exif\0\0");
        seg_body.extend_from_slice(&tiff);
        let len = (seg_body.len() + 2) as u16;
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
        j.extend_from_slice(&len.to_be_bytes());
        j.extend_from_slice(&seg_body);
        j.extend_from_slice(&[0xFF, 0xD9]);

        let mut parser = crate::formats::jpeg::JpegParser;
        let col = drive_slice(&j, &mut parser, Limits::default());
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
        assert_eq!(col.exif.len(), 2);
    }

    fn make_tiff() -> Vec<u8> {
        let mut t: Vec<u8> = Vec::new();
        t.extend_from_slice(b"II");
        t.extend_from_slice(&42u16.to_le_bytes());
        t.extend_from_slice(&8u32.to_le_bytes());
        t.extend_from_slice(&2u16.to_le_bytes());
        t.extend_from_slice(&0x010Fu16.to_le_bytes());
        t.extend_from_slice(&2u16.to_le_bytes());
        t.extend_from_slice(&5u32.to_le_bytes());
        t.extend_from_slice(&38u32.to_le_bytes());
        t.extend_from_slice(&0x0112u16.to_le_bytes());
        t.extend_from_slice(&3u16.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&6u32.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(b"Acme\0");
        t
    }

    // 抑制未使用告警：OneShot 仅作 trait 形状示例。
    #[allow(dead_code)]
    fn _use_oneshot(_: OneShot<'_>) {}
}
```

- [ ] **Step 2: 声明模块并运行测试验证**

在 `src/lib.rs` 加：

```rust
pub mod driver;
```

Run: `cargo test --lib driver`
Expected: PASS（`drives_jpeg_into_exif_collector`）。这一步首次把 JPEG→EXIF→Collector 串起来。

> 若编译器对 `OneShot` / `_use_oneshot` 报未使用，按提示保留 `#[allow(dead_code)]` 即可；它只是 trait 形状占位，不参与断言。

- [ ] **Step 3: Commit**

```bash
git add src/driver.rs src/lib.rs
git commit -m "feat: slice 驱动循环 drive_slice + Collector"
```

---

## Task 11: `read_slice` 公开 API + 端到端测试

**Files:**
- Create: `src/adapters/mod.rs`, `src/adapters/slice.rs`
- Modify: `src/lib.rs`
- Test: `tests/read_slice_jpeg.rs`

- [ ] **Step 1: 写失败测试（集成测试，走公开 API）**

Create `tests/read_slice_jpeg.rs`：

```rust
//! 端到端：从带 EXIF 的 JPEG 字节经公开 API 读出统一字段。

use omni_meta::{read_slice, FileFormat, Options, Orientation};

/// 构造小端 TIFF：Make="Acme"(0x010F) + Orientation=6(0x0112)。
fn make_tiff() -> Vec<u8> {
    let mut t: Vec<u8> = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    t.extend_from_slice(&2u16.to_le_bytes());
    t.extend_from_slice(&0x010Fu16.to_le_bytes());
    t.extend_from_slice(&2u16.to_le_bytes());
    t.extend_from_slice(&5u32.to_le_bytes());
    t.extend_from_slice(&38u32.to_le_bytes());
    t.extend_from_slice(&0x0112u16.to_le_bytes());
    t.extend_from_slice(&3u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&6u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    t.extend_from_slice(b"Acme\0");
    t
}

fn jpeg_with_exif() -> Vec<u8> {
    let tiff = make_tiff();
    let mut seg_body: Vec<u8> = Vec::new();
    seg_body.extend_from_slice(b"Exif\0\0");
    seg_body.extend_from_slice(&tiff);
    let len = (seg_body.len() + 2) as u16;
    let mut j: Vec<u8> = Vec::new();
    j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
    j.extend_from_slice(&len.to_be_bytes());
    j.extend_from_slice(&seg_body);
    j.extend_from_slice(&[0xFF, 0xD9]);
    j
}

#[test]
fn extracts_unified_fields_from_jpeg() {
    let j = jpeg_with_exif();
    let meta = read_slice(&j, Options::default()).expect("should parse");
    assert_eq!(meta.format, FileFormat::Jpeg);
    assert_eq!(meta.unified.camera_make.as_deref(), Some("Acme"));
    assert_eq!(meta.unified.camera_model, None);
    assert_eq!(meta.unified.orientation, Some(Orientation::Rotate90));
    assert!(meta.warnings.is_empty(), "warnings: {:?}", meta.warnings);
    // 原始层也应可下钻
    assert_eq!(meta.raw.exif.len(), 2);
}

#[test]
fn unrecognized_format_errors() {
    let err = read_slice(&[0x00, 0x01, 0x02], Options::default());
    assert!(err.is_err());
}
```

- [ ] **Step 2: 运行测试验证失败**

Run: `cargo test --test read_slice_jpeg`
Expected: FAIL，`unresolved import omni_meta::read_slice`（公开 API 尚未导出）。

- [ ] **Step 3: 实现 `Options` + `read_slice`**

Create `src/adapters/slice.rs`：

```rust
//! read_slice：全内存/零拷贝随机访问适配器。

use crate::driver::drive_slice;
use crate::error::Error;
use crate::formats::jpeg::JpegParser;
use crate::limits::Limits;
use crate::model::{FileFormat, Metadata, RawTags};
use crate::normalize::normalize;
use crate::probe::probe;

/// 解析选项。
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub limits: Limits,
}

/// 从一整块内存缓冲解析元数据。无法识别格式时返回 Err。
pub fn read_slice(buf: &[u8], opts: Options) -> Result<Metadata, Error> {
    match probe(buf) {
        FileFormat::Jpeg => {
            let mut parser = JpegParser;
            let col = drive_slice(buf, &mut parser, opts.limits);
            let raw = RawTags { exif: col.exif };
            let unified = normalize(&raw);
            Ok(Metadata {
                unified,
                raw,
                warnings: col.warnings,
                format: FileFormat::Jpeg,
            })
        }
        FileFormat::Unknown => Err(Error::UnrecognizedFormat),
    }
}
```

Create `src/adapters/mod.rs`：

```rust
pub mod slice;
```

在 `src/lib.rs` 模块声明区加：

```rust
pub mod adapters;
```

并在 `src/lib.rs` 末尾（模块声明之后）加公开 re-export，让集成测试用到的符号可从 crate 根访问：

```rust
pub use adapters::slice::{read_slice, Options};
pub use error::Error;
pub use model::{FileFormat, Metadata, Orientation, RawTags, Unified, Value};
```

- [ ] **Step 4: 运行测试验证通过**

Run: `cargo test --test read_slice_jpeg`
Expected: PASS（2 个测试）。

- [ ] **Step 5: 运行全部测试 + no_std 构建 + clippy**

Run: `cargo test`
Expected: 全绿（单元 + 集成）。

Run: `cargo build --no-default-features`
Expected: 编译成功（核心 no_std + alloc）。

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 无告警。

- [ ] **Step 6: Commit**

```bash
git add src/adapters/mod.rs src/adapters/slice.rs src/lib.rs tests/read_slice_jpeg.rs
git commit -m "feat: read_slice 公开 API + JPEG/EXIF 端到端"
```

---

## 自查（spec 覆盖与一致性）

- **§3 架构分层 / 模块布局**：本计划落地 cursor/limits/error/model/demand/codecs/normalize/formats/probe/driver/adapters 全部基座模块 ✅（含 model/ 暂用单文件 `model.rs`，规范注明后续可拆 `model/` 目录）。
- **§4 指令机**：`Demand`(NeedBytes/Skip/SeekTo/Done)/`Event`/`PayloadKind`/`PullResult`/`MetaParser` 全部实现，Task 5/10 ✅。`Skip`/`SeekTo` 在 slice 驱动里的语义（向前 + 绝对，越界→Warning）在 Task 10 实现 ✅。
- **§5 三级降级**：本计划只含 `read_slice`（全缓冲，所有 seek 都是 O(1) 切片定位），故只覆盖"向前/缓冲内"两级；不可 Seek 流的第三级降级随 push/blocking 适配器计划落地（已在计划开头声明）✅。
- **§6 双层模型**：`RawTags` + `Unified` + `Value` + `Warning`，Task 4/7 ✅。Unified 字段先含 orientation/make/model，规范注明受控增长 ✅。
- **§9 安全**：`#![forbid(unsafe_code)]`（Task 1）、`Limits::max_tags` 在 IFD 解析中生效（Task 6）、所有读取经 `ByteCursor` 边界检查与 `checked_add`（Task 2/6）、JPEG/EXIF 畸形输入返回 Warning 而非 panic（Task 6/8 测试覆盖）✅。
- **§10 测试**：单元测试每模块齐备；端到端黄金样本测试 Task 11 ✅。差分测试需 ≥2 适配器，已声明随 push 计划做。
- **§11 阶段**：本计划 = Phase 1（骨架 + read_slice）+ Phase 2 的 JPEG/EXIF 子集。PNG/WebP/GIF、BMFF、EBML、其余 codec、适配器、Stripper 为后续独立计划。

**类型一致性核对**：`read_value`/`decode`/`parse_ifd` 签名跨 Task 6 一致；`Value::U16`/`Value::Text` 在 model(Task4)/exif(Task6)/normalize(Task7) 用法一致；`Warning{offset,kind}` 与 `WarnKind` 变体在 model/exif/driver 一致；`drive_slice(buf, &mut dyn MetaParser, Limits)` 与 Task 11 调用点一致；`read_slice(&[u8], Options) -> Result<Metadata, Error>` 与集成测试一致。无悬空引用。
