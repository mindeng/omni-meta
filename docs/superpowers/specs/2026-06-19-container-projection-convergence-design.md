# 容器投影向 normalize 收敛 — 设计

**状态**:已通过 brainstorming 评审 · 2026-06-19
**基准设计**:[`2026-06-14-omni-meta-design.md`](2026-06-14-omni-meta-design.md)
**ROADMAP**:兑现 [§4「待评估:容器元数据投影收敛」](../../ROADMAP.md)

---

## 1. 动机:单字段优先级分裂在三处

追踪 `created` 端到端,发现单个 Unified 字段的跨源优先级没有单一归属:

| 决策 | 现归属 | 机制 |
|---|---|---|
| mdta `creationdate` **>** mvhd 1904 | `formats/bmff.rs::parse_moov` | parser 内 `mdta.created.or(mvhd)` |
| 容器 **>** EXIF/XMP | `driver.rs::finalize` | `col.created` 事后覆盖 `normalize()` 结果 |
| EXIF `DateTimeOriginal` > `DateTime` > XMP | `normalize.rs` | normalize 内部 |

`gps`(parser 解 `©xyz > mdta > loci`,finalize 覆盖 normalize 的 EXIF/XMP)、`make`/`model`
(parser 取 mdta,finalize 覆盖 normalize 的 EXIF)同形。

`finalize`(`driver.rs:122-142`)做的是「容器 Field 一律压过 normalize」的钝覆盖,
仅因当前各来源恰不碰撞而成立。**核心债:跨源优先级无单一权威。**

## 2. 组织原则:文本标签 vs 二进制字段

代码已隐含、但从未命名的边界:

- **二进制结构字段** — 无命名空间、**parser 权威**:tkhd/ispe/IHDR 维度、mvhd/EBML 时长与
  1904/2001-UTC created、`©xyz`/`loci` GPS。走 `Event::Field`。
- **命名空间/文本来源** — **normalize 权威**:`RawTags.container`(mdta/udta 文本)、EXIF、XMP、PNG 文本。

**收敛目标:normalize 成为每个 Unified 字段的唯一优先级权威。** parser 不再解释容器*语义*,
只发原始标签 + 二进制结构字段。

## 3. 机制

### 3.1 `StructuralFields`

新增结构体承载二进制候选,由 `driver.rs` 的 `Collector` 从 `Event::Field` 累积:

```rust
struct StructuralFields {
    dims: Option<(u32, u32)>,
    duration_ms: Option<u64>,
    created: Option<DateTimeParts>,  // mvhd 1904 / EBML DateUTC(二进制结构来源)
    gps: Option<Gps>,                // 见 §5 GPS 例外
}
```

字段沿用 Collector 现有「首胜」累积语义(单文件单容器格式,不会多源竞争同一结构字段)。

### 3.2 normalize 签名变更

```rust
pub fn normalize(raw: &RawTags, structural: &StructuralFields, warnings: &mut Vec<Warning>) -> Unified
```

normalize 逐字段套用完整优先级阶梯(含 `structural` 候选的正确排位),直接返回已定值。

**`finalize` 不再事后覆盖** —— `driver.rs:122-142` 整段 `if let Some(w) = …` 删除;
`finalize` 只负责:收集 `StructuralFields` → 调 `normalize` → 组装 `Metadata`。

> **子决策(已确认):改 normalize 签名,而非另加 `project()` 包装。** normalize 即投影权威,
> 结构字段只是又一类来源。代价:约 20 个 normalize 单测改为传 `&StructuralFields::default()`
> (机械改动,语义即「无容器结构字段的图片」)。诚实性胜过双函数。

## 4. 逐字段优先级阶梯(全在 normalize)

沿用 normalize 现有 `software`/`creator` 的 `.or_else` 链式模板(`normalize.rs:488-509`):

| 字段 | 阶梯(高→低) | 变化 |
|---|---|---|
| `make`/`model` | 容器 mdta → EXIF(0x010F/0x0110) → XMP(`tiff:`) | **迁入**;parser 停止投影 |
| `created` | 容器 mdta `creationdate` → 结构(mvhd/EBML) → EXIF(DateTimeOriginal>DateTime) → PNG `Creation Time` | **迁入**;parser 停止 `mdta.or(mvhd)` |
| `width`/`height` | 结构(tkhd/ispe/IHDR/EBML) → XMP `tiff:ImageWidth/Length` | 行为不变,现显式落在 normalize |
| `duration_ms` | 仅结构(mvhd/EBML) | 平凡透传 |
| `software`/`creator`/`description`/`copyright`/`title` | (已在 normalize) | **不变** |

容器 mdta 键沿用现常量:`com.apple.quicktime.make` / `.model` / `.creationdate`
(经 `container_text(raw, ContainerSource::QuickTimeMdta, …)` 读取)。

## 5. GPS 例外(唯一交错阶梯)

GPS 是**唯一**优先级阶梯交错二进制与文本来源的字段:

```
©xyz(二进制) > mdta-ISO6709(文本) > loci(二进制) > EXIF > XMP
```

两个二进制源(`©xyz`、`loci`)夹着文本源(`mdta`)。若把它们塌缩成单个 `StructuralFields.gps`
候选,无法重建「文本源夹在两个二进制源之间」的次序 —— 只迁 mdta-GPS 会重新切分 GPS 阶梯,
**正好复活本设计要消灭的多处分裂债**。

**决策:GPS 整体留在 parser**(`parse_moov` 继续解 `©xyz > mdta-ISO6709 > loci` → 一个
`Event::Field(Gps)`);normalize 保留其下的 `gps_from_exif > gps_from_xmp` 兜底;
`StructuralFields.gps` 在 normalize 中排在 EXIF/XMP 之上。

总阶梯与今日逐字节一致。mdta `location.ISO6709` 标签仍以原始文本留在 `RawTags.container`
(无害冗余:raw 层照收,normalize 不据其推导 gps)。`©xyz`「不得泄漏进 container_tags」的
既有不变量与测试**不动**。

> 例外的判据已成文,便于将来评估:**当某字段的优先级阶梯仅含单一二进制源(在阶梯端点)时,
> 该字段可干净收敛;阶梯中二进制源交错夹住文本源时,留 parser 整体解析。**

## 6. parser 清理

- `MoovInfo` / `QtMdta` 删 `make` / `model` / `created` 字段(**保留** `gps`、`tags`)。
- `parse_qt_mdta` 删 `make` / `model` / `creationdate` 三个 match 臂;**保留** `location.ISO6709→gps`
  臂、**保留**把所有 UTF-8 文本标签推入 `container`、保留 focal length 整数臂。
- `parse_moov` 的 `created` 仅取自 mvhd;删去 `mdta.created.or(...)`、`info.camera_make/model = mdta.*`。
- `formats/bmff.rs` 删除 `Event::Field(CameraMake/CameraModel)` 与 mdta-`Created` 发射(保留维度/时长/mvhd-created/gps)。
- `driver.rs::Collector` 删除 `Field::CameraMake` / `Field::CameraModel` 臂(改由 `StructuralFields`
  承接 dims/duration/created/gps);`Field` 枚举中 `CameraMake/CameraModel` 变体若全库无其它消费者则一并移除。
- EBML(`formats/ebml.rs`)的 `Field::Created`(DateUTC)、维度、时长继续作为结构字段,无语义改动。

## 7. 不变量与验证

### 不变量(不得破坏)

- 基准设计全部不变量(`#![forbid(unsafe_code)]`、显式栈迭代、`checked_*`、缺失即 `None`)。
- `©xyz` 不得泄漏进 `container_tags`(既有测试保留)。
- 四适配器差分一致性。

### 验证(行为保持的安全网)

- **MP4/MOV 黄金样本**(make/model/created/gps)必须**逐字节不变** —— 值不变,仅计算点搬家。
  这是证明收敛未改变可观测行为的核心证据。
- 现有 normalize 单测(EXIF/XMP/PNG 优先级)全绿(签名改动后传 `default()`)。
- 新增 normalize 单测:**容器 mdta 压过 EXIF**(make/model)、**容器 mdta `creationdate` 压过
  mvhd 结构 created**、**mvhd 结构 created 压过 EXIF**(模拟无 mdta 的 MP4)。
- 四适配器差分 + no_std 构建 + fuzz 构建防腐照跑。

## 8. 范围与 YAGNI

- **不**引入新 Unified 字段、**不**改 `Value` 枚举、**不**碰 `Demand`/anchor 机制。
- **不**把 `©xyz`/`loci` 表示进 `RawTags.container`(那是被否的方案 B)。
- **不**触碰 IPTC/ICC/async 等未开始里程碑。
- 纯内部重构:公开 API 的可观测输出(`Metadata`)逐字节不变,仅 `normalize` 的函数签名变化
  (库内调用方同步更新)。
