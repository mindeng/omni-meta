# A2：HEIF/AVIF `meta` box 元数据抽取 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `BmffParser` 从 HEIF/AVIF 的 `meta` box 抽取 EXIF/XMP（复用现有 codec）与图像维度（ispe），支持 `iloc` construction_method 0/1。

**Architecture:** 通用结构层（`containers/isobmff.rs`）新增 FullBox 头读取、可变位宽整数读取、子盒遍历迭代器（A3 `moov` 共用）；HEIF 语义层（`formats/bmff.rs`）解析 meta 子盒（iinf/iloc/ispe/pitm/ipma/idat）构建抽取计划，并用两阶段 sans-io 状态机（Walk 顶层找 meta → Extract 按绝对偏移 `SeekTo` 取数据）发出事件。`StreamDriver` 增加「空窗口不喂解析器（除非 EOF）」守卫，使顶层 box 链能干净终止。

**Tech Stack:** Rust edition 2024、`#![no_std]` + `alloc`、零依赖、`#![forbid(unsafe_code)]`。BMFF 全程大端。

设计文档：[`docs/superpowers/specs/2026-06-15-omni-meta-bmff-heif-meta-design.md`](../specs/2026-06-15-omni-meta-bmff-heif-meta-design.md)

---

## 文件结构

| 文件 | 职责 | 动作 |
|---|---|---|
| `omni-meta-core/src/driver.rs` | `StreamDriver.drive` 增加空窗口守卫 | Modify |
| `omni-meta-core/src/containers/isobmff.rs` | `full_box_vf` / `read_uint_be` / `iter_child_boxes`（A3 共用） | Modify |
| `omni-meta-core/src/formats/bmff.rs` | item 模型 + meta 子盒解析 + 两阶段状态机 | Modify（A1 骨架扩写） |
| `omni-meta/tests/differential.rs` | 完整 HEIC（meta+mdat）四适配器一致性 fixture | Modify |

**关键不变量**：
- 顶层 box 链无容器大小字段 → 解析器靠「空窗口=EOF」判终止（依赖 Task 1 的驱动守卫）。
- `iloc` 给的是**绝对文件偏移**（method 0）→ 用 `Demand::SeekTo(绝对偏移)`，升序排列保证多数为前向 seek。
- 解析器只在 `window.len() < need` 时返回 `NeedBytes(need)`（窗口即 `buf[cursor..]`，故 `avail==window.len()`，绝不触发驱动的零前进守卫）。

---

### Task 1: `StreamDriver` 空窗口守卫

顶层 BMFF box 链没有「总大小」字段（不像 RIFF）。解析器靠「窗口为空 = 文件结束」来干净收尾。但当前 `StreamDriver` 在一个 `Skip` 恰好在 box 边界处用尽缓冲时，会用**空窗口**回调解析器——此时若解析器返回 `Done` 会在 push 流式下漏掉边界后才到达的 `meta`。本任务让驱动在未到 EOF 时绝不用空窗口回调解析器，而是请求更多字节。

**Files:**
- Modify: `omni-meta-core/src/driver.rs`（`StreamDriver::drive`，DoS 上界检查与「2) 拉解析器」之间）
- Test: `omni-meta-core/src/driver.rs`（`mod tests`）

- [ ] **Step 1: 写失败测试**

在 `omni-meta-core/src/driver.rs` 的 `mod tests` 内（`AlwaysSeekZero` 定义之后）追加一个「两盒」解析器与测试。它模拟 BMFF 顶层走盒：窗口为空→`Done`；窗口 `1..4` 字节→`NeedBytes(4)`；窗口 `≥4` 且尚未跳过首盒→`Skip(4)`；跳过后读 4 字节发一个 `Width` 再期待下一盒。

```rust
    /// 模拟无容器大小的顶层走盒：先 Skip(4) 跳首盒，再在第二盒读 4 字节发 Width，
    /// 空窗口视为 EOF→Done。用于验证驱动不会在边界处用空窗口提前结束。
    struct TwoBoxParser {
        skipped: bool,
        emitted: bool,
    }
    impl MetaParser for TwoBoxParser {
        fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
            if input.is_empty() {
                return PullResult { demand: Demand::Done, consumed: 0, events: Vec::new() };
            }
            if !self.skipped {
                if input.len() < 4 {
                    return PullResult { demand: Demand::NeedBytes(4), consumed: 0, events: Vec::new() };
                }
                self.skipped = true;
                return PullResult { demand: Demand::Skip(4), consumed: 0, events: Vec::new() };
            }
            if !self.emitted {
                if input.len() < 4 {
                    return PullResult { demand: Demand::NeedBytes(4), consumed: 0, events: Vec::new() };
                }
                self.emitted = true;
                let events = vec![Event::Field(Field::Width(7))];
                return PullResult { demand: Demand::Done, consumed: 4, events };
            }
            PullResult { demand: Demand::Done, consumed: 0, events: Vec::new() }
        }
    }

    #[test]
    fn stream_does_not_finish_early_on_boundary_empty_window() {
        // 首盒 4 字节 + 次盒 4 字节，逐字节喂入。
        // 关键：当首盒 Skip 恰好用尽缓冲（边界空窗口）时，驱动必须等待更多字节，
        // 而非用空窗口回调解析器导致 TwoBoxParser 提前 Done、漏掉次盒的 Width。
        let bytes = [0u8; 8];
        let chunks: Vec<&[u8]> = bytes.chunks(1).collect();
        let col = run_stream(&chunks, alloc::boxed::Box::new(TwoBoxParser { skipped: false, emitted: false }));
        assert_eq!(col.width, Some(7), "次盒的 Width 必须被读到（未被边界空窗口提前结束）");
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
    }
```

> `Collector.width` 是私有字段，但测试在同一 crate 的 `driver.rs` 内，可直接访问。

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p omni-meta-core stream_does_not_finish_early_on_boundary_empty_window`
Expected: FAIL —— 边界处空窗口回调使 `TwoBoxParser` 返回 `Done`，`col.width` 为 `None`（断言失败）。

- [ ] **Step 3: 加守卫**

在 `omni-meta-core/src/driver.rs` 的 `StreamDriver::drive` 中，紧接 DoS 上界检查块（以 `self.buf.len() - self.cursor > self.max_retained` 开头的 `if`）之后、`// 2) 拉解析器` 注释之前，插入：

```rust
            // 空窗口且未到 EOF：不要用空窗口回调解析器（顶层 box 链无大小字段，
            // 解析器靠"空窗口=EOF"判终止；流式下边界处的空窗口不是 EOF）。请求更多字节。
            if self.buf.len() == self.cursor && !self.eof {
                return Outcome::Need(1);
            }
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core stream_does_not_finish_early_on_boundary_empty_window`
Expected: PASS。

- [ ] **Step 5: 回归既有驱动测试**

Run: `cargo test -p omni-meta-core driver`
Expected: 全部 PASS（JPEG 流式逐字节、Skip/Seek 等不受影响）。

- [ ] **Step 6: 提交**

```bash
git add omni-meta-core/src/driver.rs
git commit -m "fix: StreamDriver 空窗口不回调解析器(除非 EOF) — 顶层 box 链干净终止"
```

---

### Task 2: `isobmff.rs` —— FullBox 头 / 可变位宽整数 / 子盒迭代器

**Files:**
- Modify: `omni-meta-core/src/containers/isobmff.rs`（顶部加 `use`；`read_box_header` 之后加三个 API + 测试）

- [ ] **Step 1: 写失败测试**

在 `omni-meta-core/src/containers/isobmff.rs` 的 `mod tests` 内追加：

```rust
    #[test]
    fn full_box_vf_reads_version_flags() {
        assert_eq!(full_box_vf(&[2, 0, 0, 5]), Some((2, 5)));
        assert_eq!(full_box_vf(&[0, 0, 0]), None); // 不足 4 字节
    }

    #[test]
    fn read_uint_be_widths() {
        let buf = [0x00, 0x00, 0x00, 0x09, 0xAA];
        let mut c = crate::cursor::ByteCursor::new(&buf);
        assert_eq!(read_uint_be(&mut c, 0), Some(0)); // 不消费
        assert_eq!(read_uint_be(&mut c, 4), Some(9)); // 消费 4
        assert_eq!(read_uint_be(&mut c, 3), None);    // 非法位宽
        let big = [0, 0, 0, 0, 0, 0, 0, 7u8];
        let mut c2 = crate::cursor::ByteCursor::new(&big);
        assert_eq!(read_uint_be(&mut c2, 8), Some(7));
    }

    #[test]
    fn iter_child_boxes_walks_siblings_and_stops_on_overrun() {
        // 两个子盒：free(8) + ftyp(载荷 4)
        let mut buf = alloc::vec::Vec::new();
        buf.extend_from_slice(&8u32.to_be_bytes());
        buf.extend_from_slice(b"free");
        buf.extend_from_slice(&12u32.to_be_bytes());
        buf.extend_from_slice(b"ftyp");
        buf.extend_from_slice(&[1, 2, 3, 4]);
        let got: alloc::vec::Vec<([u8; 4], usize)> =
            iter_child_boxes(&buf).map(|(h, p)| (h.kind, p.len())).collect();
        assert_eq!(got, alloc::vec![(*b"free", 0usize), (*b"ftyp", 4usize)]);

        // 声明长度越界 → 停止（不产出残缺项）
        let mut bad = alloc::vec::Vec::new();
        bad.extend_from_slice(&99u32.to_be_bytes()); // 声明 99 > 实际
        bad.extend_from_slice(b"mdat");
        assert_eq!(iter_child_boxes(&bad).count(), 0);
    }
```

- [ ] **Step 2: 运行测试，确认编译失败**

Run: `cargo test -p omni-meta-core isobmff`
Expected: 编译错误 —— `full_box_vf`/`read_uint_be`/`iter_child_boxes` 未定义。

- [ ] **Step 3: 实现三个 API**

在 `omni-meta-core/src/containers/isobmff.rs` 顶部（文件首行 `//!` 注释之后）加入：

```rust
use crate::cursor::{ByteCursor, Endian};
```

在 `read_box_header` 函数之后（`#[cfg(test)]` 之前）加入：

```rust
/// 读 FullBox 的 version(1) + flags(3)。`payload` 为 box 头之后的字节。
/// 不足 4 字节返回 None。
pub fn full_box_vf(payload: &[u8]) -> Option<(u8, u32)> {
    if payload.len() < 4 {
        return None;
    }
    Some((payload[0], u32::from_be_bytes([0, payload[1], payload[2], payload[3]])))
}

/// 从游标读大端无符号整数，size ∈ {0,4,8}（ISO-BMFF 可变位宽字段）。
/// size==0 → Some(0) 且不消费；其它非法位宽或越界 → None（越界时游标不前进）。
pub fn read_uint_be(cur: &mut ByteCursor, size: u8) -> Option<u64> {
    match size {
        0 => Some(0),
        4 => cur.u32(Endian::Big).map(u64::from),
        8 => {
            let s = cur.take(8)?;
            Some(u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
        }
        _ => None,
    }
}

/// 遍历 `payload` 内连续子 box。字节不足 / size0 / 声明长度小于头部或越界 → 停止
/// （不产出残缺项）。每项产出 (头, 该 box 载荷切片)。
pub struct ChildBoxes<'a> {
    rest: &'a [u8],
}

/// 在一段载荷上构造子盒迭代器。
pub fn iter_child_boxes(payload: &[u8]) -> ChildBoxes<'_> {
    ChildBoxes { rest: payload }
}

impl<'a> Iterator for ChildBoxes<'a> {
    type Item = (BoxHeader, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let hdr = read_box_header(self.rest)?;
        let total = usize::try_from(hdr.total_size?).ok()?;
        let header_len = hdr.header_len as usize;
        if total < header_len || total > self.rest.len() {
            return None;
        }
        let payload = &self.rest[header_len..total];
        self.rest = &self.rest[total..];
        Some((hdr, payload))
    }
}
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core isobmff`
Expected: 全部 PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/containers/isobmff.rs
git commit -m "feat: isobmff 增加 full_box_vf / read_uint_be / iter_child_boxes (A2/A3 共用)"
```

---

### Task 3: `bmff.rs` —— `infe` / `iinf` 解析（item 类型表）

本任务起在 `formats/bmff.rs` 内构建解析辅助。先做 item 信息：从 `iinf` 抽出我们关心的 EXIF / XMP item 及其 ID。

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`（替换文件头的 `use`；在 `BmffParser` 之前加类型与解析函数；在 `mod tests` 加测试）

- [ ] **Step 1: 替换文件头 `use` 并加类型 + 解析函数 + 失败测试**

把 `omni-meta-core/src/formats/bmff.rs` 顶部的 `use` 区块（第 4–7 行，`use alloc::vec::Vec;` 到 `use crate::demand::{...};`）替换为：

```rust
use alloc::vec::Vec;

use crate::containers::isobmff::{full_box_vf, iter_child_boxes, read_box_header, read_uint_be};
use crate::cursor::{ByteCursor, Endian};
use crate::demand::{Demand, Event, MetaParser, PayloadKind, PullResult};
use crate::model::{Field, WarnKind, Warning};
```

在 `#[derive(Debug, Default)]\npub struct BmffParser {` 之前插入 item 模型与 `iinf`/`infe` 解析：

```rust
/// 我们关心的一个 item（EXIF 或 XMP）及其 ID。
struct Wanted {
    id: u32,
    kind: PayloadKind,
}

/// 取 null 终止字符串（到首个 0 字节为止，无 0 则取全部）。
fn take_cstr(b: &[u8]) -> &[u8] {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    &b[..end]
}

/// 解析一个 `infe`（ItemInfoEntry）载荷；仅识别 version 2/3（带 item_type）。
/// 返回我们关心的 item（Exif，或 content_type 为 application/rdf+xml 的 mime），否则 None。
fn parse_infe(payload: &[u8]) -> Option<Wanted> {
    let (version, _flags) = full_box_vf(payload)?;
    let mut cur = ByteCursor::new(payload);
    cur.seek(4)?; // 跳过 version/flags
    let id = match version {
        2 => u32::from(cur.u16(Endian::Big)?),
        3 => cur.u32(Endian::Big)?,
        _ => return None,
    };
    let _protection = cur.u16(Endian::Big)?;
    let item_type = cur.take(4)?;
    if item_type == b"Exif" {
        return Some(Wanted { id, kind: PayloadKind::Exif });
    }
    if item_type == b"mime" {
        // ItemInfoEntry v2/3：item_name(null 终止) 在 item_type 之后、content_type 之前。
        let rest = &payload[cur.position()..];
        let after_name = match rest.iter().position(|&c| c == 0) {
            Some(i) => i + 1,
            None => return None,
        };
        if take_cstr(&rest[after_name..]) == b"application/rdf+xml" {
            return Some(Wanted { id, kind: PayloadKind::Xmp });
        }
    }
    None
}

/// 解析 `iinf`（ItemInfoBox）载荷 → 我们关心的 item 列表。
fn parse_iinf(payload: &[u8]) -> Vec<Wanted> {
    let mut out = Vec::new();
    let (version, _flags) = match full_box_vf(payload) {
        Some(v) => v,
        None => return out,
    };
    let mut cur = ByteCursor::new(payload);
    if cur.seek(4).is_none() {
        return out;
    }
    let count = if version == 0 {
        match cur.u16(Endian::Big) {
            Some(c) => u32::from(c),
            None => return out,
        }
    } else {
        match cur.u32(Endian::Big) {
            Some(c) => c,
            None => return out,
        }
    };
    let entries = &payload[cur.position()..];
    let mut seen = 0u32;
    for (hdr, infe_payload) in iter_child_boxes(entries) {
        if seen >= count {
            break;
        }
        seen += 1;
        if &hdr.kind != b"infe" {
            continue;
        }
        if let Some(w) = parse_infe(infe_payload) {
            out.push(w);
        }
    }
    out
}
```

在 `mod tests` 内追加（沿用本文件 `Vec` 已 `use`）：

```rust
    fn box_bytes(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
        b.extend_from_slice(kind);
        b.extend_from_slice(payload);
        b
    }

    fn infe(id: u16, typ: &[u8; 4], content_type: Option<&[u8]>) -> Vec<u8> {
        let mut p = alloc::vec![2u8, 0, 0, 0]; // version 2, flags 0
        p.extend_from_slice(&id.to_be_bytes());
        p.extend_from_slice(&0u16.to_be_bytes()); // protection index
        p.extend_from_slice(typ);
        p.push(0); // item_name = "" (spec 要求 v2/3 存在)
        if let Some(ct) = content_type {
            p.extend_from_slice(ct);
            p.push(0);
        }
        box_bytes(b"infe", &p)
    }

    #[test]
    fn parse_iinf_picks_exif_and_xmp() {
        let mut p = alloc::vec![0u8, 0, 0, 0]; // version 0, flags 0
        p.extend_from_slice(&3u16.to_be_bytes()); // count
        p.extend_from_slice(&infe(1, b"Exif", None));
        p.extend_from_slice(&infe(2, b"mime", Some(b"application/rdf+xml")));
        p.extend_from_slice(&infe(3, b"hvc1", None)); // 图像数据，忽略
        let wanted = parse_iinf(&p);
        assert_eq!(wanted.len(), 2);
        assert_eq!(wanted[0].id, 1);
        assert_eq!(wanted[0].kind, PayloadKind::Exif);
        assert_eq!(wanted[1].id, 2);
        assert_eq!(wanted[1].kind, PayloadKind::Xmp);
    }

    #[test]
    fn parse_iinf_ignores_non_rdf_mime() {
        let mut p = alloc::vec![0u8, 0, 0, 0];
        p.extend_from_slice(&1u16.to_be_bytes());
        p.extend_from_slice(&infe(1, b"mime", Some(b"text/plain")));
        assert!(parse_iinf(&p).is_empty());
    }
```

- [ ] **Step 2: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core -- bmff::tests::parse_iinf`
Expected: 两个测试 PASS。

> 此时 `BmffParser` 仍是 A1 行为，但新增的 `read_box_header`/`read_uint_be`/部分类型暂未被状态机使用——可能触发 `dead_code` 警告。**Task 7 会接入全部**，在此之前不跑 `-D warnings`。

- [ ] **Step 3: 提交**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat: bmff 解析 iinf/infe → EXIF/XMP item 类型表"
```

---

### Task 4: `bmff.rs` —— `iloc` 解析（item 位置表）

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`（在 `parse_iinf` 之后加 `Loc` + `parse_iloc`；`mod tests` 加测试）

- [ ] **Step 1: 写解析函数 + 失败测试**

在 `omni-meta-core/src/formats/bmff.rs` 的 `parse_iinf` 之后插入：

```rust
/// 一条 item 定位（仅保留首个 extent；多 extent 在装配时按警告跳过）。
struct Loc {
    id: u32,
    method: u8,
    extent_count: u16,
    /// 首个 extent：(偏移, 长度)。method 0 为绝对文件偏移；method 1 为 idat 内相对偏移。
    first_extent: Option<(u64, u64)>,
}

/// 解析 `iloc`（ItemLocationBox）载荷 → 各 item 定位。
fn parse_iloc(payload: &[u8]) -> Vec<Loc> {
    let mut out = Vec::new();
    let (version, _flags) = match full_box_vf(payload) {
        Some(v) => v,
        None => return out,
    };
    let mut cur = ByteCursor::new(payload);
    if cur.seek(4).is_none() {
        return out;
    }
    let sizes = match cur.u8() {
        Some(b) => b,
        None => return out,
    };
    let offset_size = sizes >> 4;
    let length_size = sizes & 0x0F;
    let sizes2 = match cur.u8() {
        Some(b) => b,
        None => return out,
    };
    let base_offset_size = sizes2 >> 4;
    let index_size = sizes2 & 0x0F; // 仅 version 1/2 使用
    let item_count = if version < 2 {
        match cur.u16(Endian::Big) {
            Some(c) => u32::from(c),
            None => return out,
        }
    } else {
        match cur.u32(Endian::Big) {
            Some(c) => c,
            None => return out,
        }
    };
    for _ in 0..item_count {
        let id = if version < 2 {
            match cur.u16(Endian::Big) {
                Some(v) => u32::from(v),
                None => break,
            }
        } else {
            match cur.u32(Endian::Big) {
                Some(v) => v,
                None => break,
            }
        };
        let method = if version == 1 || version == 2 {
            match cur.u16(Endian::Big) {
                Some(v) => (v & 0x0F) as u8,
                None => break,
            }
        } else {
            0
        };
        if cur.u16(Endian::Big).is_none() {
            break; // data_reference_index
        }
        let base_offset = match read_uint_be(&mut cur, base_offset_size) {
            Some(v) => v,
            None => break,
        };
        let extent_count = match cur.u16(Endian::Big) {
            Some(v) => v,
            None => break,
        };
        let mut first_extent = None;
        let mut ok = true;
        for i in 0..extent_count {
            if (version == 1 || version == 2) && index_size > 0 && read_uint_be(&mut cur, index_size).is_none() {
                ok = false;
                break;
            }
            let eo = match read_uint_be(&mut cur, offset_size) {
                Some(v) => v,
                None => {
                    ok = false;
                    break;
                }
            };
            let el = match read_uint_be(&mut cur, length_size) {
                Some(v) => v,
                None => {
                    ok = false;
                    break;
                }
            };
            if i == 0 {
                first_extent = Some((base_offset.saturating_add(eo), el));
            }
        }
        if !ok {
            break;
        }
        out.push(Loc { id, method, extent_count, first_extent });
    }
    out
}
```

在 `mod tests` 内追加：

```rust
    #[test]
    fn parse_iloc_v0_method0_single_extent() {
        // version 0：offset_size=4,length_size=4,base_offset_size=0,index_size=0
        let mut p = alloc::vec![0u8, 0, 0, 0]; // vf
        p.push(0x44); // offset4 | length4
        p.push(0x00); // base0 | index0
        p.extend_from_slice(&1u16.to_be_bytes()); // item_count
        p.extend_from_slice(&1u16.to_be_bytes()); // item_id=1
        p.extend_from_slice(&0u16.to_be_bytes()); // data_reference_index
        p.extend_from_slice(&1u16.to_be_bytes()); // extent_count
        p.extend_from_slice(&1000u32.to_be_bytes()); // extent_offset
        p.extend_from_slice(&42u32.to_be_bytes()); // extent_length
        let locs = parse_iloc(&p);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].id, 1);
        assert_eq!(locs[0].method, 0);
        assert_eq!(locs[0].extent_count, 1);
        assert_eq!(locs[0].first_extent, Some((1000, 42)));
    }

    #[test]
    fn parse_iloc_v1_method1_idat() {
        // version 1：带 construction_method 字段；method=1（idat）
        let mut p = alloc::vec![1u8, 0, 0, 0]; // vf, version 1
        p.push(0x44); // offset4 | length4
        p.push(0x00); // base0 | index0
        p.extend_from_slice(&1u16.to_be_bytes()); // item_count
        p.extend_from_slice(&5u16.to_be_bytes()); // item_id=5
        p.extend_from_slice(&1u16.to_be_bytes()); // construction_method=1
        p.extend_from_slice(&0u16.to_be_bytes()); // data_reference_index
        p.extend_from_slice(&1u16.to_be_bytes()); // extent_count
        p.extend_from_slice(&0u32.to_be_bytes()); // extent_offset (idat 内)
        p.extend_from_slice(&8u32.to_be_bytes()); // extent_length
        let locs = parse_iloc(&p);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].id, 5);
        assert_eq!(locs[0].method, 1);
        assert_eq!(locs[0].first_extent, Some((0, 8)));
    }
```

- [ ] **Step 2: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core -- bmff::tests::parse_iloc`
Expected: 两个测试 PASS。

- [ ] **Step 3: 提交**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat: bmff 解析 iloc → item 定位 (version 0/1, method 字段, 可变位宽)"
```

---

### Task 5: `bmff.rs` —— 维度（`pitm` / `ispe` / `iprp`+`ipma`）

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`（在 `parse_iloc` 之后加维度解析；`mod tests` 加测试）

- [ ] **Step 1: 写解析函数 + 失败测试**

在 `omni-meta-core/src/formats/bmff.rs` 的 `parse_iloc` 之后插入：

```rust
/// 解析 `pitm`（PrimaryItemBox）→ 主 item ID。
fn parse_pitm(payload: &[u8]) -> Option<u32> {
    let (version, _flags) = full_box_vf(payload)?;
    let mut cur = ByteCursor::new(payload);
    cur.seek(4)?;
    if version == 0 {
        cur.u16(Endian::Big).map(u32::from)
    } else {
        cur.u32(Endian::Big)
    }
}

/// 解析 `ispe`（ImageSpatialExtentsProperty）→ (width, height)。
fn parse_ispe(payload: &[u8]) -> Option<(u32, u32)> {
    let _vf = full_box_vf(payload)?;
    let mut cur = ByteCursor::new(payload);
    cur.seek(4)?;
    let w = cur.u32(Endian::Big)?;
    let h = cur.u32(Endian::Big)?;
    Some((w, h))
}

/// 从 `ipma` 关联中找主 item 的 ispe 维度。`props` 为 ipco 子盒按序的 ispe 维度（非 ispe 为 None）。
fn dims_via_ipma(payload: &[u8], primary: u32, props: &[Option<(u32, u32)>]) -> Option<(u32, u32)> {
    let (version, flags) = full_box_vf(payload)?;
    let mut cur = ByteCursor::new(payload);
    cur.seek(4)?;
    let entry_count = cur.u32(Endian::Big)?;
    let wide_index = (flags & 1) == 1;
    for _ in 0..entry_count {
        let item_id = if version < 1 {
            u32::from(cur.u16(Endian::Big)?)
        } else {
            cur.u32(Endian::Big)?
        };
        let assoc_count = cur.u8()?;
        for _ in 0..assoc_count {
            let idx = if wide_index {
                (cur.u16(Endian::Big)? & 0x7FFF) as usize
            } else {
                (cur.u8()? & 0x7F) as usize
            };
            if item_id == primary && idx >= 1 {
                if let Some(Some(dims)) = props.get(idx - 1) {
                    return Some(*dims);
                }
            }
        }
    }
    None
}

/// 解析 `iprp`（ItemPropertiesBox）→ 主 item 维度。
/// 优先 pitm+ipma 关联；兜底：ipco 内恰好一个 ispe 时直接用。
fn dims_from_iprp(iprp_payload: &[u8], primary: Option<u32>) -> Option<(u32, u32)> {
    let mut ipco_payload: Option<&[u8]> = None;
    let mut ipma_payload: Option<&[u8]> = None;
    for (hdr, p) in iter_child_boxes(iprp_payload) {
        match &hdr.kind {
            b"ipco" => ipco_payload = Some(p),
            b"ipma" => ipma_payload = Some(p),
            _ => {}
        }
    }
    let ipco = ipco_payload?;
    let mut props: Vec<Option<(u32, u32)>> = Vec::new();
    for (hdr, p) in iter_child_boxes(ipco) {
        props.push(if &hdr.kind == b"ispe" { parse_ispe(p) } else { None });
    }
    if let (Some(ipma), Some(pid)) = (ipma_payload, primary) {
        if let Some(dims) = dims_via_ipma(ipma, pid, &props) {
            return Some(dims);
        }
    }
    // 兜底：恰好一个 ispe
    let mut found = None;
    let mut n = 0u32;
    for d in props.iter().flatten() {
        found = Some(*d);
        n += 1;
    }
    if n == 1 {
        found
    } else {
        None
    }
}
```

在 `mod tests` 内追加：

```rust
    fn ispe(w: u32, h: u32) -> Vec<u8> {
        let mut p = alloc::vec![0u8, 0, 0, 0];
        p.extend_from_slice(&w.to_be_bytes());
        p.extend_from_slice(&h.to_be_bytes());
        box_bytes(b"ispe", &p)
    }

    #[test]
    fn parse_pitm_and_ispe() {
        let mut pitm_p = alloc::vec![0u8, 0, 0, 0];
        pitm_p.extend_from_slice(&7u16.to_be_bytes());
        assert_eq!(parse_pitm(&box_bytes(b"pitm", &pitm_p)[8..]), Some(7));
        assert_eq!(parse_ispe(&ispe(4032, 3024)[8..]), Some((4032, 3024)));
    }

    #[test]
    fn dims_from_iprp_via_ipma() {
        // ipco: [ispe 4032x3024]；ipma: item 1 → property #1
        let ipco = box_bytes(b"ipco", &ispe(4032, 3024));
        let mut ipma_p = alloc::vec![0u8, 0, 0, 0];
        ipma_p.extend_from_slice(&1u32.to_be_bytes()); // entry_count
        ipma_p.extend_from_slice(&1u16.to_be_bytes()); // item_id=1
        ipma_p.push(1); // assoc_count
        ipma_p.push(1); // 属性序号 1（essential bit 0）
        let ipma = box_bytes(b"ipma", &ipma_p);
        let mut iprp_p = Vec::new();
        iprp_p.extend_from_slice(&ipco);
        iprp_p.extend_from_slice(&ipma);
        assert_eq!(dims_from_iprp(&box_bytes(b"iprp", &iprp_p)[8..], Some(1)), Some((4032, 3024)));
    }

    #[test]
    fn dims_from_iprp_single_ispe_fallback() {
        // 无 ipma 关联，但 ipco 仅一个 ispe → 兜底直接用
        let ipco = box_bytes(b"ipco", &ispe(640, 480));
        assert_eq!(dims_from_iprp(&box_bytes(b"iprp", &ipco)[8..], None), Some((640, 480)));
    }
```

- [ ] **Step 2: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core -- bmff::tests::parse_pitm bmff::tests::dims_from_iprp`
Expected: 三个测试 PASS。

- [ ] **Step 3: 提交**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat: bmff 解析 pitm/ispe/ipma → 主 item 维度 (含单 ispe 兜底)"
```

---

### Task 6: `bmff.rs` —— `parse_meta` 装配 + EXIF 前缀剥离

把前面各解析器组装成「抽取计划」：维度、idat 内联载荷、method-0 目标、警告。

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`（在 `dims_from_iprp` 之后加 `Target`/`MetaPlan`/`strip_exif_prefix`/`parse_meta`；`mod tests` 加测试）

- [ ] **Step 1: 写装配函数 + 失败测试**

在 `omni-meta-core/src/formats/bmff.rs` 的 `dims_from_iprp` 之后插入：

```rust
/// 一个 method-0 抽取目标（数据在文件别处，需 SeekTo）。
#[derive(Clone, Copy)]
struct Target {
    offset: u64,
    length: u64,
    kind: PayloadKind,
    strip_exif: bool,
}

/// meta 解析产物。
struct MetaPlan<'a> {
    dims: Option<(u32, u32)>,
    /// method-1（idat 内联）载荷：已切片、EXIF 已剥前缀。
    inline: Vec<(PayloadKind, &'a [u8])>,
    /// method-0 目标，按 offset 升序。
    targets: Vec<Target>,
    warnings: Vec<Warning>,
}

/// Exif item 数据 = 4 字节 BE tiff_header_offset N，TIFF 自 4+N 起；容错 "Exif\0\0"。
fn strip_exif_prefix(d: &[u8]) -> &[u8] {
    if d.len() < 4 {
        return d;
    }
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) as usize;
    let start = 4usize.saturating_add(n);
    let rest = d.get(start..).unwrap_or(&[]);
    rest.strip_prefix(b"Exif\0\0").unwrap_or(rest)
}

/// 解析 meta box 载荷（meta 自身是 FullBox）。`meta_abs_base` 为 meta box 在文件中的绝对起点
/// （仅用于警告偏移）。
fn parse_meta(meta_payload: &[u8], meta_abs_base: u64) -> MetaPlan<'_> {
    let mut plan = MetaPlan { dims: None, inline: Vec::new(), targets: Vec::new(), warnings: Vec::new() };
    if full_box_vf(meta_payload).is_none() {
        return plan;
    }
    let children = &meta_payload[4..];
    let mut items: Vec<Wanted> = Vec::new();
    let mut locs: Vec<Loc> = Vec::new();
    let mut idat: Option<&[u8]> = None;
    let mut primary: Option<u32> = None;
    let mut iprp: Option<&[u8]> = None;
    for (hdr, p) in iter_child_boxes(children) {
        match &hdr.kind {
            b"iinf" => items = parse_iinf(p),
            b"iloc" => locs = parse_iloc(p),
            b"idat" => idat = Some(p),
            b"pitm" => primary = parse_pitm(p),
            b"iprp" => iprp = Some(p),
            _ => {}
        }
    }
    if let Some(iprp) = iprp {
        plan.dims = dims_from_iprp(iprp, primary);
    }
    for w in &items {
        let loc = match locs.iter().find(|l| l.id == w.id) {
            Some(l) => l,
            None => continue,
        };
        if loc.extent_count != 1 {
            // 多 extent（需拼接）暂不支持
            plan.warnings.push(Warning { offset: meta_abs_base, kind: WarnKind::UnreachableSection });
            continue;
        }
        let (off, len) = match loc.first_extent {
            Some(e) => e,
            None => continue,
        };
        match loc.method {
            0 => plan.targets.push(Target {
                offset: off,
                length: len,
                kind: w.kind,
                strip_exif: w.kind == PayloadKind::Exif,
            }),
            1 => {
                let data = idat.and_then(|d| {
                    let start = usize::try_from(off).ok()?;
                    let end = start.checked_add(usize::try_from(len).ok()?)?;
                    d.get(start..end)
                });
                match data {
                    Some(d) => {
                        let payload = if w.kind == PayloadKind::Exif { strip_exif_prefix(d) } else { d };
                        plan.inline.push((w.kind, payload));
                    }
                    None => plan
                        .warnings
                        .push(Warning { offset: meta_abs_base, kind: WarnKind::UnreachableSection }),
                }
            }
            _ => plan
                .warnings
                .push(Warning { offset: meta_abs_base, kind: WarnKind::UnreachableSection }),
        }
    }
    plan.targets.sort_by_key(|t| t.offset);
    plan
}
```

在 `mod tests` 内追加：

```rust
    #[test]
    fn strip_exif_prefix_zero_offset() {
        let mut d = alloc::vec![0u8, 0, 0, 0]; // tiff_header_offset = 0
        d.extend_from_slice(b"II*\0rest");
        assert_eq!(strip_exif_prefix(&d), b"II*\0rest");
    }

    #[test]
    fn strip_exif_prefix_tolerates_exif_marker() {
        let mut d = alloc::vec![0u8, 0, 0, 0];
        d.extend_from_slice(b"Exif\0\0MM\0*");
        assert_eq!(strip_exif_prefix(&d), b"MM\0*");
    }

    #[test]
    fn parse_meta_method2_warns_and_skips() {
        // iinf: Exif item id=1；iloc version1 method=2（item 间接引用，不支持）
        let mut iinf_p = alloc::vec![0u8, 0, 0, 0];
        iinf_p.extend_from_slice(&1u16.to_be_bytes());
        iinf_p.extend_from_slice(&infe(1, b"Exif", None));
        let iinf = box_bytes(b"iinf", &iinf_p);
        let mut iloc_p = alloc::vec![1u8, 0, 0, 0]; // version 1
        iloc_p.push(0x44);
        iloc_p.push(0x00);
        iloc_p.extend_from_slice(&1u16.to_be_bytes()); // item_count
        iloc_p.extend_from_slice(&1u16.to_be_bytes()); // id=1
        iloc_p.extend_from_slice(&2u16.to_be_bytes()); // construction_method=2
        iloc_p.extend_from_slice(&0u16.to_be_bytes()); // dri
        iloc_p.extend_from_slice(&1u16.to_be_bytes()); // extent_count
        iloc_p.extend_from_slice(&0u32.to_be_bytes()); // offset
        iloc_p.extend_from_slice(&4u32.to_be_bytes()); // length
        let iloc = box_bytes(b"iloc", &iloc_p);
        let mut meta_p = alloc::vec![0u8, 0, 0, 0]; // meta vf
        meta_p.extend_from_slice(&iinf);
        meta_p.extend_from_slice(&iloc);
        let plan = parse_meta(&meta_p, 0);
        assert!(plan.targets.is_empty());
        assert!(plan.inline.is_empty());
        assert_eq!(plan.warnings.len(), 1);
        assert_eq!(plan.warnings[0].kind, WarnKind::UnreachableSection);
    }
```

- [ ] **Step 2: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core -- bmff::tests::strip_exif bmff::tests::parse_meta`
Expected: 三个测试 PASS。

- [ ] **Step 3: 提交**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat: bmff parse_meta 装配抽取计划 (维度/idat内联/method0目标/警告)"
```

---

### Task 7: `bmff.rs` —— 两阶段状态机（Walk + Extract）

把 `BmffParser` 从 A1 的「校验首盒即 Done」升级为顶层走盒找 meta、再按目标 `SeekTo` 抽取。

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`（替换 `BmffParser` 结构与 `impl MetaParser`；改写 A1 的几个单测）

- [ ] **Step 1: 替换 `BmffParser` 与 `impl MetaParser`**

把 `omni-meta-core/src/formats/bmff.rs` 中现有的 `#[derive(Debug, Default)]\npub struct BmffParser { done: bool }`、`impl BmffParser { ... }`、以及整个 `impl MetaParser for BmffParser { ... }`（到 A1 注释结束）整体替换为：

```rust
#[derive(Debug, Default)]
pub struct BmffParser {
    done: bool,
    /// Walk 阶段已走过的绝对偏移（当前待读 box 的起点），仅用于警告偏移保真。
    pos: u64,
    /// 是否已解析完 meta、进入 Extract 阶段。
    extracting: bool,
    /// Extract 阶段当前目标下标。
    idx: usize,
    /// method-0 目标，按 offset 升序。
    targets: Vec<Target>,
}

impl BmffParser {
    pub fn new() -> Self {
        Self::default()
    }
}

/// 读首个 box 头所需字节：size==1（largesize）需 16，否则 8。
fn needed_header_bytes(input: &[u8]) -> usize {
    if input.len() >= 4 && u32::from_be_bytes([input[0], input[1], input[2], input[3]]) == 1 {
        16
    } else {
        8
    }
}

impl MetaParser for BmffParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        if self.done {
            return PullResult { demand: Demand::Done, consumed: 0, events: Vec::new() };
        }
        if self.extracting {
            return self.pull_extract(input);
        }
        self.pull_walk(input)
    }
}

impl BmffParser {
    /// 顶层走盒：跳过非 meta box，命中 meta 后整盒入窗并解析。
    /// 空窗口（由驱动保证仅在 EOF 出现）= 干净结束。
    fn pull_walk<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        if input.is_empty() {
            self.done = true;
            return PullResult { demand: Demand::Done, consumed: 0, events: Vec::new() };
        }
        let hdr = match read_box_header(input) {
            Some(h) => h,
            None => {
                return PullResult {
                    demand: Demand::NeedBytes(needed_header_bytes(input)),
                    consumed: 0,
                    events: Vec::new(),
                };
            }
        };
        if &hdr.kind == b"meta" {
            let total = match hdr.total_size {
                Some(t) => t,
                None => {
                    // size0 meta（延伸至 EOF）：本里程碑不处理，干净结束。
                    self.done = true;
                    return PullResult { demand: Demand::Done, consumed: 0, events: Vec::new() };
                }
            };
            let need = match usize::try_from(total) {
                Ok(n) => n,
                Err(_) => {
                    self.done = true;
                    return PullResult { demand: Demand::Done, consumed: 0, events: Vec::new() };
                }
            };
            let header_len = hdr.header_len as usize;
            if need < header_len {
                // 畸形 meta：声明大小小于其自身头部 → 干净结束，绝不 panic。
                self.done = true;
                return PullResult { demand: Demand::Done, consumed: 0, events: Vec::new() };
            }
            if input.len() < need {
                return PullResult { demand: Demand::NeedBytes(need), consumed: 0, events: Vec::new() };
            }
            let plan = parse_meta(&input[header_len..need], self.pos);
            let mut events: Vec<Event<'a>> = Vec::new();
            if let Some((w, h)) = plan.dims {
                events.push(Event::Field(Field::Width(w)));
                events.push(Event::Field(Field::Height(h)));
            }
            for (kind, data) in plan.inline {
                events.push(Event::Payload { kind, data });
            }
            for warn in plan.warnings {
                events.push(Event::Warning(warn));
            }
            self.targets = plan.targets;
            if self.targets.is_empty() {
                self.done = true;
                return PullResult { demand: Demand::Done, consumed: need, events };
            }
            self.extracting = true;
            self.idx = 0;
            let first = self.targets[0].offset;
            return PullResult { demand: Demand::SeekTo(first), consumed: need, events };
        }
        // 非 meta：跳过整盒。size0 / 畸形（payload_len None）→ 不可能再有 meta，干净结束。
        match hdr.payload_len() {
            Some(pl) => {
                self.pos = self.pos.saturating_add(hdr.header_len).saturating_add(pl);
                PullResult { demand: Demand::Skip(pl), consumed: hdr.header_len as usize, events: Vec::new() }
            }
            None => {
                self.done = true;
                PullResult { demand: Demand::Done, consumed: 0, events: Vec::new() }
            }
        }
    }

    /// Extract 阶段：窗口起点 = 当前目标的绝对偏移（驱动已 SeekTo）。
    fn pull_extract<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        let t = self.targets[self.idx];
        let len = match usize::try_from(t.length) {
            Ok(l) => l,
            Err(_) => {
                self.done = true;
                return PullResult { demand: Demand::Done, consumed: 0, events: Vec::new() };
            }
        };
        if input.len() < len {
            return PullResult { demand: Demand::NeedBytes(len), consumed: 0, events: Vec::new() };
        }
        let raw = &input[..len];
        let data = if t.strip_exif { strip_exif_prefix(raw) } else { raw };
        let events: Vec<Event<'a>> = alloc::vec![Event::Payload { kind: t.kind, data }];
        self.idx += 1;
        if self.idx >= self.targets.len() {
            self.done = true;
            return PullResult { demand: Demand::Done, consumed: len, events };
        }
        let next = self.targets[self.idx].offset;
        PullResult { demand: Demand::SeekTo(next), consumed: len, events }
    }
}
```

- [ ] **Step 2: 改写 A1 单测以匹配走盒语义**

把 `omni-meta-core/src/formats/bmff.rs` 的 `mod tests` 中 A1 留下的 `ftyp_then_done_no_events` 与 `second_pull_after_done_stays_done` 两个测试**替换**为（`short_input_needs_bytes`、`largesize_partial_header_needs_16` 保留不动）：

```rust
    #[test]
    fn walk_skips_non_meta_box() {
        // 单次 pull：首盒 ftyp 非 meta → Skip(载荷=12)，消费头部 8。
        let buf = ftyp_box();
        let mut p = BmffParser::new();
        let res = p.pull(&buf);
        assert_eq!(res.demand, Demand::Skip(12));
        assert_eq!(res.consumed, 8);
        assert!(res.events.is_empty());
    }

    #[test]
    fn walk_empty_window_is_clean_done() {
        // 空窗口（驱动保证仅 EOF 出现）→ 干净 Done、无事件。
        let mut p = BmffParser::new();
        let res = p.pull(&[]);
        assert_eq!(res.demand, Demand::Done);
        assert!(res.events.is_empty());
    }

    #[test]
    fn drive_slice_lone_ftyp_is_clean() {
        // 仅 ftyp（无 meta）经 drive_slice 应干净收尾、无警告、无产物。
        let buf = ftyp_box();
        let col = crate::driver::drive_slice(&buf, &mut BmffParser::new(), crate::limits::Limits::default());
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
        assert!(col.exif.is_empty());
        assert!(col.xmp.is_empty());
    }
```

- [ ] **Step 3: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core bmff`
Expected: 全部 PASS（含前几个 Task 的 parse_* 测试 + 本任务的走盒测试）。

- [ ] **Step 4: 提交**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat: BmffParser 两阶段状态机 (Walk 找 meta + Extract SeekTo 抽取)"
```

---

### Task 8: `bmff.rs` —— 端到端单测（meta+mdat 与 idat）

用合成 HEIC 跑 `drive_slice`，验证 EXIF/XMP/维度齐活，并覆盖 idat（method 1）路径。

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`（`mod tests` 加端到端测试与 fixture 构造）

- [ ] **Step 1: 写端到端测试 + fixture**

在 `omni-meta-core/src/formats/bmff.rs` 的 `mod tests` 内追加：

```rust
    /// 最小 TIFF：II + 42 + IFD0(Make=Acme)。与 driver/webp 测试同款。
    fn make_tiff() -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(b"II");
        t.extend_from_slice(&42u16.to_le_bytes());
        t.extend_from_slice(&8u32.to_le_bytes());
        t.extend_from_slice(&1u16.to_le_bytes()); // 1 entry
        t.extend_from_slice(&0x010Fu16.to_le_bytes()); // Make
        t.extend_from_slice(&2u16.to_le_bytes()); // ASCII
        t.extend_from_slice(&5u32.to_le_bytes()); // count
        t.extend_from_slice(&26u32.to_le_bytes()); // 值偏移
        t.extend_from_slice(&0u32.to_le_bytes()); // next IFD
        t.extend_from_slice(b"Acme\0");
        t
    }

    fn ftyp_heic() -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(b"heic");
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(b"mif1");
        box_bytes(b"ftyp", &p)
    }

    /// 构造 meta box（Exif=item1, xmp=item2, ispe 关联 item1）。method 0 时偏移为绝对值。
    fn build_meta_method0(exif_off: u64, exif_len: u64, xmp_off: u64, xmp_len: u64) -> Vec<u8> {
        let mut pitm_p = alloc::vec![0u8, 0, 0, 0];
        pitm_p.extend_from_slice(&1u16.to_be_bytes());
        let pitm = box_bytes(b"pitm", &pitm_p);

        let mut iinf_p = alloc::vec![0u8, 0, 0, 0];
        iinf_p.extend_from_slice(&2u16.to_be_bytes());
        iinf_p.extend_from_slice(&infe(1, b"Exif", None));
        iinf_p.extend_from_slice(&infe(2, b"mime", Some(b"application/rdf+xml")));
        let iinf = box_bytes(b"iinf", &iinf_p);

        let ipco = box_bytes(b"ipco", &ispe(4032, 3024));
        let mut ipma_p = alloc::vec![0u8, 0, 0, 0];
        ipma_p.extend_from_slice(&1u32.to_be_bytes());
        ipma_p.extend_from_slice(&1u16.to_be_bytes());
        ipma_p.push(1);
        ipma_p.push(1);
        let ipma = box_bytes(b"ipma", &ipma_p);
        let mut iprp_p = Vec::new();
        iprp_p.extend_from_slice(&ipco);
        iprp_p.extend_from_slice(&ipma);
        let iprp = box_bytes(b"iprp", &iprp_p);

        let mut iloc_p = alloc::vec![0u8, 0, 0, 0]; // version 0 → method 0
        iloc_p.push(0x44);
        iloc_p.push(0x00);
        iloc_p.extend_from_slice(&2u16.to_be_bytes());
        for (id, off, len) in [(1u16, exif_off, exif_len), (2u16, xmp_off, xmp_len)] {
            iloc_p.extend_from_slice(&id.to_be_bytes());
            iloc_p.extend_from_slice(&0u16.to_be_bytes()); // dri
            iloc_p.extend_from_slice(&1u16.to_be_bytes()); // extent_count
            iloc_p.extend_from_slice(&(off as u32).to_be_bytes());
            iloc_p.extend_from_slice(&(len as u32).to_be_bytes());
        }
        let iloc = box_bytes(b"iloc", &iloc_p);

        let mut meta_p = alloc::vec![0u8, 0, 0, 0];
        meta_p.extend_from_slice(&pitm);
        meta_p.extend_from_slice(&iinf);
        meta_p.extend_from_slice(&iprp);
        meta_p.extend_from_slice(&iloc);
        box_bytes(b"meta", &meta_p)
    }

    fn exif_item_block() -> Vec<u8> {
        let mut b = alloc::vec![0u8, 0, 0, 0]; // tiff_header_offset = 0
        b.extend_from_slice(&make_tiff());
        b
    }

    /// 完整 HEIC：ftyp + meta + mdat(exif, xmp)。两遍：先测 meta 长度，再算绝对偏移。
    fn heic_method0() -> Vec<u8> {
        let exif = exif_item_block();
        let xmp = br#"<rdf:Description tiff:Make="Acme"/>"#.to_vec();
        let ftyp = ftyp_heic();
        let meta_probe = build_meta_method0(0, exif.len() as u64, 0, xmp.len() as u64);
        let base = ftyp.len() as u64 + meta_probe.len() as u64 + 8; // mdat 头 8 字节
        let exif_off = base;
        let xmp_off = base + exif.len() as u64;
        let meta = build_meta_method0(exif_off, exif.len() as u64, xmp_off, xmp.len() as u64);
        assert_eq!(meta.len(), meta_probe.len(), "两遍 meta 长度必须一致");
        let mut mdat_payload = Vec::new();
        mdat_payload.extend_from_slice(&exif);
        mdat_payload.extend_from_slice(&xmp);
        let mdat = box_bytes(b"mdat", &mdat_payload);
        let mut f = Vec::new();
        f.extend_from_slice(&ftyp);
        f.extend_from_slice(&meta);
        f.extend_from_slice(&mdat);
        f
    }

    #[test]
    fn end_to_end_heic_method0() {
        let buf = heic_method0();
        let col = crate::driver::drive_slice(&buf, &mut BmffParser::new(), crate::limits::Limits::default());
        // 经 finalize 投影为 Metadata（与 webp.rs 测试同款；维度走 unified 公有字段）。
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Heif);
        assert!(meta.warnings.is_empty(), "warnings: {:?}", meta.warnings);
        assert_eq!(meta.unified.width, Some(4032));
        assert_eq!(meta.unified.height, Some(3024));
        assert!(meta.raw.exif.iter().any(|t| t.tag == 0x010F), "应抽到 Make 标签");
        assert!(meta.raw.xmp.iter().any(|x| x.name == "Make" && x.value == "Acme"));
        assert_eq!(meta.unified.camera_make.as_deref(), Some("Acme"),
            "unified.camera_make 须经 normalize 从 EXIF IFD0 Make 投影");
    }

    #[test]
    fn end_to_end_heic_idat_method1() {
        // meta 内嵌 idat：Exif item 数据放 idat，construction_method=1。
        let exif = exif_item_block();
        let pitm = {
            let mut p = alloc::vec![0u8, 0, 0, 0];
            p.extend_from_slice(&1u16.to_be_bytes());
            box_bytes(b"pitm", &p)
        };
        let mut iinf_p = alloc::vec![0u8, 0, 0, 0];
        iinf_p.extend_from_slice(&1u16.to_be_bytes());
        iinf_p.extend_from_slice(&infe(1, b"Exif", None));
        let iinf = box_bytes(b"iinf", &iinf_p);
        let idat = box_bytes(b"idat", &exif);
        let mut iloc_p = alloc::vec![1u8, 0, 0, 0]; // version 1（带 method）
        iloc_p.push(0x44);
        iloc_p.push(0x00);
        iloc_p.extend_from_slice(&1u16.to_be_bytes()); // item_count
        iloc_p.extend_from_slice(&1u16.to_be_bytes()); // id=1
        iloc_p.extend_from_slice(&1u16.to_be_bytes()); // method=1
        iloc_p.extend_from_slice(&0u16.to_be_bytes()); // dri
        iloc_p.extend_from_slice(&1u16.to_be_bytes()); // extent_count
        iloc_p.extend_from_slice(&0u32.to_be_bytes()); // idat 内偏移 0
        iloc_p.extend_from_slice(&(exif.len() as u32).to_be_bytes()); // 长度
        let iloc = box_bytes(b"iloc", &iloc_p);
        let mut meta_p = alloc::vec![0u8, 0, 0, 0];
        meta_p.extend_from_slice(&pitm);
        meta_p.extend_from_slice(&iinf);
        meta_p.extend_from_slice(&idat);
        meta_p.extend_from_slice(&iloc);
        let meta = box_bytes(b"meta", &meta_p);
        let mut f = ftyp_heic();
        f.extend_from_slice(&meta);
        let col = crate::driver::drive_slice(&f, &mut BmffParser::new(), crate::limits::Limits::default());
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Heif);
        assert!(meta.warnings.is_empty(), "warnings: {:?}", meta.warnings);
        assert!(meta.raw.exif.iter().any(|t| t.tag == 0x010F), "idat 内联 Exif 应被抽到");
        assert_eq!(meta.unified.camera_make.as_deref(), Some("Acme"),
            "idat 路径 EXIF 同样须经 normalize 投影至 unified");
    }
```

- [ ] **Step 2: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core -- bmff::tests::end_to_end`
Expected: 两个端到端测试 PASS。若 `end_to_end_heic_method0` 的偏移断言失败，先确认 `heic_method0` 的两遍 `assert_eq!(meta.len(), ...)` 未 panic（位宽固定，长度应一致）。

- [ ] **Step 3: 提交**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "test: bmff 端到端 (method0 mdat + method1 idat) EXIF/XMP/维度"
```

---

### Task 9: 四适配器差分（完整 HEIC）

把 A1 的 `differential_bmff_heic` fixture 升级为含 `meta`+`mdat` 的完整 HEIC，验证 slice/blocking/seek/push 对 `SeekTo` 抽取逐字段一致。

**Files:**
- Modify: `omni-meta/tests/differential.rs`（替换 `fixture_bmff_heic`，新增完整 HEIC fixture 与测试）

- [ ] **Step 1: 替换/新增 fixture 与测试**

把 `omni-meta/tests/differential.rs` 末尾的 `fixture_bmff_heic` 与 `differential_bmff_heic`（约 445–463 行）**替换**为以下内容（沿用文件内已有的 `make_tiff`）：

```rust
fn bmff_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    b.extend_from_slice(kind);
    b.extend_from_slice(payload);
    b
}

fn bmff_infe(id: u16, typ: &[u8; 4], content_type: Option<&[u8]>) -> Vec<u8> {
    let mut p = vec![2u8, 0, 0, 0];
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes());
    p.extend_from_slice(typ);
    p.push(0); // item_name = "" (spec 要求 v2/3 存在)
    if let Some(ct) = content_type {
        p.extend_from_slice(ct);
        p.push(0);
    }
    bmff_box(b"infe", &p)
}

fn bmff_ispe(w: u32, h: u32) -> Vec<u8> {
    let mut p = vec![0u8, 0, 0, 0];
    p.extend_from_slice(&w.to_be_bytes());
    p.extend_from_slice(&h.to_be_bytes());
    bmff_box(b"ispe", &p)
}

fn bmff_meta(exif_off: u64, exif_len: u64, xmp_off: u64, xmp_len: u64) -> Vec<u8> {
    let mut pitm_p = vec![0u8, 0, 0, 0];
    pitm_p.extend_from_slice(&1u16.to_be_bytes());
    let pitm = bmff_box(b"pitm", &pitm_p);

    let mut iinf_p = vec![0u8, 0, 0, 0];
    iinf_p.extend_from_slice(&2u16.to_be_bytes());
    iinf_p.extend_from_slice(&bmff_infe(1, b"Exif", None));
    iinf_p.extend_from_slice(&bmff_infe(2, b"mime", Some(b"application/rdf+xml")));
    let iinf = bmff_box(b"iinf", &iinf_p);

    let ipco = bmff_box(b"ipco", &bmff_ispe(4032, 3024));
    let mut ipma_p = vec![0u8, 0, 0, 0];
    ipma_p.extend_from_slice(&1u32.to_be_bytes());
    ipma_p.extend_from_slice(&1u16.to_be_bytes());
    ipma_p.push(1);
    ipma_p.push(1);
    let ipma = bmff_box(b"ipma", &ipma_p);
    let mut iprp_p = Vec::new();
    iprp_p.extend_from_slice(&ipco);
    iprp_p.extend_from_slice(&ipma);
    let iprp = bmff_box(b"iprp", &iprp_p);

    let mut iloc_p = vec![0u8, 0, 0, 0];
    iloc_p.push(0x44);
    iloc_p.push(0x00);
    iloc_p.extend_from_slice(&2u16.to_be_bytes());
    for (id, off, len) in [(1u16, exif_off, exif_len), (2u16, xmp_off, xmp_len)] {
        iloc_p.extend_from_slice(&id.to_be_bytes());
        iloc_p.extend_from_slice(&0u16.to_be_bytes());
        iloc_p.extend_from_slice(&1u16.to_be_bytes());
        iloc_p.extend_from_slice(&(off as u32).to_be_bytes());
        iloc_p.extend_from_slice(&(len as u32).to_be_bytes());
    }
    let iloc = bmff_box(b"iloc", &iloc_p);

    let mut meta_p = vec![0u8, 0, 0, 0];
    meta_p.extend_from_slice(&pitm);
    meta_p.extend_from_slice(&iinf);
    meta_p.extend_from_slice(&iprp);
    meta_p.extend_from_slice(&iloc);
    bmff_box(b"meta", &meta_p)
}

/// 完整 HEIC：ftyp + meta + mdat(exif, xmp)，method 0 绝对偏移指向 mdat。
fn fixture_bmff_heic() -> Vec<u8> {
    let mut exif = vec![0u8, 0, 0, 0]; // tiff_header_offset = 0
    exif.extend_from_slice(&make_tiff());
    let xmp = br#"<rdf:Description tiff:Make="Acme"/>"#.to_vec();

    let mut ftyp_p = Vec::new();
    ftyp_p.extend_from_slice(b"heic");
    ftyp_p.extend_from_slice(&0u32.to_be_bytes());
    ftyp_p.extend_from_slice(b"mif1");
    let ftyp = bmff_box(b"ftyp", &ftyp_p);

    let meta_probe = bmff_meta(0, exif.len() as u64, 0, xmp.len() as u64);
    let base = ftyp.len() as u64 + meta_probe.len() as u64 + 8;
    let meta = bmff_meta(base, exif.len() as u64, base + exif.len() as u64, xmp.len() as u64);
    assert_eq!(meta.len(), meta_probe.len());

    let mut mdat_payload = Vec::new();
    mdat_payload.extend_from_slice(&exif);
    mdat_payload.extend_from_slice(&xmp);
    let mdat = bmff_box(b"mdat", &mdat_payload);

    let mut f = Vec::new();
    f.extend_from_slice(&ftyp);
    f.extend_from_slice(&meta);
    f.extend_from_slice(&mdat);
    f
}

#[test]
fn differential_bmff_heic() {
    // 四适配器对 SeekTo 抽取（meta 在前、数据在 mdat）逐字段一致。
    assert_all_equal(&fixture_bmff_heic());
}
```

- [ ] **Step 2: 运行差分测试，确认通过**

Run: `cargo test -p omni-meta --test differential differential_bmff_heic`
Expected: PASS —— slice/blocking/seek/push（含 chunk=1,3,7）四路一致：format=Heif、width=4032、height=3024、camera_make=Acme、xmp Make=Acme。

> push chunk=1 是关键回归点：它会在 ftyp 跳过后的边界处产生空窗口，验证 Task 1 的驱动守卫使 `meta` 不被漏读。

- [ ] **Step 3: 提交**

```bash
git add omni-meta/tests/differential.rs
git commit -m "test: BMFF(heic) 完整 meta+mdat 四适配器差分 (SeekTo 抽取一致性)"
```

---

### Task 10: 全量验证（no_std + clippy + 测试）

**Files:** 无（仅验证 + 收尾提交，如有 lint 修整）

- [ ] **Step 1: 跑全量测试**

Run: `cargo test`
Expected: 工作区全部 PASS（既有 JPEG/PNG/WebP/GIF/EXIF/XMP/A1 + 本计划新增）。

- [ ] **Step 2: 验证 no_std 构建未被破坏**

Run: `cargo build -p omni-meta-core --no-default-features`
Expected: 干净编译（BMFF/容器代码只依赖 `alloc` + core）。

- [ ] **Step 3: clippy 清零**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 无告警。可能项：`Loc.extent_count` 等字段若仅部分使用 → 加 `#[allow(dead_code)]` 或调整；`dims_from_iprp` 兜底循环可换 `iterator` 写法；`Target` 派生。按提示就地修整，**不改变逻辑**。

- [ ] **Step 4: 若 Step 3 有修整则提交**

```bash
git add -A
git commit -m "style: A2 BMFF clippy 清整"
```

（无修整则跳过本步。）

---

## 完成定义（A2）

- HEIF/AVIF 文件从 `meta` 抽出 EXIF（→ camera_make 等）与 XMP（→ raw），并经 ispe 拿到 `width`/`height`。
- `iloc` construction_method 0（mdat 绝对偏移）/ 1（idat 内联）均可定位；method 2 / 越界 / 截断均产出恰当警告、绝不 panic。
- 四适配器对完整 HEIC（meta 在前、数据在 mdat、需 `SeekTo`）逐字段一致，含 push 逐字节。
- `no_std` 构建与 clippy 均干净。
- Unified 仅 `width`/`height` 变动；`created`/`duration` 未引入。

## 不在本计划范围（→ A3）

- MP4/MOV `moov` / `mvhd` / `tkhd` → 维度 / `duration` / `created`；新 Unified 字段。
- `iloc` 多 extent 拼接、construction_method 2（item 间接引用）。
- `grid` / 派生 item 拼接维度。
