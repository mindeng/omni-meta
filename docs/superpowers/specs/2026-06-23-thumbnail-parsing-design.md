# 缩略图解析（EXIF IFD1 / QuickTime covr / HEIF·AVIF thmb）设计

**状态**：设计已确认，待写 plan
**日期**：2026-06-23
**基准设计**：[`2026-06-14-omni-meta-design.md`](2026-06-14-omni-meta-design.md)（sans-io 四层架构）
**关联**：复用现有 EXIF codec、BMFF 两阶段 `BmffParser`（Walk→Extract `SeekTo`）、QuickTime 容器标签解析；扩展 `Limits` 与四适配器差分

---

## 1. 动机与用例

应用层希望拿到文件**内嵌的缩略图/封面/预览图**：相册秒出缩略、视频封面、图库列表预览，免去自行解码整图再缩放。

现状缺口：缩略图的**定位标签**部分已在 raw 层（EXIF IFD1 已被遍历，`0x0201`/`0x0202` 等已落 `RawTags.exif`），但**实际缩略图字节从未暴露**到顶层 `Metadata`；covr 二进制 ROADMAP 标记为「留待可选 Phase 3」；HEIF `thmb` 派生项尚未解析（`iref` 未解）。

---

## 2. 确定的决策（brainstorm 收敛结果）

| 维度 | 决策 | 理由 |
|---|---|---|
| 产出形态 | **分层**：描述符恒可得 + 字节按需提取 | 描述符廉价（缩略图是否存在/编码/尺寸/大小），字节是重路径，按需 |
| 来源范围 | **EXIF IFD1 + QuickTime covr + HEIF/AVIF thmb**（三个同构「文件偏移切片」源） | 三者本质都是「定位 → 切字节」；XMP base64 异质，本期不做 |
| XMP Thumbnails | **本期不做** | base64 内联、需解码，破「不解码」不变量；实际罕见（多为 Photoshop） |
| 提取触发 | **普通 parse 上的开关**（`extract_thumbnails`，默认关） | 单一解析路径；与流式里程碑 G 提前终止天然契合；默认关零额外开销 |
| 字节归属 | **`Metadata` 拥有**（owned `Vec<u8>`，提取时从借用 Event 拷出） | 与现有 codec「借用 Event → 拥有 `ExifTag`」一致；顶层无生命周期负担 |
| 描述符位置 | **顶层 `Metadata.thumbnails`** | 缩略图跨命名空间（EXIF / 容器 / BMFF），非单一 raw 命名空间 |
| `offset` 字段 | **best-effort `Option<u64>`**（可廉价解出即填） | 服务「描述符-only、自取字节」用法；idat 内联 → `None` |
| 交付批次 | **H1–H3 单一 spec/分支（里程碑 H）**，内部三纵切片 | 共享模型脚手架在 H1 建好，H2/H3 复用 |

---

## 3. 数据模型

新增于 `omni-meta-core/src/model.rs`。单一类型统一「描述符 + 可选字节」——`data` 仅当开关开启且未超上界时为 `Some`：

```rust
/// 缩略图来源命名空间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailSource {
    /// EXIF IFD1（next-IFD 链）——任何携 EXIF 的格式。
    ExifIfd1,
    /// QuickTime `moov/udta/meta/ilst/covr` 封面原子。
    QuickTimeCovr,
    /// HEIF/AVIF `iref` 的 `thmb` 派生图像项。
    HeifThmb,
}

/// 缩略图字节的编码。EXIF/covr 为自包含可解码文件；Hevc/Av1 为编码项，
/// 非独立文件，调用方需相同解码器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailCodec {
    Jpeg,
    Png,
    Tiff,    // EXIF IFD1 未压缩 strip（Compression=1）
    Hevc,    // HEIF thmb hvc1
    Av1,     // AVIF thmb av01
    Unknown,
}

/// 一条缩略图：恒有描述（source/codec/尺寸/len），字节按需。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub source: ThumbnailSource,
    pub codec: ThumbnailCodec,
    /// 已知则填（HEIF thmb 经 ispe；EXIF 视 IFD1 0x0100/0x0101 而定，常缺）。
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// 声明字节长度（描述符真相，独立于是否提取）。
    pub len: u64,
    /// 绝对文件偏移；仅当来源为稳定文件位置时填，idat 内联 → None。
    pub offset: Option<u64>,
    /// 字节：仅当 `extract_thumbnails` 开启且 `len ≤ max_thumbnail_bytes`
    /// 且边界合法时为 `Some`；否则 `None`（描述符仍在，附 Warning）。
    pub data: Option<Vec<u8>>,
}
```

`Metadata` 新增一字段：

```rust
pub struct Metadata {
    pub unified: Unified,
    pub raw: RawTags,
    pub thumbnails: Vec<Thumbnail>,   // 新增
    pub warnings: Vec<Warning>,
    pub format: FileFormat,
}
```

> **分层语义**：开关关（默认）→ `thumbnails` 仍按描述符填充，每条 `data: None`，仅多一个小 `Vec`；开关开 → `data: Some(owned bytes)`，或 `None` + `Warning`（超界/畸形）。

---

## 4. Limits 扩展

`omni-meta-core/src/limits.rs`：

```rust
pub struct Limits {
    // …现有字段…
    /// 单条缩略图提取的字节上界，超过则 data=None + Warning。
    pub max_thumbnail_bytes: usize,
}
```

- 默认 `4 * 1024 * 1024`（缩略图本就小，慷慨且封顶额外分配）。
- 计入 `max_total_alloc` 全局计数（fuzz 计数分配器照旧 tripwire）。
- **提取开关不在 `Limits`**——它是「要不要做」而非「上界」。落在现有公开 `Options`（`adapters::slice::Options`，当前 `{ limits: Limits }`、`Copy + Default`）新增 `pub extract_thumbnails: bool`（默认 `false`，保持 `Default`/`Copy`）。`read_slice` 已收 `Options`；`PushParser`/`read_blocking`/`read_seek` 需一并把该布尔透传到驱动（四适配器一致前提）。

---

## 5. 三个来源的机制

### 5.1 H1 — EXIF IFD1（所有携 EXIF 格式，一次解锁）

IFD1（`IfdKind::Thumbnail`）已遍历、其标签已在 `RawTags.exif`。新增**纯读** `thumbnail_from_ifd1(tiff, &[ExifTag]) -> Option<(描述, Range)>`：

- 读 IFD1 `0x0103` Compression：
  - `6`（JPEG）→ `0x0201` JPEGInterchangeFormat（TIFF 相对偏移）+ `0x0202` Length → codec `Jpeg`。
  - `1`（未压缩）→ `0x0111` StripOffsets + `0x0117` StripByteCounts → codec `Tiff`。**仅支持单 strip**；多 strip → 描述符 + `Warning`、`data=None`（不拼接）。
  - 其它/缺失 → codec `Unknown`，尽力取 offset+len，取不到则不产此条。
- dims：IFD1 `0x0100` ImageWidth / `0x0101` ImageLength（常在）。
- **字节**：缩略图字节是 **EXIF payload 缓冲的子切片**（`tiff[off..off+len]`，`checked_*`），零额外 I/O；提取时 owned 拷出。
- **绝对 offset**：`exif_payload_base + tiff_off`。需把 EXIF payload 的绝对文件基址传到此助手（见 §6 驱动改动）；拿不到基址 → `offset=None`（不阻塞字节提取）。
- **覆盖面**：JPEG APP1 / PNG `eXIf` / WebP EXIF / HEIF exif item 同走 EXIF payload，H1 一并覆盖。

### 5.2 H2 — QuickTime covr（moov 内，已入窗）

A3 已「moov 整盒入窗」。扩展容器解析识别 `ilst` 下 **`covr` 的二进制 `data` 原子**（现仅文本 `©`-atoms / mdta）：

- `data` 原子头 type 标志：`13`=JPEG → `Jpeg`，`14`=PNG → `Png`，其它 → `Unknown`。
- 可有多条 covr `data`（多封面）→ 多条 `Thumbnail`。
- **字节**：moov 窗口内子切片，owned 拷出；**绝对 offset** 可由 moov 文件基址 + 原子内偏移算出 → `Some`。
- dims：covr 不带尺寸 → `width/height = None`。

### 5.3 H3 — HEIF/AVIF thmb（文件别处，需 seek）

新增 `iref`（ItemReferenceBox）解析，找 `thmb` 引用：`thmb` 的**引用项即缩略图项**，被引项为主图。定位该缩略图项：

- `iinf` 取其 type（`hvc1` → `Hevc`，`av01` → `Av1`，否则 `Unknown`）。
- `iloc` 定位：method 0 → `SeekTo` 进 mdat（**复用现有 exif/xmp item 抽取机制**，新增一个抽取目标）；method 1 → idat 内联（`offset=None`，字节取自 idat）；method 2 / 越界 / 截断 → 描述符 + `Warning`、`data=None`、不 panic。
- dims：经 `ipma` 关联该项的 `ispe` → `width/height`（拿不到则 `None`）。
- **字节为原始 HEVC/AV1 编码数据**，非自包含文件——`codec` 字段昭示调用方需相同解码器。文档显式说明。

---

## 6. 驱动 / 适配器改动

- **开关默认关 → 行为零变**（除新增 `thumbnails` 描述符字段，additive）。描述符恒发；字节仅开关开时拷贝。
- **EXIF payload 绝对基址**：当前 `Event::Payload{Exif, data}` 只借字节、不带绝对偏移。H1 需让 Collector/driver 在交付 EXIF payload 时知道其绝对文件位置，供 `offset` 与（无影响于）字节切片。基址拿不到时 `offset=None`，字节提取不受影响。
- **提取作为 Collector 行为**：开关开时，Collector 把缩略图字节从借用 Event 拷入 `Thumbnail.data`（同现有「借用→owned」路径）。H3 的 mdat seek 复用 BMFF Walk→Extract 两阶段，新增 thmb 抽取目标。
- **四适配器一致**：`read_slice`/`read_blocking`/`read_seek`/`PushParser` 描述符与提取字节须 byte-identical（H3 跨适配器验证 `SeekTo`，同既有 exif/xmp item）。

---

## 7. 警告与不变量

- 新增 `WarnKind`（plan 定名，候选 `ThumbnailUnresolved` / 复用 `Truncated`+`UnreachableSection`）覆盖：超 `max_thumbnail_bytes`、offset/len 越界、多 strip、covr 坏 type、thmb method2/idat 异常。一律**描述符-only + Warning，绝不 panic**。
- 维持全部既有不变量：`#![forbid(unsafe_code)]`、显式栈迭代、`checked_*` 偏移、best-effort + warnings、缺失即 `None` 绝不臆造、新功能过**全部适配器差分**。

---

## 8. 测试

- **四适配器差分**：descriptors + 提取字节跨 `read_slice`/`blocking`/`seek`/`push` byte-identical；H3 走 `SeekTo`。
- **黄金样本 + exiftool 核对**：真实相机 JPEG（`exiftool -b -ThumbnailImage` 锚定）；covr-tagged MP4（AtomicParsley/ffmpeg `attached_pic`）；真实 HEIC（thmb）。HEIF 若本机 ffmpeg 无 HEIF 复用器则合成 fixture 兜底（同既有 A2 处理）。
- **合成畸形**：offset/len 越界、多 strip、covr 坏 type、HEIF thmb method2/idat/截断 → Warning + 描述符-only，无 panic。
- **开关关 vs 开**：关→所有 `data=None` 且 `Metadata` 其余字段与未加本功能前一致（回归锚定）；开→字节正确、超界→`None`+Warning。
- **no_std** 裸机构建；**fuzz** 扩展（提取路径 honors `max_thumbnail_bytes` tripwire；differential oracle 纳入 `thumbnails` 严格相等）。

---

## 9. 里程碑切片（里程碑 H，各自可独立测试、可合并）

- **H1 — EXIF IFD1**：建共享脚手架（`Thumbnail`/`ThumbnailSource`/`ThumbnailCodec`、`Metadata.thumbnails`、`extract_thumbnails` 开关、`max_thumbnail_bytes`、EXIF payload 绝对基址）+ 缓冲内切片。最高 ROI，一并解锁所有携 EXIF 格式。
- **H2 — QuickTime covr**：ilst 二进制 `data` 原子识别，moov 窗口内切片。
- **H3 — HEIF/AVIF thmb**：`iref` 解析 + 抽取目标 + 编码项（Hevc/Av1）语义，唯一 seek 场景。最复杂。

每片完成跑：四适配器差分 + 黄金/畸形单测 + no_std 构建 + fuzz-build。

---

## 10. 不在本期

- **XMP Thumbnails**（base64 内联，需解码，破「不解码」不变量）——留后续单独增量。
- **XMP/covr 之外的 base64 / 解压缩缩略图**。
- **缩略图投影进 Unified**——缩略图是二进制 blob，不属「≥2 来源标量」受控增长，恒留 `Metadata.thumbnails`。
- **流式 G 的 `MetaItem::Thumbnail` 开口**——描述符/字节模型与开关已为 G 预留，具体流式条目交付归 G 自身工作。
