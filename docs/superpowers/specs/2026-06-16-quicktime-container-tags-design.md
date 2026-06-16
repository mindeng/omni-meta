# QuickTime/udta 容器标签解析 + software/creator 投影 设计

**日期** 2026-06-16 · **状态** 设计已批准，待写实现计划
**关联** ROADMAP §1（受控增长 / 当前 Unified 字段）、§5（不变量）；既有 BMFF moov/meta 解析 `formats/bmff.rs`（提交 `e5b71b5`…）

## 1. 背景与动机

`com.apple.quicktime.software` / `com.apple.quicktime.author` / udta `©aut`（"author"，标准原子是 `©aut` = `0xA9 'a' 'u' 't'`，并无裸 `auth`）等字段，当前**未被解析**。现状：

- BMFF QuickTime mdta 仅在能投影到既有 Unified 字段时才被读取——`parse_qt_mdta` 只认 4 个键（`location.ISO6709` / `make` / `model` / `creationdate`），其余键直接丢弃。
- 容器原生数据走 `Event::Field` → 类型化 `Collector` slot → `Unified`，**没有 raw 容器层**：`RawTags` 只有 `exif` + `xmp` 两个篮子。

**评估结论**：
- 绝大多数有价值的 QuickTime/udta 元数据本身是文本。非文本类里唯一值得纳入的是 `…camera.focal_length.35mm_equivalent`（有 EXIF 0xA405 作第二来源，但 niche）。
- `software` / `author(creator)` 在其它格式有天然同义来源（EXIF/XMP），**可凑齐 ≥2 来源**进 Unified。
- 二进制封面/缩略图属"内嵌媒体"而非元数据字段，默认不收（见 §6 Phase 3 可选 opt-in）。

**采用两步走**：先在 raw 层无损捕获 QuickTime/udta 键，再单独把 `software`/`creator` 投影进 Unified。

## 2. 范围

**纳入**
- raw 层新增容器标签篮子，捕获 QuickTime mdta 文本键 + udta `©`-atoms + focal length 35mm 等效（整数型）。
- Unified 新增 `software` / `creator` 两字段（≥2 来源投影）。

**不纳入（本设计）**
- 二进制封面/缩略图捕获 → 独立可选 Phase 3（§6）。
- `video-orientation`（与 `tkhd` 矩阵语义重叠，避免冲突来源）。
- HEIF 缩略图（是 `iloc` 编码图像 item，非元数据原子，完全不同的 seek 抽取路径）。

## 3. 数据模型（`model.rs`）

```rust
/// 容器原生标签的命名空间来源。
pub enum ContainerSource {
    QuickTimeMdta,  // moov/meta/ilst，键为反向 DNS 全名
    Udta,           // moov/udta，键为 FourCC（© → U+00A9）
}

/// 一条容器原生标签（QuickTime mdta / udta ©-atoms）。
pub struct ContainerTag {
    pub source: ContainerSource,
    pub key: String,    // mdta: "com.apple.quicktime.software"；udta: "©swr"/"©aut"/"©nam"
    pub value: Value,   // 复用既有 Value：文本→Text，focal length→U32
}

pub struct RawTags {
    pub exif: Vec<ExifTag>,
    pub xmp: Vec<XmpProperty>,
    pub container: Vec<ContainerTag>,   // 新增
}
```

**键表示**
- mdta：反向 DNS 全名原样（`"com.apple.quicktime.software"`）。
- udta `©`-atoms：首字节 `0xA9` 单独非合法 UTF-8，构造 key 时显式映射为字符 `'©'`(U+00A9) + 后 3 个 ASCII 字节 → `"©swr"`。`source` 字段消歧两个命名空间。
- `udta.auth`：按出现的实际 FourCC 原样收（标准作者 `©aut` → `"©aut"`）。

**值表示**：复用既有 `Value`（U16/U32/Text/Rational/SRational/Bytes/List），**不新增变体**。文本→`Value::Text`；focal length 整数型→`Value::U32`。

> **focal length 类型存疑**：若实现期（TDD）真实样本显示其为 float32（data type 23），届时再决定加 `Value::F32`，**不现在臆测**。整数型先用 `Value::U32`。

## 4. 解析范围与抽取规则（`formats/bmff.rs`）

**事件通道**：新增 `Event::ContainerTag(ContainerTag)`；`Collector` 增 `container: Vec<ContainerTag>` 累积分支，受既有 `Limits::max_tags` 封顶（共享预算，不新增 Limits 字段）；`finalize` 装入 `RawTags.container`。

**QuickTime mdta**（扩展 `parse_qt_mdta`）：当前 `qt_data_value` 用 `p.get(8..)` 丢弃了 data atom 类型码。重构为读类型码：

- `qt_data_typed(item) -> Option<(u32 type_code, &[u8])>`（data 载荷布局：type=`p[0..4]`、locale=`p[4..8]`、value=`p[8..]`）。
- 抽取规则：
  - `type==1`（UTF-8）→ `Value::Text`。
  - `type==21|22`（BE 有符/无符整数）**且** key 结尾为 `…focal_length.35mm_equivalent` → `Value::U32`。
  - 其余类型（float / JPEG 13 / PNG 14 / 二进制）→ **干净跳过、不收**。
- 既有 4 个键（location.ISO6709 / make / model / creationdate）继续走 `Event::Field` 投影**不变**；同时所有文本键**额外**发一条 `Event::ContainerTag` 入 raw（raw 层完整 vs Unified 投影各管各，互不影响）。

**udta `©`-atoms**：载荷为 `u16 size + u16 lang + text`，恒为文本。遍历 udta 子盒，首字节 `0xA9` 者解出文本 → `Value::Text` 发 `ContainerTag`。`©xyz`(GPS) 与 `loci` 维持现有 `Field` 投影、**不重复入 raw**（坐标非文本元数据）。

**畸形处理**：非 UTF-8 文本、越界、截断 → 跳过该条，不 panic、不臆造（沿用现有静默跳过风格）。

## 5. Step 2：Unified 投影（Phase 2）

```rust
pub struct Unified {
    // …既有字段…
    pub software: Option<String>,   // 创建软件/固件
    pub creator:  Option<String>,   // 作者（dc:creator 主词汇）
}
```

| Unified 字段 | 来源（≥2 ✓） |
|---|---|
| `software` | EXIF `Software`(0x0131) · XMP `xmp:CreatorTool` · 容器 mdta `…quicktime.software` / udta `©swr` |
| `creator` | EXIF `Artist`(0x013B) · XMP `dc:creator` · 容器 mdta `…quicktime.author` / udta `©aut` |

**投影位置**：因 Phase 1 已把容器标签放进 `RawTags.container`，投影逻辑**全部落在 `normalize(&raw)`**——一处读 exif/xmp/container 三命名空间、按优先级择一，**不需要** `finalize` override（不像 width/created 走独立 Field 事件）。

**优先级**：**容器 > EXIF > XMP**，与现有"容器原生覆盖 EXIF 派生"（`created`）一致——视频场景容器值最权威。

**命名**：用 `creator`（对齐 dc:creator 现代主词汇），EXIF Artist / QuickTime author 都归一到它。

## 6. Phase 3（可选）— covr 二进制封面 opt-in

默认不收；经 `Limits` 开关 opt-in。**连带约束**（必须一并处理，否则埋坑）：

- **仅适用 MP4/MOV 的 iTunes 式 `udta/meta/ilst/covr`**（data type 13=JPEG / 14=PNG）。HEIF 缩略图不在此路径（见 §2）。
- **专用尺寸上界** `max_binary_atom_bytes`（默认 0 = 关闭），且 ≤ `max_total_alloc`。
- 它是全库唯一"保留大二进制"路径（`Value::Bytes(Vec<u8>)`，KB–MB 真实分配，非零拷贝），需隔离、封顶。

Phase 1 的类型设计（`Value::Bytes` + `ContainerSource`）已天然能装下封面，`Limits` 预留开关位即可，真正实现延到有需要时。

## 7. 不变量 · 测试

**不变量**（ROADMAP §5）：`#![forbid(unsafe_code)]`；`iter_child_boxes` 显式迭代；偏移全 `checked_*`，畸形→跳过不 panic；`RawTags.container` 受既有 `max_tags` 封顶；非文本/未知类型/非 UTF-8 → 静默跳过，**绝不臆造**；`--no-default-features` no_std 构建。

**测试**
- 单测：mdta 文本键→`ContainerTag`；udta `©`-atom→`ContainerTag`；focal length 整数型→`Value::U32`；坏 UTF-8 跳过；未知类型码跳过；截断 data atom；`max_tags` 封顶。
- 四适配器差分：含 software/author/©swr 的 .MOV/.MP4 → 四路 `RawTags.container` 完全一致。
- Phase 2：normalize 优先级（容器>EXIF>XMP）；跨格式 ≥2 来源差分（JPEG EXIF Software + MOV mdta software 都投影到 `Unified.software`）。
- 合成畸形：永不 panic / 不超 `Limits`。

## 8. 分期（各为可独立测试、可 ff-merge 的纵切片）

- **Phase 1** — raw 捕获：model 新类型（`ContainerSource`/`ContainerTag`/`RawTags.container`）+ `Event::ContainerTag` + Collector 分支 + bmff mdta/udta 抽取（`qt_data_typed` 重构）+ 单测/差分。
- **Phase 2** — Unified `software`/`creator` 投影（normalize，≥2 来源）+ 单测/差分。
- **Phase 3（可选）** — `covr` 二进制 opt-in（§6）。

完成后更新 ROADMAP「当前 Unified 字段」与受控增长记录。
