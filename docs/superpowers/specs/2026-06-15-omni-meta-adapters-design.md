# omni-meta 适配器阶段（blocking / seek / push）设计

**日期**: 2026-06-15
**状态**: 已批准，待实现计划
**对应路线**: `docs/superpowers/specs/2026-06-14-omni-meta-design.md` §11 阶段 6（适配器）
**前置**: 阶段 1–2 已完成（骨架 + `read_slice` + JPEG/EXIF 端到端，见 `2026-06-14-omni-meta-skeleton-jpeg-exif.md`）

## 1. 目标与范围

落地路线图阶段 6 的**同步适配器**，让同一套 sans-io 解析逻辑能从真实 I/O 源读取元数据：

- `read_blocking<R: Read>`：仅顺序读源（管道、网络流等）。
- `read_seek<R: Read + Seek>`：可 Seek 源，跳过非元数据段时**原生 seek 省 I/O**。
- `PushParser`：调用者掌握主动权的 push 适配器，`no_std` 亦可用。

并以**跨适配器差分测试**（spec §10 的核心正确性保证）证明 `slice / blocking / seek / push` 四条路径对同一输入产出逐字段相同的 `Metadata`。

### 本阶段范围

1. **workspace 拆分**：核心 sans-io（无 I/O）与依赖 I/O 的部分拆成两个 crate，用编译器强制 sans-io 纯净。
2. **`JpegParser` 增量化**：从"整块缓冲一次 pull 即 Done"改为逐段推进、按需 `NeedBytes` / `Skip` 的可恢复状态机。
3. **`StreamDriver`**：拥有增长缓冲的流式驱动引擎，复用现有 `Collector` / codecs / normalize，实现 spec §5 的三级 Skip/seek 降级。
4. **三个适配器**：`PushParser`（核心 crate）+ `read_blocking` / `read_seek`（facade crate）。
5. **差分测试**：fixture × chunk-size 矩阵。

### 非目标（本阶段）

- **async/tokio 适配器**：留待下一阶段（彼时引入 tokio 这一真正的重型外部依赖，crate 边界隔离依赖的收益最大）。
- 新格式、新 codec、新容器（PNG/WebP/BMFF/EBML/XMP/IPTC/ICC 等）。
- Stripper（写路径）。
- JPEG 之外的回跳（`SeekTo` 向后）实战场景——JPEG 元数据纯向前，向后降级逻辑本阶段以假解析器单测覆盖，符合现有 `drive_slice` 测试惯例。

## 2. 设计基线决策

| 维度 | 决定 | 理由 |
|---|---|---|
| 适配器档位 | blocking + seek + push | 三者足以支撑差分测试且不引入任何第三方依赖；async 留待下一阶段 |
| 流式真实度 | 真流式 Driver + JpegParser 增量化 | 让 sans-io 流式机制被真正行使；seek 适配器才真正区别于 blocking，差分测试才有意义 |
| crate 边界 | 现在就拆 workspace 两 crate | 引入 `std::io` 是首个 I/O 触点，正是硬化边界的拐点；crate 边界由编译器强制，远强于 `#[cfg(feature="std")]` 约定 |
| 零拷贝 slice | `read_slice` 保留现有 `drive_slice` 路径不动 | slice 全缓冲在场，可零拷贝借用输入；这是相对流式路径的真实优势，值得保留，并作为差分测试第 4 条路径 |

## 3. 架构

一句话：**一个流式引擎，三个薄封装，push 为原语**。精确对应 spec §7"适配器都是 Driver 的薄封装，区别仅在'如何补字节'与'如何执行 Skip/SeekTo'"。

```
                 ┌─────────────────────────────────────┐
                 │  driver::StreamDriver  (新增)         │
                 │  自有增长 buf + parser +             │
                 │  Collector + skip_remaining          │
                 │  step() -> Need / Skip(n) / Done     │
                 └─────────────────────────────────────┘
                    ▲            ▲              ▲
        feed/skip   │            │ read+feed    │ read+feed/seek
                    │            │              │
              ┌─────┴────┐  ┌────┴─────┐  ┌─────┴──────┐
              │PushParser│  │read_     │  │read_seek   │
              │(no_std)  │  │blocking  │  │(seek 省I/O)│
              └──────────┘  └──────────┘  └────────────┘

   read_slice（现有 drive_slice，零拷贝）保持不变 —— 第 4 条路径。
```

四个适配器共享**同一 parser、同一 codecs、同一 normalize**，只有"补字节"方式不同——这正是差分测试要证明的 sans-io 不变量。

## 4. Workspace 布局

```
omni-meta/                    (workspace 根)
├── Cargo.toml                # [workspace] members = ["omni-meta-core", "omni-meta"]
├── omni-meta-core/
│   ├── Cargo.toml            # no_std+alloc, 零依赖, [features] 空
│   └── src/
│       ├── lib.rs            # #![no_std] #![forbid(unsafe_code)]; re-export facade
│       ├── cursor.rs limits.rs error.rs model.rs demand.rs normalize.rs probe.rs
│       ├── codecs/  formats/
│       ├── driver.rs         # StreamDriver(新) + drive_slice(迁入)
│       └── adapters/{slice.rs, push.rs}   # read_slice(迁入) + PushParser(新)
└── omni-meta/
    ├── Cargo.toml            # 依赖 omni-meta-core; [features] default=["std"], std=[]
    ├── src/
    │   ├── lib.rs            # pub use omni_meta_core::*;  + std 适配器 re-export
    │   └── adapters/{blocking.rs, seek.rs}   # read_blocking / read_seek(新)
    └── tests/
        ├── read_slice_jpeg.rs   # （从当前 tests/ 迁入）
        └── differential.rs       # 新增：slice vs blocking vs seek vs push
```

- 核心 crate 无条件 `#![no_std]` + `#![forbid(unsafe_code)]`，**核心内不再有 `std` feature**——sans-io 纯净由编译器保证（`std::io` 无法进入）。
- `omni-meta` facade 用 `pub use omni_meta_core::*;` 再导出全部核心符号，现有用户路径（`omni_meta::read_slice` / `Options` / `Metadata` / …）不受影响。
- `read_slice` 与 `PushParser` 都只吃 `&[u8]` / chunk，本身就是 sans-io，归核心 crate。`read_blocking` / `read_seek` 需要 `std::io::{Read, Seek}`，归 facade crate。
- 差分测试落在 `omni-meta`（能同时看到四条路径）。

## 5. `JpegParser` 增量化

现状：`JpegParser` 是单元结构体，单次 `pull` 走完整块缓冲后无条件 `Done`，从不发 `NeedBytes` / `Skip`。截断输入会静默返回部分结果——流式下会出错。

改为可恢复状态机：

- **状态**：`{ saw_soi: bool, done: bool }`。逻辑位置由 driver 经 `consumed` 维护；parser 只看驱动递给它的窗口。
- **每次 `pull(window)`**：
  - 读 SOI / 下一个 marker+length 需 2 字节：窗口不足则返回 `NeedBytes(needed)`，`consumed: 0`。
  - **元数据段**（APP1 且以 `Exif\0\0` 开头）：需整段在窗内 → 不足则 `NeedBytes(2+len)`；齐了 `take` 出来，发 `Payload { Exif, &body[6..] }`，`consumed = 2+len`。
  - **非元数据段**（其它 APPn、DQT、DHT…）：消费 4 字节段头后发 `Skip(len-2)` 跳过段体——**这正是 `read_seek` 原生 seek 省 I/O 的来源**（大缩略图 / ICC 段）。
  - **SOS / EOI** → `Done`（元数据在熵编码扫描之前）。
  - 畸形（非 SOI、len < 2、截断等）：best-effort，发对应 Warning / 停止，已收集结果照常返回，绝不 panic。

不变量：对**完整 slice**，增量化后产出与现状逐字段相同（差分测试的 slice 基准保持）；对**分块源**，正确请求更多字节而非静默截断；对**可 Seek 源**，原生跳过非元数据段。

## 6. `StreamDriver` 字节管理 + 三级 Skip/seek 降级

```rust
pub struct StreamDriver<P: MetaParser> {
    buf: Vec<u8>,          // 自有增长窗口（非零拷贝——流式必须自有）
    cursor: usize,         // buf 内逻辑读位置
    parser: P,
    collector: Collector,  // 复用现有 Collector + Collector::handle
    skip_remaining: u64,   // 尚待跳过的字节（在途的向前 Skip）
    pos_base: u64,         // buf[0] 的绝对文件偏移（用于 Warning offset 保真）
}
```

step 循环（由适配器驱动）：

1. `parser.pull(&buf[cursor..])` → 事件**在作用域内**立即喂给 `collector`（buf 改动前处理完，保证 Payload 借用有效——与 `drive_slice` 同一纪律）。
2. 分派 `Demand`：
   - `Done` → 结束。
   - `NeedBytes(n)` → 向适配器要更多（`Outcome::Need`）。适配器报 EOF 仍不足 → `Warning::Truncated`，结束。
   - `Skip(n)` → `cursor += consumed`，设 `skip_remaining = n`，对外 `Outcome::SkipHint(n)`。随后喂入的字节先抵扣 `skip_remaining`（吞掉不解析）；seek 适配器则根本不读这些字节。向前 = §5 级别 1，**永不报错**。
   - `SeekTo(p)`：**向前或落在保留缓冲内** → 调 cursor / 丢弃并重填（§5 级别 1–2）。**向后且早于保留下界 + 源不可 Seek** → `Warning::UnreachableSection`，跳过该段，继续（§5 级别 3）。JPEG 从不向后，本阶段级别 3 由假解析器单测覆盖。
3. **内存上界**：消费后丢弃前缀 `buf[..cursor]`（JPEG 无需锚点 → `retain_floor == cursor`）；受 `Limits::max_retained_bytes` 约束。沿用 `drive_slice` 的防卡死迭代预算。

三个适配器**仅**在如何应答 `Need` / `SkipHint` 上不同：

| | `Need` | `SkipHint(n)` |
|---|---|---|
| **PushParser** | 调用者 `feed(chunk)` | 调用者可 `feed`（driver 吞掉）**或**自行 seek 后 `skip(n)` |
| **read_blocking** | `read()` 下一块 | **读并丢弃** n 字节 |
| **read_seek** | `read()` 下一块 | **`seek(Current(n))`** + `driver.skip_external(n)` —— *省 I/O* |

### Push API（对齐 spec §7）

```rust
pub enum Outcome { Need(usize), SkipHint(u64), Done }

pub struct PushParser { /* 持有 StreamDriver */ }
impl PushParser {
    pub fn new(opts: Options) -> Self;
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Outcome, Error>;
    pub fn skip(&mut self, n: u64);     // 调用者已自行向前跳 n 字节后，推进逻辑位置
    pub fn finish(self) -> Metadata;
}
```

`feed` 与 `skip` 两条路结果一致，区别只在那 N 字节是否真流过内存。`SkipHint` 纯属可选优化：忽略它、永远只 `feed` 也完全正确。

### std 适配器签名

```rust
pub fn read_blocking<R: Read>(r: R, opts: Options) -> Result<Metadata, Error>;
pub fn read_seek<R: Read + Seek>(r: R, opts: Options) -> Result<Metadata, Error>;
```

与 `read_slice` / `read_async` 一致：仅"连格式都识别不了 / I/O 源直接报错"才返回 `Err`；格式内局部损坏走 `warnings`。`read_blocking` 内部以 `Read` 读固定大小块喂 driver；`read_seek` 额外在 `SkipHint` 上原生 seek。

## 7. 差分测试与测试策略

核心正确性保证（spec §10）。`omni-meta/tests/differential.rs`：

```rust
// 同一 JPEG 字节经四个适配器 → 断言 Metadata 逐字段相等。
let want = read_slice(&bytes, opts);
assert_eq!(want, read_blocking(&bytes[..], opts));         // R = &[u8]
assert_eq!(want, read_seek(Cursor::new(&bytes), opts));    // R = std::io::Cursor
assert_eq!(want, push_drive(&bytes, opts));                // 以 1/3/7 字节分块 feed
```

**矩阵**：多个 fixture（EXIF-first JPEG；含大非元数据段以行使 `Skip` 的 JPEG；截断 JPEG → 相同 `Warning`；非 JPEG → 相同 `Err`）× 流式路径的多种 chunk 大小（1、3、7、整块）。`Metadata` 已派生 `PartialEq`，直接相等比较。

**单元 TDD**：
- `StreamDriver`：NeedBytes / Skip / SeekTo / 向后降级（假解析器，复刻现有 `drive_slice` 测试惯例）。
- 增量 `JpegParser`：逐段推进、`Skip` 发射、截断→`NeedBytes`、畸形→Warning。
- `PushParser`：`feed` 与 `skip` 等价性。
- **no_std 构建**：CI 单独 `cargo build -p omni-meta-core`（无 std）。

## 8. 安全与错误（沿用 spec §9）

- 两 crate 均 `#![forbid(unsafe_code)]`；核心 crate `#![no_std]`。
- `StreamDriver` 的 `buf` 增长前查 `max_retained_bytes`；所有偏移/长度 `checked_*`，溢出→Warning 跳过。
- 防卡死迭代预算（沿用 `drive_slice`），任何畸形/恶意 parser 都不致死循环。
- 顶层 API 错误姿态与 `read_slice` 一致。

## 9. 实现顺序（纵切片，每步可独立测试/合并）

1. **workspace 纯重构**：建两 crate、迁移文件、facade re-export；现有全部测试保持绿、`read_slice` 公开路径不变。
2. **`JpegParser` 增量化**：状态机化 + `Skip` 发射；slice 路径输出不变（现有测试守门）。
3. **`StreamDriver`**：流式引擎 + 三级降级；假解析器单测。
4. **`PushParser`**：no_std push 适配器 + 单测（feed/skip 等价）。
5. **`read_blocking` / `read_seek`**：std 适配器。
6. **差分测试**：fixture × chunk-size 矩阵，证明四路一致。
```