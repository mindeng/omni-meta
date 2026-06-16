# 里程碑 C：EBML 容器（MKV/WebM）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `EbmlParser` 从 MKV/WebM 的 EBML 元素树抽取 `width`/`height`/`duration_ms`/`created`，为 `duration_ms` 补齐第二格式来源。

**Architecture:** 镜像 BMFF 的 `containers`（结构层 vint/元素遍历）/`formats`（语义层 EbmlParser 状态机）分层。单阶段前向走盒：跳过 EBML 头与不关心元素、下钻 `Segment`（不缓冲）、整元素缓冲并解析 `Info`/`Tracks`、遇未知大小媒体即干净停止。容器字段经 `Field` 事件入 `Collector`，`normalize.rs` 零改动。

**Tech Stack:** Rust，`#![forbid(unsafe_code)]`，no_std（`alloc`），零依赖，sans-io `MetaParser` 契约。

设计依据：[`docs/superpowers/specs/2026-06-16-omni-meta-ebml-design.md`](../specs/2026-06-16-omni-meta-ebml-design.md)

**全局约定**
- 工作目录 `/home/min/dev/omni-meta`，分支 `milestone-c-ebml`。
- 单测跑 `cargo test -p omni-meta-core`；差分跑 `cargo test -p omni-meta`。
- 不变量：显式迭代非递归、全程 `checked_*`/`get`、越界返回 `None`/警告不 panic、缺失即 `None`。

---

## Task 1：提取共享 `civil` 模块（DRY，为两纪元复用）

**Files:**
- Create: `omni-meta-core/src/civil.rs`
- Modify: `omni-meta-core/src/lib.rs`（加 `mod civil;`）
- Modify: `omni-meta-core/src/formats/bmff.rs`（删本地 `civil_from_days` 与其测试，改调共享）

- [ ] **Step 1: 创建 `civil.rs`（含从 bmff 迁移来的算法与测试向量）**

`omni-meta-core/src/civil.rs`：
```rust
//! 民用历法：自 1970-01-01 起的天数 → (year, month, day)。
//! Howard Hinnant `civil_from_days` 算法，纯整数、no_std 安全、无浮点。
//! BMFF（1904 纪元）与 EBML（2001 纪元）共用此换算。

/// 自 1970-01-01 起的天数 → (year, month, day)。负天数表示 1970 之前。
pub(crate) fn civil_from_days(days: i64) -> (u16, u8, u8) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8; // [1, 12]
    let year = (y + if m <= 2 { 1 } else { 0 }) as u16;
    (year, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_known_vectors() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(31), (1970, 2, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(59), (1970, 3, 1)); // 1970 非闰
    }
}
```

- [ ] **Step 2: 在 `lib.rs` 注册模块**

`omni-meta-core/src/lib.rs`，在 `pub(crate) mod cursor;` 行附近加入（与现有 mod 同区块）：
```rust
pub(crate) mod civil;
```

- [ ] **Step 3: 改 `bmff.rs` 调用共享、删除本地副本**

在 `omni-meta-core/src/formats/bmff.rs` 中删除本地 `civil_from_days` 定义（连同其上方两行文档注释）：
```rust
/// 民用历法：自 1970-01-01 起的天数 → (year, month, day)。
/// Howard Hinnant `civil_from_days` 算法，纯整数、no_std 安全。
fn civil_from_days(days: i64) -> (u16, u8, u8) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8; // [1, 12]
    let year = (y + if m <= 2 { 1 } else { 0 }) as u16;
    (year, m, d)
}
```

在 `datetime_from_mp4_epoch` 中把 `let (year, month, day) = civil_from_days(days);` 改为：
```rust
    let (year, month, day) = crate::civil::civil_from_days(days);
```

删除 bmff `mod tests` 中现已迁移走的测试（避免重复 + 引用已删函数）：
```rust
    #[test]
    fn civil_from_days_known_vectors() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(31), (1970, 2, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(59), (1970, 3, 1)); // 1970 非闰
    }
```

- [ ] **Step 4: 跑测试验证 bmff 行为不变 + civil 新测试通过**

Run: `cargo test -p omni-meta-core civil`（civil 测试）与 `cargo test -p omni-meta-core bmff`（bmff 全绿）
Expected: PASS；尤其 `datetime_from_mp4_epoch_anchor`、`parse_mvhd_v0_duration_and_created` 仍通过。

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/civil.rs omni-meta-core/src/lib.rs omni-meta-core/src/formats/bmff.rs
git commit -m "refactor(civil): 提取 civil_from_days 为共享模块，BMFF 改调（C 前置）"
```

---

## Task 2：EBML vint 原语（元素 ID / size 解码）

**Files:**
- Create: `omni-meta-core/src/containers/ebml.rs`
- Modify: `omni-meta-core/src/containers/mod.rs`

- [ ] **Step 1: 注册模块**

`omni-meta-core/src/containers/mod.rs` 追加：
```rust
pub mod ebml;
```

- [ ] **Step 2: 写失败测试（先建文件骨架 + 测试）**

`omni-meta-core/src/containers/ebml.rs`：
```rust
//! EBML（Matroska/WebM）结构层：变长整数（vint）与元素遍历。
//! 全程大端。边界安全：字节不足返回 None，绝不 panic、不分配、不前进。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elem_id_widths() {
        assert_eq!(read_elem_id(&[0xB0]), Some((0xB0, 1)));            // PixelWidth
        assert_eq!(read_elem_id(&[0x42, 0x82]), Some((0x4282, 2)));   // DocType
        assert_eq!(read_elem_id(&[0x2A, 0xD7, 0xB1]), Some((0x2AD7B1, 3))); // TimestampScale
        assert_eq!(read_elem_id(&[0x1A, 0x45, 0xDF, 0xA3]), Some((0x1A45DFA3, 4))); // EBML
        assert_eq!(read_elem_id(&[0x00]), None); // 长度 > 8 非法
        assert_eq!(read_elem_id(&[0x08, 0, 0, 0, 0]), None); // 长度 5 > 4 上限
        assert_eq!(read_elem_id(&[]), None);
    }

    #[test]
    fn elem_size_known_unknown_truncated() {
        // 单字节 size：0x81 → 值 1
        assert_eq!(read_elem_size(&[0x81]), Some((Some(1), 1)));
        // 双字节 size：0x40 0x05 → 值 5
        assert_eq!(read_elem_size(&[0x40, 0x05]), Some((Some(5), 2)));
        // 单字节未知大小：0xFF（数据位全 1）→ None size
        assert_eq!(read_elem_size(&[0xFF]), Some((None, 1)));
        // 八字节未知大小：0x01 + 7×0xFF → None size
        assert_eq!(read_elem_size(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]), Some((None, 8)));
        // 八字节定长：0x01 + 7 字节 = 256
        assert_eq!(read_elem_size(&[0x01, 0, 0, 0, 0, 0, 1, 0]), Some((Some(256), 8)));
        // 截断：声明 2 字节但只给 1
        assert_eq!(read_elem_size(&[0x40]), None);
        assert_eq!(read_elem_size(&[0x00]), None); // 长度 > 8
        assert_eq!(read_elem_size(&[]), None);
    }
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p omni-meta-core ebml::tests::elem_id_widths`
Expected: FAIL（`read_elem_id` 未定义，编译错误）。

- [ ] **Step 4: 实现 vint 解码**

在 `ebml.rs` 顶部（`#[cfg(test)]` 之前）加入：
```rust
/// 读 EBML 元素 ID（**保留**标记位，ID 即规范值）。长度 1–4。
/// 首字节为 0（长度 >8）或长度 >4 → None。
pub fn read_elem_id(input: &[u8]) -> Option<(u32, usize)> {
    let first = *input.first()?;
    if first == 0 {
        return None; // 长度 > 8，非法
    }
    let len = first.leading_zeros() as usize + 1; // 1..=8
    if len > 4 {
        return None; // ID 长度上限 4（EBMLMaxIDLength 默认）
    }
    let bytes = input.get(..len)?;
    let mut id = 0u32;
    for &b in bytes {
        id = (id << 8) | u32::from(b);
    }
    Some((id, len))
}

/// 读 EBML 元素 size（**剥去**标记位取值）。长度 1–8。
/// 数据位全 1 → 未知大小（返回 `(None, len)`）。截断/长度 >8 → None。
pub fn read_elem_size(input: &[u8]) -> Option<(Option<u64>, usize)> {
    let first = *input.first()?;
    if first == 0 {
        return None; // 长度 > 8
    }
    let len = first.leading_zeros() as usize + 1; // 1..=8
    let bytes = input.get(..len)?;
    let mask = if len == 8 { 0u8 } else { 0xFFu8 >> len }; // 首字节数据位
    let mut val = u64::from(first & mask);
    for &b in &bytes[1..] {
        val = (val << 8) | u64::from(b);
    }
    let data_bits = 7 * len;
    let all_ones = if data_bits >= 64 {
        val == u64::MAX
    } else {
        val == (1u64 << data_bits) - 1
    };
    Some((if all_ones { None } else { Some(val) }, len))
}
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p omni-meta-core ebml::tests::elem_id_widths ebml::tests::elem_size_known_unknown_truncated`
Expected: PASS。

- [ ] **Step 6: Commit**

```bash
git add omni-meta-core/src/containers/ebml.rs omni-meta-core/src/containers/mod.rs
git commit -m "feat(ebml): vint 元素 ID/size 解码（保留/剥离标记位，未知大小）(C)"
```

---

## Task 3：EBML 元素头、子元素遍历、定长读数

**Files:**
- Modify: `omni-meta-core/src/containers/ebml.rs`

- [ ] **Step 1: 写失败测试**

在 `ebml.rs` 的 `mod tests` 中追加：
```rust
    fn elem(id: &[u8], payload: &[u8]) -> alloc::vec::Vec<u8> {
        // 用 8 字节 vint size 编码，便于构造
        let mut e = alloc::vec::Vec::new();
        e.extend_from_slice(id);
        e.push(0x01);
        e.extend_from_slice(&(payload.len() as u64).to_be_bytes()[1..]); // 低 7 字节
        e.extend_from_slice(payload);
        e
    }

    #[test]
    fn element_header_reads_id_size() {
        let e = elem(&[0xA3], &[1, 2, 3]); // 单字节 ID 0xA3
        let h = read_element_header(&e).unwrap();
        assert_eq!(h.id, 0xA3);
        assert_eq!(h.header_len, 9); // 1 id + 8 size
        assert_eq!(h.size, Some(3));
    }

    #[test]
    fn child_iter_walks_and_stops_on_overrun() {
        let mut buf = alloc::vec::Vec::new();
        buf.extend_from_slice(&elem(&[0xB0], &[0, 0, 5, 0])); // 子元素 A
        buf.extend_from_slice(&elem(&[0xBA], &[0, 0, 2, 0])); // 子元素 B
        let got: alloc::vec::Vec<(u32, usize)> =
            iter_child_elements(&buf).map(|(h, p)| (h.id, p.len())).collect();
        assert_eq!(got, alloc::vec![(0xB0u32, 4usize), (0xBA, 4)]);

        // 声明长度越界 → 停止，不产出残缺项
        let mut bad = alloc::vec::Vec::new();
        bad.extend_from_slice(&[0xB0, 0x01]);
        bad.extend_from_slice(&999u64.to_be_bytes()[1..]); // 声明 999 > 实际
        assert_eq!(iter_child_elements(&bad).count(), 0);
    }

    #[test]
    fn be_readers() {
        assert_eq!(read_uint(&[0x01, 0x00]), 256);
        assert_eq!(read_uint(&[]), 0);
        assert_eq!(read_int(&[0xFF]), -1);        // 符号扩展
        assert_eq!(read_int(&[0x00, 0x05]), 5);
        assert_eq!(read_int(&[]), 0);
        assert_eq!(read_float(&[]), Some(0.0));
        assert_eq!(read_float(&5000.0f64.to_be_bytes()), Some(5000.0));
        assert_eq!(read_float(&1.5f32.to_be_bytes()), Some(1.5));
        assert_eq!(read_float(&[1, 2, 3]), None); // 非法长度
    }

    #[test]
    fn needed_header_bytes_progresses() {
        // 仅 ID 首字节可见（4 字节 ID）→ 需 4 + size 首字节
        assert_eq!(needed_header_bytes(&[0x1A]), 5);
        // ID 全到 + size 首字节可见（8 字节 size）→ 需 4 + 8
        assert_eq!(needed_header_bytes(&[0x1A, 0x45, 0xDF, 0xA3, 0x01]), 12);
    }
```

注意：`ebml.rs` 顶部需 `extern crate alloc;`？否——`omni-meta-core` 已在 crate 根 `extern crate alloc;`，子模块直接用 `alloc::` 即可。测试内 `alloc::vec!`/`alloc::vec::Vec` 可用。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p omni-meta-core ebml::tests::element_header_reads_id_size`
Expected: FAIL（`read_element_header` 等未定义）。

- [ ] **Step 3: 实现元素头 / 遍历 / 读数**

在 `ebml.rs`（`read_elem_size` 之后、`#[cfg(test)]` 之前）加入：
```rust
/// 一个 EBML 元素头。`size` 为 None 表示未知大小（数据位全 1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElemHeader {
    pub id: u32,
    pub header_len: u64, // id 字节数 + size 字节数
    pub size: Option<u64>,
}

/// 读一个元素头（ID + size）。字节不足返回 None。
pub fn read_element_header(input: &[u8]) -> Option<ElemHeader> {
    let (id, idlen) = read_elem_id(input)?;
    let (size, szlen) = read_elem_size(input.get(idlen..)?)?;
    Some(ElemHeader { id, header_len: (idlen + szlen) as u64, size })
}

/// 在已见引导字节后，精确计算读出完整元素头所需字节数（供增量索取）。
pub fn needed_header_bytes(input: &[u8]) -> usize {
    let idlen = match input.first() {
        Some(&f) if f != 0 => ((f.leading_zeros() as usize) + 1).min(4),
        _ => return 2, // 首字节尚不可见：最小元素头 = 2
    };
    match input.get(idlen) {
        Some(&f) if f != 0 => idlen + (f.leading_zeros() as usize) + 1,
        _ => idlen + 1, // size 首字节尚不可见
    }
}

/// 大端读无符号整数（1–8 B；空 → 0）。
pub fn read_uint(b: &[u8]) -> u64 {
    let mut v = 0u64;
    for &x in b.iter().take(8) {
        v = (v << 8) | u64::from(x);
    }
    v
}

/// 大端读有符号整数（1–8 B，符号扩展；空 → 0）。
pub fn read_int(b: &[u8]) -> i64 {
    let n = b.len().min(8);
    if n == 0 {
        return 0;
    }
    let mut v = 0u64;
    for &x in &b[..n] {
        v = (v << 8) | u64::from(x);
    }
    let shift = 64 - 8 * n as u32;
    ((v << shift) as i64) >> shift
}

/// 大端读 IEEE 浮点（4 → f32、8 → f64、0 → 0.0、其它 → None）。
pub fn read_float(b: &[u8]) -> Option<f64> {
    match b.len() {
        0 => Some(0.0),
        4 => Some(f64::from(f32::from_be_bytes([b[0], b[1], b[2], b[3]]))),
        8 => Some(f64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])),
        _ => None,
    }
}

/// 遍历已缓冲定长载荷内的连续子元素。未知大小子元素 / 声明长度越界 → 停止
/// （不产出残缺项）。每项产出 (元素头, 子载荷切片)。
pub struct ChildElements<'a> {
    rest: &'a [u8],
}

/// 在一段载荷上构造子元素迭代器。
pub fn iter_child_elements(payload: &[u8]) -> ChildElements<'_> {
    ChildElements { rest: payload }
}

impl<'a> Iterator for ChildElements<'a> {
    type Item = (ElemHeader, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let hdr = read_element_header(self.rest)?;
        let size = usize::try_from(hdr.size?).ok()?; // 未知大小 → 停止
        let header_len = usize::try_from(hdr.header_len).ok()?;
        let total = header_len.checked_add(size)?;
        if total > self.rest.len() {
            return None; // 越界 → 停止
        }
        let payload = &self.rest[header_len..total];
        self.rest = &self.rest[total..];
        Some((hdr, payload))
    }
}
```

- [ ] **Step 4: 跑测试确认通过（全部 ebml 容器测试）**

Run: `cargo test -p omni-meta-core containers::ebml`
Expected: PASS（全部）。

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/containers/ebml.rs
git commit -m "feat(ebml): 元素头/子元素遍历/大端 uint·int·float 读数 (C)"
```

---

## Task 4：`FileFormat::Mkv`/`Webm` 变体

**Files:**
- Modify: `omni-meta-core/src/model.rs`

- [ ] **Step 1: 写失败测试**

在 `model.rs` 的 `mod tests` 追加：
```rust
    #[test]
    fn fileformat_has_ebml_family() {
        assert_ne!(FileFormat::Mkv, FileFormat::Webm);
        assert_ne!(FileFormat::Mkv, FileFormat::Unknown);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p omni-meta-core model::tests::fileformat_has_ebml_family`
Expected: FAIL（`FileFormat::Mkv` 未定义）。

- [ ] **Step 3: 加变体**

`model.rs` 的 `FileFormat` 枚举，在 `Mov,` 之后、`Unknown,` 之前插入：
```rust
    Mkv,
    Webm,
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p omni-meta-core model::tests::fileformat_has_ebml_family`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/model.rs
git commit -m "feat(model): FileFormat 增 Mkv/Webm 变体 (C)"
```

---

## Task 5：`EbmlParser` 骨架 + `probe` 接线（DocType→Mkv/Webm）

> 先建立**立即 `Done`** 的 `EbmlParser` 骨架，使 `parser_for` 能接线编译；语义在 Task 6–9 增量填充。

**Files:**
- Create: `omni-meta-core/src/formats/ebml.rs`
- Modify: `omni-meta-core/src/formats/mod.rs`
- Modify: `omni-meta-core/src/probe.rs`

- [ ] **Step 1: 注册 formats 模块 + 写 EbmlParser 骨架**

`omni-meta-core/src/formats/mod.rs` 追加：
```rust
pub mod ebml;
```

`omni-meta-core/src/formats/ebml.rs`（骨架）：
```rust
//! EBML（Matroska/WebM）顶层解析器。前向走盒：跳过 EBML 头与不关心元素、
//! 下钻 Segment（不缓冲）、整元素缓冲解析 Info/Tracks、遇未知大小媒体即干净停止。

use alloc::vec::Vec;

use crate::demand::{Demand, Event, MetaParser, PullResult};

#[derive(Debug, Default)]
pub struct EbmlParser {
    done: bool,
}

impl EbmlParser {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MetaParser for EbmlParser {
    fn pull<'a>(&mut self, _input: &'a [u8]) -> PullResult<'a> {
        self.done = true;
        PullResult { demand: Demand::Done, consumed: 0, events: Vec::<Event<'a>>::new() }
    }
}
```

- [ ] **Step 2: 写 probe 失败测试**

在 `probe.rs` 的 `mod tests` 追加：
```rust
    fn ebml(doctype: &[u8]) -> alloc::vec::Vec<u8> {
        // EBML 头 { DocType } —— 用 8 字节 vint size 编码
        let mut dt = alloc::vec::Vec::new();
        dt.extend_from_slice(&[0x42, 0x82, 0x01]);
        dt.extend_from_slice(&(doctype.len() as u64).to_be_bytes()[1..]);
        dt.extend_from_slice(doctype);
        let mut hdr = alloc::vec::Vec::new();
        hdr.extend_from_slice(&[0x1A, 0x45, 0xDF, 0xA3, 0x01]);
        hdr.extend_from_slice(&(dt.len() as u64).to_be_bytes()[1..]);
        hdr.extend_from_slice(&dt);
        hdr
    }

    #[test]
    fn detects_mkv_and_webm_via_doctype() {
        assert_eq!(probe(&ebml(b"webm")), FileFormat::Webm);
        assert_eq!(probe(&ebml(b"matroska")), FileFormat::Mkv);
        assert!(parser_for(FileFormat::Mkv).is_some());
        assert!(parser_for(FileFormat::Webm).is_some());
    }
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p omni-meta-core probe::tests::detects_mkv_and_webm_via_doctype`
Expected: FAIL（EBML 未识别 / `parser_for` 未接线）。

- [ ] **Step 4: 实现 probe（PROBE_MAX、DocType、parser_for）**

`probe.rs`：把 `PROBE_MAX` 常量改为 64（保留断言）：
```rust
/// 探测窗口上界：EBML DocType（区分 MKV/WebM）可能落在头部数十字节内。
pub(crate) const PROBE_MAX: usize = 64;
// 编译期断言：PROBE_MAX 必须覆盖最长签名（WebP = 12 字节）。
const _: () = assert!(PROBE_MAX >= 12);
```

在 `probe` 函数体内、`ftyp` 判断之后、`FileFormat::Unknown` 之前插入：
```rust
    // EBML（Matroska/WebM）：魔数 1A45DFA3 在偏移 0。
    if buf.len() >= 4 && buf[0..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        return ebml_format(buf);
    }
```

在 `brand_to_format` 之后加入两个辅助：
```rust
/// 魔数已匹配。在已缓冲头部内定位 DocType → Mkv/Webm；尚不可见且未达 PROBE_MAX
/// → Unknown（请求更多字节）；达 PROBE_MAX 仍无 → 默认 Mkv（给出确定答案）。
fn ebml_format(buf: &[u8]) -> FileFormat {
    if let Some(dt) = find_doctype(buf) {
        return if dt == b"webm" { FileFormat::Webm } else { FileFormat::Mkv };
    }
    if buf.len() >= PROBE_MAX {
        return FileFormat::Mkv;
    }
    FileFormat::Unknown
}

/// 在前 PROBE_MAX 字节内查找 DocType 元素（ID 0x42 0x82）并读取其字符串值。
/// 元素存在但字符串尚未完整缓冲 → None（继续等待）。
fn find_doctype(buf: &[u8]) -> Option<&[u8]> {
    let scan = &buf[..buf.len().min(PROBE_MAX)];
    let mut i = 0usize;
    while i + 1 < scan.len() {
        if scan[i] == 0x42 && scan[i + 1] == 0x82 {
            let rest = &scan[i + 2..];
            let (size, szlen) = crate::containers::ebml::read_elem_size(rest)?;
            let size = usize::try_from(size?).ok()?;
            let end = szlen.checked_add(size)?;
            return rest.get(szlen..end);
        }
        i += 1;
    }
    None
}
```

在 `parser_for` 的 `match fmt` 中，BMFF 分支之后加入：
```rust
        FileFormat::Mkv | FileFormat::Webm => {
            Some(Box::new(crate::formats::ebml::EbmlParser::new()))
        }
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p omni-meta-core probe`
Expected: PASS（新测试 + 既有 probe 测试均绿）。

- [ ] **Step 6: Commit**

```bash
git add omni-meta-core/src/formats/ebml.rs omni-meta-core/src/formats/mod.rs omni-meta-core/src/probe.rs
git commit -m "feat(ebml): EbmlParser 骨架 + probe DocType→Mkv/Webm 接线 (C)"
```

---

## Task 6：`datetime_from_matroska_epoch`（2001 纪元 → DateTimeParts）

**Files:**
- Modify: `omni-meta-core/src/formats/ebml.rs`

- [ ] **Step 1: 写失败测试**

在 `ebml.rs` 末尾加入测试模块（首个测试）：
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matroska_epoch_anchor_and_offsets() {
        // date_ns = 0 → 2001-01-01T00:00:00 UTC
        let dt = datetime_from_matroska_epoch(0);
        assert_eq!((dt.year, dt.month, dt.day), (2001, 1, 1));
        assert_eq!((dt.hour, dt.minute, dt.second), (0, 0, 0));
        assert_eq!(dt.tz_offset_min, Some(0));
        // +1 天
        let nd = datetime_from_matroska_epoch(86_400 * 1_000_000_000);
        assert_eq!((nd.year, nd.month, nd.day), (2001, 1, 2));
        // +01:01:01
        let tod = datetime_from_matroska_epoch(3_661 * 1_000_000_000);
        assert_eq!((tod.hour, tod.minute, tod.second), (1, 1, 1));
        // 负值（2000-12-31）
        let neg = datetime_from_matroska_epoch(-1 * 1_000_000_000);
        assert_eq!((neg.year, neg.month, neg.day), (2000, 12, 31));
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p omni-meta-core formats::ebml::tests::matroska_epoch_anchor_and_offsets`
Expected: FAIL（`datetime_from_matroska_epoch` 未定义）。

- [ ] **Step 3: 实现纪元换算 + 引入 model/容器 use**

在 `ebml.rs` 顶部 `use` 区，补充导入（与现有 `use` 合并）：
```rust
use crate::containers::ebml::{
    iter_child_elements, needed_header_bytes, read_element_header, read_float, read_int,
    read_uint, ElemHeader,
};
use crate::model::{DateTimeParts, Field, WarnKind, Warning};
```
> 说明：本任务仅用到 `DateTimeParts`；其余导入为 Task 7–9 预置，避免反复改 `use` 行。若中途 `cargo test`（非 `-D warnings`）出现 unused import 警告，属预期，Task 9 完成后清零；如需保持 `cargo build` 干净，可暂在本任务只引入 `DateTimeParts`，并在 Task 7/8/9 增量补齐对应 use。

在骨架的 `impl EbmlParser` 之前加入纪元常量与函数：
```rust
/// Matroska DateUTC 纪元（2001-01-01）相对 Unix 纪元（1970-01-01）的天数差。
const MATROSKA_EPOCH_DAYS_AFTER_UNIX: i64 = 11_323;

/// Matroska `DateUTC`（自 2001-01-01 00:00:00 UTC 的纳秒，有符号）→ DateTimeParts（UTC）。
fn datetime_from_matroska_epoch(date_ns: i64) -> DateTimeParts {
    let secs = date_ns.div_euclid(1_000_000_000);
    let days = secs.div_euclid(86_400) + MATROSKA_EPOCH_DAYS_AFTER_UNIX;
    let tod = secs.rem_euclid(86_400) as u32;
    let (year, month, day) = crate::civil::civil_from_days(days);
    DateTimeParts {
        year,
        month,
        day,
        hour: (tod / 3600) as u8,
        minute: ((tod % 3600) / 60) as u8,
        second: (tod % 60) as u8,
        tz_offset_min: Some(0),
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p omni-meta-core formats::ebml::tests::matroska_epoch_anchor_and_offsets`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/formats/ebml.rs
git commit -m "feat(ebml): DateUTC 2001 纪元 → DateTimeParts（含负值/UTC）(C)"
```

---

## Task 7：`parse_info`（Duration × TimestampScale → ms，DateUTC → created，守卫）

**Files:**
- Modify: `omni-meta-core/src/formats/ebml.rs`

- [ ] **Step 1: 写失败测试**

在 `ebml.rs` 的 `mod tests` 追加（并在测试模块内加构造辅助）：
```rust
    fn elem(id: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut e = Vec::new();
        e.extend_from_slice(id);
        e.push(0x01);
        e.extend_from_slice(&(payload.len() as u64).to_be_bytes()[1..]);
        e.extend_from_slice(payload);
        e
    }

    fn info_payload(scale: Option<u64>, duration: Option<f64>, date_ns: Option<i64>) -> Vec<u8> {
        let mut p = Vec::new();
        if let Some(s) = scale {
            p.extend_from_slice(&elem(&[0x2A, 0xD7, 0xB1], &s.to_be_bytes()));
        }
        if let Some(d) = duration {
            p.extend_from_slice(&elem(&[0x44, 0x89], &d.to_be_bytes()));
        }
        if let Some(n) = date_ns {
            p.extend_from_slice(&elem(&[0x44, 0x61], &n.to_be_bytes()));
        }
        p
    }

    #[test]
    fn parse_info_duration_default_scale() {
        // 默认 scale = 1_000_000 ns；duration 5000.0 → 5000 ms
        let info = parse_info(&info_payload(None, Some(5000.0), None));
        assert_eq!(info.duration_ms, Some(5000));
        assert!(!info.invalid);
    }

    #[test]
    fn parse_info_explicit_scale_and_f32_path() {
        // scale 1_000_000；duration 1500.0 → 1500 ms（用 f64 构造）
        let info = parse_info(&info_payload(Some(1_000_000), Some(1500.0), Some(0)));
        assert_eq!(info.duration_ms, Some(1500));
        assert_eq!(info.created.map(|d| d.year), Some(2001));
    }

    #[test]
    fn parse_info_invalid_duration_warns() {
        // 负 duration → 无 duration、invalid
        let neg = parse_info(&info_payload(Some(1_000_000), Some(-1.0), None));
        assert_eq!(neg.duration_ms, None);
        assert!(neg.invalid);
        // NaN → invalid
        let nan = parse_info(&info_payload(Some(1_000_000), Some(f64::NAN), None));
        assert!(nan.invalid);
        // scale == 0 → invalid
        let zero = parse_info(&info_payload(Some(0), Some(5000.0), None));
        assert_eq!(zero.duration_ms, None);
        assert!(zero.invalid);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p omni-meta-core formats::ebml::tests::parse_info_duration_default_scale`
Expected: FAIL（`parse_info` / `InfoData` 未定义）。

- [ ] **Step 3: 实现 parse_info**

在 `ebml.rs`（`datetime_from_matroska_epoch` 之后）加入元素 ID 常量与 `parse_info`：
```rust
// EBML / Matroska 元素 ID（保留标记位的规范值）。
const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_A966;
const TIMESTAMP_SCALE: u32 = 0x2AD7_B1; // 旧名 TimecodeScale，同 ID
const DURATION: u32 = 0x4489;
const DATE_UTC: u32 = 0x4461;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const VIDEO: u32 = 0xE0;
const PIXEL_WIDTH: u32 = 0xB0;
const PIXEL_HEIGHT: u32 = 0xBA;

/// `Info` 解析产物。`invalid` 标记 Duration 存在但不可用（非有限/负/scale=0/溢出）。
struct InfoData {
    duration_ms: Option<u64>,
    created: Option<DateTimeParts>,
    invalid: bool,
}

/// 解析 `Info` 载荷 → 时长 + 创建时间。
fn parse_info(payload: &[u8]) -> InfoData {
    let mut scale: Option<u64> = None;
    let mut duration_raw: Option<f64> = None;
    let mut date_ns: Option<i64> = None;
    for (hdr, p) in iter_child_elements(payload) {
        match hdr.id {
            TIMESTAMP_SCALE => scale = Some(read_uint(p)),
            DURATION => duration_raw = read_float(p),
            DATE_UTC => date_ns = Some(read_int(p)),
            _ => {}
        }
    }
    let mut out = InfoData { duration_ms: None, created: None, invalid: false };
    let scale = scale.unwrap_or(1_000_000);
    if let Some(d) = duration_raw {
        if scale == 0 || !d.is_finite() || d < 0.0 {
            out.invalid = true;
        } else {
            let ms = d * scale as f64 / 1_000_000.0;
            if ms < 0.0 || ms > u64::MAX as f64 {
                out.invalid = true;
            } else {
                out.duration_ms = Some(ms as u64);
            }
        }
    }
    if let Some(ns) = date_ns {
        out.created = Some(datetime_from_matroska_epoch(ns));
    }
    out
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p omni-meta-core formats::ebml::tests::parse_info`
Expected: PASS（三个 parse_info 测试）。

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/formats/ebml.rs
git commit -m "feat(ebml): parse_info 时长(Duration×TimestampScale)+created+守卫 (C)"
```

---

## Task 8：`parse_tracks`（首个视频轨 PixelWidth/Height）

**Files:**
- Modify: `omni-meta-core/src/formats/ebml.rs`

- [ ] **Step 1: 写失败测试**

在 `ebml.rs` 的 `mod tests` 追加：
```rust
    fn video_track(w: u32, h: u32) -> Vec<u8> {
        let mut vid = Vec::new();
        vid.extend_from_slice(&elem(&[0xB0], &w.to_be_bytes())); // PixelWidth
        vid.extend_from_slice(&elem(&[0xBA], &h.to_be_bytes())); // PixelHeight
        let video = elem(&[0xE0], &vid);
        elem(&[0xAE], &video) // TrackEntry { Video }
    }

    fn audio_track() -> Vec<u8> {
        // TrackEntry 无 Video 子元素（仅一个占位子元素 0x83 TrackType=2）
        let inner = elem(&[0x83], &[2]);
        elem(&[0xAE], &inner)
    }

    #[test]
    fn parse_tracks_picks_first_video() {
        let mut tracks = Vec::new();
        tracks.extend_from_slice(&audio_track());          // 音频轨在前
        tracks.extend_from_slice(&video_track(1280, 720)); // 视频轨
        assert_eq!(parse_tracks(&tracks), Some((1280, 720)));
    }

    #[test]
    fn parse_tracks_audio_only_is_none() {
        assert_eq!(parse_tracks(&audio_track()), None);
    }

    #[test]
    fn parse_tracks_zero_dims_is_none() {
        assert_eq!(parse_tracks(&video_track(0, 0)), None);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p omni-meta-core formats::ebml::tests::parse_tracks_picks_first_video`
Expected: FAIL（`parse_tracks` 未定义）。

- [ ] **Step 3: 实现 parse_tracks**

在 `ebml.rs`（`parse_info` 之后）加入：
```rust
/// 解析 `Tracks` 载荷 → 首个含非零 PixelWidth/Height 的视频轨维度。
fn parse_tracks(payload: &[u8]) -> Option<(u32, u32)> {
    for (hdr, p) in iter_child_elements(payload) {
        if hdr.id != TRACK_ENTRY {
            continue;
        }
        if let Some(dims) = track_entry_dims(p) {
            return Some(dims);
        }
    }
    None
}

/// 在一个 `TrackEntry` 内找 `Video` → (PixelWidth, PixelHeight)，任一为 0 / 缺失 → None。
fn track_entry_dims(payload: &[u8]) -> Option<(u32, u32)> {
    for (hdr, p) in iter_child_elements(payload) {
        if hdr.id != VIDEO {
            continue;
        }
        let mut w: Option<u32> = None;
        let mut h: Option<u32> = None;
        for (vh, vp) in iter_child_elements(p) {
            match vh.id {
                PIXEL_WIDTH => w = Some(read_uint(vp) as u32),
                PIXEL_HEIGHT => h = Some(read_uint(vp) as u32),
                _ => {}
            }
        }
        if let (Some(w), Some(h)) = (w, h)
            && w != 0
            && h != 0
        {
            return Some((w, h));
        }
    }
    None
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p omni-meta-core formats::ebml::tests::parse_tracks`
Expected: PASS（三个测试）。

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/formats/ebml.rs
git commit -m "feat(ebml): parse_tracks 首个视频轨 PixelWidth/Height (C)"
```

---

## Task 9：`EbmlParser` 状态机（替换骨架：走盒/下钻/缓冲/停止）

**Files:**
- Modify: `omni-meta-core/src/formats/ebml.rs`

- [ ] **Step 1: 写失败测试（单元 + 端到端 slice）**

在 `ebml.rs` 的 `mod tests` 追加构造辅助与测试：
```rust
    use crate::demand::PayloadKind; // 仅为保持 use 一致（可省略，若未用）

    fn doctype_header(doctype: &[u8]) -> Vec<u8> {
        let dt = elem(&[0x42, 0x82], doctype);
        elem(&[0x1A, 0x45, 0xDF, 0xA3], &dt)
    }

    fn segment(children: &[u8]) -> Vec<u8> {
        elem(&[0x18, 0x53, 0x80, 0x67], children)
    }

    /// 构造完整 MKV/WebM：EBML头 + Segment{ Info, Tracks, Cluster }。
    fn full_ebml(doctype: &[u8], w: u32, h: u32, dur: f64, date_ns: i64) -> Vec<u8> {
        let info = elem(&[0x15, 0x49, 0xA9, 0x66], &info_payload(Some(1_000_000), Some(dur), Some(date_ns)));
        let tracks = elem(&[0x16, 0x54, 0xAE, 0x6B], &video_track(w, h));
        let cluster = elem(&[0x1F, 0x43, 0xB6, 0x75], &[0u8; 8]);
        let mut seg_children = Vec::new();
        seg_children.extend_from_slice(&info);
        seg_children.extend_from_slice(&tracks);
        seg_children.extend_from_slice(&cluster);
        let mut f = doctype_header(doctype);
        f.extend_from_slice(&segment(&seg_children));
        f
    }

    #[test]
    fn end_to_end_webm_slice() {
        let buf = full_ebml(b"webm", 1280, 720, 5000.0, 0);
        let col = crate::driver::drive_slice(&buf, &mut EbmlParser::new(), crate::limits::Limits::default());
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Webm);
        assert!(meta.warnings.is_empty(), "warnings: {:?}", meta.warnings);
        assert_eq!(meta.unified.width, Some(1280));
        assert_eq!(meta.unified.height, Some(720));
        assert_eq!(meta.unified.duration_ms, Some(5000));
        assert_eq!(meta.unified.created.map(|d| d.year), Some(2001));
        assert_eq!(meta.unified.created.and_then(|d| d.tz_offset_min), Some(0));
    }

    #[test]
    fn walk_skips_ebml_header_and_descends_segment() {
        // 第一次 pull：顶层 EBML 头 → Skip(载荷)。
        let buf = full_ebml(b"matroska", 640, 480, 1000.0, 0);
        let mut p = EbmlParser::new();
        let res = p.pull(&buf);
        match res.demand {
            Demand::Skip(_) => {}
            other => panic!("expected Skip over EBML header, got {other:?}"),
        }
    }

    #[test]
    fn unknown_size_media_before_info_warns_and_stops() {
        // Segment 内首个子元素是未知大小 Cluster（在集齐 Info+Tracks 前）→ 警告 + Done。
        let mut cluster = Vec::new();
        cluster.extend_from_slice(&[0x1F, 0x43, 0xB6, 0x75]); // Cluster id
        cluster.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]); // 未知大小
        cluster.extend_from_slice(&[0u8; 4]);
        let mut f = doctype_header(b"webm");
        f.extend_from_slice(&segment(&cluster));
        let col = crate::driver::drive_slice(&f, &mut EbmlParser::new(), crate::limits::Limits::default());
        assert!(col.warnings.iter().any(|w| w.kind == crate::model::WarnKind::UnreachableSection));
    }

    #[test]
    fn truncated_info_warns_truncated() {
        // Info 声明 size 远大于实际 → driver 到 EOF 记 Truncated，不 panic。
        let mut info = Vec::new();
        info.extend_from_slice(&[0x15, 0x49, 0xA9, 0x66]); // Info id
        info.extend_from_slice(&[0x01]);
        info.extend_from_slice(&300u64.to_be_bytes()[1..]); // 声明 300
        info.extend_from_slice(&[0u8; 8]); // 实际仅 8
        let mut f = doctype_header(b"webm");
        f.extend_from_slice(&segment(&info));
        let col = crate::driver::drive_slice(&f, &mut EbmlParser::new(), crate::limits::Limits::default());
        assert!(col.warnings.iter().any(|w| w.kind == crate::model::WarnKind::Truncated));
    }
```
> `use crate::demand::PayloadKind;` 若触发 unused 警告则删去该行——它仅为占位，状态机不产出 Payload。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p omni-meta-core formats::ebml::tests::end_to_end_webm_slice`
Expected: FAIL（骨架立即 Done，无字段产出）。

- [ ] **Step 3: 用真实状态机替换骨架**

把 `ebml.rs` 中骨架的 `struct EbmlParser`、`impl EbmlParser`、`impl MetaParser for EbmlParser` 三段整体替换为：
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    TopLevel,
    InSegment,
}

#[derive(Debug)]
pub struct EbmlParser {
    done: bool,
    phase: Phase,
    got_info: bool,
    got_tracks: bool,
    /// 当前待读元素的绝对偏移，仅用于警告偏移保真。
    pos: u64,
}

impl Default for EbmlParser {
    fn default() -> Self {
        Self { done: false, phase: Phase::TopLevel, got_info: false, got_tracks: false, pos: 0 }
    }
}

impl EbmlParser {
    pub fn new() -> Self {
        Self::default()
    }
}

fn done_result<'a>() -> PullResult<'a> {
    PullResult { demand: Demand::Done, consumed: 0, events: Vec::new() }
}

fn need_result<'a>(n: usize) -> PullResult<'a> {
    PullResult { demand: Demand::NeedBytes(n), consumed: 0, events: Vec::new() }
}

impl MetaParser for EbmlParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        if self.done {
            return done_result();
        }
        if input.is_empty() {
            self.done = true; // 空窗口（驱动保证仅 EOF 出现）= 干净结束
            return done_result();
        }
        let hdr = match read_element_header(input) {
            Some(h) => h,
            None => {
                let need = needed_header_bytes(input);
                if input.len() >= need {
                    // 字节已够却仍读不出头 → 畸形，干净结束（防卡死）。
                    self.done = true;
                    return done_result();
                }
                return need_result(need);
            }
        };
        let header_len = hdr.header_len as usize;
        match self.phase {
            Phase::TopLevel => self.step_top(&hdr, header_len),
            Phase::InSegment => self.step_segment(input, &hdr, header_len),
        }
    }
}

impl EbmlParser {
    /// 顶层：下钻 Segment（仅消费其头部，不缓冲）；其它元素跳过整体。
    fn step_top<'a>(&mut self, hdr: &ElemHeader, header_len: usize) -> PullResult<'a> {
        if hdr.id == SEGMENT {
            self.phase = Phase::InSegment;
            self.pos = self.pos.saturating_add(header_len as u64);
            // 仅消费 Segment 头，索要首个子元素头（最小 2 字节）。
            return PullResult { demand: Demand::NeedBytes(2), consumed: header_len, events: Vec::new() };
        }
        match hdr.size {
            Some(sz) => {
                self.pos = self.pos.saturating_add(header_len as u64).saturating_add(sz);
                PullResult { demand: Demand::Skip(sz), consumed: header_len, events: Vec::new() }
            }
            None => {
                // 未知大小且非 Segment → 不可能再有 Segment，干净结束。
                self.done = true;
                done_result()
            }
        }
    }

    /// Segment 内：缓冲并解析 Info/Tracks；跳过定长不关心元素；遇未知大小媒体即停止。
    fn step_segment<'a>(&mut self, input: &'a [u8], hdr: &ElemHeader, header_len: usize) -> PullResult<'a> {
        let sz = match hdr.size {
            Some(s) => s,
            None => {
                // 未知大小媒体（如直播 Cluster）。
                self.done = true;
                if self.got_info && self.got_tracks {
                    return done_result();
                }
                let events = alloc::vec![Event::Warning(Warning {
                    offset: self.pos,
                    kind: WarnKind::UnreachableSection,
                })];
                return PullResult { demand: Demand::Done, consumed: 0, events };
            }
        };
        let wanted = hdr.id == INFO || hdr.id == TRACKS;
        if !wanted {
            self.pos = self.pos.saturating_add(header_len as u64).saturating_add(sz);
            return PullResult { demand: Demand::Skip(sz), consumed: header_len, events: Vec::new() };
        }
        // 关心的元素：须整元素入窗。
        let total = match usize::try_from(sz).ok().and_then(|s| header_len.checked_add(s)) {
            Some(t) => t,
            None => {
                self.done = true;
                return done_result();
            }
        };
        if input.len() < total {
            return need_result(total); // 不足 → 索要整元素（slice 下即截断；stream 下补字节）
        }
        let payload = &input[header_len..total];
        let mut events: Vec<Event<'a>> = Vec::new();
        if hdr.id == INFO {
            let info = parse_info(payload);
            if let Some(ms) = info.duration_ms {
                events.push(Event::Field(Field::Duration(ms)));
            }
            if let Some(dt) = info.created {
                events.push(Event::Field(Field::Created(dt)));
            }
            if info.invalid {
                events.push(Event::Warning(Warning { offset: self.pos, kind: WarnKind::UnrecognizedValue }));
            }
            self.got_info = true;
        } else {
            if let Some((w, h)) = parse_tracks(payload) {
                events.push(Event::Field(Field::Width(w)));
                events.push(Event::Field(Field::Height(h)));
            }
            self.got_tracks = true;
        }
        self.pos = self.pos.saturating_add(total as u64);
        if self.got_info && self.got_tracks {
            self.done = true;
            return PullResult { demand: Demand::Done, consumed: total, events };
        }
        PullResult { demand: Demand::NeedBytes(2), consumed: total, events }
    }
}
```

- [ ] **Step 4: 跑测试确认通过（全部 ebml 格式测试）**

Run: `cargo test -p omni-meta-core formats::ebml`
Expected: PASS（端到端 webm、走盒、未知大小停止、截断、纪元、parse_info、parse_tracks 全绿）。

- [ ] **Step 5: 跑 core 全量 + no_std 构建**

Run:
```bash
cargo test -p omni-meta-core
cargo build -p omni-meta-core --no-default-features
```
Expected: PASS；no_std 构建成功。

- [ ] **Step 6: Commit**

```bash
git add omni-meta-core/src/formats/ebml.rs
git commit -m "feat(ebml): EbmlParser 状态机—走盒/下钻Segment/缓冲Info·Tracks/停止 (C)"
```

---

## Task 10：四适配器差分一致性（WebM + MKV，含 seek 与未知大小 Segment）

**Files:**
- Modify: `omni-meta/tests/differential.rs`

- [ ] **Step 1: 写差分 fixture + 测试**

在 `omni-meta/tests/differential.rs` 末尾追加：
```rust
// ---- EBML（Matroska/WebM）----

fn ebml_elem(id: &[u8], payload: &[u8]) -> Vec<u8> {
    // 8 字节 vint size 编码
    let mut e = Vec::new();
    e.extend_from_slice(id);
    e.push(0x01);
    e.extend_from_slice(&(payload.len() as u64).to_be_bytes()[1..]);
    e.extend_from_slice(payload);
    e
}

fn ebml_video_track(w: u32, h: u32) -> Vec<u8> {
    let mut vid = Vec::new();
    vid.extend_from_slice(&ebml_elem(&[0xB0], &w.to_be_bytes())); // PixelWidth
    vid.extend_from_slice(&ebml_elem(&[0xBA], &h.to_be_bytes())); // PixelHeight
    let video = ebml_elem(&[0xE0], &vid);
    ebml_elem(&[0xAE], &video) // TrackEntry { Video }
}

fn ebml_info() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&ebml_elem(&[0x2A, 0xD7, 0xB1], &1_000_000u64.to_be_bytes())); // TimestampScale
    p.extend_from_slice(&ebml_elem(&[0x44, 0x89], &5000.0f64.to_be_bytes()));          // Duration
    p.extend_from_slice(&ebml_elem(&[0x44, 0x61], &0i64.to_be_bytes()));               // DateUTC=0 → 2001
    ebml_elem(&[0x15, 0x49, 0xA9, 0x66], &p)
}

fn ebml_header(doctype: &[u8]) -> Vec<u8> {
    let dt = ebml_elem(&[0x42, 0x82], doctype);
    ebml_elem(&[0x1A, 0x45, 0xDF, 0xA3], &dt)
}

/// EBML头 + Segment{ Info, Void(大), Tracks, Cluster }。
/// 大 Void 在 Tracks 之前被 Skip（>8192 → 行使 read_seek 原生 seek 路径）。
fn fixture_ebml(doctype: &[u8]) -> Vec<u8> {
    let void = ebml_elem(&[0xEC], &vec![0u8; 10_000]); // 大 Void，跳过
    let tracks = ebml_elem(&[0x16, 0x54, 0xAE, 0x6B], &ebml_video_track(1280, 720));
    let cluster = ebml_elem(&[0x1F, 0x43, 0xB6, 0x75], &[0u8; 16]);
    let mut seg_children = Vec::new();
    seg_children.extend_from_slice(&ebml_info());
    seg_children.extend_from_slice(&void);
    seg_children.extend_from_slice(&tracks);
    seg_children.extend_from_slice(&cluster);
    let segment = ebml_elem(&[0x18, 0x53, 0x80, 0x67], &seg_children);
    let mut f = ebml_header(doctype);
    f.extend_from_slice(&segment);
    f
}

/// Segment 用「未知大小」编码（直播常见）；下钻不依赖 Segment size。
fn fixture_ebml_unknown_size_segment() -> Vec<u8> {
    let tracks = ebml_elem(&[0x16, 0x54, 0xAE, 0x6B], &ebml_video_track(640, 480));
    let mut seg_children = Vec::new();
    seg_children.extend_from_slice(&ebml_info());
    seg_children.extend_from_slice(&tracks);
    let mut segment = Vec::new();
    segment.extend_from_slice(&[0x18, 0x53, 0x80, 0x67]); // Segment id
    segment.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]); // 未知大小
    segment.extend_from_slice(&seg_children);
    let mut f = ebml_header(b"webm");
    f.extend_from_slice(&segment);
    f
}

#[test]
fn differential_webm() {
    assert_all_equal(&fixture_ebml(b"webm"));
}

#[test]
fn differential_mkv() {
    assert_all_equal(&fixture_ebml(b"matroska"));
}

#[test]
fn differential_ebml_unknown_size_segment() {
    assert_all_equal(&fixture_ebml_unknown_size_segment());
}
```

- [ ] **Step 2: 跑差分测试确认通过**

Run: `cargo test -p omni-meta differential_webm differential_mkv differential_ebml_unknown_size_segment`
Expected: PASS（四适配器 slice/blocking/seek/push 对每个 fixture 逐字段一致）。

> 若 `differential_webm` 失败且为 push vs slice 不一致：检查 `probe` 的 `PROBE_MAX`/DocType 早返回逻辑——
> push 在累积到 DocType 字符串完整前应返回 `Unknown`（继续缓冲），不得提前定格式。

- [ ] **Step 3: 端到端断言时长来源升级（可选健壮性断言）**

确认 `fixture_ebml(b"webm")` 经任一适配器得到 `duration_ms == Some(5000)`、`width == Some(1280)`，
`created.year == 2001`。（已由 `assert_all_equal` + 单测覆盖，无需新增。）

- [ ] **Step 4: Commit**

```bash
git add omni-meta/tests/differential.rs
git commit -m "test(differential): WebM/MKV 四适配器一致性（含大 Void seek + 未知大小 Segment）(C)"
```

---

## Task 11：清理（移除临时 `allow`、统一 use）+ 全量验证

**Files:**
- Modify: `omni-meta-core/src/containers/ebml.rs`、`omni-meta-core/src/formats/ebml.rs`（按需）

- [ ] **Step 1: 跑 clippy 找出残留问题**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 可能报 unused import / dead_code / 可简化项。逐条处理。

- [ ] **Step 2: 按 clippy 提示修正**

常见处理：
- `formats/ebml.rs` 顶部 `use` 行删去未实际使用的导入（若 Task 6 一次性引入而某项未用）。最终应为：
```rust
use alloc::vec::Vec;

use crate::containers::ebml::{
    iter_child_elements, needed_header_bytes, read_element_header, read_float, read_int,
    read_uint, ElemHeader,
};
use crate::demand::{Demand, Event, MetaParser, PullResult};
use crate::model::{DateTimeParts, Field, WarnKind, Warning};
```
- 删去 Task 9 测试中占位的 `use crate::demand::PayloadKind;`（若未用）。
- `containers/ebml.rs`：所有 pub fn 此时均被 `formats::ebml` 或 `probe` 使用，无需 `allow(dead_code)`；若早前加过，移除之。
- 若 clippy 提示 `let...if` 可合并为 let-chains（仓库风格，见近期提交 `eec7f44`），照改。

- [ ] **Step 3: 全量测试 + no_std 复验**

Run:
```bash
cargo test
cargo build -p omni-meta-core --no-default-features
cargo clippy --all-targets -- -D warnings
```
Expected: 全绿；no_std 构建成功；clippy 零警告。

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "style(ebml): clippy 清整（use/let-chains/dead_code）(C)"
```

---

## Task 12：ROADMAP 更新（勾选里程碑 C，标注 duration_ms 升至 ≥2 来源）

**Files:**
- Modify: `docs/ROADMAP.md`

- [ ] **Step 1: 勾选里程碑 C 并补充说明**

在 `docs/ROADMAP.md` 把里程碑 C 区块改为已完成（替换原三条 `- [ ]`）：
```markdown
### 里程碑 C — EBML 容器（MKV/WebM）✅ 完成 — 设计 `specs/2026-06-16-omni-meta-ebml-design.md` / 计划 `plans/2026-06-16-omni-meta-ebml.md`

- [x] `containers/ebml.rs`：vint 元素 ID/size（保留/剥离标记位、未知大小）+ 元素头/子元素显式迭代 + 大端 uint/int/float
- [x] `formats/ebml.rs`：前向走盒（跳 EBML 头/下钻 Segment 不缓冲/缓冲 Info·Tracks/遇未知大小媒体即停）
- [x] `Info`→`duration_ms`（Duration×TimestampScale，隔离 f64 守卫）/`created`（DateUTC 2001 UTC）；`Tracks`→`width`/`height`（首个视频轨 PixelWidth/Height）
- [x] `probe` 经 `DocType` 区分 `FileFormat::Mkv`/`Webm`（PROBE_MAX→64）；复用里程碑 A 的 `duration_ms`/`created`
- [x] 四适配器差分（WebM/MKV，含大 Void seek + 未知大小 Segment）+ 合成畸形单测（截断/未知大小/越界永不 panic）
```

在「当前状态快照」的已完成表后、或 §4 横切待办中，更新 `duration_ms` 来源说明：把 §4 第一条
```markdown
- [ ] **Unified 受控增长**：`created`（A3 已纳入，BMFF+EXIF）/ `duration_ms`（A3 纳入，单来源 BMFF，待 EBML 补 ≥2）/ `gps` / `video_codec` / `audio_codec` 等随来源达到 ≥2 时纳入
```
改为：
```markdown
- [ ] **Unified 受控增长**：`created`（BMFF+EXIF）/ `duration_ms`（BMFF+EBML，**C 起达 ≥2 来源**）/ `gps`（EXIF GPS IFD + XMP，raw 已就绪，待 normalize 投影）/ `video_codec` / `audio_codec` 等随来源达到 ≥2 时纳入
```

并把「尚未开始 ⬜」段中的 `EBML 容器（MKV/WebM…）` 条目移除（已完成）。在「当前 Unified 字段」注记追加一行：
```markdown
> C 起 `duration_ms` 增 EBML（MKV/WebM `Info > Duration × TimestampScale`）第二来源；`created` 增 EBML `DateUTC`（2001 UTC）第三来源。`width`/`height` 增 EBML `Video PixelWidth/Height`（第 6 来源）。
```

- [ ] **Step 2: Commit**

```bash
git add docs/ROADMAP.md
git commit -m "docs: ROADMAP 标记里程碑 C 完成（EBML + duration_ms 达 ≥2 来源）(C)"
```

---

## 收尾（人工/评审）

- [ ] 跑一遍 `cargo test && cargo clippy --all-targets -- -D warnings && cargo build -p omni-meta-core --no-default-features`，全绿。
- [ ] `superpowers:requesting-code-review` 复核（可选）。
- [ ] `superpowers:finishing-a-development-branch`：ff-only 合入 `main`（用户偏好）。

---

## 自检对照（spec 覆盖）

| spec 要求 | 对应任务 |
|---|---|
| `containers/ebml.rs` vint id/size + 遍历 + 读数 | T2, T3 |
| `civil` 共享提取 | T1 |
| `FileFormat::Mkv`/`Webm` | T4 |
| `probe` PROBE_MAX + DocType | T5 |
| `datetime_from_matroska_epoch`（2001） | T6 |
| `parse_info`（Duration×scale 守卫 + DateUTC） | T7 |
| `parse_tracks`（首个视频轨） | T8 |
| 状态机（走盒/下钻/缓冲/停止/截断） | T9 |
| 四适配器差分（seek + 未知大小 Segment） | T10 |
| no_std + clippy 清零 | T9, T11 |
| ROADMAP 勾选 + duration_ms ≥2 来源 | T12 |
| Unified 无新字段（复用 duration_ms/created） | 全程（无 model 字段改动，仅加 FileFormat 变体） |
