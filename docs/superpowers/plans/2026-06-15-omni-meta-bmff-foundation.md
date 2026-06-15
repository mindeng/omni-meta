# ISO-BMFF 基座（box 读取器 + ftyp 识别）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 omni-meta 识别 ISO-BMFF 容器（HEIF/AVIF/MP4/MOV），并提供后续里程碑共享的 box 头读取器——本计划只到「识别 + 校验首个 `ftyp` box」，不抽取元数据。

**Architecture:** 新增共享模块 `containers/isobmff.rs`（纯函数 `read_box_header`，边界安全、不分配），`probe` 通过 `ftyp` box 的 major brand 把文件分类到新的 `FileFormat::{Heif,Avif,Mp4,Mov}`，新增 `formats/bmff.rs` 的 `BmffParser` 校验首盒为 `ftyp` 后即 `Done`。`meta`/`moov` 下钻留给 A2/A3。沿用既有 sans-io `MetaParser` 契约，因此四条适配器（slice/push/blocking/seek）零改动即获得 BMFF 识别能力。

**Tech Stack:** Rust edition 2024、`#![no_std]` + `alloc`、零依赖、`#![forbid(unsafe_code)]`。BMFF 全程大端（big-endian）。

---

## 文件结构

| 文件 | 职责 | 动作 |
|---|---|---|
| `omni-meta-core/src/model.rs` | `FileFormat` 增加 BMFF 家族四变体 | Modify |
| `omni-meta-core/src/containers/mod.rs` | 容器读取器模块根 | Create |
| `omni-meta-core/src/containers/isobmff.rs` | `read_box_header` + `BoxHeader`（A2/A3 共享） | Create |
| `omni-meta-core/src/lib.rs` | 声明 `pub(crate) mod containers;` | Modify |
| `omni-meta-core/src/probe.rs` | `ftyp` 识别 + `brand_to_format` + `parser_for` 接入 | Modify |
| `omni-meta-core/src/formats/bmff.rs` | `BmffParser`（校验 ftyp → Done） | Create |
| `omni-meta-core/src/formats/mod.rs` | 声明 `pub mod bmff;` | Modify |
| `omni-meta/tests/differential.rs` | BMFF 四适配器差分 fixture | Modify |

**设计边界**：`read_box_header` 只解析 `size`/`type`/`largesize`，不前进、不分配、越界返回 `None`——和 `cursor::ByteCursor` 同样的「边界安全、绝不 panic」姿态。`BmffParser` 本计划内只读首盒；A2/A3 会让它续走 top-level box 链。

---

### Task 1: `FileFormat` 增加 BMFF 家族变体

**Files:**
- Modify: `omni-meta-core/src/model.rs:8-15`（`enum FileFormat`）
- Test: `omni-meta-core/src/model.rs`（既有 `mod tests`）

- [ ] **Step 1: 写失败测试**

在 `omni-meta-core/src/model.rs` 的 `mod tests` 内追加：

```rust
    #[test]
    fn fileformat_has_bmff_family() {
        // 四个 BMFF 家族变体可构造且互不相等。
        let all = [
            FileFormat::Heif,
            FileFormat::Avif,
            FileFormat::Mp4,
            FileFormat::Mov,
        ];
        assert_eq!(all[0], FileFormat::Heif);
        assert_ne!(FileFormat::Heif, FileFormat::Avif);
        assert_ne!(FileFormat::Mp4, FileFormat::Mov);
    }
```

- [ ] **Step 2: 运行测试，确认编译失败**

Run: `cargo test -p omni-meta-core fileformat_has_bmff_family`
Expected: 编译错误 `no variant named Heif found for enum FileFormat`。

- [ ] **Step 3: 加变体**

把 `omni-meta-core/src/model.rs` 的 `FileFormat` 改为：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileFormat {
    Jpeg,
    Png,
    Webp,
    Gif,
    Heif,
    Avif,
    Mp4,
    Mov,
    Unknown,
}
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core fileformat_has_bmff_family`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/model.rs
git commit -m "feat: FileFormat 增加 BMFF 家族 (Heif/Avif/Mp4/Mov)"
```

---

### Task 2: `containers/isobmff.rs` —— box 头读取器

**Files:**
- Create: `omni-meta-core/src/containers/mod.rs`
- Create: `omni-meta-core/src/containers/isobmff.rs`
- Modify: `omni-meta-core/src/lib.rs:8-18`（模块声明区）

- [ ] **Step 1: 建模块骨架**

创建 `omni-meta-core/src/containers/mod.rs`：

```rust
//! 跨格式共享的容器读取器。BMFF（HEIF/AVIF/MP4/MOV）首个落地。
pub mod isobmff;
```

在 `omni-meta-core/src/lib.rs` 模块声明区（`pub(crate) mod codecs;` 一行附近）加入，保持字母序：

```rust
pub(crate) mod containers;
```

- [ ] **Step 2: 写失败测试（含实现占位，先让文件存在）**

创建 `omni-meta-core/src/containers/isobmff.rs`：

```rust
//! ISO-BMFF (ISO/IEC 14496-12) box 头部读取。共享给 HEIF/AVIF/MP4/MOV。
//! 全程大端。边界安全：字节不足返回 None，绝不 panic、不分配、不前进。

/// 一个 box 的头部信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxHeader {
    /// 四字节 box 类型（如 `b"ftyp"`）。
    pub kind: [u8; 4],
    /// 头部自身字节数：8（32 位 size）或 16（size==1 的 64 位 largesize）。
    pub header_len: u64,
    /// box 总字节数（含头部）。size==0（延伸至文件尾）时为 None。
    pub total_size: Option<u64>,
}

impl BoxHeader {
    /// 载荷字节数 = total_size − header_len；size==0 时未知，返回 None。
    pub fn payload_len(&self) -> Option<u64> {
        self.total_size.map(|t| t.saturating_sub(self.header_len))
    }
}

/// 从 `input` 起点读一个 box 头。字节不足以读出完整头部时返回 None。
pub fn read_box_header(input: &[u8]) -> Option<BoxHeader> {
    if input.len() < 8 {
        return None;
    }
    let size32 = u32::from_be_bytes([input[0], input[1], input[2], input[3]]);
    let mut kind = [0u8; 4];
    kind.copy_from_slice(&input[4..8]);
    match size32 {
        1 => {
            if input.len() < 16 {
                return None;
            }
            let large = u64::from_be_bytes([
                input[8], input[9], input[10], input[11],
                input[12], input[13], input[14], input[15],
            ]);
            Some(BoxHeader { kind, header_len: 16, total_size: Some(large) })
        }
        0 => Some(BoxHeader { kind, header_len: 8, total_size: None }),
        n => Some(BoxHeader { kind, header_len: 8, total_size: Some(n as u64) }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_32bit_size_box() {
        // size=16, type="ftyp"
        let mut b = alloc::vec::Vec::new();
        b.extend_from_slice(&16u32.to_be_bytes());
        b.extend_from_slice(b"ftyp");
        b.extend_from_slice(&[0u8; 8]); // 载荷
        let h = read_box_header(&b).unwrap();
        assert_eq!(&h.kind, b"ftyp");
        assert_eq!(h.header_len, 8);
        assert_eq!(h.total_size, Some(16));
        assert_eq!(h.payload_len(), Some(8));
    }

    #[test]
    fn reads_64bit_largesize_box() {
        // size=1 → 紧跟 8 字节 largesize
        let mut b = alloc::vec::Vec::new();
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(b"mdat");
        b.extend_from_slice(&4_000_000_000u64.to_be_bytes());
        let h = read_box_header(&b).unwrap();
        assert_eq!(&h.kind, b"mdat");
        assert_eq!(h.header_len, 16);
        assert_eq!(h.total_size, Some(4_000_000_000));
        assert_eq!(h.payload_len(), Some(4_000_000_000 - 16));
    }

    #[test]
    fn size_zero_means_to_eof() {
        let mut b = alloc::vec::Vec::new();
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(b"mdat");
        let h = read_box_header(&b).unwrap();
        assert_eq!(h.total_size, None);
        assert_eq!(h.payload_len(), None);
    }

    #[test]
    fn too_short_returns_none() {
        assert!(read_box_header(&[0, 0, 0]).is_none());
        // 声明 largesize 但缺字节
        let mut b = alloc::vec::Vec::new();
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(b"mdat");
        b.extend_from_slice(&[0u8; 3]); // 只 3 字节，不足 8
        assert!(read_box_header(&b).is_none());
    }
}
```

> 注：测试里用 `alloc::vec::Vec`——crate 是 `no_std`，`Vec` 来自 `alloc`，与既有测试（如 `webp.rs`）一致。

- [ ] **Step 3: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core isobmff`
Expected: 4 个测试全 PASS。

- [ ] **Step 4: 提交**

```bash
git add omni-meta-core/src/containers/ omni-meta-core/src/lib.rs
git commit -m "feat: containers/isobmff 共享 box 头读取器 (size32/largesize64/size0)"
```

---

### Task 3: `probe` 识别 ftyp + brand 映射

**Files:**
- Modify: `omni-meta-core/src/probe.rs`（`probe` 函数 + 新增 `brand_to_format`）
- Test: `omni-meta-core/src/probe.rs`（既有 `mod tests`）

- [ ] **Step 1: 写失败测试**

在 `omni-meta-core/src/probe.rs` 的 `mod tests` 内追加：

```rust
    fn ftyp(major: &[u8; 4]) -> alloc::vec::Vec<u8> {
        let mut b = alloc::vec::Vec::new();
        b.extend_from_slice(&20u32.to_be_bytes()); // size
        b.extend_from_slice(b"ftyp");
        b.extend_from_slice(major);
        b.extend_from_slice(&0u32.to_be_bytes());   // minor version
        b.extend_from_slice(b"mif1");               // 一个兼容品牌
        b
    }

    #[test]
    fn detects_bmff_brands() {
        assert_eq!(probe(&ftyp(b"heic")), FileFormat::Heif);
        assert_eq!(probe(&ftyp(b"mif1")), FileFormat::Heif);
        assert_eq!(probe(&ftyp(b"avif")), FileFormat::Avif);
        assert_eq!(probe(&ftyp(b"qt  ")), FileFormat::Mov);
        assert_eq!(probe(&ftyp(b"isom")), FileFormat::Mp4);
        // 未知品牌但确为 ftyp → 归类 Mp4（ISO-BMFF 兜底）
        assert_eq!(probe(&ftyp(b"zzzz")), FileFormat::Mp4);
    }

    #[test]
    fn bmff_parsers_wired() {
        assert!(parser_for(FileFormat::Heif).is_some());
        assert!(parser_for(FileFormat::Avif).is_some());
        assert!(parser_for(FileFormat::Mp4).is_some());
        assert!(parser_for(FileFormat::Mov).is_some());
    }
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p omni-meta-core detects_bmff_brands`
Expected: FAIL —— `probe` 对 ftyp 返回 `Unknown`（断言不等）。
（`bmff_parsers_wired` 在 Task 5 才转绿，本步先红。）

- [ ] **Step 3: 实现识别**

在 `omni-meta-core/src/probe.rs` 的 `probe` 函数里，GIF 判断之后、`FileFormat::Unknown` 之前插入：

```rust
    // ISO-BMFF：偏移 4 处的 "ftyp" box，major brand 在 [8..12]。
    if buf.len() >= 12 && &buf[4..8] == b"ftyp" {
        return brand_to_format(&buf[8..12]);
    }
```

在 `probe` 函数之后（`parser_for` 之前）新增：

```rust
/// 把 ftyp major brand 映射到 FileFormat。未知品牌但确为 ftyp → Mp4（ISO-BMFF 兜底）。
fn brand_to_format(brand: &[u8]) -> FileFormat {
    match brand {
        b"avif" | b"avis" => FileFormat::Avif,
        b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"mif1"
        | b"msf1" => FileFormat::Heif,
        b"qt  " => FileFormat::Mov,
        // isom/iso2/mp41/mp42/M4V /M4A /dash/avc1… 及其余未知 ISO-BMFF
        _ => FileFormat::Mp4,
    }
}
```

> 注意 `b"qt  "` 末尾是两个空格（QuickTime major brand 为 `qt` 加两个空格补足 4 字节）。

- [ ] **Step 4: 运行 brand 测试，确认通过**

Run: `cargo test -p omni-meta-core detects_bmff_brands`
Expected: PASS。（`bmff_parsers_wired` 仍 FAIL —— Task 5 修复。）

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/probe.rs
git commit -m "feat: probe 识别 ISO-BMFF ftyp + brand→FileFormat 映射"
```

---

### Task 4: `formats/bmff.rs` —— `BmffParser`

**Files:**
- Create: `omni-meta-core/src/formats/bmff.rs`
- Modify: `omni-meta-core/src/formats/mod.rs`

- [ ] **Step 1: 声明模块 + 写失败测试**

在 `omni-meta-core/src/formats/mod.rs` 顶部按字母序加入：

```rust
pub mod bmff;
```

创建 `omni-meta-core/src/formats/bmff.rs`：

```rust
//! ISO-BMFF 顶层解析骨架。本里程碑（A1）只校验首个 box 是 `ftyp` 即 `Done`；
//! `meta`/`moov` 下钻在 A2/A3 引入。沿用既有 sans-io MetaParser 契约。

use alloc::vec::Vec;

use crate::containers::isobmff::read_box_header;
use crate::demand::{Demand, Event, MetaParser, PullResult};

#[derive(Debug, Default)]
pub struct BmffParser {
    done: bool,
}

impl BmffParser {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MetaParser for BmffParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        let events: Vec<Event<'a>> = Vec::new();
        if self.done {
            return PullResult { demand: Demand::Done, consumed: 0, events };
        }
        // 读首个 box 头需 ≥8 字节（largesize 也只需头部，不读 ftyp 载荷）。
        let hdr = match read_box_header(input) {
            Some(h) => h,
            None => {
                return PullResult { demand: Demand::NeedBytes(8), consumed: 0, events };
            }
        };
        // probe 已确保首盒为 ftyp（hdr 仅用于确认头部可完整读出）。
        // A1 不抽取元数据，读到首盒头即完成；box 链续走留给 A2/A3。
        let _ = hdr.kind;
        self.done = true;
        PullResult { demand: Demand::Done, consumed: 0, events }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ftyp_box() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&20u32.to_be_bytes());
        b.extend_from_slice(b"ftyp");
        b.extend_from_slice(b"heic");
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(b"mif1");
        b
    }

    #[test]
    fn ftyp_then_done_no_events() {
        let buf = ftyp_box();
        let mut p = BmffParser::new();
        let res = p.pull(&buf);
        assert_eq!(res.demand, Demand::Done);
        assert!(res.events.is_empty());
    }

    #[test]
    fn short_input_needs_bytes() {
        let mut p = BmffParser::new();
        let res = p.pull(&[0, 0, 0]); // <8 字节
        assert_eq!(res.demand, Demand::NeedBytes(8));
        assert_eq!(res.consumed, 0);
    }

    #[test]
    fn second_pull_after_done_stays_done() {
        let buf = ftyp_box();
        let mut p = BmffParser::new();
        let _ = p.pull(&buf);
        let res = p.pull(&buf);
        assert_eq!(res.demand, Demand::Done);
    }
}
```

- [ ] **Step 2: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core bmff`
Expected: 3 个 BmffParser 测试 PASS。

- [ ] **Step 3: 提交**

```bash
git add omni-meta-core/src/formats/bmff.rs omni-meta-core/src/formats/mod.rs
git commit -m "feat: BmffParser 校验首个 ftyp box (A1 骨架, 暂不抽取元数据)"
```

---

### Task 5: `parser_for` 接入 BMFF + read_slice 端到端

**Files:**
- Modify: `omni-meta-core/src/probe.rs`（`parser_for` 的 `match`）
- Test: `omni-meta-core/src/probe.rs`（`bmff_parsers_wired`，Task 3 已写）+ 新增 read_slice 端到端

- [ ] **Step 1: 写失败的端到端测试**

在 `omni-meta-core/src/probe.rs` 的 `mod tests` 内追加（`ftyp` 辅助函数 Task 3 已加）：

```rust
    #[test]
    fn read_slice_recognizes_heic_empty_meta() {
        use crate::adapters::slice::{read_slice, Options};
        let buf = ftyp(b"heic");
        let meta = read_slice(&buf, Options::default()).unwrap();
        assert_eq!(meta.format, FileFormat::Heif);
        // A1 不抽取任何字段，但必须干净返回、无警告。
        assert!(meta.warnings.is_empty(), "warnings: {:?}", meta.warnings);
        assert_eq!(meta.unified.width, None);
        assert!(meta.raw.exif.is_empty());
    }
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p omni-meta-core read_slice_recognizes_heic_empty_meta`
Expected: FAIL —— `read_slice` 因 `parser_for(Heif)` 返回 `None` 而 `Err(UnrecognizedFormat)`，`.unwrap()` panic。

- [ ] **Step 3: 接入 parser_for**

把 `omni-meta-core/src/probe.rs` 的 `parser_for` 改为：

```rust
pub(crate) fn parser_for(fmt: FileFormat) -> Option<Box<dyn MetaParser>> {
    match fmt {
        FileFormat::Jpeg => Some(Box::new(crate::formats::jpeg::JpegParser::new())),
        FileFormat::Png => Some(Box::new(crate::formats::png::PngParser::new())),
        FileFormat::Webp => Some(Box::new(crate::formats::webp::WebpParser::new())),
        FileFormat::Gif => Some(Box::new(crate::formats::gif::GifParser::new())),
        FileFormat::Heif | FileFormat::Avif | FileFormat::Mp4 | FileFormat::Mov => {
            Some(Box::new(crate::formats::bmff::BmffParser::new()))
        }
        _ => None,
    }
}
```

- [ ] **Step 4: 运行相关测试，确认通过**

Run: `cargo test -p omni-meta-core bmff_parsers_wired read_slice_recognizes_heic_empty_meta`
Expected: 两个测试都 PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/probe.rs
git commit -m "feat: parser_for 把 BMFF 四格式接到 BmffParser + read_slice 端到端"
```

---

### Task 6: 四适配器差分测试

**Files:**
- Modify: `omni-meta/tests/differential.rs`（末尾追加 fixture + 测试）

- [ ] **Step 1: 写差分测试**

在 `omni-meta/tests/differential.rs` 末尾追加：

```rust
/// 最小 BMFF：ftyp(heic) + 一个尾随 free box（A1 全部忽略）。
fn fixture_bmff_heic() -> Vec<u8> {
    let mut f = Vec::new();
    // ftyp box: size=20
    f.extend_from_slice(&20u32.to_be_bytes());
    f.extend_from_slice(b"ftyp");
    f.extend_from_slice(b"heic");
    f.extend_from_slice(&0u32.to_be_bytes());
    f.extend_from_slice(b"mif1");
    // free box: size=8（仅头部）
    f.extend_from_slice(&8u32.to_be_bytes());
    f.extend_from_slice(b"free");
    f
}

#[test]
fn differential_bmff_heic() {
    assert_all_equal(&fixture_bmff_heic());
}
```

- [ ] **Step 2: 运行差分测试，确认通过**

Run: `cargo test -p omni-meta --test differential differential_bmff_heic`
Expected: PASS —— slice/blocking/seek/push 四路对该 BMFF 输入逐字段一致（format=Heif，空 unified，无警告）。

> 若 push 路径因 PROBE_MAX 预缓冲在 12 字节处分类，而首个 free box 落在其后——这是预期的：`BmffParser` 见 ftyp 即 Done，free box 不进解析。四路仍一致。

- [ ] **Step 3: 提交**

```bash
git add omni-meta/tests/differential.rs
git commit -m "test: BMFF(heic) 四适配器差分一致性"
```

---

### Task 7: 全量验证（no_std + clippy + 测试）

**Files:** 无（仅验证 + 收尾提交，如有 lint 修整）

- [ ] **Step 1: 跑全量测试**

Run: `cargo test`
Expected: 工作区全部测试 PASS（含既有 JPEG/PNG/WebP/GIF/EXIF/XMP 与本计划新增）。

- [ ] **Step 2: 验证 no_std 构建未被破坏**

Run: `cargo build -p omni-meta-core --no-default-features`
Expected: 干净编译（BMFF 全部代码只依赖 `alloc` + core，无 `std`）。

- [ ] **Step 3: clippy 清零**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 无告警。常见可能项：`brand_to_format` 的 `match` 可读性、`BoxHeader` 派生。若出现 `collapsible_match`/`needless_return` 等，按提示就地修整。

- [ ] **Step 4: 若 Step 3 有修整则提交**

```bash
git add -A
git commit -m "style: BMFF 基座 clippy 清整"
```

（无修整则跳过本步。）

---

## 完成定义（A1）

- `probe` 能把 `ftyp` 文件分类到 `Heif`/`Avif`/`Mp4`/`Mov`，未知品牌兜底 `Mp4`。
- `read_box_header` 正确处理 size32 / largesize64 / size0 / 截断，永不 panic。
- 四条适配器对 BMFF 输入返回 `format` 正确、`unified` 全空、`warnings` 空的一致 `Metadata`。
- `no_std` 构建与 clippy 均干净。

## 不在本计划范围（→ A2 / A3）

- `meta` / `iinf` / `iloc` / `idat` 解析与 EXIF/XMP item 抽取（A2）。
- `moov` / `tkhd` / `mvhd` → 维度 / `duration` / `created`（A3，含新 Unified 字段与 normalize 规则）。
- 顶层 box 链续走与「box 边界处 EOF = 干净结束」的终止语义（A2 首次需要，届时在 `BmffParser` 引入 top-level walk + 停在目标 box 后 `Done`）。
