# A3：MP4/MOV `moov` 元数据抽取（设计）

**日期** 2026-06-15 · **里程碑** A 的 A3 切片 · 上游基座见 A1/A2
计划：[`docs/superpowers/plans/2026-06-15-omni-meta-bmff-moov.md`](../plans/2026-06-15-omni-meta-bmff-moov.md)
基准设计：[`docs/superpowers/specs/2026-06-14-omni-meta-design.md`](2026-06-14-omni-meta-design.md)

---

## 1. 目标与范围

让 `BmffParser` 从 MP4/MOV 的 `moov` box 抽取**维度 + 时长 + 创建时间**，并把 `duration`、`created`
两个新 Unified 字段以**受控增长**方式纳入（各满足「≥2 格式来源」）。

**纳入：**
- **维度**：`moov` → `trak` → `tkhd`（TrackHeaderBox）的 `width`/`height`（16.16 定点，取整数部分
  `val >> 16`）。逐 `trak` 取**首个 width 与 height 均非零**的 tkhd（天然选中视频轨，跳过音频/数据轨）。
- **时长**：`moov` → `mvhd`（MovieHeaderBox）的 `duration` / `timescale` → `duration_ms`
  （毫秒，`duration * 1000 / timescale`，u128 中间量防溢出；`timescale == 0` → 跳过并警告）。
- **创建时间**：`mvhd` 的 `creation_time`（自 **1904-01-01 00:00:00 UTC** 的秒）→ `DateTimeParts`
  （`tz_offset_min = Some(0)`，即 UTC）。`creation_time == 0`（未设置惯例）→ 不产出 created。
- **created 第二来源（EXIF）**：`normalize` 从 `raw.exif` 取 `DateTimeOriginal`（0x9003，Exif IFD）→
  回退 `DateTime`（0x0132，IFD0），解析 `YYYY:MM:DD HH:MM:SS` → `DateTimeParts`；时区默认 `None`，
  若对应 `OffsetTime*` 标签（0x9011 / 0x9010，Exif IFD，形如 `±HH:MM`）存在则解析填入 `Some(offset)`。

**不纳入（后续里程碑）：**
- iTunes/`ilst` 风格的键值元数据（`moov/udta/meta/ilst`）。
- 多视频轨选择策略（取首个非零 tkhd 即可）；`grid`/派生维度。
- `cargo-fuzz`：作为贯穿各里程碑的独立横切项另立（见 ROADMAP §4）。本切片用**合成畸形 fixture
  单测**锁定「永不 panic / 不超 `Limits` / 不死循环」不变量。
- 进一步的时区推断（仅按 EXIF `OffsetTime*` 字面解析，不查地理/夏令时）。

**Unified 改动面**：新增 `duration_ms: Option<u64>` 与 `created: Option<DateTimeParts>`。
- `duration_ms` 来源：BMFF moov（本切片）。第二来源由里程碑 C（EBML）补齐 —— 故本切片落地时
  `duration_ms` 暂为**单来源**，符合「评估后纳入、随来源达 ≥2 完善」的受控增长节奏，且字段为
  format-neutral 毫秒、为 EBML 复用预置（见 §6 决策记录）。
- `created` 来源：BMFF moov + EXIF（≥2，本切片即满足）。

---

## 2. 模块边界

| 文件 | 职责 | 动作 |
|---|---|---|
| `model.rs` | 新增 `DateTimeParts` 类型；`Field::Duration`/`Field::Created`；`Unified.duration_ms`/`Unified.created` | Modify |
| `lib.rs` | `pub use` 导出 `DateTimeParts` | Modify |
| `driver.rs` `Collector` | 收集容器 `Duration`/`Created` Field；`finalize` 中容器值覆盖 EXIF 派生值 | Modify |
| `normalize.rs` | EXIF 日期标签（DateTimeOriginal/DateTime + OffsetTime）→ `Unified.created` 回退来源 | Modify |
| `formats/bmff.rs` | `moov` 语义层：`civil_from_*`（1904 epoch→民用历法）、`parse_mvhd`、`parse_tkhd`、`parse_moov`；`pull_walk` 增 `moov` 分支 | Modify |
| `formats/bmff.rs` `mod tests` | 合成 MP4 fixture 单测（mvhd/tkhd/moov、v0/v1、timescale=0、creation=0、截断、溢出、嵌套） | Modify |
| `tests/differential.rs` | 完整 MP4 fixture（含 moov-after-mdat 变体）跑四适配器一致性 | Modify |
| `docs/ROADMAP.md` | 勾选 A3，链接本 spec/plan | Modify |

**职责切分**：`isobmff.rs`（A1）已提供 `read_box_header`/`full_box_vf`/`iter_child_boxes`/`read_uint_be`，
A3 **零新增**结构层 API —— `moov`/`trak` 子盒遍历直接复用 `iter_child_boxes`（深度 2，仍是显式迭代非递归）。
`bmff.rs` 只回答「MP4 元数据什么含义」（哪个盒是 mvhd/tkhd、定点维度、1904 纪元换算）。

---

## 3. 状态机（沿用 A2 的两阶段框架）

`BmffParser` 当前 `pull_walk` 仅识别 `meta`（HEIF）。A3 在同一走盒循环里加 `moov` 分支：

### Phase `Walk`
- 非 `meta`/`moov` box：`Demand::Skip(payload_len)` 跳过盒体 —— **绝不缓冲 `mdat`**（视频 mdat 常达 GB）。
- 命中 `moov`：`Demand::NeedBytes(moov.total_size)` 整盒入窗（与 meta 同款「缓冲整盒再解析」）。
  - **DoS 边界**：`moov` 受 driver 现有 `max_retained_bytes` 守卫封顶。极大 sample table 致 moov
    超限时 → driver 报 `UnreachableSection` 并干净 `Done`（best-effort 降级，不 panic）。此为已知
    限制：mvhd/tkhd 头位于 moov/trak 起始，未来可改流式下钻 moov 子盒以避免缓冲整盒（A3 不做）。
- 命中 `meta`：A2 路径不变。
- 走到 EOF（空窗口）仍未命中：干净 `Done`（A1/A2 行为）。

### 解析 `moov`（窗口含完整 moov 时一次性执行）
在 `iter_child_boxes(moov_payload)` 上遍历：
- **`mvhd`**：FullBox。version 0：`creation_time`/`modification_time`/`timescale`/`duration` 皆 u32；
  version 1：creation/modification/duration 为 u64、timescale 仍 u32 → `duration_ms` + `created`。
- **`trak`**：对每个 trak，`iter_child_boxes(trak_payload)` 找 `tkhd`：
  - **`tkhd`** FullBox。version 0/1 仅影响 creation/mod/duration 字段位宽；`width`/`height`（各 u32
    16.16 定点）恒为载荷**末 8 字节**，但本设计按 version 计算偏移（v0=76/80，v1=88/92）以避免误读
    可能的尾随字节。取 `>> 16` 为像素整数；w 或 h 为 0 → 跳过该轨。
  - 取**首个**非零 tkhd 维度。

产出 `MoovInfo { dims, duration_ms, created, warnings }` → 立即发 `Field` 事件
（`Width`/`Height`/`Duration`/`Created`），随后 `Done`（moov 内无需 SeekTo，全部已入窗）。

> moov 处理**不进入 Extract 阶段**（无 method-0 目标）。因全部数据已在缓冲窗口内，四适配器对
> 「Skip(mdat) → NeedBytes(moov) → 一次性解析」路径天然一致。

---

## 4. 数据流与正确性

- **维度优先级**：tkhd 走容器 `Field::Width/Height` 路径，`finalize` 中容器维度覆盖 EXIF/XMP →
  与 PNG/WebP/HEIF(ispe) 一致，`normalize` 零改动。
- **created 优先级**：镜像维度机制 —— 容器（moov）值经 `Field::Created` 入 `Collector`；EXIF 派生值经
  `normalize` 进 `Unified`；`finalize` 中**容器值（若 Some）覆盖** EXIF。因 moov `creation_time==0` →
  容器无产出，此时 EXIF `DateTimeOriginal` 自然回填。
- **duration**：仅 moov 提供，`finalize` 容器值直接落 `Unified.duration_ms`。
- **错误处理**（不臆造、不 panic、全程 `checked_*`）：
  - 截断的 moov / mvhd / tkhd → 字段读不全即该字段 `None`；整盒声明 size 超实际 → driver `Truncated`。
  - `timescale == 0` 或 `duration * 1000` 溢出 u128→u64 → 不产出 duration，记 `UnrecognizedValue` 警告。
  - `creation_time == 0` → 不产出 created（视作未设置，非 1904-01-01）。
  - 畸形 box 头 / 子盒长度自洽性破坏 → `iter_child_boxes` 停止遍历，已抽取结果保留。
  - EXIF 日期串格式不符 / 字段越界（月>12 等）→ 不产出 created（不臆造）。
- **DoS 边界**：整个 `moov` 须先缓冲再解析，受 `max_retained_bytes` 封顶；`trak` 数量与子盒迭代被盒体
  字节数（游标边界）限死，无需额外 `max_tracks`（沿用 `parser_for` 不向格式解析器传 `Limits` 约定）。

### `DateTimeParts` 表示

```rust
/// 民用时间戳。tz_offset_min:
///   None     = 无时区信息（如 EXIF 本地时间，不臆造）
///   Some(0)  = UTC（如 BMFF moov 1904 纪元）
///   Some(±n) = UTC±n 分钟（如 EXIF OffsetTime "+09:00" → Some(540)）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTimeParts {
    pub year: u16, pub month: u8, pub day: u8,
    pub hour: u8, pub minute: u8, pub second: u8,
    pub tz_offset_min: Option<i16>,
}
```

Copy/Eq → 四适配器逐字段 `assert_eq` 友好。1904 纪元→民用历法用纯整数算法（Howard Hinnant
`civil_from_days`，no_std 安全，无浮点）。锚点校验向量：`2_082_844_800` 秒（= 24107 天）后即
**1970-01-01T00:00:00**。

---

## 5. 测试策略

全部**合成 fixture**（仓库不引入二进制样本），延续既有风格。

- **bmff `mod tests`**：
  - `civil_from_days`：`0→1970-01-01`、`31→1970-02-01`、`-1→1969-12-31`。
  - `datetime_from_mp4_epoch`：`2_082_844_800→1970-01-01T00:00:00 tz=Some(0)`；`+86400→次日`；`+3661→01:01:01`。
  - `parse_mvhd`：v0 → 正确 `duration_ms`/`created`；v1（u64 字段）；`timescale=0`→无 duration；`creation=0`→无 created；截断载荷→None。
  - `parse_tkhd`：v0 定点 1920×1080；audio 轨 0×0 → 跳过；v1 偏移正确。
  - `parse_moov`：`moov{mvhd, trak{tkhd 视频}, trak{tkhd 音频 0×0}}` → 选中视频维度 + 时长 + created。
  - `pull_walk`：命中 `moov` → `NeedBytes(total)`；整盒入窗后发 Field 事件并 `Done`。
  - 端到端：`ftyp + moov` 经 `drive_slice`+`finalize` → `unified.{width,height,duration_ms,created}`。
  - 畸形：截断 moov（声明 size>实际）→ `Truncated`；`duration` 溢出 → 无 panic、无 duration、有警告；
    嵌套深 trak、声明子盒越界 → 停止遍历不 panic。
- **`tests/differential.rs`**：完整 MP4 fixture（`ftyp + moov`，及 **moov-after-mdat** 变体以行使
  Skip/seek 路径）跑 `assert_all_equal`，验证 slice/blocking/seek/push 四路逐字段一致。
- **收尾验证**：`cargo test` 全绿；`cargo build -p omni-meta-core --no-default-features`（no_std 不破）；
  `cargo clippy --all-targets -- -D warnings` 清零。

---

## 6. 决策记录（brainstorm 结论）

1. **created 表示** → `DateTimeParts{…, tz_offset_min: Option<i16>}`。同时承载有/无时区两种语义：BMFF=UTC
   `Some(0)`、EXIF 默认 `None`。诚实反映来源，不丢「BMFF 本是 UTC」也不臆造「EXIF 时区」。
2. **EXIF 时区** → 存在 `OffsetTime*` 即解析填入，否则 `None`（字段已预留，无 OffsetTime 时不破坏）。
3. **duration 表示** → `duration_ms: Option<u64>`（毫秒）。Unified 契约是 format-neutral 归一投影；
   `(value, timescale)` 元组本质是 raw（timescale 在 BMFF/EBML 语义不同，逼消费方知源格式）。毫秒是
   两边归一进去的公分母，u64 ms 分辨率充裕。
4. **fuzz 范围** → 合成畸形单测锁定不变量；`cargo-fuzz` 作为独立横切里程碑另立，避免引 nightly 破坏
   零依赖/no_std 基调。

---

## 7. 完成定义（A3）

- MP4/MOV 文件能从 `moov` 抽出 `width`/`height`（tkhd）、`duration_ms`（mvhd）、`created`（mvhd 1904 UTC）。
- EXIF 作 created 第二来源（DateTimeOriginal/DateTime + OffsetTime 解析）；容器值优先、EXIF 回退。
- `timescale=0`/`creation=0`/溢出/截断/越界均产出恰当警告或干净缺失、**绝不 panic**。
- 四适配器对同一 MP4 输入逐字段一致（含 moov-after-mdat）。
- no_std 构建与 clippy 均干净。
- Unified 新增 `duration_ms`/`created`；`DateTimeParts` 导出。
