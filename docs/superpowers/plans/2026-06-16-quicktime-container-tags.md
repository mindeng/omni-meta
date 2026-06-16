# QuickTime/udta 容器标签解析 + software/creator 投影 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 raw 层无损捕获 QuickTime mdta / udta `©`-atoms 文本标签（+ focal length 整数），再把 `software`/`creator` 投影进 Unified（≥2 来源）。

**Architecture:** 新增 `RawTags.container` 篮子（与 exif/xmp 同构）+ `Event::ContainerTag` 事件通道，由 `Collector` 累积、`finalize` 装入 `RawTags`；BMFF 解析器在 moov 的 meta(mdta)/udta 下钻时额外发 `ContainerTag`。Phase 2 的投影全部落在 `normalize(&raw)`，按「容器 > EXIF > XMP」择一。

**Tech Stack:** Rust（`no_std` + `alloc`，`#![forbid(unsafe_code)]`），workspace crate `omni-meta-core`（核心）+ `omni-meta`（适配器/差分测试）。

**设计依据:** `docs/superpowers/specs/2026-06-16-quicktime-container-tags-design.md`

**分期:** Phase 1（Task 1–6，raw 捕获）→ Phase 2（Task 7–9，Unified 投影）。各 Task 末提交；Phase 3（covr 二进制 opt-in）不在本计划，另立。

**通用命令:**
- 单测（核心）：`cargo test -p omni-meta-core <test_name>`
- 差分测试：`cargo test -p omni-meta --test differential <test_name>`
- no_std 构建：`cargo build -p omni-meta-core --no-default-features`

---

## Phase 1 — raw 捕获

### Task 1: 数据模型（ContainerSource / ContainerTag / RawTags.container）

**Files:**
- Modify: `omni-meta-core/src/model.rs`（在 `RawTags` 定义附近，约 `:82` Gps 之后、`:132` RawTags 处）

- [ ] **Step 1: 写失败测试**

在 `model.rs` 的 `#[cfg(test)] mod tests` 内追加：

```rust
    #[test]
    fn container_tag_constructs_and_eq() {
        let a = ContainerTag {
            source: ContainerSource::QuickTimeMdta,
            key: String::from("com.apple.quicktime.software"),
            value: Value::Text(String::from("13.5.1")),
        };
        let b = ContainerTag {
            source: ContainerSource::QuickTimeMdta,
            key: String::from("com.apple.quicktime.software"),
            value: Value::Text(String::from("13.5.1")),
        };
        assert_eq!(a, b);
        assert_ne!(a.source, ContainerSource::Udta);
    }

    #[test]
    fn rawtags_default_has_empty_container() {
        let r = RawTags::default();
        assert!(r.container.is_empty());
    }
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p omni-meta-core container_tag_constructs_and_eq`
Expected: 编译错误 `cannot find type ContainerTag` / `ContainerSource`。

- [ ] **Step 3: 写最小实现**

在 `model.rs` 的 `Gps` struct 之后、`Field` enum 之前插入：

```rust
/// 容器原生标签的命名空间来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerSource {
    /// QuickTime `moov/meta/ilst`，键为反向 DNS 全名。
    QuickTimeMdta,
    /// QuickTime `moov/udta` 的 `©`-atoms，键为 FourCC（© → U+00A9）。
    Udta,
}

/// 一条容器原生标签（QuickTime mdta / udta ©-atoms）。复用 `Value` 表示值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerTag {
    pub source: ContainerSource,
    pub key: String,
    pub value: Value,
}
```

在 `RawTags` 结构体加字段（`Default` 派生自动覆盖）：

```rust
pub struct RawTags {
    pub exif: Vec<ExifTag>,
    pub xmp: Vec<XmpProperty>,
    pub container: Vec<ContainerTag>,
}
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core container_tag_constructs_and_eq && cargo test -p omni-meta-core rawtags_default_has_empty_container`
Expected: PASS（2 个测试）。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/model.rs
git commit -m "feat(model): ContainerSource/ContainerTag + RawTags.container 篮子"
```

---

### Task 2: Event::ContainerTag 通道 + Collector 累积（受 max_tags 封顶）

**Files:**
- Modify: `omni-meta-core/src/demand.rs:32`（Event enum）
- Modify: `omni-meta-core/src/driver.rs:10`（import）、`:14`（Collector struct）、`:29`（handle）、`:94`（finalize）、`:141` 与 `:348`（两处 Collector 构造）

- [ ] **Step 1: 写失败测试**

在 `driver.rs` 的 `#[cfg(test)] mod tests` 内追加（该模块已 `use` 了 Event/Field 等；ContainerTag 走全路径引用）：

```rust
    #[test]
    fn collector_accumulates_container_tags_and_caps_at_max_tags() {
        use crate::model::{ContainerSource, ContainerTag, Value};
        let mut limits = crate::limits::Limits::default();
        limits.max_tags = 2;
        let mut col = Collector {
            exif: Vec::new(),
            xmp: Vec::new(),
            warnings: Vec::new(),
            width: None, height: None, duration_ms: None, created: None,
            gps: None, camera_make: None, camera_model: None,
            container: Vec::new(),
            limits,
        };
        for i in 0..5u32 {
            col.handle(Event::ContainerTag(ContainerTag {
                source: ContainerSource::QuickTimeMdta,
                key: alloc::format!("k{i}"),
                value: Value::Text(alloc::string::String::from("v")),
            }));
        }
        assert_eq!(col.container.len(), 2, "超过 max_tags 的标签须被丢弃");
    }
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p omni-meta-core collector_accumulates_container_tags_and_caps_at_max_tags`
Expected: 编译错误（`Event::ContainerTag` 不存在 / `Collector` 无 `container` 字段）。

- [ ] **Step 3: 写最小实现**

`demand.rs` 的 `Event` enum 加变体：

```rust
pub enum Event<'a> {
    Payload { kind: PayloadKind, data: &'a [u8] },
    /// 容器原生字段（width/height 等）。
    Field(Field),
    /// 容器原生 key-value 标签（QuickTime mdta / udta），原样入 raw.container。
    ContainerTag(crate::model::ContainerTag),
    Warning(Warning),
}
```

`driver.rs:10` import 增 `ContainerTag`：

```rust
use crate::model::{ContainerTag, ExifTag, Field, FileFormat, Metadata, RawTags, WarnKind, Warning, XmpProperty};
```

`Collector` struct（`:14`）加字段（放在 `camera_model` 之后、`limits` 之前）：

```rust
    container: Vec<ContainerTag>,
```

`Collector::handle`（`:30` 的 `match ev`）在 `Event::Warning` 臂之外加一臂：

```rust
            Event::ContainerTag(t) => {
                if self.container.len() < self.limits.max_tags {
                    self.container.push(t);
                }
            }
```

两处 Collector 构造（`StreamDriver::new` 约 `:141`、`drive_slice` 约 `:348`）各加一行（与 `warnings: Vec::new(),` 同区）：

```rust
                container: Vec::new(),
```

`finalize`（`:94`）改 RawTags 组装：

```rust
    let raw = RawTags { exif: col.exif, xmp: col.xmp, container: col.container };
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core collector_accumulates_container_tags_and_caps_at_max_tags`
Expected: PASS。
再跑回归：`cargo test -p omni-meta-core` → 全绿。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/demand.rs omni-meta-core/src/driver.rs
git commit -m "feat(driver): Event::ContainerTag 通道 + Collector 累积（max_tags 封顶）+ finalize 装入 RawTags.container"
```

---

### Task 3: qt_data_typed 重构（保留类型码，不破坏既有 4 键投影）

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs:1021-1029`（`qt_data_value`）、`:948`（4 键 match 取值处）

- [ ] **Step 1: 写失败测试**

在 `bmff.rs` 的 `#[cfg(test)] mod tests` 内追加：

```rust
    #[test]
    fn qt_data_typed_returns_type_and_value() {
        // data atom: type(4)=1(UTF-8) + locale(4) + "hi"
        let mut data = alloc::vec::Vec::new();
        data.extend_from_slice(&1u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(b"hi");
        let item = box_bytes(b"data", &data);
        let (ty, val) = qt_data_typed(&item).expect("data");
        assert_eq!(ty, 1);
        assert_eq!(val, b"hi");
    }
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p omni-meta-core qt_data_typed_returns_type_and_value`
Expected: 编译错误 `cannot find function qt_data_typed`。

- [ ] **Step 3: 写最小实现**

把 `qt_data_value`（`:1021`）替换为 `qt_data_typed`：

```rust
/// 从 ilst item 载荷取内层 `data` atom 的 (类型码, 值)。
/// data 载荷布局：type(4) + locale(4) + value。越界 → None。
fn qt_data_typed(item_payload: &[u8]) -> Option<(u32, &[u8])> {
    for (hdr, p) in iter_child_boxes(item_payload) {
        if &hdr.kind == b"data" {
            let type_code = u32::from_be_bytes(p.get(0..4)?.try_into().ok()?);
            let value = p.get(8..)?;
            return Some((type_code, value));
        }
    }
    None
}
```

在 `parse_qt_mdta` 的 ilst 循环（`:948`）把取值改为忽略类型码（保持既有行为）：

```rust
        let Some((_type_code, value)) = qt_data_typed(item_payload) else { continue };
```

（其余 4 键 `match key.as_str()` 不变。）

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core qt_data_typed_returns_type_and_value`
Expected: PASS。
回归：`cargo test -p omni-meta-core parse_qt_meta_harvests_four_keys` → PASS（既有 type=0 fixture 仍正确投影 4 键）。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "refactor(bmff): qt_data_value → qt_data_typed（保留 data 类型码，4 键投影不变）"
```

---

### Task 4: mdta 文本键 + focal length → ContainerTag

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs:9`（import）、`:907`（QtMdta struct）、`:919`（parse_qt_mdta 初始化）、`:948-980`（ilst 循环加捕获）；新增 `be_uint_u32` 辅助

- [ ] **Step 1: 写失败测试**

先在 `bmff.rs` test 模块加一个带类型码的 fixture 构造器（放在 `qt_meta_with_keys` 之后）：

```rust
    /// 同 qt_meta_with_keys，但每键带 data 类型码。
    fn qt_meta_with_typed_keys(items: &[(&str, u32, &[u8])]) -> alloc::vec::Vec<u8> {
        let mut hdlr = alloc::vec::Vec::new();
        hdlr.extend_from_slice(&[0u8; 8]);
        hdlr.extend_from_slice(b"mdta");
        hdlr.extend_from_slice(&[0u8; 12]);
        hdlr.push(0);

        let mut keys = alloc::vec::Vec::new();
        keys.extend_from_slice(&[0u8; 4]);
        keys.extend_from_slice(&(items.len() as u32).to_be_bytes());
        for (k, _, _) in items {
            let entry_size = 8 + k.len();
            keys.extend_from_slice(&(entry_size as u32).to_be_bytes());
            keys.extend_from_slice(b"mdta");
            keys.extend_from_slice(k.as_bytes());
        }

        let mut ilst = alloc::vec::Vec::new();
        for (i, (_, ty, v)) in items.iter().enumerate() {
            let idx = (i as u32) + 1;
            let mut data = alloc::vec::Vec::new();
            data.extend_from_slice(&ty.to_be_bytes());
            data.extend_from_slice(&0u32.to_be_bytes()); // locale
            data.extend_from_slice(v);
            let data_box = box_bytes(b"data", &data);
            let mut item_box = alloc::vec::Vec::new();
            item_box.extend_from_slice(&((8 + data_box.len()) as u32).to_be_bytes());
            item_box.extend_from_slice(&idx.to_be_bytes());
            item_box.extend_from_slice(&data_box);
            ilst.extend_from_slice(&item_box);
        }

        let mut meta = alloc::vec::Vec::new();
        meta.extend_from_slice(&box_bytes(b"hdlr", &hdlr));
        meta.extend_from_slice(&box_bytes(b"keys", &keys));
        meta.extend_from_slice(&box_bytes(b"ilst", &ilst));
        meta
    }

    #[test]
    fn parse_qt_mdta_captures_text_and_focal_length_tags() {
        use crate::model::{ContainerSource, Value};
        let meta = qt_meta_with_typed_keys(&[
            ("com.apple.quicktime.software", 1, b"13.5.1"),
            ("com.apple.quicktime.author", 1, b"Jane"),
            ("com.apple.quicktime.camera.focal_length.35mm_equivalent", 22, &28u32.to_be_bytes()),
            ("com.apple.quicktime.junkbinary", 13, &[0xFF, 0xD8, 0xFF]), // JPEG 类型 → 跳过
        ]);
        let out = parse_qt_mdta(&meta);
        let find = |k: &str| out.tags.iter().find(|t| t.key == k);
        assert!(matches!(find("com.apple.quicktime.software").map(|t| &t.value),
            Some(Value::Text(s)) if s == "13.5.1"));
        assert!(matches!(find("com.apple.quicktime.author").map(|t| &t.value),
            Some(Value::Text(s)) if s == "Jane"));
        assert!(matches!(find("com.apple.quicktime.camera.focal_length.35mm_equivalent").map(|t| &t.value),
            Some(Value::U32(28))));
        assert!(find("com.apple.quicktime.junkbinary").is_none(), "二进制类型不收");
        assert!(out.tags.iter().all(|t| t.source == ContainerSource::QuickTimeMdta));
    }
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p omni-meta-core parse_qt_mdta_captures_text_and_focal_length_tags`
Expected: 编译错误（`QtMdta` 无 `tags` 字段）。

- [ ] **Step 3: 写最小实现**

`bmff.rs:9` import 增 `ContainerSource, ContainerTag, Value`：

```rust
use crate::model::{ContainerSource, ContainerTag, DateTimeParts, Field, Gps, Value, WarnKind, Warning};
```

新增 BE 整数辅助（放在 `qt_data_typed` 附近）：

```rust
/// 大端无符整数（1/2/4 字节）→ u32；其它长度 → None。
fn be_uint_u32(b: &[u8]) -> Option<u32> {
    match b.len() {
        1 => Some(u32::from(b[0])),
        2 => Some(u32::from(u16::from_be_bytes(b.try_into().ok()?))),
        4 => Some(u32::from_be_bytes(b.try_into().ok()?)),
        _ => None,
    }
}
```

`QtMdta` struct（`:907`）加字段：

```rust
struct QtMdta {
    gps: Option<Gps>,
    make: Option<alloc::string::String>,
    model: Option<alloc::string::String>,
    created: Option<DateTimeParts>,
    tags: alloc::vec::Vec<ContainerTag>,
}
```

`parse_qt_mdta` 内两处 `QtMdta { ... }` 初始化（`:919` 与早退 `:127` 处若有）补 `tags: alloc::vec::Vec::new(),`。
> 注意：`parse_moov:127` 的 `let mut mdta = QtMdta { ... }` 也需补 `tags: alloc::vec::Vec::new(),`。

ilst 循环（`:948` 起，已改为 `let Some((type_code, value)) = qt_data_typed(...)`——把 Task 3 的 `_type_code` 改回 `type_code` 以便使用）。在 `match key.as_str() { ... }` 闭合之后、循环体末尾，追加捕获：

```rust
        // raw 层：UTF-8 文本键（type==1）原样入 container；focal length 整数（type 21/22）→ U32。
        const DATA_UTF8: u32 = 1;
        const DATA_INT_SIGNED: u32 = 21;
        const DATA_INT_UNSIGNED: u32 = 22;
        if type_code == DATA_UTF8 {
            if let Ok(s) = core::str::from_utf8(value) {
                out.tags.push(ContainerTag {
                    source: ContainerSource::QuickTimeMdta,
                    key: alloc::string::String::from(key.as_str()),
                    value: Value::Text(alloc::string::String::from(s)),
                });
            }
        } else if (type_code == DATA_INT_SIGNED || type_code == DATA_INT_UNSIGNED)
            && key.ends_with("focal_length.35mm_equivalent")
            && let Some(n) = be_uint_u32(value)
        {
            out.tags.push(ContainerTag {
                source: ContainerSource::QuickTimeMdta,
                key: alloc::string::String::from(key.as_str()),
                value: Value::U32(n),
            });
        }
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core parse_qt_mdta_captures_text_and_focal_length_tags`
Expected: PASS。
回归：`cargo test -p omni-meta-core parse_qt_meta_harvests_four_keys` → PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat(bmff): mdta 文本键 + focal length(35mm) → ContainerTag（二进制/未知类型跳过）"
```

---

### Task 5: udta ©-atoms → ContainerTag + MoovInfo 汇集 + 事件发射

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs:102`（MoovInfo）、`:115`（parse_moov：udta 循环 + meta tags 汇集 + 末尾装配）、`:683-708`（事件发射）；新增 `udta_key_string`、`parse_udta_text`

- [ ] **Step 1: 写失败测试**

在 `bmff.rs` test 模块追加：

```rust
    #[test]
    fn parse_moov_collects_udta_and_mdta_container_tags() {
        use crate::model::{ContainerSource, Value};
        // udta { ©swr="MyCam 1.0" }
        let swr_text = b"MyCam 1.0";
        let mut swr_payload = alloc::vec::Vec::new();
        swr_payload.extend_from_slice(&(swr_text.len() as u16).to_be_bytes());
        swr_payload.extend_from_slice(&0u16.to_be_bytes()); // lang
        swr_payload.extend_from_slice(swr_text);
        let udta = box_bytes(b"\xA9swr", &swr_payload);

        // meta { mdta software }
        let meta = qt_meta_with_typed_keys(&[
            ("com.apple.quicktime.software", 1, b"13.5.1"),
        ]);

        let mut moov_p = alloc::vec::Vec::new();
        moov_p.extend_from_slice(&box_bytes(b"udta", &udta));
        moov_p.extend_from_slice(&box_bytes(b"meta", &meta));
        let info = parse_moov(&moov_p, 0);

        let find = |src: ContainerSource, k: &str| info.container_tags.iter()
            .find(|t| t.source == src && t.key == k);
        assert!(matches!(find(ContainerSource::Udta, "©swr").map(|t| &t.value),
            Some(Value::Text(s)) if s == "MyCam 1.0"));
        assert!(matches!(find(ContainerSource::QuickTimeMdta, "com.apple.quicktime.software").map(|t| &t.value),
            Some(Value::Text(s)) if s == "13.5.1"));
    }

    #[test]
    fn end_to_end_mov_container_tags_reach_raw() {
        let meta = qt_meta_with_typed_keys(&[
            ("com.apple.quicktime.software", 1, b"13.5.1"),
        ]);
        let mut moov_p = alloc::vec::Vec::new();
        moov_p.extend_from_slice(&box_bytes(b"meta", &meta));
        let mut f = ftyp_mp4();
        f.extend_from_slice(&box_bytes(b"moov", &moov_p));

        let col = crate::driver::drive_slice(&f, &mut BmffParser::new(), crate::limits::Limits::default());
        let meta_out = crate::driver::finalize(col, crate::model::FileFormat::Mov);
        assert!(meta_out.raw.container.iter().any(|t|
            t.key == "com.apple.quicktime.software"
            && matches!(&t.value, crate::model::Value::Text(s) if s == "13.5.1")));
    }
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p omni-meta-core parse_moov_collects_udta_and_mdta_container_tags`
Expected: 编译错误（`MoovInfo` 无 `container_tags` 字段）。

- [ ] **Step 3: 写最小实现**

新增两个辅助（放在 `parse_xyz`/`parse_loci` 附近）：

```rust
/// udta ©-atom 的 FourCC → key 串：首字节 0xA9 映射为 '©'(U+00A9)，后 3 字节须 ASCII。
fn udta_key_string(kind: &[u8; 4]) -> Option<alloc::string::String> {
    if kind[0] != 0xA9 {
        return None;
    }
    let mut s = alloc::string::String::from("©");
    for &c in &kind[1..] {
        if !c.is_ascii() {
            return None;
        }
        s.push(c as char);
    }
    Some(s)
}

/// 解析 udta ©-atom 文本载荷：u16 size + u16 lang + text。越界/非 UTF-8 → None。
fn parse_udta_text(payload: &[u8]) -> Option<&str> {
    let size = u16::from_be_bytes(payload.get(0..2)?.try_into().ok()?) as usize;
    let text = payload.get(4..4 + size)?;
    core::str::from_utf8(text).ok()
}
```

`MoovInfo`（`:102`）加字段：

```rust
    container_tags: Vec<ContainerTag>,
```

`parse_moov` 初始化 `MoovInfo { ... }`（`:116`）补：

```rust
        container_tags: Vec::new(),
```

在 `parse_moov` 顶部（`let mut mdta = ...` 附近）新增 udta 标签累积器：

```rust
    let mut udta_tags: Vec<ContainerTag> = Vec::new();
```

udta 循环（`:148`）加一臂（在 `b"loci"` 臂之后、`_ =>` 之前）：

```rust
                        k if k[0] == 0xA9 => {
                            if let (Some(key), Some(text)) = (udta_key_string(k), parse_udta_text(up)) {
                                udta_tags.push(ContainerTag {
                                    source: ContainerSource::Udta,
                                    key,
                                    value: Value::Text(alloc::string::String::from(text)),
                                });
                            }
                        }
```
> `b"\xA9xyz"` 臂在前，故 ©xyz 不会落入此臂（坐标不重复入 raw）。

meta 臂（`:157`）追加汇集 mdta tags：

```rust
            b"meta" => {
                let m = parse_qt_mdta(p);
                if mdta.gps.is_none() { mdta.gps = m.gps; }
                if mdta.make.is_none() { mdta.make = m.make; }
                if mdta.model.is_none() { mdta.model = m.model; }
                if mdta.created.is_none() { mdta.created = m.created; }
                mdta.tags.extend(m.tags);
            }
```

`parse_moov` 末尾（`info.camera_model = mdta.model;` 之后、`info` 之前）装配：

```rust
    info.container_tags = mdta.tags;
    info.container_tags.append(&mut udta_tags);
```

事件发射（`BmffParser::pull`，`:703` 的 camera_model 块之后、`for warn in info.warnings` 之前）：

```rust
            for t in info.container_tags {
                events.push(Event::ContainerTag(t));
            }
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core parse_moov_collects_udta_and_mdta_container_tags && cargo test -p omni-meta-core end_to_end_mov_container_tags_reach_raw`
Expected: PASS（2 个）。
回归 + no_std：`cargo test -p omni-meta-core` 全绿；`cargo build -p omni-meta-core --no-default-features` 成功。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat(bmff): udta ©-atoms → ContainerTag + MoovInfo 汇集 mdta/udta 标签 + Event::ContainerTag 发射"
```

---

### Task 6: 四适配器差分（含 mdta software/author + udta ©swr）

**Files:**
- Modify: `omni-meta/tests/differential.rs`（在既有 `fixture_bmff_mp4` 之后追加）

- [ ] **Step 1: 写失败测试**

在 `differential.rs` 追加（复用文件内既有 `bmff_box` 助手；内联一个带类型码的 qt-meta 构造器）：

```rust
fn qt_meta_typed(items: &[(&str, u32, &[u8])]) -> Vec<u8> {
    let mut hdlr = Vec::new();
    hdlr.extend_from_slice(&[0u8; 8]);
    hdlr.extend_from_slice(b"mdta");
    hdlr.extend_from_slice(&[0u8; 12]);
    hdlr.push(0);
    let mut keys = Vec::new();
    keys.extend_from_slice(&[0u8; 4]);
    keys.extend_from_slice(&(items.len() as u32).to_be_bytes());
    for (k, _, _) in items {
        keys.extend_from_slice(&((8 + k.len()) as u32).to_be_bytes());
        keys.extend_from_slice(b"mdta");
        keys.extend_from_slice(k.as_bytes());
    }
    let mut ilst = Vec::new();
    for (i, (_, ty, v)) in items.iter().enumerate() {
        let idx = (i as u32) + 1;
        let mut data = Vec::new();
        data.extend_from_slice(&ty.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(v);
        let data_box = bmff_box(b"data", &data);
        let mut item = Vec::new();
        item.extend_from_slice(&((8 + data_box.len()) as u32).to_be_bytes());
        item.extend_from_slice(&idx.to_be_bytes());
        item.extend_from_slice(&data_box);
        ilst.extend_from_slice(&item);
    }
    let mut meta = Vec::new();
    meta.extend_from_slice(&bmff_box(b"hdlr", &hdlr));
    meta.extend_from_slice(&bmff_box(b"keys", &keys));
    meta.extend_from_slice(&bmff_box(b"ilst", &ilst));
    meta
}

fn fixture_bmff_mp4_container_tags() -> Vec<u8> {
    let mut ftyp_p = Vec::new();
    ftyp_p.extend_from_slice(b"isom");
    ftyp_p.extend_from_slice(&0u32.to_be_bytes());
    ftyp_p.extend_from_slice(b"mp42");
    let ftyp = bmff_box(b"ftyp", &ftyp_p);

    // udta { ©swr }
    let swr_text = b"MyCam 1.0";
    let mut swr_payload = Vec::new();
    swr_payload.extend_from_slice(&(swr_text.len() as u16).to_be_bytes());
    swr_payload.extend_from_slice(&0u16.to_be_bytes());
    swr_payload.extend_from_slice(swr_text);
    let udta = bmff_box(b"\xA9swr", &swr_payload);

    let meta = qt_meta_typed(&[
        ("com.apple.quicktime.software", 1, b"13.5.1"),
        ("com.apple.quicktime.author", 1, b"Jane"),
    ]);

    let mut moov_p = Vec::new();
    moov_p.extend_from_slice(&bmff_box(b"mvhd", &mp4_mvhd_v0(2_082_844_800, 600, 900_900)));
    moov_p.extend_from_slice(&bmff_box(b"udta", &udta));
    moov_p.extend_from_slice(&bmff_box(b"meta", &meta));
    let moov = bmff_box(b"moov", &moov_p);

    let mut f = ftyp;
    f.extend_from_slice(&moov);
    f
}

#[test]
fn differential_bmff_mp4_container_tags() {
    assert_all_equal(&fixture_bmff_mp4_container_tags());
}
```

- [ ] **Step 2: 运行测试，确认失败 → 然后通过**

Run: `cargo test -p omni-meta --test differential differential_bmff_mp4_container_tags`
Expected: 若 Task 1–5 已落地，应直接 PASS（四适配器对 `raw.container` 逐字段一致）。若失败，说明某适配器路径未透传 ContainerTag——按报错定位。

- [ ] **Step 3: 提交**

```bash
git add omni-meta/tests/differential.rs
git commit -m "test(differential): MP4 mdta software/author + udta ©swr 四适配器 raw.container 一致"
```

---

## Phase 2 — Unified software/creator 投影

### Task 7: Unified.software / Unified.creator 字段

**Files:**
- Modify: `omni-meta-core/src/model.rs:140`（Unified struct）

- [ ] **Step 1: 写失败测试**

在 `model.rs` test 模块追加：

```rust
    #[test]
    fn unified_has_software_and_creator_default_none() {
        let u = Unified::default();
        assert!(u.software.is_none());
        assert!(u.creator.is_none());
    }
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p omni-meta-core unified_has_software_and_creator_default_none`
Expected: 编译错误（`Unified` 无 `software`/`creator`）。

- [ ] **Step 3: 写最小实现**

`Unified` struct 加字段（`gps` 之后）：

```rust
    pub software: Option<String>,
    pub creator: Option<String>,
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core unified_has_software_and_creator_default_none`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/model.rs
git commit -m "feat(model): Unified.software / Unified.creator 字段"
```

---

### Task 8: normalize 投影（容器 > EXIF > XMP）

**Files:**
- Modify: `omni-meta-core/src/normalize.rs:5`（import）、`:7-13`（常量）、`:217` 起 `normalize` 末尾（投影）；新增 3 个辅助

- [ ] **Step 1: 写失败测试**

在 `normalize.rs` test 模块追加：

```rust
    #[test]
    fn software_precedence_container_over_exif_over_xmp() {
        use crate::model::{ContainerSource, ContainerTag, ExifTag, IfdKind, Value, XmpProperty};
        let mut warnings = Vec::new();
        // 三来源齐备 → 取容器
        let raw = RawTags {
            exif: alloc::vec![ExifTag { ifd: IfdKind::Primary, tag: 0x0131, value: Value::Text(alloc::string::String::from("ExifSW")) }],
            xmp: alloc::vec![XmpProperty { prefix: alloc::string::String::from("xmp"), name: alloc::string::String::from("CreatorTool"), value: alloc::string::String::from("XmpSW") }],
            container: alloc::vec![ContainerTag { source: ContainerSource::QuickTimeMdta, key: alloc::string::String::from("com.apple.quicktime.software"), value: Value::Text(alloc::string::String::from("ContainerSW")) }],
        };
        let u = normalize(&raw, &mut warnings);
        assert_eq!(u.software.as_deref(), Some("ContainerSW"));
    }

    #[test]
    fn software_falls_back_exif_then_xmp() {
        use crate::model::{ExifTag, IfdKind, Value, XmpProperty};
        let mut warnings = Vec::new();
        // 仅 EXIF
        let raw_exif = RawTags {
            exif: alloc::vec![ExifTag { ifd: IfdKind::Primary, tag: 0x0131, value: Value::Text(alloc::string::String::from("ExifSW")) }],
            xmp: Vec::new(), container: Vec::new(),
        };
        assert_eq!(normalize(&raw_exif, &mut warnings).software.as_deref(), Some("ExifSW"));
        // 仅 XMP
        let raw_xmp = RawTags {
            exif: Vec::new(),
            xmp: alloc::vec![XmpProperty { prefix: alloc::string::String::from("xmp"), name: alloc::string::String::from("CreatorTool"), value: alloc::string::String::from("XmpSW") }],
            container: Vec::new(),
        };
        assert_eq!(normalize(&raw_xmp, &mut warnings).software.as_deref(), Some("XmpSW"));
    }

    #[test]
    fn creator_from_container_udta_and_exif_artist() {
        use crate::model::{ContainerSource, ContainerTag, ExifTag, IfdKind, Value};
        let mut warnings = Vec::new();
        // udta ©aut
        let raw_udta = RawTags {
            exif: Vec::new(), xmp: Vec::new(),
            container: alloc::vec![ContainerTag { source: ContainerSource::Udta, key: alloc::string::String::from("©aut"), value: Value::Text(alloc::string::String::from("Auteur")) }],
        };
        assert_eq!(normalize(&raw_udta, &mut warnings).creator.as_deref(), Some("Auteur"));
        // EXIF Artist 0x013B
        let raw_artist = RawTags {
            exif: alloc::vec![ExifTag { ifd: IfdKind::Primary, tag: 0x013B, value: Value::Text(alloc::string::String::from("Shooter")) }],
            xmp: Vec::new(), container: Vec::new(),
        };
        assert_eq!(normalize(&raw_artist, &mut warnings).creator.as_deref(), Some("Shooter"));
    }
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p omni-meta-core software_precedence_container_over_exif_over_xmp`
Expected: 失败（`u.software` 永远 None）。

- [ ] **Step 3: 写最小实现**

`normalize.rs:5` import 增 `ContainerSource`：

```rust
use crate::model::{ContainerSource, DateTimeParts, Gps, IfdKind, Orientation, RawTags, Unified, Value, WarnKind, Warning};
```

常量区（`:13` 之后）加：

```rust
const TAG_SOFTWARE: u16 = 0x0131;
const TAG_ARTIST: u16 = 0x013B;
```

新增 3 个辅助（放在 `normalize` 函数之前）：

```rust
/// 取指定来源/键的容器文本标签值。
fn container_text<'a>(raw: &'a RawTags, source: ContainerSource, key: &str) -> Option<&'a str> {
    raw.container.iter().find_map(|t| {
        if t.source == source && t.key == key
            && let Value::Text(s) = &t.value
        {
            return Some(s.as_str());
        }
        None
    })
}

/// 取 Primary IFD 指定 tag 的文本值。
fn exif_primary_text(raw: &RawTags, tag: u16) -> Option<alloc::string::String> {
    raw.exif.iter().find_map(|t| {
        if t.ifd == IfdKind::Primary && t.tag == tag
            && let Value::Text(s) = &t.value
        {
            return Some(s.clone());
        }
        None
    })
}

/// 取指定 prefix/name 的 XMP 属性值。
fn xmp_text(raw: &RawTags, prefix: &str, name: &str) -> Option<alloc::string::String> {
    raw.xmp.iter().find_map(|p| {
        if p.prefix == prefix && p.name == name {
            Some(p.value.clone())
        } else {
            None
        }
    })
}
```

在 `normalize` 函数末尾（`u` 返回之前）加投影：

```rust
    // software：容器 > EXIF(0x0131) > XMP(xmp:CreatorTool)
    u.software = container_text(raw, ContainerSource::QuickTimeMdta, "com.apple.quicktime.software")
        .or_else(|| container_text(raw, ContainerSource::Udta, "©swr"))
        .map(alloc::string::String::from)
        .or_else(|| exif_primary_text(raw, TAG_SOFTWARE))
        .or_else(|| xmp_text(raw, "xmp", "CreatorTool"));
    // creator：容器 > EXIF(0x013B Artist) > XMP(dc:creator)
    u.creator = container_text(raw, ContainerSource::QuickTimeMdta, "com.apple.quicktime.author")
        .or_else(|| container_text(raw, ContainerSource::Udta, "©aut"))
        .map(alloc::string::String::from)
        .or_else(|| exif_primary_text(raw, TAG_ARTIST))
        .or_else(|| xmp_text(raw, "dc", "creator"));
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p omni-meta-core software_precedence_container_over_exif_over_xmp && cargo test -p omni-meta-core software_falls_back_exif_then_xmp && cargo test -p omni-meta-core creator_from_container_udta_and_exif_artist`
Expected: PASS（3 个）。
回归 + no_std：`cargo test -p omni-meta-core` 全绿；`cargo build -p omni-meta-core --no-default-features` 成功。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/normalize.rs
git commit -m "feat(normalize): software/creator 投影（容器>EXIF>XMP，≥2 来源）"
```

---

### Task 9: 跨格式 ≥2 来源端到端 + ROADMAP 更新

**Files:**
- Modify: `omni-meta/tests/differential.rs`（端到端断言）
- Modify: `docs/ROADMAP.md`

- [ ] **Step 1: 写失败测试**

在 `differential.rs` 追加（验证 MOV 容器来源 → unified.software，并复用 Phase 1 fixture）：

```rust
#[test]
fn mov_container_projects_software_and_creator() {
    use omni_meta::{read_slice, Options};
    let bytes = fixture_bmff_mp4_container_tags();
    let m = read_slice(&bytes, Options::default()).expect("metadata");
    assert_eq!(m.unified.software.as_deref(), Some("13.5.1"));
    assert_eq!(m.unified.creator.as_deref(), Some("Jane"));
}
```

> JPEG EXIF Software 第二来源已由 Task 8 的 `software_falls_back_exif_then_xmp` 单测覆盖（EXIF Software 路径）；此处补容器格式来源，共同满足 ≥2。

- [ ] **Step 2: 运行测试，确认通过**

Run: `cargo test -p omni-meta --test differential mov_container_projects_software_and_creator`
Expected: PASS。
全量回归：`cargo test` → 全绿。

- [ ] **Step 3: 更新 ROADMAP**

在 `docs/ROADMAP.md` §1「当前 Unified 字段」段落，把字段清单追加 `software`、`creator`，并在受控增长说明后补一行：

```markdown
> QuickTime 容器标签里程碑起新增 `software`（EXIF 0x0131 + XMP xmp:CreatorTool + 容器 mdta software/udta ©swr，≥2 来源）与 `creator`（EXIF 0x013B Artist + XMP dc:creator + 容器 mdta author/udta ©aut，≥2 来源），并在 raw 层新增 `RawTags.container`（QuickTime mdta 文本键 + udta ©-atoms + focal length；二进制 covr 留待可选 Phase 3）。
```

并在已完成表格补一行：

```markdown
| **QuickTime 容器标签** | `RawTags.container`（mdta 文本键/udta ©-atoms/focal length）+ `software`/`creator` 投影（容器>EXIF>XMP） | （本次提交） |
```

- [ ] **Step 4: 提交**

```bash
git add omni-meta/tests/differential.rs docs/ROADMAP.md
git commit -m "test(e2e): MOV 容器→unified.software/creator + docs(roadmap) 标记完成"
```

---

## Self-Review 记录

- **Spec 覆盖**：§3 数据模型→Task 1；§4 事件通道/mdta/udta/畸形→Task 2–5；§5 投影→Task 7–8；§7 测试（单测/差分/畸形/no_std）→各 Task Step + Task 6/9；§8 分期→Phase 1/2，Phase 3 显式排除。
- **类型一致性**：`ContainerTag{source,key,value}`、`ContainerSource::{QuickTimeMdta,Udta}`、`qt_data_typed`、`be_uint_u32`、`udta_key_string`、`parse_udta_text`、`container_text`/`exif_primary_text`/`xmp_text` 跨任务签名一致；`MoovInfo.container_tags`、`QtMdta.tags` 命名前后统一。
- **无占位符**：每个改动步骤均含完整代码与精确路径/命令。
- **畸形/不变量**：所有取字节经 `get(..)?`；非 UTF-8/未知类型/越界→跳过不 panic；`max_tags` 在 Collector 单一收口封顶；focal length 仅 1/2/4 字节整数。
