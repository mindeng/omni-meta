# GPS 字段投影 + 视频元数据来源 — 设计

**日期**：2026-06-16
**状态**：已批准，待写实现计划
**基准设计**：[`2026-06-14-omni-meta-design.md`](2026-06-14-omni-meta-design.md)
**ROADMAP**：§4 横切「Unified 受控增长 · gps（EXIF GPS IFD + XMP）」+ 视频 make/model/created 补源

---

## 1. 目标与范围

把地理坐标 `gps` 纳入 Unified，并补齐**视频家族**的位置/相机/创建时间来源。一个内聚纵切片，含三类提取路径：

1. **图像** EXIF GPS IFD + XMP `exif:GPS*` → `normalize()` 投影。
2. **视频** `moov/udta` 的 `©xyz`（ISO 6709 串）与 `loci`（3GPP FullBox）。
3. **视频** Apple QuickTime `moov/meta` 的 `keys`/`ilst`（mdta）四键：
   `location.ISO6709` / `make` / `model` / `creationdate`。

**为何一并做**：`gps` 的图像投影（raw 已就绪）与视频 udta/mdta 提取（全新解析）虽性质不同，但同属「让坐标进 Unified」这一目标；而 QuickTime mdta 的 `keys`/`ilst` 解析器是一次性成本，建成后顺手补上视频 `make`/`model`（当前视频**零来源**）与更优的 `created`，杠杆最高。

**不做**（留作后续里程碑）：
- 轨道级 `trak/udta` GPS 扫描（仅 moov 顶层 udta + moov/meta）。
- 把视频原生 GPS/坐标串落入 `RawTags`（与现有 width/duration/created 等容器原生字段一致，容器原生字段不进 raw）。
- GPS 时间戳（`GPSDateStamp`/`GPSTimeStamp`）、速度、航向等额外分量。
- IPTC/ICC（独立里程碑）。

---

## 2. 受控增长核对（≥2 格式来源）

| Unified 字段 | 来源 | 是否达标 |
|---|---|---|
| `gps`（**新增**） | EXIF GPS IFD（JPEG/PNG/WebP/HEIF/AVIF…）＋ XMP `exif:GPS*` ＋ 视频 ©xyz/loci/mdta | ✅ 远超 2 |
| `camera_make`/`camera_model` | 既有 EXIF ＋ XMP，**新增** QuickTime mdta（第 3 来源，且首次覆盖视频） | ✅ |
| `created` | 既有 BMFF mvhd ＋ EXIF ＋ EBML DateUTC，**新增** QuickTime mdta creationdate（第 4 来源） | ✅ |

---

## 3. 数据模型（`model.rs`）

### 3.1 `Gps` 结构

```rust
/// 地理坐标。E7 = 度 × 10^7（±180e7 < i32 上限；Android/Google Location 行业标准定点）。
/// alt_mm = 高程毫米（正=海平面以上）。全整数 → 保留 Eq、无浮点相等脆弱性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gps {
    pub lat_e7: i32,
    pub lon_e7: i32,
    pub alt_mm: Option<i32>,
}
```

**E7 范围核对**：纬 ±90×10⁷ = ±9.0e8、经 ±180×10⁷ = ±1.8e9，均 < i32 上限 2.147e9。✅

### 3.2 `Unified` 增字段

```rust
pub gps: Option<Gps>,
```

### 3.3 `Field` 改动

`Field` **去掉 `Copy`**（保留 `Clone`），新增三个变体：

```rust
pub enum Field {
    Width(u32),
    Height(u32),
    Duration(u64),
    Created(DateTimeParts),
    Gps(Gps),            // 新增
    CameraMake(String),  // 新增（String → Field 不能再 Copy）
    CameraModel(String), // 新增
}
```

**`Copy` 移除的影响面**：`Event<'a>` 本就只有 `Clone`（含 `&[u8]`），`Collector::handle` 按值消费 `Event`。去 `Copy` 是受控小改；实现期 TDD 会暴露任何遗留的 `Copy` 依赖。

---

## 4. 双路径合并架构（沿用既有 `created`/`duration` 模式）

| 来源 | 通道 | 落点 |
|---|---|---|
| 图像 EXIF GPS IFD + XMP | `Event::Payload` → codec → `RawTags` → `normalize()` | `unified.gps` |
| 视频 ©xyz / loci / mdta | `Event::Field(...)` → `Collector` → `finalize()` | `unified.gps` / `camera_make` / `camera_model` / `created`（覆盖） |

`finalize()` 既有顺序：先 `normalize(raw)`，再用容器 `Field` 覆盖（`created` 注释已确立「容器（moov）优先于 EXIF」）。`gps`/`make`/`model` 沿用同一「容器优先」约定。实际单个文件是「图像 XOR 视频」，两路几乎不冲突。

---

## 5. 图像投影（`normalize.rs`）

### 5.1 EXIF GPS IFD（`IfdKind::Gps`，优先）

| Tag | 名称 | 类型 | 用途 |
|---|---|---|---|
| `0x0001` | GPSLatitudeRef | ASCII `N`/`S` | 纬度定号 |
| `0x0002` | GPSLatitude | RATIONAL×3（度/分/秒） | `Value::List([Rational;3])` |
| `0x0003` | GPSLongitudeRef | ASCII `E`/`W` | 经度定号 |
| `0x0004` | GPSLongitude | RATIONAL×3 | 同上 |
| `0x0005` | GPSAltitudeRef | BYTE（0=海平面上,1=下） | 高程定号 |
| `0x0006` | GPSAltitude | RATIONAL（米） | |

**换算**：`deg = d + m/60 + s/3600`，按 Ref 取负（`S`/`W`/AltRef=1）。`lat_e7 = round(deg × 1e7)`，`alt_mm = round(meters × 1000)`。换算用**隔离的 f64**（同 EBML duration「隔离 f64 守卫」先例），仅在此 helper 内出现浮点，结果即时 round 回整数存储。

### 5.2 XMP 回退（仅 EXIF GPS 缺失时，镜像 `camera_make` 习惯）

- `exif:GPSLatitude` / `exif:GPSLongitude`：标准 EXIF-XMP 形式 `"DDD,MM.mmmm[NSEW]"`（亦容 `"DDD,MM,SS[NSEW]"`）。末字符为半球字母，去字母后按 `,` 切「度,十进制分」或「度,分,秒」。
- `exif:GPSAltitude`（`"num/den"` 米）+ `exif:GPSAltitudeRef`（`"0"`/`"1"`）。

### 5.3 产出规则

- **lat 与 lon 都解析成功**才产出 `gps`（孤经纬无意义）；`alt` 可选。
- GPS 标签存在但无法解析 → 追加 `WarnKind::UnrecognizedValue`（区分「缺失」与「存在但坏」）。
- 完全缺失 → 静默 `None`，**绝不臆造**。

---

## 6. 视频提取（`formats/bmff.rs`，`parse_moov` 下钻）

当前 `parse_moov` 仅走 `mvhd`/`trak`。新增对 `udta` 与 `meta` 两个 `moov` 子盒的下钻。`parse_moov` 内先把候选聚合进扩展后的 `MoovInfo`，**统一定优先级后再发 `Field` 事件**，确保跨子盒取舍确定。

### 6.1 `MoovInfo` 扩展

```rust
struct MoovInfo {
    dims: Option<(u32, u32)>,
    duration_ms: Option<u64>,
    created: Option<DateTimeParts>,   // mvhd 来源
    // 新增：
    gps_xyz: Option<Gps>,
    gps_loci: Option<Gps>,
    gps_mdta: Option<Gps>,
    mdta_created: Option<DateTimeParts>,
    camera_make: Option<String>,
    camera_model: Option<String>,
    warnings: Vec<Warning>,
}
```

### 6.2 布局一：`moov/udta/©xyz`（`0xA9 'x' 'y' 'z'`）

载荷 = `u16 size`(big-endian，后随文本长度) + `u16 lang`(packed) + ISO 6709 串。
ISO 6709 解析：按 `+`/`-` 切分有符号十进制数序列 → ①纬 ②经 ③可选高(米)，可有尾随 `/`。例 `+27.5916+086.5640+8850/`。

### 6.3 布局二：`moov/udta/loci`（FullBox，3GPP TS 26.244）

`version(1)+flags(3)` → `language(2,packed)` → **name(变长，UTF-8 或 UTF-16-by-BOM，null 终止，边界安全跳过)** → `role(1)` → **`longitude(4,16.16 有符号)` → `latitude(4,16.16)` → `altitude(4,16.16)`**（注意**经在前**）。
`16.16 → deg = raw_i32 / 65536`，再 ×1e7 取整。

### 6.4 布局三：`moov/meta` QuickTime keys/ilst（mdta）

**关键区别**：QuickTime 的 `moov/meta` 是**普通容器盒，非 FullBox**（与 HEIF 的 `meta` FullBox 相反）。探测：peek `meta` 载荷前 8 字节，若是合法子盒头（`hdlr` 等）则按 QuickTime 处理；若像 version/flags 则按 ISO 跳过。本路径只在 `moov/meta`（QuickTime）下生效，不影响 A2 的 HEIF `meta`（其在文件顶层、走既有 `BmffParser` 抽取阶段）。

结构：
- `hdlr`：校验 handler type == `mdta`，否则放弃本路径。
- `keys`：`version(1)+flags(3)+entry_count(4)`，逐项 `size(4)+namespace(4)+key_string`；建立 **1-based 索引 → 键名** 表（`max_*` 上界约束条目数与串长）。
- `ilst`：逐子盒，盒类型 = key 索引（big-endian u32）；内含 `data` 原子 `type(4)+locale(4)+payload`。按索引回查键名。

取四键（命中即填 `MoovInfo`）：

| 键名 | payload | 目标 |
|---|---|---|
| `com.apple.quicktime.location.ISO6709` | ISO 6709 串（同 §6.2 解析） | `gps_mdta` |
| `com.apple.quicktime.make` | UTF-8 文本 | `camera_make` |
| `com.apple.quicktime.model` | UTF-8 文本 | `camera_model` |
| `com.apple.quicktime.creationdate` | ISO 8601 串 | `mdta_created` |

### 6.5 优先级与事件发射

`parse_moov` 末尾按优先级定值后发 `Field`：

- **GPS**：`©xyz > mdta > loci`（最常见/最高质在前；三者本应编码同一坐标，loci 16.16 精度更糙、最罕见，垫底）。最多发一个 `Field::Gps`。
- **created**：`mdta creationdate > mvhd`（mdta 带真实时区，优于 mvhd 的 1904 本地误存 UTC）。发一个 `Field::Created`。
- **make/model**：mdta 唯一视频来源 → `Field::CameraMake`/`Field::CameraModel`。

### 6.6 安全

全程 `checked_*` 偏移、显式迭代（非递归）；name 变长串、keys/ilst 条目数、串长均设上界；截断/越界/未知 handler/坏 ISO6709/坏 ISO8601 → `Warning` 或干净缺失，**永不 panic、不超 `Limits`**。

---

## 7. ISO 8601 解析（`normalize.rs` 旁 helper，mdta 专用）

`YYYY-MM-DDThh:mm:ss[Z|±hh:mm]` → `DateTimeParts`。严格定长定分隔；`Z` → `tz_offset_min = Some(0)`，`±hh:mm` → 解析分钟，无后缀 → `None`；任一段越界 → `None`（不臆造）。仅 mdta creationdate 使用。

---

## 8. 引擎（`driver.rs`）

`Collector` 增 `gps: Option<Gps>`、`camera_make: Option<String>`、`camera_model: Option<String>`，均「首个非空胜出」（沿用现有 `Field` 习惯）。`handle` 增三个 `Field` 分支。`finalize` 在 `normalize` 之后：

```rust
if let Some(g) = col.gps { unified.gps = Some(g); }
if let Some(m) = col.camera_make { unified.camera_make = Some(m); }   // 容器优先
if let Some(m) = col.camera_model { unified.camera_model = Some(m); }
```

---

## 9. 测试

**`normalize` 单测**
- EXIF GPS 四象限（N/S/E/W 各定号）、高程正/负（AltitudeRef 0/1）。
- XMP `"DDD,MM.mmmm[NSEW]"` 与 `"DDD,MM,SS[NSEW]"` 两形式；EXIF 优先于 XMP。
- 仅 lat 或仅 lon → `None`；坏值 → `None` + `UnrecognizedValue`；缺失 → 静默 `None`。
- ISO 8601：`Z`/`±hh:mm`/无后缀/越界。

**`bmff` 单测**
- `©xyz`（含/不含高程）；`loci`（经在前、16.16、name 跳过、UTF-16 BOM name）。
- mdta：keys/ilst 索引关联、四键、QuickTime `meta` **非-FullBox** 探测、与 HEIF `meta` 不相互污染。
- 三源优先级：©xyz＞mdta＞loci；created mdta＞mvhd。
- 截断/越界/未知 handler/坏串 → 永不 panic、不超 `Limits`。

**四适配器差分**（`read_slice`/`push`/`read_blocking`/`read_seek` 一致）
- 带 `©xyz` 的合成 mp4；带 mdta 四键的合成 `.MOV`；带 GPS IFD 的 JPEG。验证 `unified.gps`（及视频 make/model/created）跨四路一致。

**no_std**：`--no-default-features` 构建通过。

---

## 10. 涉及文件

| 文件 | 改动 |
|---|---|
| `omni-meta-core/src/model.rs` | `Gps` 结构；`Unified.gps`；`Field` 去 `Copy` + 三新变体 |
| `omni-meta-core/src/normalize.rs` | EXIF GPS + XMP GPS 投影 helper；ISO 8601 helper |
| `omni-meta-core/src/driver.rs` | `Collector` 三新字段 + `handle` 分支 + `finalize` 覆盖 |
| `omni-meta-core/src/formats/bmff.rs` | `parse_moov` 下钻 `udta`(©xyz/loci) 与 `meta`(mdta keys/ilst)；`MoovInfo` 扩展；优先级发射 |
| `omni-meta-core/src/containers/isobmff.rs` | （按需）16.16 定点 / 子盒 peek helper |
| `omni-meta/tests/differential.rs` | 三个新样本的四适配器一致性 |

---

## 11. 不变量（不得破坏）

- `#![forbid(unsafe_code)]` 全库。
- 容器/IFD 遍历**显式栈迭代**，非递归。
- 所有偏移/长度 `checked_*`，溢出 → `Warning` 跳过，不 panic。
- 顶层 API 永返回 `Metadata`（best-effort + `warnings`）。
- 缺失即 `None`，**绝不臆造**。
- 新增路径通过**全部适配器差分一致性**。
- E7/mm 整数表示保 `Unified`/`Metadata` 的 `Eq` derive 全库不变。
