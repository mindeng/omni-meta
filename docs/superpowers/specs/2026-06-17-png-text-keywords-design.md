# PNG tEXt/zTXt/iTXt 文本关键字 —— 设计

- 日期：2026-06-17
- 状态：设计已批准，待写实现计划
- 关联：ROADMAP §4「待评估：PNG tEXt/zTXt 注册关键字」（第 150 行）；缺口由 commit `f644533` 记录（PNG tEXt `keyword=Make` 被忽略）

## 1. 背景与动机

当前 PNG 解析器（`formats/png.rs:76`）只把 `IHDR`/`eXIf`/`iTXt` 当元数据，且 `iTXt`
**仅**支持 `XML:com.adobe.xmp` 这一个 keyword（`handle_itxt` 对其它 keyword 早返回丢弃）。
因此：

- `tEXt`、`zTXt` 整块跳过；
- `iTXt` 承载的**非 XMP 注册关键字**（现实中 exiftool 等以 UTF-8 iTXt 写 `Author`/`Description`）被读出 keyword 后丢弃。

PNG 注册关键字（`Title`/`Author`/`Description`/`Copyright`/`Creation Time`/`Software`/
`Comment` 等）语义明确、现实常用，属于**读取覆盖缺口**。本设计在读取侧把这些文本关键字
收入 raw 层，并把可投影者纳入 Unified（受「≥2 来源」约束），同时纠正 strip 侧文档的失实
表述并补测试。

## 2. 范围

- **读取侧**：解析 `tEXt` / `iTXt`（含非 XMP keyword）/ `zTXt`，落入新 `RawTags.text`。
- **投影侧**：注册关键字投影到 Unified（既有字段作末位兜底；新增 `title`/`description`/`copyright`）。
- **Strip 侧**：行为本就正确（默认全删文本块），仅纠正文档 + 补测试。
- **压缩数据**：本库零依赖、**不解压**；保留压缩字节供上层自理（见 §6）。

非目标：实现 inflate 解压（留待未来独立 feature-gated crate，见 §8）。

## 3. 数据模型（`model.rs`）

```rust
/// PNG 文本块（tEXt/iTXt/zTXt）的一条 keyword→value。
/// keyword 在四种块里都是明文，故始终可读；value 的载体/编码/压缩状态
/// 由 `TextValue` 单一表达——不单设 source 字段（避免与 value 变体冲突）。
pub struct TextTag {
    pub keyword: String,
    pub value: TextValue,
}

/// 文本值，自描述其编码与压缩状态。
/// 压缩变体仅保留原始压缩字节（本库零依赖、不解压）；上层可按变体决定
/// 解压后用 Latin-1 还是 UTF-8 解码。未来解压走独立 feature-gated crate。
pub enum TextValue {
    /// tEXt：Latin-1 已逐字节无损映射为 UTF-8 String（永不失败）。
    Latin1(String),
    /// iTXt 未压缩、非 XMP：原生 UTF-8。
    Utf8(String),
    /// zTXt：zlib 压缩字节，未解压；解压后应按 Latin-1 解码。
    CompressedLatin1(Vec<u8>),
    /// 压缩 iTXt：zlib 压缩字节，未解压；解压后应按 UTF-8 解码。
    CompressedUtf8(Vec<u8>),
}
```

- `RawTags` 新增 `text: Vec<TextTag>`。
- `Unified` 新增 `title` / `description` / `copyright`，均 `Option<String>`。
- **不引入** `TextSource`（对话演进中的中间产物，最终不存在）——载体/编码/压缩状态全由 `TextValue` 表达。

## 4. 读取侧解析（`formats/png.rs`）

把 `tEXt` / `zTXt` 纳入 `is_meta`（与 `iTXt` 一样整块读入窗口 `8+len+4`，不再走 `Skip`）。

### 路由表

| 块 | keyword | 产出 | 落点 |
|---|---|---|---|
| `eXIf` | — | `Payload{Exif}`（不变） | `raw.exif` |
| `iTXt` | `XML:com.adobe.xmp`，未压缩 | `Payload{Xmp}`（不变） | `raw.xmp` |
| `iTXt` | 其它，未压缩，合法 UTF-8 | `Text(TextValue::Utf8)` | `raw.text` |
| `iTXt` | 其它，未压缩，非法 UTF-8 | `Warning(UnrecognizedValue)`（带真实 offset） | — |
| `iTXt` | 任意，**压缩** | `Text(TextValue::CompressedUtf8)`，**不发 warning** | `raw.text` |
| `tEXt` | 任意 | `Text(TextValue::Latin1)` | `raw.text` |
| `zTXt` | 任意 | `Text(TextValue::CompressedLatin1)`，**不发 warning** | `raw.text` |

### 解析规则

- **keyword 切分**：按第一个 `\0` 切分 keyword / 余下。无 `\0` 或空 keyword → 视为畸形，丢弃该块，不 panic（与现有 `handle_itxt` 早返回同风格）。
- **keyword 长度上界**：**强制 ≤ 79 字节**（PNG 规范硬性规定 1–79）。超出 → 丢弃该 tag + 发 `Warning(UnrecognizedValue)`（带真实 offset）。理由见 §7「DoS 边界」。
- **tEXt**：keyword/value 均 Latin-1，逐字节 `char::from(b)` 无损映射为 UTF-8 String，永不失败、零依赖、no_std 友好 → `TextValue::Latin1`。
- **iTXt（未压缩、非 XMP）**：按 `keyword\0 compflag compmethod lang\0 transkw\0 text` 切分，`text` 用 `core::str::from_utf8`：成功 → `TextValue::Utf8`；失败 → 丢弃 value + 发 `UnrecognizedValue`（带真实 offset）。
- **iTXt（keyword==`XML:com.adobe.xmp`、未压缩）**：保持发 `Payload{Xmp}`，路径不变。
- **iTXt（压缩，任意 keyword）/ zTXt**：保留压缩字节为 `CompressedUtf8` / `CompressedLatin1`，进 `raw.text`，**不发 warning**（数据无损保留，`TextValue` 变体已自描述「需解压」）。
- **条目数上界**：`raw.text` 条目数受 `Limits.max_tags`（默认 8192）封顶，在 collector 侧实施（与 `container` 一致）。

### 行为变更（需同步差分/golden 断言）

- **压缩 iTXt 不再发 `CompressedChunkSkipped`**（改为收进 `raw.text`）。
- **zTXt 从静默跳过改为收进 `raw.text`**。
- `WarnKind::CompressedChunkSkipped` 在 PNG 文本块场景**不再使用**（枚举保留，他处不动）。

## 5. Unified 投影（`normalize.rs`）

新增助手 `png_text(raw, keyword) -> Option<String>`：在 `raw.text` 找首个匹配 keyword 的条目，**仅取明文变体**（`Latin1`/`Utf8`），跳过压缩变体（未解压不可投影）。

接入优先级链：

| Unified 字段 | 优先级（高 → 低） |
|---|---|
| `software` | 容器 > EXIF `0x0131` > XMP `xmp:CreatorTool` > **PNG `Software`** |
| `creator` | 容器 > EXIF `0x013B` Artist > XMP `dc:creator` > **PNG `Author`** |
| `description`（新） | EXIF `0x010E` ImageDescription > XMP `dc:description` > PNG `Description` |
| `copyright`（新） | EXIF `0x8298` Copyright > XMP `dc:rights` > PNG `Copyright` |
| `title`（新） | XMP `dc:title` > PNG `Title`（无标准 EXIF title） |
| `created` | 现有来源（BMFF/EXIF/EBML/QuickTime）> **PNG `Creation Time`** |

- PNG 一律作**末位兜底**——PNG 文本可被任意工具改写，可信度最低。
- 新增常量 `TAG_IMAGE_DESCRIPTION = 0x010E`、`TAG_COPYRIGHT = 0x8298`。
- `Comment` 仅留 `raw.text`，不投影（无干净 Unified 目标）。

### Creation Time 解析（`parse_png_creation_time(&str) -> Option<DateTimeParts>`）

依次尝试，命中即返回：

1. **ISO 8601**：`YYYY-MM-DDTHH:MM:SS`（可选时区 `Z` / `±HH:MM`）。
2. **RFC 1123**：`Day, DD Mon YYYY HH:MM:SS GMT`（PNG 规范钦定格式）。
3. **裸日期**：`YYYY-MM-DD` → 时分秒填 `00:00:00`，`tz_offset_min = None`。

三者均不匹配 → 返回 `None`，值仍留 `raw.text`，**不报 warning**（不臆造）。

### ≥2 来源核验

| 字段 | 来源数 | 结论 |
|---|---|---|
| `creator` | EXIF Artist + XMP dc:creator + PNG Author | ✅ |
| `software` | EXIF 0x0131 + XMP CreatorTool + PNG Software | ✅ |
| `created` | BMFF/EXIF/EBML/QuickTime + PNG Creation Time | ✅ |
| `description` | EXIF 0x010E + XMP dc:description + PNG | ✅ |
| `copyright` | EXIF 0x8298 + XMP dc:rights + PNG | ✅ |
| `title` | XMP dc:title + PNG（恰好 2） | ⚠️ 满足 ≥2 |

## 6. Strip 侧（`strip/png.rs`）—— 仅纠偏 + 测试，无行为变更

`classify`（`strip/png.rs:105-124`）已对 `iTXt | tEXt | zTXt` 一律 `Drop`，**默认全删文本块，行为本就正确**。本节只做：

1. **纠正 ROADMAP §4 第 150 行**失实表述——把「strip 当前不碰 / strip 盲区」改为「strip 默认已剥离全部文本块（含 tEXt/zTXt 携带的 PII）」。
2. **补显式测试**：构造带 `Author`/`Comment`（PII）的 `tEXt` + `zTXt`，断言默认 strip 后字节里不含这些 keyword/值，且保持幂等。
3. `classify` 中对 `tEXt`/`zTXt` 判 `starts_with("XML:com.adobe.xmp")` 是**死枝**（XMP 只走 iTXt），加注释说明，不改逻辑。

## 7. 不变量与 DoS 边界

- `#![forbid(unsafe_code)]`、显式栈迭代、所有偏移/长度 `checked_*`、缺失即 `None`、不 panic —— 全部保持。
- **keyword 上界（≤79）的理由**：
  - 流式路径有 DoS 上界（`driver.rs:261`：缓冲超 `max_retained_bytes` 16MB 即停发 `UnreachableSection`），超大块不会无界分配。
  - slice 路径 parser 直接拿整窗口，`max_retained` 不拦；若 keyword 完全不限，畸形 PNG 可用 16MB 全非 `\0` 的 tEXt 诱导一个 16MB keyword String 分配。
  - PNG 规范 keyword 本就 1–79 字节，现实注册关键字皆短；强制 79 既合规又防御，且基本不误伤真实文件。
  - value 可合法很长（Description/Comment），不设小上界，依赖 `max_retained`（流式）+ 输入边界（slice）+ `max_total_alloc` 计数分配器（fuzz）兜底。
- `raw.text` 条目数受 `max_tags` 封顶。
- 新格式/codec 必须通过**全部适配器差分一致性**（slice/push/blocking/seek）。

## 8. 未来扩展（仅记档，不实现）

压缩文本块（`CompressedLatin1`/`CompressedUtf8`）的解压：

- **不**走「上层注入解压回调」（会把外部代码引入确定性解析核心，污染 `MetaParser` 签名、扩大测试矩阵、解压炸弹责任模糊）。
- 推荐形态：独立 **feature-gated crate**（如 `omni-meta-inflate`），可依赖纯 Rust inflate（如 `miniz_oxide`）或自带实现，通过 Cargo feature 可选启用；核心库默认不带，需要者显式开启并把 `Compressed*` 变体解出。核心永远零依赖，解压是 opt-in 旁路层。

## 9. 测试计划

- **`formats/png.rs` 单测**：路由表每个分支（tEXt→Latin1 / 非XMP iTXt→Utf8 / 非法UTF-8→UnrecognizedValue / 压缩iTXt→CompressedUtf8 不发warning / zTXt→CompressedLatin1 不发warning / XMP iTXt 路径不变）；畸形（无`\0`、空keyword、keyword>79→UnrecognizedValue、声明长度截断）不 panic。
- **`normalize.rs` 单测**：各字段投影；PNG 末位优先级（前序来源存在时不被 PNG 覆盖）；description/copyright/title 三新字段；Creation Time 三格式解析 + 无法解析时留 raw 不报 warning；压缩变体不投影。
- **四适配器差分一致性**：新 codec 必过（铁律）。
- **黄金样本**：`regen.sh` 注入 tEXt 文本块；`golden.rs` 期望（`raw.text` 子集 + 投影字段），兑现 `f644533` 记录的「keyword=Make 被忽略」缺口；以 exiftool 独立核对。

## 10. 决策记录（本次 brainstorming）

- 范围：读取 + 投影（含新字段）+ strip 纠偏。
- raw 模型：格式中性 `RawTags.text`；最终用 `TextTag{keyword, value: TextValue}`，弃用 `TextSource`。
- iTXt 仅 XMP 半支持 → 非 XMP 未压缩 iTXt 现纳入 `Utf8`。
- 非法 UTF-8 → `UnrecognizedValue`（复用现有枚举，语义「存在但读不出」）。
- Creation Time → ISO/RFC1123/裸日期；末位兜底。
- 压缩数据：路线 B（保留压缩字节，不解压，不发 warning），解压留未来独立 crate。
- keyword 上界：强制 ≤79，超出丢弃 + `UnrecognizedValue`。
