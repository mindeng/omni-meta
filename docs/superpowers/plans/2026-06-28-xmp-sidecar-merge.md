# XMP sidecar 合并 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 omni-meta 增加 `Metadata::with_xmp_sidecar`——把旁挂 `.xmp` sidecar（Apple Photos `Export IPTC as XMP` 等场景）解析后注入已解析结果，描述字段（title/description/creator/copyright）以 sidecar 为先、技术字段（make/model/dims/gps）以内嵌为先。

**Architecture:** 解析后 method（非 `Options` 注入），避免时间耦合、对 slice/blocking/push 三入口一视同仁。sidecar 字节经现有 XMP codec 解码 → 落 `RawTags.xmp_sidecar` 单列（留 provenance）；基于 `Metadata` 内保留的 `StructuralFields` 快照重跑 `normalize` 重投影 Unified。normalize 各字段 `.or_else()` 链按描述/技术分别插入 sidecar 档。

**Tech Stack:** Rust edition 2024，`#![no_std]` + `alloc`，`#![forbid(unsafe_code)]`，零外部依赖。测试用内置 `#[test]`，复用现有 `read_slice` + XMP codec。

**基准 spec:** `docs/superpowers/specs/2026-06-28-xmp-sidecar-merge-design.md`

**全局不变量:** 缺失即不臆造（Unified 全 Option）；sidecar 来源的 normalize 回退分支**一律静默**（失败不产告警）——本不变量是 Task 3 丢弃重投影告警的依据，后续若新增会告警的 sidecar 分支必须重审 Task 3。

---

## 文件结构

**新建:**
- `omni-meta-core/src/sidecar.rs` — `impl Metadata { with_xmp_sidecar }`，单一职责：sidecar 解码 + 重投影

**修改:**
- `omni-meta-core/src/model.rs` — `RawTags` 加 `xmp_sidecar` 列；`Metadata` 加 `pub(crate) structural`；`StructuralFields` 从 normalize.rs 迁入此处（model 无依赖，层次更顺）
- `omni-meta-core/src/normalize.rs` — 删 `StructuralFields` 定义（迁出）；加 `xmp_sidecar_text` helper；描述字段链首插 sidecar；技术字段末位插 sidecar；`gps_from_xmp` 泛化扫 sidecar
- `omni-meta-core/src/driver.rs` — `finalize` 存 `structural` 进 `Metadata`；`RawTags` 字面量补字段；`use` 路径改 `crate::model::StructuralFields`
- `omni-meta-core/src/lib.rs` — 加 `pub(crate) mod sidecar;`
- `docs/ROADMAP.md` — 勾选里程碑 H

> `omni-meta` facade 经 `pub use omni_meta_core::*;` 自动透出 `Metadata::with_xmp_sidecar`，**无需改动**。

---

## Task 1: 数据模型基座（StructuralFields 迁移 + RawTags/Metadata 新字段 + finalize 存储）

**Files:**
- Modify: `omni-meta-core/src/model.rs`（`RawTags` ~170、`Metadata` ~217、新增 `StructuralFields`）
- Modify: `omni-meta-core/src/normalize.rs:15-24`（删 `StructuralFields` 定义）+ `use`
- Modify: `omni-meta-core/src/driver.rs:10-13, 98-123`（`use` + `finalize`）

- [ ] **Step 1: 把 `StructuralFields` 迁入 model.rs 并加 Eq**

删除 `omni-meta-core/src/normalize.rs` 中第 15-24 行的 `StructuralFields` 定义（含其上方注释），改在 `omni-meta-core/src/model.rs` 的 `RawTags` 定义**之前**插入（补 `PartialEq, Eq`，供 `Metadata` 派生 Eq）：

```rust
/// 二进制结构来源候选（无命名空间、parser 权威）：容器结构头与二进制 udta。
/// 由 `driver::Collector` 从 `Event::Field` 累积，作为 normalize 的一类来源传入；
/// 并由 `finalize` 存入 `Metadata.structural` 供 `with_xmp_sidecar` 重投影复用。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructuralFields {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub created: Option<DateTimeParts>,
    pub gps: Option<Gps>,
}
```

- [ ] **Step 2: 修 normalize.rs 的 use（StructuralFields 现来自 model）**

在 `omni-meta-core/src/normalize.rs` 顶部 `use crate::model::{...}` 列表中加入 `StructuralFields`（与 `RawTags` 等并列）。删除定义后 `normalize()` 签名里的 `&StructuralFields` 引用保持不变。

- [ ] **Step 3: RawTags 加 xmp_sidecar 列**

`omni-meta-core/src/model.rs` 的 `RawTags`（保持 `#[derive(Debug, Clone, Default, PartialEq, Eq)]`）：

```rust
pub struct RawTags {
    pub exif: Vec<ExifTag>,
    pub xmp: Vec<XmpProperty>,
    /// 旁挂 `.xmp` sidecar 来源（`Metadata::with_xmp_sidecar` 注入），与内嵌 `xmp` 分列以留 provenance。
    pub xmp_sidecar: Vec<XmpProperty>,
    pub container: Vec<ContainerTag>,
    pub text: Vec<TextTag>,
}
```

- [ ] **Step 4: Metadata 加 structural 快照**

`omni-meta-core/src/model.rs` 的 `Metadata`：

```rust
pub struct Metadata {
    pub unified: Unified,
    pub raw: RawTags,
    pub warnings: Vec<Warning>,
    pub format: FileFormat,
    /// 重投影所需的结构来源快照（内部辅助；内容已全在 `unified` 暴露，故 `pub(crate)`）。
    pub(crate) structural: StructuralFields,
}
```

- [ ] **Step 5: finalize 写入 structural + 补 RawTags 字面量**

`omni-meta-core/src/driver.rs`：顶部 `use` 把 `crate::model::{... RawTags ...}` 之外，确保 `StructuralFields` 可名（已由 `crate::normalize::StructuralFields` 改为 `crate::model::StructuralFields`——driver.rs 现引用 `crate::normalize::StructuralFields` 于第 109 行，改为 `crate::model::StructuralFields`）。

`finalize`（约 98-123 行）改为：

```rust
pub(crate) fn finalize(col: Collector, format: FileFormat) -> Metadata {
    let (width, height) = (col.width, col.height);
    let (duration_ms, created) = (col.duration_ms, col.created);
    let gps = col.gps;
    let raw = RawTags {
        exif: col.exif,
        xmp: col.xmp,
        xmp_sidecar: Vec::new(),
        container: col.container,
        text: col.text,
    };
    let mut warnings = col.warnings;
    let structural = crate::model::StructuralFields {
        width,
        height,
        duration_ms,
        created,
        gps,
    };
    let unified = normalize(&raw, &structural, &mut warnings);
    Metadata {
        unified,
        raw,
        warnings,
        format,
        structural,
    }
}
```

- [ ] **Step 6: 编译，按报错补齐所有 RawTags 字面量**

Run: `cargo build -p omni-meta-core --all-features 2>&1 | head -40`
Expected: 一批 `error[E0063]: missing field xmp_sidecar in initializer of RawTags`（normalize.rs 内约 27 处测试字面量）。逐处在该 `RawTags { ... }` 内补 `xmp_sidecar: Vec::new(),`（与 `xmp:` 同风格）。重复 build 直到无 E0063。

- [ ] **Step 7: 跑全量测试确认零回归**

Run: `cargo test -p omni-meta-core --all-features 2>&1 | tail -20`
Expected: 全绿（新字段为空 vec，行为不变）。

- [ ] **Step 8: 提交**

```bash
git add omni-meta-core/src/model.rs omni-meta-core/src/normalize.rs omni-meta-core/src/driver.rs
git commit -m "refactor(model): RawTags.xmp_sidecar 列 + Metadata.structural 快照 + StructuralFields 迁入 model"
```

---

## Task 2: `xmp_sidecar_text` helper + 描述字段 sidecar 优先

**Files:**
- Modify: `omni-meta-core/src/normalize.rs`（helper 近 `xmp_text` ~279；字段链 538-545）
- Test: `omni-meta-core/src/normalize.rs`（`#[cfg(test)] mod tests`）

- [ ] **Step 1: 写失败测试（描述字段 sidecar 压过 EXIF + 内嵌 XMP）**

在 normalize.rs 测试模块末尾加入。`raw_with_text` 等既有辅助不便覆盖此场景，直接构造 `RawTags`：

```rust
#[test]
fn sidecar_description_beats_exif_and_embedded_xmp() {
    use crate::model::{ExifTag, IfdKind, RawTags, Value, XmpProperty};
    let raw = RawTags {
        exif: alloc::vec![ExifTag {
            ifd: IfdKind::Primary,
            tag: 0x010E, // ImageDescription
            value: Value::Text(alloc::string::String::from("from-exif")),
        }],
        xmp: alloc::vec![xmp_p("dc", "description", "from-embedded")],
        xmp_sidecar: alloc::vec![xmp_p("dc", "description", "from-sidecar")],
        container: alloc::vec![],
        text: alloc::vec![],
    };
    let mut w = alloc::vec::Vec::new();
    let u = normalize(&raw, &StructuralFields::default(), &mut w);
    assert_eq!(u.description.as_deref(), Some("from-sidecar"));
}
```

> `xmp_p` 若测试模块尚无，复用既有 helper（normalize.rs 测试区已有构造 `XmpProperty` 的辅助，见 ~1087 行 `fn ...(&str, name, value) -> XmpProperty`）；若名称不同，按现有名调用，勿新增重复 helper。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p omni-meta-core sidecar_description_beats -- --nocapture`
Expected: FAIL，`description` 为 `Some("from-exif")`（sidecar 未参与）。

- [ ] **Step 3: 加 `xmp_sidecar_text` helper**

在 normalize.rs `xmp_text`（~279-287）正下方加入：

```rust
/// 取指定 prefix/name 的 sidecar XMP 属性值（扫 `raw.xmp_sidecar`）。
fn xmp_sidecar_text(raw: &RawTags, prefix: &str, name: &str) -> Option<alloc::string::String> {
    raw.xmp_sidecar.iter().find_map(|p| {
        if p.prefix == prefix && p.name == name {
            Some(p.value.clone())
        } else {
            None
        }
    })
}
```

- [ ] **Step 4: 描述字段链首插 sidecar**

把 normalize.rs 中 description/copyright/title/creator 四处链改为 sidecar 居首（538-545 行附近 + creator 在 528-537）：

```rust
u.creator = xmp_sidecar_text(raw, "dc", "creator")
    .or_else(|| container_text(raw, ContainerSource::QuickTimeMdta, "com.apple.quicktime.author").map(alloc::string::String::from))
    .or_else(|| container_text(raw, ContainerSource::Udta, "©aut").map(alloc::string::String::from))
    .or_else(|| exif_primary_text(raw, TAG_ARTIST))
    .or_else(|| xmp_text(raw, "dc", "creator"))
    .or_else(|| png_text(raw, "Author"));
u.description = xmp_sidecar_text(raw, "dc", "description")
    .or_else(|| exif_primary_text(raw, TAG_IMAGE_DESCRIPTION))
    .or_else(|| xmp_text(raw, "dc", "description"))
    .or_else(|| png_text(raw, "Description"));
u.copyright = xmp_sidecar_text(raw, "dc", "rights")
    .or_else(|| exif_primary_text(raw, TAG_COPYRIGHT))
    .or_else(|| xmp_text(raw, "dc", "rights"))
    .or_else(|| png_text(raw, "Copyright"));
u.title = xmp_sidecar_text(raw, "dc", "title")
    .or_else(|| xmp_text(raw, "dc", "title"))
    .or_else(|| png_text(raw, "Title"));
```

> 注意 creator 原链中容器档用 `container_text(...).map(String::from)` 后接 `.or_else`；上方已把 `.map` 收进各自 `or_else` 闭包以保持 sidecar 居首的类型一致（全 `Option<String>`）。改写后逐项核对值类型一致。

- [ ] **Step 5: 运行确认通过 + 零回归**

Run: `cargo test -p omni-meta-core --all-features 2>&1 | tail -20`
Expected: 新测试 PASS；既有描述/creator 测试（`png_creator_does_not_override_xmp` 等）仍 PASS（sidecar 为空时链行为不变）。

- [ ] **Step 6: 提交**

```bash
git add omni-meta-core/src/normalize.rs
git commit -m "feat(normalize): 描述字段 sidecar 优先（title/description/creator/copyright）+ xmp_sidecar_text"
```

---

## Task 3: `Metadata::with_xmp_sidecar` method

**Files:**
- Create: `omni-meta-core/src/sidecar.rs`
- Modify: `omni-meta-core/src/lib.rs:8-21`（加 `pub(crate) mod sidecar;`）
- Test: `omni-meta-core/src/sidecar.rs`

- [ ] **Step 1: 注册模块**

`omni-meta-core/src/lib.rs` 的 `pub(crate) mod ...;` 区（8-21 行）按字母序插入：

```rust
pub(crate) mod sidecar;
```

- [ ] **Step 2: 写失败测试（注入 + provenance + 一致性）**

新建 `omni-meta-core/src/sidecar.rs`，仅放测试（method 未写 → 编译失败）：

```rust
//! `Metadata::with_xmp_sidecar`：解析后注入旁挂 .xmp sidecar。

#[cfg(test)]
mod tests {
    use crate::adapters::slice::{Options, read_slice};
    use alloc::vec::Vec;

    /// 最小 JPEG：SOI + APP1(Exif TIFF: IFD0 Make="Acme") + EOI。无 description。
    fn jpeg_with_make() -> Vec<u8> {
        let mut t: Vec<u8> = Vec::new();
        t.extend_from_slice(b"II");
        t.extend_from_slice(&42u16.to_le_bytes());
        t.extend_from_slice(&8u32.to_le_bytes());
        t.extend_from_slice(&1u16.to_le_bytes()); // IFD0 count=1
        t.extend_from_slice(&0x010Fu16.to_le_bytes()); // Make
        t.extend_from_slice(&2u16.to_le_bytes()); // ASCII
        t.extend_from_slice(&5u32.to_le_bytes()); // count=5
        t.extend_from_slice(&26u32.to_le_bytes()); // offset
        t.extend_from_slice(&0u32.to_le_bytes()); // next IFD
        t.extend_from_slice(b"Acme\0");
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(b"Exif\0\0");
        body.extend_from_slice(&t);
        let len = (body.len() + 2) as u16;
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
        j.extend_from_slice(&len.to_be_bytes());
        j.extend_from_slice(&body);
        j.extend_from_slice(&[0xFF, 0xD9]);
        j
    }

    const SIDECAR: &[u8] =
        br#"<rdf:Description xmlns:dc="n" dc:description="Sunset" dc:subject="beach"/>"#;

    #[test]
    fn sidecar_injects_description_and_keeps_provenance() {
        let img = jpeg_with_make();
        let meta = read_slice(&img, Options::default())
            .unwrap()
            .with_xmp_sidecar(SIDECAR, crate::limits::Limits::default());
        // 描述字段从 sidecar 注入
        assert_eq!(meta.unified.description.as_deref(), Some("Sunset"));
        // provenance：sidecar 属性落 xmp_sidecar，不污染内嵌 xmp
        assert!(meta.raw.xmp_sidecar.iter().any(|p| p.name == "description"));
        assert!(meta.raw.xmp.iter().all(|p| p.name != "description"));
        // keywords（dc:subject）留 raw 层，Unified 无对应字段
        assert!(meta.raw.xmp_sidecar.iter().any(|p| p.name == "subject"));
    }

    #[test]
    fn with_sidecar_unified_matches_manual_renormalize() {
        let img = jpeg_with_make();
        let meta = read_slice(&img, Options::default()).unwrap();
        let merged = meta.clone().with_xmp_sidecar(SIDECAR, crate::limits::Limits::default());
        // 手动路径：把同样的 sidecar props 预置进 raw 后直接 normalize
        let mut raw = meta.raw.clone();
        let mut w = Vec::new();
        crate::codecs::xmp::decode(
            SIDECAR,
            &mut raw.xmp_sidecar,
            &mut w,
            &crate::limits::Limits::default(),
        );
        let manual = crate::normalize::normalize(&raw, &meta.structural, &mut w);
        assert_eq!(merged.unified, manual); // Unified 字节级一致
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test -p omni-meta-core --all-features sidecar 2>&1 | tail -20`
Expected: 编译失败，`no method named with_xmp_sidecar`。

- [ ] **Step 4: 实现 method**

在 `omni-meta-core/src/sidecar.rs` 顶部（`#[cfg(test)]` 之前）加入：

```rust
use alloc::vec::Vec;

use crate::codecs;
use crate::limits::Limits;
use crate::model::{Metadata, XmpProperty};
use crate::normalize::normalize;

impl Metadata {
    /// 把一段 `.xmp` sidecar 字节折进已解析结果，返回更新后的 Metadata。
    ///
    /// sidecar 经 XMP codec 解析后落 `raw.xmp_sidecar`，随后基于保留的
    /// `structural` 快照重跑 normalize 重投影 `unified`。`packet` 仅在本次调用内借用。
    /// 空/无效 UTF-8/超 `max_payload_bytes` → 一条 Truncated 告警，`unified` 不变
    /// （与 XMP codec 既有失败语义一致）。
    ///
    /// 告警去重：sidecar 来源的 normalize 回退分支均静默，故重投影的 normalize
    /// 告警等同解析期已记录者——丢弃重投影告警，仅保留 XMP 解码自身告警。
    pub fn with_xmp_sidecar(mut self, packet: &[u8], limits: Limits) -> Self {
        let mut props: Vec<XmpProperty> = Vec::new();
        // 解码告警（truncated/无效 UTF-8/超限）是新增的 → 保留进 self.warnings。
        codecs::xmp::decode(packet, &mut props, &mut self.warnings, &limits);
        self.raw.xmp_sidecar.extend(props);
        // 重投影 Unified；normalize 告警丢弃（见 doc）。
        let mut discard: Vec<crate::model::Warning> = Vec::new();
        self.unified = normalize(&self.raw, &self.structural, &mut discard);
        self
    }
}
```

- [ ] **Step 5: 运行确认通过**

Run: `cargo test -p omni-meta-core --all-features sidecar 2>&1 | tail -20`
Expected: 两测试 PASS。

- [ ] **Step 6: 提交**

```bash
git add omni-meta-core/src/sidecar.rs omni-meta-core/src/lib.rs
git commit -m "feat(sidecar): Metadata::with_xmp_sidecar 解析后注入 + Unified 一致性"
```

---

## Task 4: 技术字段 sidecar 兜底（make/model/orientation/width/height）

**Files:**
- Modify: `omni-meta-core/src/normalize.rs`（make/model ~456-471；orientation/dims XMP 回退循环 433-454）
- Test: `omni-meta-core/src/normalize.rs`

- [ ] **Step 1: 写失败测试（内嵌胜 + 缺失时 sidecar 兜底）**

normalize.rs 测试模块加入：

```rust
#[test]
fn sidecar_make_only_fills_when_embedded_absent() {
    use crate::model::{ExifTag, IfdKind, RawTags, Value};
    // (a) 内嵌 EXIF Make 存在 → sidecar 不得覆盖
    let raw_a = RawTags {
        exif: alloc::vec![ExifTag {
            ifd: IfdKind::Primary,
            tag: 0x010F,
            value: Value::Text(alloc::string::String::from("EmbeddedCam")),
        }],
        xmp: alloc::vec![],
        xmp_sidecar: alloc::vec![xmp_p("tiff", "Make", "SidecarCam")],
        container: alloc::vec![],
        text: alloc::vec![],
    };
    let mut w = alloc::vec::Vec::new();
    assert_eq!(
        normalize(&raw_a, &StructuralFields::default(), &mut w).camera_make.as_deref(),
        Some("EmbeddedCam")
    );
    // (b) 内嵌全缺 → sidecar 兜底
    let raw_b = RawTags {
        exif: alloc::vec![],
        xmp: alloc::vec![],
        xmp_sidecar: alloc::vec![xmp_p("tiff", "Make", "SidecarCam")],
        container: alloc::vec![],
        text: alloc::vec![],
    };
    assert_eq!(
        normalize(&raw_b, &StructuralFields::default(), &mut w).camera_make.as_deref(),
        Some("SidecarCam")
    );
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p omni-meta-core sidecar_make_only_fills -- --nocapture`
Expected: FAIL，(b) 分支 `camera_make` 为 `None`。

- [ ] **Step 3: make/model 链末位加 sidecar 档**

normalize.rs `u.camera_make` / `u.camera_model`（456-471）各在末尾追加：

```rust
u.camera_make = container_text(raw, ContainerSource::QuickTimeMdta, "com.apple.quicktime.make")
    .map(alloc::string::String::from)
    .or_else(|| exif_primary_text(raw, TAG_MAKE))
    .or_else(|| xmp_text(raw, "tiff", "Make"))
    .or_else(|| xmp_sidecar_text(raw, "tiff", "Make"));
u.camera_model = container_text(raw, ContainerSource::QuickTimeMdta, "com.apple.quicktime.model")
    .map(alloc::string::String::from)
    .or_else(|| exif_primary_text(raw, TAG_MODEL))
    .or_else(|| xmp_text(raw, "tiff", "Model"))
    .or_else(|| xmp_sidecar_text(raw, "tiff", "Model"));
```

- [ ] **Step 4: orientation/width/height 加 sidecar 兜底循环**

在 normalize.rs 既有 `for p in &raw.xmp { match ... }`（433-454）**之后**，紧接一段对 `raw.xmp_sidecar` 的同构循环（仅在槽仍为 None 时填充，故技术字段内嵌恒胜）：

```rust
// sidecar XMP 兜底：仅填内嵌（EXIF + 内嵌 xmp）仍缺的技术槽。
for p in &raw.xmp_sidecar {
    match (p.prefix.as_str(), p.name.as_str()) {
        ("tiff", "Orientation") if u.orientation.is_none() => {
            if let Ok(v) = p.value.parse::<u16>()
                && let Some(o) = Orientation::from_u16(v)
            {
                u.orientation = Some(o);
            }
        }
        ("tiff", "ImageWidth") if u.width.is_none() => {
            if let Ok(v) = p.value.parse::<u32>() {
                u.width = Some(v);
            }
        }
        ("tiff", "ImageLength") if u.height.is_none() => {
            if let Ok(v) = p.value.parse::<u32>() {
                u.height = Some(v);
            }
        }
        _ => {}
    }
}
```

> 放置位置：紧接内嵌 `for p in &raw.xmp { ... }` 之后（约 454 行）。**无需**移到函数尾。normalize 尾部（约 554-565）的 `u.width = structural.width.or(u.width)` / `u.gps = structural.gps.or(u.gps)` 在此循环**之后**执行，故结构来源仍恒压过 sidecar——最终优先级「结构 > EXIF > 内嵌 xmp > sidecar」由「sidecar 仅在槽为 None 时填 + 结构在尾部 `.or` 覆盖」共同保证。

- [ ] **Step 5: 运行确认通过 + 零回归**

Run: `cargo test -p omni-meta-core --all-features 2>&1 | tail -20`
Expected: 新测试 PASS；`container_dims_beat_xmp_dims` 等仍 PASS。

- [ ] **Step 6: 提交**

```bash
git add omni-meta-core/src/normalize.rs
git commit -m "feat(normalize): 技术字段 sidecar 兜底（make/model/orientation/dims，内嵌恒胜）"
```

---

## Task 5: GPS sidecar 兜底（泛化 `gps_from_xmp`）

**Files:**
- Modify: `omni-meta-core/src/normalize.rs`（`gps_from_xmp` 187-201；调用点 512-514）
- Test: `omni-meta-core/src/normalize.rs`

- [ ] **Step 1: 写失败测试（EXIF 无 GPS 时 sidecar exif:GPS* 兜底）**

```rust
#[test]
fn sidecar_gps_fills_when_exif_absent() {
    use crate::model::RawTags;
    let raw = RawTags {
        exif: alloc::vec![],
        xmp: alloc::vec![],
        xmp_sidecar: alloc::vec![
            xmp_p("exif", "GPSLatitude", "37,48.0N"),
            xmp_p("exif", "GPSLongitude", "122,25.0W"),
        ],
        container: alloc::vec![],
        text: alloc::vec![],
    };
    let mut w = alloc::vec::Vec::new();
    let g = normalize(&raw, &StructuralFields::default(), &mut w).gps;
    assert!(g.is_some(), "sidecar exif:GPS* 应兜底投影");
    let g = g.unwrap();
    assert!(g.lat_e7 > 0 && g.lon_e7 < 0); // N 正、W 负
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p omni-meta-core sidecar_gps_fills -- --nocapture`
Expected: FAIL，`gps` 为 `None`。

- [ ] **Step 3: 泛化 `gps_from_xmp` 为按属性切片**

把 normalize.rs `gps_from_xmp`（187-201）改为对属性切片操作，并保留按 `raw` 的薄包装：

```rust
/// XMP 回退坐标：从给定属性切片读 exif:GPSLatitude/Longitude。lat+lon 都成功才 Some。
fn gps_from_xmp_props(props: &[XmpProperty]) -> Option<Gps> {
    let get = |name: &str| {
        props
            .iter()
            .find(|p| p.prefix == "exif" && p.name == name)
            .map(|p| p.value.as_str())
    };
    let lat = parse_xmp_coord(get("GPSLatitude")?)?;
    let lon = parse_xmp_coord(get("GPSLongitude")?)?;
    Some(Gps { lat_e7: lat, lon_e7: lon, alt_mm: None })
}
```

并在 normalize.rs 顶部 `use crate::model::{...}` 加入 `XmpProperty`（若尚未导入）。

- [ ] **Step 4: GPS 投影链接 sidecar 兜底**

normalize.rs GPS 投影段（498-515）改为：EXIF GPS IFD > 内嵌 xmp exif:GPS* > sidecar exif:GPS*：

```rust
if let Some(g) = gps_from_exif(raw) {
    u.gps = Some(g);
} else {
    if has_gps_exif {
        warnings.push(Warning { offset: 0, kind: WarnKind::UnrecognizedValue });
    }
    u.gps = gps_from_xmp_props(&raw.xmp).or_else(|| gps_from_xmp_props(&raw.xmp_sidecar));
}
```

> 既有 `gps_from_xmp(raw)` 的其余调用点（若有）一并改为 `gps_from_xmp_props(&raw.xmp)`。grep 确认仅此一处使用。

- [ ] **Step 5: 运行确认通过 + 零回归**

Run: `cargo test -p omni-meta-core --all-features 2>&1 | tail -20`
Expected: 新测试 PASS；既有 `gps_from_xmp_*` 测试仍 PASS。

- [ ] **Step 6: 提交**

```bash
git add omni-meta-core/src/normalize.rs
git commit -m "feat(normalize): GPS sidecar 兜底（gps_from_xmp 泛化扫 sidecar）"
```

---

## Task 6: 退化路径 + 幂等叠加（sidecar.rs 测试补全）

**Files:**
- Modify: `omni-meta-core/src/sidecar.rs`（`#[cfg(test)] mod tests`）

- [ ] **Step 1: 写测试（空/无效 UTF-8/幂等）**

在 sidecar.rs 测试模块追加：

```rust
#[test]
fn empty_sidecar_no_change_no_warning() {
    let img = jpeg_with_make();
    let meta = read_slice(&img, Options::default()).unwrap();
    let before_warns = meta.warnings.len();
    let after = meta.clone().with_xmp_sidecar(b"", crate::limits::Limits::default());
    assert_eq!(after.unified, meta.unified); // 空包不改 Unified
    assert_eq!(after.warnings.len(), before_warns); // 空包不增告警
    assert!(after.raw.xmp_sidecar.is_empty());
}

#[test]
fn invalid_utf8_sidecar_warns_truncated_unified_unchanged() {
    use crate::model::WarnKind;
    let img = jpeg_with_make();
    let meta = read_slice(&img, Options::default()).unwrap();
    let after = meta
        .clone()
        .with_xmp_sidecar(&[0xFF, 0xFE, 0x00], crate::limits::Limits::default());
    assert_eq!(after.unified, meta.unified); // 无效字节不改 Unified
    assert!(after.warnings.iter().any(|w| w.kind == WarnKind::Truncated));
}

#[test]
fn double_sidecar_is_stable() {
    let img = jpeg_with_make();
    let meta = read_slice(&img, Options::default()).unwrap();
    let once = meta
        .clone()
        .with_xmp_sidecar(SIDECAR, crate::limits::Limits::default());
    let twice = once
        .clone()
        .with_xmp_sidecar(SIDECAR, crate::limits::Limits::default());
    assert_eq!(twice.unified, once.unified); // 重复注入同一 sidecar，Unified 稳定
}
```

- [ ] **Step 2: 运行确认通过**

Run: `cargo test -p omni-meta-core --all-features sidecar 2>&1 | tail -20`
Expected: 全部 PASS。

> 若 `empty_sidecar_no_change_no_warning` 失败于告警计数：核对 `codecs::xmp::decode` 对空包是否产告警——空 `&[u8]` 经 `from_utf8` 得 `Ok("")`、扫描无属性、不产告警（预期通过）。若 `invalid_utf8` 未见 Truncated：核对 decode 的无效 UTF-8 分支（xmp.rs ~26-33）。

- [ ] **Step 3: 提交**

```bash
git add omni-meta-core/src/sidecar.rs
git commit -m "test(sidecar): 退化路径（空/无效 UTF-8）+ 幂等叠加"
```

---

## Task 7: no_std 构建验证 + ROADMAP 勾选

**Files:**
- Modify: `docs/ROADMAP.md`（里程碑 H）

- [ ] **Step 1: no_std 构建**

Run: `cargo build -p omni-meta-core --no-default-features --features alloc 2>&1 | tail -10`
（特性名以 crate 现有 `Cargo.toml` 为准；与既有 CI/构建命令一致——若仓库用别的 no_std 验证命令，照搬。）
Expected: 成功，无 `std` 泄漏报错。

- [ ] **Step 2: 全量测试 + facade 透出确认**

Run: `cargo test --all-features 2>&1 | tail -20`
Expected: 全绿。facade 经 `pub use omni_meta_core::*;` 自动透出 `with_xmp_sidecar`，无需新测试；如需可加一行 facade 冒烟（可选，非必需）。

- [ ] **Step 3: 勾选 ROADMAP 里程碑 H**

`docs/ROADMAP.md` 里程碑 H：标题行 `⬜ 设计已定` 改 `✅ 完成`，补 `计划 plans/2026-06-28-xmp-sidecar-merge.md`；勾选 4 个 `- [ ]` → `- [x]`（created 兜底保持「不在本期」）。同步 §1「尚未开始」清单移除该项或标注完成。

- [ ] **Step 4: 提交**

```bash
git add docs/ROADMAP.md
git commit -m "docs(roadmap): 勾选里程碑 H XMP sidecar 合并"
```

---

## Self-Review 记录

- **spec 覆盖**：API method（T3）、RawTags 列 + structural 快照（T1）、描述 sidecar 胜（T2）、技术内嵌胜 make/model/dims（T4）、GPS 兜底（T5）、keywords 留 raw（T3 测试断言）、退化/幂等（T6）、no_std + ROADMAP（T7）。spec §5 中 `created` 兜底已在 spec 明确「本期不做」，无对应 task（一致）。
- **类型一致**：`xmp_sidecar_text`/`gps_from_xmp_props` 全程同名；`StructuralFields` 迁 model 后各处 `crate::model::StructuralFields`；`RawTags.xmp_sidecar`、`Metadata.structural` 命名贯穿。
- **占位符**：无 TBD；每个改动给出完整代码与命令。
