# omni-meta 阶段 3 设计规范：PNG / WebP / GIF 格式 + XMP codec

**日期**: 2026-06-15
**状态**: 已批准，待实现计划
**对应路线**: 总设计 §11 分阶段路线 第 3 项
**crate**: `omni-meta-core` + `omni-meta`（Rust edition 2024）

## 1. 目标与范围

为 omni-meta 增加三种 Web 图片格式的元数据解析与 XMP 解码，复用既有 sans-io 引擎与 EXIF codec。

### 在范围内

- **PNG / WebP / GIF** 三个 `MetaParser` 格式解析器。
- **XMP codec**：非校验式结构化扫描，把 RDF/XML 包扫成 `(prefix, name, value)` 属性列表。
- **容器原生 width/height**：四种格式（含 JPEG）统一填充 `Unified.width/height`。
- **JPEG SOF 维度解析**：扩展既有 `JpegParser` 读取 SOF 标记的高/宽，使 width/height 跨格式一致。
- `probe` 扩展 + 适配器格式分派收口（消除 slice/push 重复）。

### 非目标（本阶段，明确推迟）

- **IPTC codec**：其天然载体是 JPEG APP13（Photoshop 8BIM），与本阶段三格式无关，单独切片实现。
- **inflate / zlib 解压**：保持零第三方依赖。压缩块（PNG `zTXt`、压缩 `iTXt`、`iCCP`）跳过并告警。
- **ImageMagick `zTXt "Raw profile type …"` 约定**：非标准且需 inflate，推迟。
- **XMP 命名空间 URI 解析**：只按惯用前缀（`tiff:`/`dc:`/`xmp:`…）存储与匹配。
- **共享 `containers/` 层**：本阶段三格式语法各异（PNG=chunk / GIF=block / WebP=RIFF），无两者共享同一容器语法；RIFF 抽取留待第二个 RIFF 格式（AVI，未来阶段）。
- ICC、视频容器、Stripper —— 后续阶段。

### 关键事实：压缩块与在范围内载荷不相交

仅 PNG 存在压缩块，且**没有任何在范围内的载荷是压缩的**：EXIF 走 `eXIf`（未压缩），XMP 走 `iTXt`（XMP 规范要求 compression flag=0，未压缩）。故 `CompressedChunkSkipped` 告警纯属防御性，对标准文件的 EXIF/XMP 永不触发。

## 2. 架构与模块布局

沿用既有四层与 sans-io 契约（`MetaParser::pull` 发 `Demand`、产 `Event`；`Driver`/`drive_slice`/`StreamDriver` 驱动；codec 解码 `Payload`）。

新增文件（`omni-meta-core/src/`）：

```
formats/png.rs      // PngParser   —— PNG chunk 遍历
formats/webp.rs     // WebpParser  —— RIFF chunk 遍历
formats/gif.rs      // GifParser   —— GIF block / sub-block 遍历
codecs/xmp.rs       // xmp::decode —— XMP 包 → XmpProperty
```

改动文件：`model.rs`、`demand.rs`、`driver.rs`（Collector）、`normalize.rs`、`probe.rs`、`formats/mod.rs`、`codecs/mod.rs`、`formats/jpeg.rs`（SOF）、`lib.rs`（re-export）、`adapters/slice.rs`、`adapters/push.rs`（分派收口）。

格式解析器**自包含**：各自遍历自身结构，复用 `cursor::ByteCursor` 读定长头，不引入新的容器抽象层。

## 3. 核心 model / demand 扩展

`demand.rs`：

```rust
pub enum PayloadKind { Exif, Xmp }          // + Xmp

pub enum Event<'a> {
    Payload { kind: PayloadKind, data: &'a [u8] },
    Field(Field),                            // 新增：容器原生字段
    Warning(Warning),
}
```

`model.rs`：

```rust
pub enum FileFormat { Jpeg, Png, Webp, Gif, Unknown }    // + Png/Webp/Gif

pub enum Field { Width(u32), Height(u32) }                // 新增

pub struct XmpProperty {                                  // 新增
    pub prefix: String,   // 惯用前缀，原样保留（如 "tiff"）
    pub name: String,     // 局部名（如 "Orientation"）
    pub value: String,
}

pub struct RawTags {
    pub exif: Vec<ExifTag>,
    pub xmp: Vec<XmpProperty>,                            // 新增
}

pub struct Unified {
    pub width: Option<u32>,                               // 新增
    pub height: Option<u32>,                              // 新增
    pub orientation: Option<Orientation>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
}

pub enum WarnKind {
    Truncated, BadExifHeader, UnreachableSection,
    UnrecognizedValue,
    CompressedChunkSkipped,                               // 新增
}
```

`driver.rs` `Collector` 增加 `xmp: Vec<XmpProperty>`、`width: Option<u32>`、`height: Option<u32>`；`handle()` 路由 `Payload{Xmp}` → `xmp::decode`，`Field::Width/Height` → 记录（容器级权威，先到为准）；`finalize()` 把维度写入 `Unified`、把 `xmp` 写入 `RawTags`。

## 4. PNG 解析器（`formats/png.rs`）

消费 8 字节签名 `89 50 4E 47 0D 0A 1A 0A` 后进入 chunk 循环：`len:u32 BE | type:[4] | data | crc:u32`。

| chunk | 动作 |
|---|---|
| `IHDR` | 读 width/height（BE u32 @ data 0、4）→ `Field::Width/Height` |
| `eXIf` | 整段入窗 → `Payload{Exif, data}`（裸 TIFF，无 `Exif\0\0` 前缀） |
| `iTXt` | 解头；keyword==`XML:com.adobe.xmp` 且 compression flag==0 → `Payload{Xmp, text}`；flag==1 → `Warning(CompressedChunkSkipped)` 后跳过 |
| `zTXt` / `iCCP` / 其他压缩 | `Skip(len+4)`（含 CRC），不解压 |
| `IEND` | `Done` |
| 其他 | `Skip(len+4)` |

增量契约同 `jpeg.rs`：须整读的 chunk（`IHDR`/`eXIf`/XMP `iTXt`）窗口不足时 `NeedBytes(8+len)`，足够则消费头+数据、`Skip(4)` 跳 CRC；可跳过的 chunk 消费 8 字节头后 `Skip(len+4)`。CRC **不校验**（best-effort、零依赖，不引入 crc32）。width/height 用 checked 运算，畸形不 panic。

## 5. WebP 解析器（`formats/webp.rs`）

消费 `RIFF`(4) + filesize(4 LE) + `WEBP`(4) 后进入 RIFF chunk 循环：`fourcc:[4] | size:u32 LE | data | 偶数对齐填充`。

| chunk | 动作 |
|---|---|
| `VP8X` | width=`u24LE(data[4..7])+1`，height=`u24LE(data[7..10])+1` → `Field` |
| `VP8 ` | 有损关键帧：width=`u16LE@6 & 0x3FFF`，height=`u16LE@8 & 0x3FFF` → `Field` |
| `VP8L` | 无损：`data[1..5]` 位流解出 14-bit width-1/height-1 → `Field` |
| `EXIF` | `Payload{Exif, data}`（裸 TIFF；若存在 `Exif\0\0` 前缀则容错剥离） |
| `XMP ` | `Payload{Xmp, data}` |
| 其他（`ANIM`/`ICCP`/`ALPH`…） | `Skip(size + 对齐填充)` |

每 chunk 前进 `size + (size & 1)`。EXIF/`XMP ` 仅存在于扩展（VP8X）文件，故维度与元数据同时出现。VP8/VP8L 维度解析尽力而为：头畸形则不发 `Field`、不 panic。

## 6. GIF 解析器（`formats/gif.rs`）

消费 `GIF87a`/`GIF89a`(6) 后读 Logical Screen Descriptor：width=`u16LE@0`、height=`u16LE@2` → `Field`；packed 字节若置 Global Color Table 标志则 `Skip(3 · 2^(size+1))`。随后 block 循环：

| 引导字节 | 动作 |
|---|---|
| `0x2C` 图像描述符 | 跳 9 字节描述符（+ 局部色表，如有）+ LZW 最小码长字节 + 图像 sub-block 链至终止符 |
| `0x21 0xFF` 应用扩展 | app-id+auth == `XMP DataXMP` → 捕获 XMP 包（裸字节，直到魔数 sub-block 尾 / 包结束）→ `Payload{Xmp}`；否则遍历 sub-block 跳过 |
| `0x21 0xFE` 注释扩展 | 遍历 sub-block 跳过（注释不在范围） |
| `0x21` 其他 | 遍历 sub-block 跳过 |
| `0x3B` Trailer | `Done` |

GIF **无 EXIF**。XMP-in-GIF 的魔数尾（`0x01 0xFF 0xFE … 0x00` 递降长度逃逸序列）通过扫描尾标/包结束处理；畸形 → `Warning(Truncated)` + best-effort `Done`。sub-block 遍历 = `NeedBytes(1)` 读长度字节后 `Skip(len)`，`0x00` 终止链。

## 7. JPEG SOF 维度（`formats/jpeg.rs`）

既有解析器把 SOF 当普通定长段 Skip。改为：遇 SOF 标记（`0xC0..=0xCF`，排除 `0xC4` DHT / `0xC8` JPG / `0xCC` DAC）时读段体，height/width = 1 字节精度后的两个 BE u16 → 发 `Field::Width/Height`，随后继续扫描至 SOS。其余行为不变。

## 8. XMP codec（`codecs/xmp.rs`）

```rust
pub fn decode(packet: &[u8], out: &mut Vec<XmpProperty>,
              warnings: &mut Vec<Warning>, limits: &Limits);
```

**非校验式扫描器**（非完整 XML 解析）：

1. `core::str::from_utf8`；无效 UTF-8 → `Warning(Truncated)` 后返回。
2. 可选剥除 `<?xpacket …?>` 包裹；定位 `rdf:Description` 元素。
3. **属性形式**：`rdf:Description` 上每个 `prefix:name="value"` 属性 → `XmpProperty`（跳过结构属性 `rdf:about`、`xmlns:*`）。
4. **元素形式**：每个子元素 `<prefix:name>…</prefix:name>` 取文本内容。RDF 容器：`rdf:Bag`/`rdf:Seq` 的每个 `rdf:li` 各产一条同 `prefix:name` 的 `XmpProperty`（保留多值）；`rdf:Alt`（语言备选）只取首个 `rdf:li` 产一条。
5. 解码五个基本 XML 实体（`&lt; &gt; &amp; &quot; &apos;`），其余原样保留。
6. 受 `limits.max_tags`（与 EXIF 共享上界）与 `limits.max_payload_bytes` 约束，触界干净停止。

范围守卫：不解析命名空间 URI（前缀原样存）；不处理 DTD/CDATA/PI（跳过）；不展平嵌套结构（深层值按原始内部文本捕获）。符合本库 best-effort 姿态。

## 9. normalize、probe 与分派

**`normalize.rs`** 增加 XMP→Unified 投影，仅在能补充*新*统一值时生效（EXIF 对 orientation/make/model 优先）。初始保守集合：

- `tiff:Orientation` → `orientation`（EXIF 未设时）
- `tiff:Make` / `tiff:Model` → `camera_make` / `camera_model`（回退）
- `tiff:ImageWidth` / `tiff:ImageLength` → `width` / `height`（回退；容器值优先）

EXIF 优先：XMP 只填 `None` 槽。无法识别的 XMP 值留在 `raw.xmp` 不动（不产 `UnrecognizedValue` 噪声）。

**`probe.rs`**：新增 PNG/WebP/GIF 签名；新增 `PROBE_MAX = 12`（最长签名）与 `pub(crate) fn parser_for(FileFormat) -> Option<Box<dyn MetaParser>>`。`probe()` 对短输入仍尽早决断（JPEG@2、GIF@6、PNG@8、WebP@12）。

**`slice.rs` / `push.rs`**：改用 `parser_for` 单一分派点；`push.rs` 探测前累积到 `PROBE_MAX` 字节再探测，finish() 末次探测逻辑不变。

## 10. 错误处理与安全

姿态不变：顶层仅"格式不可识别 / I/O 源报错"返回 `Err`，其余一律 `Warning` + 部分 `Metadata`。新增防御路径全部经既有 `WarnKind`（+ `CompressedChunkSkipped`）。所有偏移/长度用 `checked_*`/`saturating_*`；chunk / sub-block 遍历严格前进或终止（driver 的防卡死预算兜底）。维度为 u32 不涉分配；`max_tags` 约束 XMP 属性数；`max_payload_bytes` 约束 driver 缓冲的载荷大小。不引入新 limits，不引入递归（全为显式循环）。

## 11. 测试策略

沿用阶段 2 纪律（TDD、逐模块单测、差分一致性）：

- **格式单测**：手工最小样本——PNG(sig+IHDR+eXIf+iTXt-xmp+IEND)、WebP(RIFF+VP8X+EXIF+`XMP `)、GIF(header+LSD+app-ext-XMP+trailer)；外加截断/畸形/压缩块用例，断言不 panic + 正确告警。
- **`codecs/xmp.rs` 单测**：属性形式、元素形式、rdf:Alt 语言、实体解码、坏 UTF-8、max_tags 触界。
- **JPEG SOF** 回归测试覆盖新 width/height。
- **差分测试**（`omni-meta/tests/differential.rs`）：把 PNG/WebP/GIF 样本接入既有 `assert_all_equal`，验证 slice/blocking/seek/push 逐字段一致——正确性核心保证。
- **no_std 构建**：`--no-default-features` 保持通过；零新依赖。

## 12. 实现顺序（路线方案 A：基座 → 三切片）

1. 扩展核心 model/demand/driver（§3）+ probe 分派收口（§9）。
2. XMP codec（§8）+ 单测。
3. JPEG SOF（§7）+ 回归测试。
4. PNG 解析器（§4）→ 端到端 + 差分。
5. WebP 解析器（§5）→ 端到端 + 差分。
6. GIF 解析器（§6）→ 端到端 + 差分。
7. normalize XMP 投影（§9）+ 全量差分与 no_std CI。

每步为可独立测试、可合并的纵切片。
