# 容器投影向 normalize 收敛 — 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把容器元数据(make/model/created)的「→Unified」投影从 parser 侧 `Event::Field` 收敛到 normalize,使 normalize 成为每个 Unified 字段的唯一优先级权威。

**Architecture:** 引入 `StructuralFields`(二进制结构候选,由 `Collector` 从 `Event::Field` 累积),`normalize` 增此入参并逐字段套用完整优先级阶梯;`finalize` 不再事后覆盖。make/model/created 改由 normalize 从 `RawTags.container` 文本标签派生;GPS 因阶梯交错二进制源(`©xyz>mdta>loci`)整体留 parser(成文例外)。

**Tech Stack:** Rust,`#![no_std]` + `alloc`,`#![forbid(unsafe_code)]`;cargo workspace(omni-meta-core / omni-meta / omni-meta-fixtures)。

**设计依据:** [`specs/2026-06-19-container-projection-convergence-design.md`](../specs/2026-06-19-container-projection-convergence-design.md)

**全局安全网(每个 Task 结束都应仍绿):**
```bash
cargo test -p omni-meta-core
cargo test -p omni-meta            # 含 MP4/MOV 黄金样本 + 四适配器差分
```
黄金样本(make/model/created/gps)必须**逐字节不变**——这是证明纯内部重构未改变可观测行为的核心证据。

---

## Task 1: 引入 `StructuralFields` + normalize 接管结构字段排序(行为保持)

把 `finalize` 中对 width/height/duration/created/gps 的事后覆盖,搬进 `normalize`,经新入参 `StructuralFields` 传入。**make/model 暂不动**(仍由 parser 投影 + finalize 覆盖,下个 Task 处理)。本 Task 行为完全不变,由现有测试守护;新增一条直接验证 normalize 接收结构字段的单测。

**Files:**
- Modify: `omni-meta-core/src/normalize.rs`(新增 `StructuralFields`;`normalize` 改签名;末尾套用结构候选)
- Modify: `omni-meta-core/src/driver.rs:110-148`(`finalize` 构造 `StructuralFields`、删除 dims/duration/created/gps 覆盖块)
- Modify: 全部 `normalize(&.., &mut ..)` 测试调用点(normalize.rs 内约 30 处)

- [ ] **Step 1: 写失败测试**(normalize.rs 的 `#[cfg(test)] mod tests` 内追加)

```rust
    #[test]
    fn structural_created_outranks_exif_derived() {
        use crate::model::DateTimeParts;
        // EXIF DateTime(IFD0 0x0132)=2003,结构候选=2018
        let mut raw = RawTags::default();
        raw.exif.push(make_exif_tag(IfdKind::Primary, 0x0132, "2003:01:02 03:04:05"));
        let structural = StructuralFields {
            created: Some(DateTimeParts {
                year: 2018, month: 1, day: 1, hour: 0, minute: 0, second: 0, tz_offset_min: Some(0),
            }),
            ..StructuralFields::default()
        };
        let mut w = Vec::new();
        let u = normalize(&raw, &structural, &mut w);
        assert_eq!(u.created.map(|d| d.year), Some(2018));
    }
```
> `make_exif_tag` 是 normalize 测试已有辅助(见 `normalize.rs` 既有测试,签名 `(IfdKind, u16, &str) -> ExifTag`);`RawTags::default()` 已存在。

- [ ] **Step 2: 运行,确认编译失败**

Run: `cargo test -p omni-meta-core structural_created_outranks_exif_derived`
Expected: 编译错误——`StructuralFields` 未定义 / `normalize` 仅接受 2 参数。

- [ ] **Step 3: 定义 `StructuralFields` 并改 `normalize` 签名**

在 `normalize.rs` 顶部(`pub fn normalize` 之前)新增:
```rust
/// 二进制结构来源候选(无命名空间、parser 权威):容器结构头与二进制 udta。
/// 由 `driver::Collector` 从 `Event::Field` 累积,作为 normalize 的一类来源传入。
#[derive(Debug, Clone, Default)]
pub struct StructuralFields {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub created: Option<crate::model::DateTimeParts>,
    pub gps: Option<crate::model::Gps>,
}
```

把签名
```rust
pub fn normalize(raw: &RawTags, warnings: &mut Vec<Warning>) -> Unified {
```
改为
```rust
pub fn normalize(raw: &RawTags, structural: &StructuralFields, warnings: &mut Vec<Warning>) -> Unified {
```

在 `normalize` 函数体**最末尾**(`u` 返回之前)追加结构候选套用——结构源压过 XMP/EXIF 派生(复现旧 `finalize` 覆盖语义):
```rust
    // 结构字段(二进制 parser 权威):压过 XMP/EXIF 派生(复现旧 finalize 覆盖)。
    u.width = structural.width.or(u.width);
    u.height = structural.height.or(u.height);
    u.duration_ms = structural.duration_ms.or(u.duration_ms);
    u.created = structural.created.clone().or(u.created);
    u.gps = structural.gps.or(u.gps);
```

- [ ] **Step 4: 更新 `finalize`(driver.rs:110-148)**

把 `finalize` 中
```rust
    let mut unified = normalize(&raw, &mut warnings);
    if let Some(w) = width { unified.width = Some(w); }
    if let Some(h) = height { unified.height = Some(h); }
    if let Some(d) = duration_ms { unified.duration_ms = Some(d); }
    if let Some(c) = created { unified.created = Some(c); }
    if let Some(g) = gps { unified.gps = Some(g); }
    if let Some(m) = camera_make { unified.camera_make = Some(m); }
    if let Some(m) = camera_model { unified.camera_model = Some(m); }
```
替换为(保留 make/model 覆盖,其余移入 normalize):
```rust
    let structural = crate::normalize::StructuralFields {
        width,
        height,
        duration_ms,
        created,
        gps,
    };
    let mut unified = normalize(&raw, &structural, &mut warnings);
    if let Some(m) = camera_make { unified.camera_make = Some(m); }
    if let Some(m) = camera_model { unified.camera_model = Some(m); }
```
> `width/height/duration_ms/created/gps/camera_make/camera_model` 是 `finalize` 开头从 `col` 解构出的本地变量(见 driver.rs:111-113),保持不变。

- [ ] **Step 5: 批量更新 normalize 测试调用点**

normalize.rs 内所有 `normalize(<X>, &mut <W>)` 改为 `normalize(<X>, &StructuralFields::default(), &mut <W>)`。约 30 处(行号见 grep:710/730/759/780/806/828/849/865/883/903/959/1005/1031/1062/1144/1161/1179/1206/1225/1239/1257/1264/1272/1278/1284/1306/1317/1326/1344/1362/1376)。测试 mod 顶部确保 `use super::StructuralFields;` 可见(`use super::*;` 已涵盖)。

Run: `cargo test -p omni-meta-core 2>&1 | grep -E "error|normalize" | head`
Expected(逐条修完后):无编译错误。

- [ ] **Step 6: 运行全套,确认绿**

Run: `cargo test -p omni-meta-core && cargo test -p omni-meta`
Expected: PASS(含新 `structural_created_outranks_exif_derived`、既有 `container_created_beats_exif_derived`、黄金样本)。

- [ ] **Step 7: fmt + commit**

```bash
cargo fmt -p omni-meta-core
git add omni-meta-core/src/normalize.rs omni-meta-core/src/driver.rs
git commit -m "refactor(normalize): 引入 StructuralFields，结构字段排序由 finalize 移入 normalize（行为保持）"
```

---

## Task 2: `make`/`model` 迁入 normalize 容器阶梯,删除 parser 投影

normalize 用与 `software`/`creator` 相同的链式模板派生 make/model(容器 mdta > EXIF > XMP)。删除 parser 的 `Field::CameraMake/CameraModel` 投影、`MoovInfo`/`QtMdta` 的 make/model 字段、`Field` 枚举的两个变体、`Collector` 两字段及 finalize 覆盖。

**Files:**
- Modify: `omni-meta-core/src/normalize.rs`(make/model 改链式 + 删 EXIF/XMP 散落臂 + 新单测)
- Modify: `omni-meta-core/src/formats/bmff.rs`(删 make/model 解析与发射)
- Modify: `omni-meta-core/src/driver.rs`(删 Collector 字段、handle 臂、finalize 覆盖、改一个测试)
- Modify: `omni-meta-core/src/model.rs`(删 `Field::CameraMake/CameraModel` 变体 + 改一个测试)

- [ ] **Step 1: 写失败测试**(normalize.rs tests 内)

```rust
    #[test]
    fn container_mdta_make_model_outrank_exif() {
        use crate::model::{ContainerSource, ContainerTag, Value};
        let mut raw = RawTags::default();
        // EXIF make/model 存在
        raw.exif.push(make_exif_tag(IfdKind::Primary, 0x010F, "ExifMake"));
        raw.exif.push(make_exif_tag(IfdKind::Primary, 0x0110, "ExifModel"));
        // 容器 mdta make/model 应压过 EXIF
        raw.container.push(ContainerTag {
            source: ContainerSource::QuickTimeMdta,
            key: alloc::string::String::from("com.apple.quicktime.make"),
            value: Value::Text(alloc::string::String::from("Apple")),
        });
        raw.container.push(ContainerTag {
            source: ContainerSource::QuickTimeMdta,
            key: alloc::string::String::from("com.apple.quicktime.model"),
            value: Value::Text(alloc::string::String::from("iPhone 15")),
        });
        let mut w = Vec::new();
        let u = normalize(&raw, &StructuralFields::default(), &mut w);
        assert_eq!(u.camera_make.as_deref(), Some("Apple"));
        assert_eq!(u.camera_model.as_deref(), Some("iPhone 15"));
    }
```

- [ ] **Step 2: 运行,确认失败**

Run: `cargo test -p omni-meta-core container_mdta_make_model_outrank_exif`
Expected: FAIL——当前 normalize 不读 container 的 make/model,返回 `ExifMake`/`ExifModel`。

- [ ] **Step 3: normalize 改链式派生 make/model**

删除 EXIF 主循环中的 make/model 臂(`normalize.rs:403-404`):
```rust
            (TAG_MAKE, Value::Text(s)) => u.camera_make = Some(s.clone()),
            (TAG_MODEL, Value::Text(s)) => u.camera_model = Some(s.clone()),
```
删除 XMP 回退中的 make/model 臂(`normalize.rs:418-423`):
```rust
            ("tiff", "Make") if u.camera_make.is_none() => {
                u.camera_make = Some(p.value.clone());
            }
            ("tiff", "Model") if u.camera_model.is_none() => {
                u.camera_model = Some(p.value.clone());
            }
```
在 `software`/`creator` 链式块之前(`normalize.rs:488` 附近)新增,与其同构:
```rust
    // make/model:容器 mdta > EXIF(0x010F/0x0110) > XMP(tiff:)
    u.camera_make = container_text(raw, ContainerSource::QuickTimeMdta, "com.apple.quicktime.make")
        .map(alloc::string::String::from)
        .or_else(|| exif_primary_text(raw, TAG_MAKE))
        .or_else(|| xmp_text(raw, "tiff", "Make"));
    u.camera_model = container_text(raw, ContainerSource::QuickTimeMdta, "com.apple.quicktime.model")
        .map(alloc::string::String::from)
        .or_else(|| exif_primary_text(raw, TAG_MODEL))
        .or_else(|| xmp_text(raw, "tiff", "Model"));
```
> `TAG_MAKE`/`TAG_MODEL` 常量已在 normalize.rs(EXIF 臂原用),`exif_primary_text`/`xmp_text`/`container_text` 均已存在(software/creator 在用)。

- [ ] **Step 4: 运行新测试 + 既有 EXIF/XMP make/model 测试,确认绿**

Run: `cargo test -p omni-meta-core container_mdta_make_model_outrank_exif exif_wins_over_xmp xmp_fills_when_exif_absent`
Expected: PASS(链式保持 EXIF>XMP,且容器最高)。

- [ ] **Step 5: 删除 parser 的 make/model 解析与发射(bmff.rs)**

`MoovInfo`(`bmff.rs:126-135`)删字段:
```rust
    camera_make: Option<alloc::string::String>,
    camera_model: Option<alloc::string::String>,
```
`MoovInfo` 初始化(`bmff.rs:142-151`)删对应 `camera_make: None, camera_model: None,`。
`QtMdta`(其定义处,`bmff.rs:1118` 附近的 struct)删 `make`/`model` 字段及其初始化(`bmff.rs:156-157` 等处)。
`parse_qt_mdta` 删两个 match 臂(`bmff.rs:1179-1192`):
```rust
            "com.apple.quicktime.make" => { ... }
            "com.apple.quicktime.model" => { ... }
```
(**保留** `location.ISO6709` 臂、`creationdate` 臂暂留待 Task 3、UTF-8 推 container 块)。
`parse_moov` 删(`bmff.rs:213-218` 的 `if mdta.make.is_none()`/`model` 合并)与(`bmff.rs:232-233`):
```rust
    info.camera_make = mdta.make;
    info.camera_model = mdta.model;
```
事件发射删(`bmff.rs:849-854`):
```rust
            if let Some(make) = info.camera_make { events.push(Event::Field(Field::CameraMake(make))); }
            if let Some(model) = info.camera_model { events.push(Event::Field(Field::CameraModel(model))); }
```

- [ ] **Step 6: 删除 `Field` 变体与 Collector 承接(model.rs / driver.rs)**

`model.rs:116-119` 删:
```rust
    /// 相机厂商（容器原生，如 QuickTime mdta）。
    CameraMake(String),
    /// 相机型号（容器原生，如 QuickTime mdta）。
    CameraModel(String),
```
`model.rs` Field 相等测试(`model.rs:395-402`)删两段 `assert_eq!/assert_ne!`(CameraMake/CameraModel)。
`driver.rs` `Collector` 结构(`driver.rs:25-26`)删 `camera_make`/`camera_model` 字段;`handle` 删两臂(`driver.rs:83-92`);全部 `Collector { .. }` 字面构造(`driver.rs:171`、`390`、`1210` 附近)删 `camera_make: None, camera_model: None,` 行;`finalize` 删 Step(Task1)保留的两行 make/model 覆盖:
```rust
    if let Some(m) = camera_make { unified.camera_make = Some(m); }
    if let Some(m) = camera_model { unified.camera_model = Some(m); }
```
及 `finalize` 开头解构里的 `camera_make`/`camera_model`(driver.rs:113)。

- [ ] **Step 7: 改写 driver 集成测试 `collector_applies_gps_make_model_fields`(driver.rs:1166-1201)**

make/model 不再走 `Field`,改经 `ContainerTag`;gps 仍走 `Field`。整段替换为:
```rust
    #[test]
    fn collector_applies_gps_field_and_container_make_model() {
        use crate::model::{ContainerSource, ContainerTag, Gps, Value};
        struct Emitter;
        impl MetaParser for Emitter {
            fn pull<'a>(&mut self, _input: &'a [u8]) -> crate::demand::PullResult<'a> {
                crate::demand::PullResult {
                    demand: Demand::Done,
                    consumed: 0,
                    events: alloc::vec![
                        Event::Field(Field::Gps(Gps { lat_e7: 1, lon_e7: 2, alt_mm: Some(3) })),
                        Event::ContainerTag(ContainerTag {
                            source: ContainerSource::QuickTimeMdta,
                            key: alloc::string::String::from("com.apple.quicktime.make"),
                            value: Value::Text(alloc::string::String::from("Apple")),
                        }),
                        Event::ContainerTag(ContainerTag {
                            source: ContainerSource::QuickTimeMdta,
                            key: alloc::string::String::from("com.apple.quicktime.model"),
                            value: Value::Text(alloc::string::String::from("iPhone 15")),
                        }),
                    ],
                }
            }
        }
        let buf = [0u8; 4];
        let mut p = Emitter;
        let col = drive_slice(&buf, &mut p, Limits::default());
        let meta = finalize(col, FileFormat::Mov);
        assert_eq!(meta.unified.gps, Some(Gps { lat_e7: 1, lon_e7: 2, alt_mm: Some(3) }));
        assert_eq!(meta.unified.camera_make.as_deref(), Some("Apple"));
        assert_eq!(meta.unified.camera_model.as_deref(), Some("iPhone 15"));
    }
```

- [ ] **Step 8: 运行全套,确认绿(含黄金样本)**

Run: `cargo test -p omni-meta-core && cargo test -p omni-meta`
Expected: PASS。MP4/MOV 黄金样本 make/model 仍为期望值(现经 container_text 派生)。

- [ ] **Step 9: fmt + commit**

```bash
cargo fmt -p omni-meta-core
git add omni-meta-core/src/normalize.rs omni-meta-core/src/formats/bmff.rs omni-meta-core/src/driver.rs omni-meta-core/src/model.rs
git commit -m "refactor(normalize): make/model 迁入容器阶梯，删除 parser Field 投影与 CameraMake/Model 变体"
```

---

## Task 3: `created` 的 mdta 来源迁入 normalize,parser 仅留 mvhd

normalize 在 created 阶梯顶端加入容器 mdta `creationdate`(高于结构 mvhd/EBML);parser 停止 `mdta.created.or(mvhd)`,`created` 仅取自 mvhd。容器 creationdate 文本标签仍由 `parse_qt_mdta` 推入 `RawTags.container`(UTF-8 块),normalize 据此派生。

**Files:**
- Modify: `omni-meta-core/src/normalize.rs`(created 阶梯加容器 mdta 顶 + 新单测)
- Modify: `omni-meta-core/src/formats/bmff.rs`(删 `QtMdta.created`、creationdate 语义臂、`mdta.created.or`)

- [ ] **Step 1: 写失败测试**(normalize.rs tests 内)

```rust
    #[test]
    fn container_creationdate_outranks_structural_created() {
        use crate::model::{ContainerSource, ContainerTag, DateTimeParts, Value};
        let mut raw = RawTags::default();
        raw.container.push(ContainerTag {
            source: ContainerSource::QuickTimeMdta,
            key: alloc::string::String::from("com.apple.quicktime.creationdate"),
            value: Value::Text(alloc::string::String::from("2018-05-06T12:00:00+09:00")),
        });
        // 结构候选(mvhd)= 2001,容器 creationdate(2018)应胜出
        let structural = StructuralFields {
            created: Some(DateTimeParts {
                year: 2001, month: 1, day: 1, hour: 0, minute: 0, second: 0, tz_offset_min: Some(0),
            }),
            ..StructuralFields::default()
        };
        let mut w = Vec::new();
        let u = normalize(&raw, &structural, &mut w);
        assert_eq!(u.created.map(|d| d.year), Some(2018));
        assert_eq!(u.created.and_then(|d| d.tz_offset_min), Some(540)); // +09:00
    }
```

- [ ] **Step 2: 运行,确认失败**

Run: `cargo test -p omni-meta-core container_creationdate_outranks_structural_created`
Expected: FAIL——normalize 不读 container creationdate,返回结构候选 2001。

- [ ] **Step 3: normalize created 阶梯加容器 mdta 顶端**

在 Task 1 追加的结构套用块中,把
```rust
    u.created = structural.created.clone().or(u.created);
```
改为(容器 mdta creationdate > 结构 > EXIF/PNG):
```rust
    let container_created = container_text(
        raw,
        ContainerSource::QuickTimeMdta,
        "com.apple.quicktime.creationdate",
    )
    .and_then(parse_iso8601);
    u.created = container_created.or(structural.created.clone()).or(u.created);
```
> `parse_iso8601` 是 normalize.rs 内函数(bmff 原经 `crate::normalize::parse_iso8601` 调用),normalize 内可直呼 `parse_iso8601`。`ContainerSource` 需在 normalize 顶部 `use`(software/creator 已用 `ContainerSource::QuickTimeMdta`,已 import)。

- [ ] **Step 4: 运行新测试 + 既有 created 测试,确认绿**

Run: `cargo test -p omni-meta-core container_creationdate_outranks_structural_created structural_created_outranks_exif_derived`
Expected: PASS。

- [ ] **Step 5: parser 停止 mdta.created 语义(bmff.rs)**

`QtMdta` struct 删 `created` 字段及其初始化(`bmff.rs:158`、`1137` 附近)。
`parse_qt_mdta` 删 creationdate 语义臂(`bmff.rs:1193-1199`):
```rust
            "com.apple.quicktime.creationdate" => { ... out.created = parse_iso8601(s); ... }
```
(creationdate 仍命中下方 UTF-8 块推入 container——**保留该块**)。
`parse_moov` 删 mdta.created 合并(`bmff.rs:219-221`)与 mdta>mvhd 覆盖(`bmff.rs:229-230`):
```rust
                if mdta.created.is_none() { mdta.created = m.created; }
    // created：mdta creationdate 优先于 mvhd（mdta 带真实时区）。
    info.created = mdta.created.or(info.created);
```
`info.created` 即保持 mvhd 解析结果(`bmff.rs:167` `info.created = m.created;` 不动)。

- [ ] **Step 6: 运行全套,确认绿(含黄金样本)**

Run: `cargo test -p omni-meta-core && cargo test -p omni-meta`
Expected: PASS。MP4/MOV 黄金样本 created 仍为期望值(现:mdta creationdate 经 normalize container_text 派生,高于 mvhd 结构值)。

- [ ] **Step 7: fmt + commit**

```bash
cargo fmt -p omni-meta-core
git add omni-meta-core/src/normalize.rs omni-meta-core/src/formats/bmff.rs
git commit -m "refactor(normalize): created mdta creationdate 迁入容器阶梯顶，parser 仅留 mvhd"
```

---

## Task 4: GPS 例外成文 + 模块注释 + ROADMAP 勾选 + 全门禁

GPS 经 Task 1 已通过 `StructuralFields.gps`(parser 全解析 `©xyz>mdta>loci`)正确排在 EXIF/XMP 之上,无需再改逻辑。本 Task 落实文档与全套门禁。

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`(模块/函数头注释:GPS 留 parser 的成文例外)
- Modify: `omni-meta-core/src/normalize.rs`(模块头注释:normalize 为优先级唯一权威 + 结构字段一类来源)
- Modify: `docs/ROADMAP.md`(§4 勾选「容器元数据投影收敛」)

- [ ] **Step 1: bmff.rs `parse_moov` 头注释补 GPS 例外**

把 `parse_moov` 文档注释(`bmff.rs:137-140`)更新为反映现状,追加一句:
```rust
/// GPS 例外:其优先级阶梯交错二进制源与文本源(©xyz > mdta-ISO6709 > loci),
/// 故 GPS 整体在此解析为单一 Field::Gps;make/model/created 已迁 normalize。
```
并删去原注释里「取 GPS/make/model/creationdate」中已迁走的 make/model/creationdate 措辞,仅留 GPS。

- [ ] **Step 2: normalize.rs 模块头注释补权威声明**

在 `normalize.rs` 模块头(文件首注释)追加:
```rust
//! normalize 是每个 Unified 字段跨源优先级的唯一权威。来源分两类:
//! 文本/命名空间来源(RawTags.container / exif / xmp / png 文本)在此直接读取;
//! 二进制结构来源(容器结构头、二进制 udta)经 `StructuralFields` 由 driver 传入。
//! 例外:GPS 因阶梯交错二进制源,整体在 parser 解析后作为 StructuralFields.gps 传入。
```

- [ ] **Step 3: ROADMAP §4 勾选**

`docs/ROADMAP.md` 把「待评估:容器元数据投影收敛」条目从 `- [ ]` 改为 `- [x]`,并把描述更新为已完成:make/model/created 经 normalize 从 `RawTags.container` 统一解释;GPS 作成文例外留 parser;结构头字段经 `StructuralFields` 传入 normalize。引用本 plan 与 spec。

- [ ] **Step 4: 全门禁(对齐 CI:fmt / clippy / test / no_std / fuzz-build)**

Run:
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p omni-meta-core && cargo test -p omni-meta
cargo build -p omni-meta --target thumbv7em-none-eabi --no-default-features
( cd fuzz && cargo build --locked )
```
Expected: 全部 PASS / 零警告。
> 若本机未装 `thumbv7em-none-eabi` target:`rustup target add thumbv7em-none-eabi`。若 fuzz 构建需 nightly,按 `fuzz/README.md` 既有约定执行。

- [ ] **Step 5: commit**

```bash
git add omni-meta-core/src/formats/bmff.rs omni-meta-core/src/normalize.rs docs/ROADMAP.md
git commit -m "docs(normalize): GPS 留 parser 成文例外 + normalize 优先级权威声明 + ROADMAP 勾选收敛"
```

---

## 自检(写计划后,对照 spec)

- **§3 机制**:Task 1 引入 `StructuralFields` + 改签名 + finalize 改造。✓
- **§4 逐字段阶梯**:make/model(Task 2)、created(Task 3)、width/height/duration(Task 1)。software/creator 不变(无 Task,符合「不变」)。✓
- **§5 GPS 例外**:Task 1 经 StructuralFields.gps 排序 + Task 4 成文。✓
- **§6 parser 清理**:Task 2(make/model 字段/臂/发射/变体)、Task 3(created 字段/臂/合并)。✓
- **§7 验证**:每 Task 跑黄金样本;Task 4 全门禁(fmt/clippy/test/no_std/fuzz)。新单测覆盖容器>EXIF(make/model)、容器 creationdate>结构、结构 created>EXIF。✓
- **§8 范围**:无新 Unified 字段、不改 Value、不碰 Demand/IPTC/ICC/async。✓
- **类型一致**:`StructuralFields` 字段名(width/height/duration_ms/created/gps)在 Task 1 定义,Task 3 复用一致;`container_text`/`exif_primary_text`/`xmp_text`/`parse_iso8601` 均为既有符号。✓
- **占位符扫描**:无 TBD/TODO;每个改码 Step 含完整代码或精确删除指引。✓
