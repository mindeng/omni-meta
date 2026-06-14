# omni-meta 设计规范

**日期**: 2026-06-14
**状态**: 已批准，待实现计划
**crate**: `omni-meta`（Rust edition 2024）

## 1. 目标与范围

`omni-meta` 是一个多媒体文件**元数据解析**库，设计目标：

- **sans-io 风格 API**：核心解析逻辑不接触任何 I/O，只与调用者交换"需求/事件"。
- **按需跳字节**：当数据源支持 Seek 时，能跳过指定字节数以省去 I/O；不支持时优雅降级。
- **覆盖常见图片/视频格式**。

### 职责边界

- **读（解析）为主**：从文件提取元数据。
- **写仅限"剥离"**：提供"删除全部元数据"能力（隐私场景），**不支持**通用的字段编辑/插入回写。

### v1 支持的格式

| 类别 | 格式 |
|---|---|
| 图片（传统） | JPEG、TIFF |
| 图片（Web） | PNG、WebP、GIF |
| 图片（现代/BMFF） | HEIF/HEIC、AVIF |
| 视频 | MP4/MOV（ISO-BMFF）、MKV/WebM（Matroska/EBML） |

元数据载体：EXIF、XMP、IPTC、ICC（摘要）、以及容器原生字段。

### 非目标（v1）

- 通用元数据写回/编辑（仅支持剥离）。
- 完整 ICC profile 解析（只取摘要）。
- 解码实际像素/音视频帧。
- AVI、FLV、OGG 等额外容器（架构预留，后续版本）。

## 2. 设计基线决策

| 维度 | 决定 |
|---|---|
| 职责 | 读 + 仅"删除全部元数据"剥离 |
| 数据模型 | 统一规范模型 + 原始标签双层 |
| sans-io | Pull 指令机为唯一核心 + Push/同步/异步适配器 |
| 指令集 | `NeedBytes` / `Skip`（向前）/ `SeekTo`（绝对，兜底）/ `Done` |
| 运行环境 | 核心 `no_std` + `alloc`，零第三方依赖 |
| 错误姿态 | best-effort：尽力解析，返回部分结果 + 警告列表 |
| 安全 | `#![forbid(unsafe_code)]`，全程 `Limits` 上界 + 显式栈迭代 |

## 3. 架构分层

四层，依赖单向向下：

```
┌─────────────────────────────────────────────────────────┐
│  适配器层 (adapters)  —— feature-gated, 可选            │
│  read_blocking · read_seek · read_slice · async · push  │
├─────────────────────────────────────────────────────────┤
│  编排/驱动层 (engine)                                    │
│  Probe(格式探测) · Driver(指令循环) · MetaParser(trait)  │
├─────────────────────────────────────────────────────────┤
│  格式层 (formats)         共享容器读取器 (containers)    │
│  jpeg/png/gif/webp/  ←→   isobmff(box) · riff · ebml ·   │
│  heif/avif/mp4/mkv        jpeg_seg · tiff_ifd            │
├─────────────────────────────────────────────────────────┤
│  解码层 (codecs)          数据模型 (model)               │
│  exif · xmp · iptc · icc  RawTags · Metadata(统一) · Warn │
└─────────────────────────────────────────────────────────┘
        ↑ no_std + alloc, 零依赖, 全部 #![forbid(unsafe_code)]
```

### 模块布局

单 crate、内部模块（后续可拆 workspace）：

```
src/
  lib.rs            // #![no_std] extern crate alloc; pub re-exports
  demand.rs         // Demand / Event / Limits —— 核心指令机类型
  driver.rs         // Driver: 缓冲管理 + 推进 MetaParser + 三级 seek 降级
  probe.rs          // 魔数嗅探 → 选格式解析器
  parser.rs         // trait MetaParser
  model/            // Metadata(统一)、RawTags、GpsCoord、Rational、Warning
  containers/       // isobmff.rs、riff.rs、ebml.rs、jpeg_seg.rs、tiff_ifd.rs
  formats/          // jpeg.rs、png.rs、gif.rs、webp.rs、heif.rs、avif.rs、mp4.rs、mkv.rs
  codecs/           // exif.rs、xmp.rs、iptc.rs、icc.rs
  adapters/         // blocking.rs、seek.rs、slice.rs、async_tokio.rs、push.rs
  strip.rs          // 剥离全部元数据的 sans-io 重写器
```

### feature flags

- `default = ["std"]`
- `std`：开启 blocking/seek/slice 适配器与 `std::error::Error` 实现。
- `tokio`：异步适配器。
- `alloc`：永远开启，no_std 基座。
- 关闭 `std`（`--no-default-features`）即纯 `no_std` + `alloc`，仅保留 `read_slice` 与 `push` 路径。

## 4. 核心 sans-io 指令机

整个库的心脏。格式解析器永不碰 I/O，只与调用者交换需求/事件。

```rust
/// 解析器对调用者提出的需求。驱动循环据此喂数据。
pub enum Demand {
    /// 需要至少 n 字节可读才能继续（当前缓冲不足）。
    NeedBytes(usize),
    /// 从当前位置向前跳过 n 字节（可 Seek 源 → seek(Current(n))，省 I/O）。
    Skip(u64),
    /// 跳到绝对偏移（兜底：尾部索引→回跳；按三级降级处理）。
    SeekTo(u64),
    /// 解析完成。
    Done,
}

/// 解析过程中增量产出的事件。
pub enum Event<'a> {
    /// 一段原始元数据载荷已定位（借用驱动缓冲，零拷贝）。
    Payload { kind: PayloadKind, data: &'a [u8] },
    /// 容器级直接得到的字段（如 mp4 时长、mkv 维度）。
    Field(Field),
    /// 非致命问题（best-effort）。
    Warning(Warning),
}

pub enum PayloadKind { Exif, Xmp, Iptc, Icc }

/// 格式解析器实现的唯一 trait —— 纯状态机，无 I/O、无 async。
pub trait MetaParser {
    /// 用当前可见的输入窗口推进。
    /// `input` 是驱动维护的连续缓冲；返回下一步需求 + 本步消耗字节 + 产出事件。
    fn pull(&mut self, input: &[u8]) -> PullResult;
}

pub struct PullResult {
    pub demand: Demand,
    pub consumed: usize,
    pub events: EventBatch,   // 内联小缓冲，避免每步堆分配
}
```

### 驱动循环算法（`Driver`，被所有适配器复用）

1. 维护增长缓冲 `buf`、逻辑文件偏移 `pos`、保留下界 `retain_floor`。
2. 调用 `parser.pull(&buf[cursor..])`：
   - `NeedBytes(n)` → 适配器把缓冲补到 ≥ n，重试。
   - `Skip(n)` → 见 §5 三级 seek 处理（向前路径）。
   - `SeekTo(p)` → 见 §5 三级 seek 处理。
   - `Done` → 收尾，打包 `RawTags` + 统一字段 + 警告。
3. `Payload` 事件交给对应 `codecs::*` 解码器，产出 `RawTags`，再喂 normalization（§6）。

**核心不变量**：引擎核心只依赖 `&[u8]` 与 `Demand`，对数据来源、同步/异步一无所知——这是 sans-io 的全部意义。

## 5. Seek 语义与三级降级

### 关键事实：v1 格式几乎不需要"源级回跳"

| 格式 | 元数据位置 | 是否需要源级回跳 |
|---|---|---|
| JPEG | APP1/APP13 段靠前 | 否，纯向前 |
| PNG/GIF | chunk/block 顺序读 | 否，纯向前 |
| WebP(RIFF) | EXIF/XMP chunk 顺序 | 否，纯向前 |
| HEIF/AVIF/MP4/MOV | 元数据在 `meta`/`moov`，即便 `moov` 在 `mdat` 之后也是**向前**到达 | 否，纯向前 |
| MKV/WebM(EBML) | Info/Tags 经 SeekHead 定位，通常向前 | 几乎从不 |
| EXIF/TIFF 内部 | IFD 偏移指向"前面" | 否——在已缓冲的**有界 payload 切片**内随机访问，不发指令给源 |

结论：纯元数据提取里真正的"源级回跳"几乎不存在。`SeekTo` 主要为越界 MP4、未来 AVI `idx1` 这类边角格式与健壮性兜底准备。

### 三级降级处理

```rust
pub struct SourceCaps { pub seekable: bool, pub total_len: Option<u64> }
```

驱动把 `Skip` / `SeekTo` 按目标位置分三种情况：

1. **向前**（`target ≥ pos`）：任何源都可满足。可 Seek 源 → 原生 seek（省 I/O）；不可 Seek/Push → 读并丢弃。**永不报错。**
2. **回跳但落在保留缓冲内**（`target ≥ retain_floor`）：只把 cursor 在内存缓冲里移回。Push/管道/socket 全支持，无需源能力。
3. **回跳且早于保留下界 + 源不可 Seek**：唯一硬约束。按 best-effort 降级——发 `Warning::UnreachableSection { offset }`，跳过该段，继续解析其余，最终仍返回**部分结果**。**不是 `Err`，不中断整次解析。**

### 锚点/保留机制

为支撑情况 2：解析器在预判可能回看时调 `anchor()`，驱动从最低活跃锚点起保留字节，松锚后释放。保留量受 `Limits::max_retained_bytes`（默认 16 MiB）约束防 DoS。绝大多数格式从不设锚 → 零额外内存。

### 各源类型行为矩阵

| 源类型 | 向前 Skip/SeekTo | 回跳（缓冲内） | 回跳（超出保留） |
|---|---|---|---|
| `Read + Seek` / mmap / slice | 原生 seek，省 I/O | 原生 seek | 全支持 |
| `Read`（仅顺序） | 读弃 | 缓冲内移动 | Warning + 部分结果 |
| Push / 异步流 | 读弃 / SkipHint | 缓冲内移动 | Warning + 部分结果 |

## 6. 数据模型（统一层 + 原始层）

```rust
pub struct Metadata {
    pub unified: Unified,          // 跨格式规范字段
    pub raw: RawTags,              // 原始标签，分命名空间保留
    pub warnings: Vec<Warning>,    // best-effort 收集
    pub format: FileFormat,        // 探测到的容器格式
}

pub struct Unified {              // 全部 Option —— 缺失即 None，绝不臆造
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: Option<Orientation>,
    pub created: Option<DateTime>,        // 自带时区信息（如有）
    pub gps: Option<GpsCoord>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub duration: Option<Duration>,       // 视频
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    // … 受控增长：每个字段需有 ≥2 种格式来源才纳入
}

pub struct RawTags {
    pub exif: Vec<ExifTag>,        // IFD + tag id + 类型化 value
    pub xmp: Vec<XmpProperty>,     // 命名空间/属性/值
    pub iptc: Vec<IptcRecord>,
    pub icc: Option<IccSummary>,   // 摘要（不解全 profile）
    pub container: Vec<ContainerField>, // box/EBML/chunk 原生字段
}

pub enum Value { U64(u64), I64(i64), Rational(Rational), Text(String), Bytes(Vec<u8>), /* … */ }
```

**设计原则**：统一层是原始层的"投影"。`normalization` 模块把 `raw` 映射到 `unified`，映射规则集中、可测；高级用户随时下钻 `raw` 取原生值。`width/height` 优先用容器级权威值，回退 EXIF。

## 7. 适配器层

所有适配器都是 `Driver` 的薄封装，区别仅在"如何补字节"与"如何执行 Skip/SeekTo"。格式逻辑零重复。

```rust
// 一次性便利 API（std）
pub fn read_blocking<R: Read>(r: R, opts: Options) -> Result<Metadata, Error>;
pub fn read_seek<R: Read + Seek>(r: R, opts: Options) -> Result<Metadata, Error>;
pub fn read_slice(buf: &[u8], opts: Options) -> Result<Metadata, Error>;  // 零拷贝随机访问

// 异步（feature="tokio"）
pub async fn read_async<R: AsyncRead + Unpin>(r: R, opts: Options) -> Result<Metadata, Error>;
pub async fn read_async_seek<R: AsyncRead + AsyncSeek + Unpin>(r: R, opts: Options) -> Result<Metadata, Error>;
```

### Push 适配器（no_std 亦可用）

调用者掌握主动权；`SkipHint` 是向前 `Skip` 在 Push 下的等价物——一条**给调用者的省 I/O 建议**。

```rust
pub enum Outcome {
    Need(usize),      // 需要更多字节，继续 feed
    SkipHint(u64),    // 建议：接下来 n 字节可丢弃；能 seek 就 seek + skip(n)，不能就照常 feed
    Done,
}

pub struct PushParser { /* 持有 Driver */ }

impl PushParser {
    pub fn new(opts: Options) -> Self;
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Outcome, Error>;
    pub fn skip(&mut self, n: u64);   // 调用者已自行向前跳 n 字节后，推进解析器逻辑位置
    pub fn finish(self) -> Metadata;
}
```

**内部机制**：`PushParser` 维护 `skip_remaining: u64`。
- 解析器发 `Skip(n)` → 设 `skip_remaining = n`，对外暴露 `SkipHint(n)`。
- 调用者**喂**字节 → driver 先用这些字节抵扣 `skip_remaining`（吞掉不解析），扣到 0 恢复正常。
- 调用者**自己 seek** 并调 `skip(n)` → 直接把 `skip_remaining` 减 n，那批字节**永不进内存**（省 I/O 来源）。

两条路结果一致，区别只在那 N 字节是否真流过内存。`SkipHint` 纯属可选优化：忽略它、永远只 `feed` 也完全正确。回跳（`SeekTo`）在 Push 下走 §5 的保留缓冲/警告降级，不经由 `SkipHint`。

### 使用示例

```rust
let mut p = PushParser::new(opts);
let mut chunk = src.read_some();
loop {
    match p.feed(&chunk)? {
        Outcome::Need(_)      => { chunk = src.read_some(); }
        Outcome::SkipHint(n)  => {
            if src.can_seek() { src.seek_forward(n); p.skip(n); }  // 省 I/O
            else              { chunk = src.read_some(); }          // 照常喂，内部吞掉
        }
        Outcome::Done         => break,
    }
}
let meta = p.finish();
```

## 8. 剥离 API（唯一的"写"路径）

```rust
/// 把"删除全部元数据"实现为 sans-io 重写状态机：
/// 顺序读容器结构，原样转发媒体数据段，丢弃 EXIF/XMP/IPTC/ICC 等元数据段。
pub struct Stripper { /* sans-io：发 Demand + 产出"写出字节"事件 */ }

pub fn strip_blocking<R: Read, W: Write>(src: R, dst: W, opts: StripOptions)
    -> Result<StripReport, Error>;
```

`Stripper` 复用同一套容器读取器（`isobmff`/`riff`/`jpeg_seg`…），把"识别到元数据载荷"的动作从"解码"换成"跳过并重写长度/结构"。**不重排媒体数据**，复杂度可控。`StripReport` 报告删除项与字节变化。

**v1 范围**：JPEG/PNG/WebP 优先（结构最简单、最常用于隐私场景）；box 类（HEIF/MP4）作为 stretch。

## 9. 错误处理与安全

### 错误姿态

顶层 API 永远返回 `Metadata`（含 `warnings`），只有"连格式都识别不了 / I/O 源直接报错"才返回 `Err`。格式内局部损坏 → `Warning`，继续解析。

### 安全（解析不可信输入是核心威胁面）

- `#![forbid(unsafe_code)]` 全库强制。
- `Limits`：`max_payload_bytes`、`max_retained_bytes`、`max_depth`（box/EBML 嵌套）、`max_tags`、`max_total_alloc`。所有分配前先查上界，防 OOM / 解压炸弹 / 深度递归。
- box/EBML 用**显式栈迭代，非递归**，杜绝栈溢出。
- 所有偏移/长度运算用 `checked_*`，溢出 → Warning 跳过而非 panic。

## 10. 测试策略

- **单元测试**：每个容器读取器 + 每个 codec 用最小构造样本（TDD）。
- **黄金样本**：真实图片/视频小样本 + 已知期望 `Metadata`（快照测试）。
- **差分测试**：同一文件喂 `read_slice` / `read_blocking` / `push` 三条路径，断言结果**逐字段相同**——验证 sans-io 各适配器等价，本库正确性的核心保证。
- **模糊测试**：`cargo-fuzz` 对每个格式解析器，断言永不 panic、永不超 `Limits`、永不死循环。
- **no_std 构建**：CI 单独验证 `--no-default-features`。

## 11. 分阶段路线

每阶段都是可独立测试、可合并的纵切片：

1. **骨架**：`Demand`/`Event`/`Driver`/`MetaParser`/`Limits` + `read_slice` + 差分测试脚手架。
2. **TIFF/EXIF codec**（最高复用价值）+ **JPEG** 格式 → 第一个端到端可用。
3. **PNG/WebP/GIF** + **XMP/IPTC** codec。
4. **ISO-BMFF 容器** → HEIF/AVIF/MP4/MOV 共享落地。
5. **EBML 容器** → MKV/WebM。
6. **适配器**：blocking/seek → async → push（每步跑差分测试）。
7. **Stripper**（JPEG/PNG/WebP 优先）。
