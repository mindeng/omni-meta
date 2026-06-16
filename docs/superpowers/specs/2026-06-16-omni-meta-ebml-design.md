# 里程碑 C：EBML 容器（MKV/WebM）元数据抽取（设计）

**日期** 2026-06-16 · **里程碑** C · 上游基座见里程碑 A（BMFF）
计划：[`docs/superpowers/plans/2026-06-16-omni-meta-ebml.md`](../plans/2026-06-16-omni-meta-ebml.md)
基准设计：[`docs/superpowers/specs/2026-06-14-omni-meta-design.md`](2026-06-14-omni-meta-design.md)

---

## 1. 目标与范围

让 `EbmlParser` 从 MKV/WebM 的 EBML 元素树抽取**维度 + 时长 + 创建时间**，复用里程碑 A 引入的
`duration_ms`/`created` Unified 字段。本切片为 `duration_ms` 补齐**第二来源**（A3 起一直是
BMFF 单来源，至此满足「≥2 格式来源」），并为 `width`/`height` 增加第 6 个来源。

**纳入：**
- **维度**：`Segment → Tracks → TrackEntry → Video` 的 `PixelWidth`(0xB0)/`PixelHeight`(0xBA)（uint）。
  取**首个**两者均非零的 TrackEntry（天然选中视频轨；音频轨无 `Video` 子元素）。
- **时长**：`Segment → Info → Duration`(0x4489，float，单位为 `TimestampScale`)。
  `duration_ms = round(Duration × TimestampScale_ns ÷ 1e6)`。`TimestampScale`(0x2AD7B1) 缺失时默认
  `1_000_000` ns（= 1 ms）。这是 Matroska 权威的整体时长，位于 `Info`、在所有 `Cluster` 之前，
  线性前向走盒即可拿到，**不需要 SeekHead**。
- **创建时间**：`Info → DateUTC`(0x4461，**有符号** int64，自 **2001-01-01 00:00:00 UTC** 的纳秒)
  → `DateTimeParts`（`tz_offset_min = Some(0)`，UTC）。
- **MKV vs WebM**：由 EBML 头的 `DocType`(0x4282) 区分（`"webm"` → WebM，否则含 `"matroska"` → MKV）。

**不纳入（后续里程碑 / 受控增长未达标）：**
- **SeekHead 随机访问 + Matroska `Tags`/`SimpleTag`**：`Tags`/`Cues` 惯例写在 `Cluster` **之后**
  （文件尾部），需读 `SeekHead`(0x114D9B74) 取偏移后 `SeekTo`，或线性跳过全部 Cluster（遇未知大小
  Cluster 即失败）。本切片采用前向线性走盒、**遇未知大小媒体即干净停止**，故 `Tags` 不可达。留作
  独立 SeekHead 里程碑（见 §8）。
- **每轨独立总时长**：Matroska `TrackEntry` 无标准「轨道总时长」元素；`DefaultDuration`(0x23E383)
  是单帧间隔（ns/帧）非总时长；真正的每轨 `DURATION` 标签在尾部 `Tags` 里、与整体时长冗余。不做。
- **`Title`/编解码标识（`video_codec`/`audio_codec`）等 raw/Unified 候选**：单来源、未达「≥2 格式
  来源」门槛（见 §8）。
- **完整未知大小下钻**：不为跳过未知大小 `Cluster` 而构建「元素层级表」逐子下钻；遇此情形若尚未集齐
  Info+Tracks 即干净停止并记 `UnreachableSection`（用户决策）。
- **`cargo-fuzz`**：作为贯穿各里程碑的独立横切项另立（ROADMAP §4）。本切片以**合成畸形 fixture
  单测**锁定「永不 panic / 不超 `Limits` / 不死循环」不变量。

**Unified 改动面**：**无新字段**。复用 `duration_ms: Option<u64>` 与 `created: Option<DateTimeParts>`，
并为 `width`/`height` 增来源。`duration_ms` 由本切片从单来源（BMFF）升为 **≥2 来源**（BMFF + EBML）。

---

## 2. 模块边界（镜像 BMFF 的 `containers`/`formats` 分层）

| 文件 | 职责 | 动作 |
|---|---|---|
| `containers/ebml.rs` | **结构层**：vint 元素 ID 读取（保留标记位，1–4 B）、vint size 读取（剥标记位，1–8 B，全 1 → 未知大小）、`ElemHeader`、`iter_child_elements`（已缓冲定长子元素遍历，越界即停）、大端 `read_uint`/`read_int`/`read_float`。边界安全、绝不 panic。类比 `isobmff.rs`。 | Create |
| `containers/mod.rs` | `pub mod ebml;` | Modify |
| `formats/ebml.rs` | **语义层**：`EbmlParser`（`MetaParser`）、`parse_info`、`parse_tracks`、`datetime_from_matroska_epoch`、时长换算、元素 ID 常量。类比 `bmff.rs`。 | Create |
| `formats/mod.rs` | `pub mod ebml;` | Modify |
| `civil.rs` | 把 `civil_from_days`（现私有于 `bmff.rs`）提取为共享 `pub(crate)` 模块——BMFF（1904）与 EBML（2001）两纪元复用。定向 DRY。 | Create |
| `formats/bmff.rs` | 改调 `crate::civil::civil_from_days`，删除本地副本 | Modify |
| `model.rs` | `FileFormat` 增 `Mkv`、`Webm` | Modify |
| `probe.rs` | 抬高 `PROBE_MAX`（→ 64）；EBML 魔数 `1A45DFA3` + `DocType` 扫描 → Mkv/Webm；`parser_for` 两者 → `EbmlParser` | Modify |
| `lib.rs` | `mod civil;`（`FileFormat` 变体经既有 `pub use` 自动导出） | Modify |
| `tests/differential.rs` | WebM + MKV fixture 跑 `assert_all_equal`（含 >8192 B 的 Cluster 行使 seek/skip 路径、未知大小 Segment 变体） | Modify |
| `docs/ROADMAP.md` | 勾选里程碑 C；标注 `duration_ms` 升至 ≥2 来源；链接本 spec/plan | Modify |

**`normalize.rs` 零改动**——duration/created 经容器 `Field` 路径入 `Collector`，与 BMFF moov 完全一致。

**职责切分**：`ebml.rs`（结构层）只回答「字节怎么读」（vint 解码、子元素边界）；`formats/ebml.rs`
（语义层）只回答「哪个元素 ID 是什么含义、怎么换算时长/纪元」。两者均显式迭代、非递归。

---

## 3. EBML 基础原语（`containers/ebml.rs`）

EBML 用两种变长整数（vint），二者**长度均由首字节前导零数 +1 决定**，但取值规则不同：

- **元素 ID**——**保留**标记位（ID 即规范值，如 EBML 头 = `0x1A45DFA3`）。长度上限 4（`EBMLMaxIDLength`
  默认值）。`read_elem_id(&[u8]) -> Option<(u32 id, usize len)>`：首字节为 0（长度 >8）或长度 >4 → None。
- **元素 size**——**剥去**标记位取数值；**数据位全 1 → 未知大小**。长度 1–8。
  `read_elem_size(&[u8]) -> Option<(Option<u64> size, usize len)>`，`None` size = 未知大小。
  数据位数 = `7 × len`；判全 1：`len*7 >= 64` 时即 `u64::MAX`，否则 `(1<<(7*len)) - 1`。

辅助：
- `struct ElemHeader { id: u32, header_len: u64, size: Option<u64> }`，`read_element_header(&[u8]) -> Option<ElemHeader>`。
- `iter_child_elements(payload) -> ChildElements`：在**已缓冲定长**载荷上遍历连续子元素；未知大小子元素 /
  声明长度越界 → 停止（不产出残缺项）。每项产出 `(ElemHeader, &[u8] 子载荷)`。类比 `iter_child_boxes`。
- 大端读数（供语义层）：`read_uint(&[u8]) -> u64`（1–8 B）、`read_int(&[u8]) -> i64`（符号扩展）、
  `read_float(&[u8]) -> Option<f64>`（4 → f32、8 → f64、0 → 0.0、其它长度 → None）。
- `needed_header_bytes(&[u8]) -> usize`：在已见前两个引导字节后精确算 `idlen+szlen`（镜像 BMFF）。

全程 `checked_*`/`get`，越界返回 None、游标不前进、绝不 panic、不分配。

---

## 4. 状态机（`EbmlParser`，单阶段——比 BMFF 更简）

无 `SeekTo` 抽取阶段（所有目标元素都在窗口内解析）。状态：
`phase: TopLevel | InSegment`、`got_info: bool`、`got_tracks: bool`、`pos: u64`（仅用于警告偏移保真）。
`done: bool`。**每次 `pull` 处理恰好一个元素**：

- **空窗口**（驱动保证仅 EOF 出现）→ `Done`（与 BMFF 同款 EOF 约定）。
- **不足以读出头部** → `NeedBytes(needed)`（`needed` 由 `needed_header_bytes` 精确计算）。
- **TopLevel：**
  - `Segment`(0x18538067) → **下钻但不缓冲**：`consumed = header_len`，置 `InSegment`，返回 `NeedBytes(2)`
    （前进 + 索要首个子元素头）。*这是与 BMFF 的关键差异：Segment 跨整个文件，须「步入」而非整盒缓冲。*
    `NeedBytes(2)` 中 2 = 最小元素头（1 B id + 1 B size）；正常 Segment 必有子元素故 `avail ≥ 2`。
  - 其它（EBML 头、`Void` 等）定长 → `Skip(body)`，`consumed = header_len`；未知大小非 Segment → `Done`。
- **InSegment（一个子元素）：**
  - `Info`(0x1549A966)/`Tracks`(0x1654AE6B) 定长、**整元素已入窗** → 解析、发 `Field`、置标志、
    `consumed = total`；若 `got_info && got_tracks` → `Done`，否则 `NeedBytes(2)`（取下一子元素）。
  - `Info`/`Tracks` 定长、**未完整入窗** → `NeedBytes(total)`、`consumed = 0`（安全：phase 已是 `InSegment`，
    重试时同一子元素头被等价重读，无 state/consumed 冲突）。整元素缓冲受驱动 `max_retained_bytes` 封顶。
  - 不关心的定长元素（`SeekHead`/`Cues`/`Cluster`/`Void`/`Tags`…）→ `Skip(size)`，`consumed = header_len`。
  - **未知大小**元素（如直播 `Cluster`）：若 `got_info && got_tracks` → `Done`；否则记 `UnreachableSection`
    警告 + `Done`（用户决策——遇未知大小媒体即干净停止）。

复用既有驱动/适配器的 `Skip`/`NeedBytes` 路径（无 `SeekTo`），四适配器一致性天然成立——
`Skip(Cluster)` 行使的正是 BMFF 跳 `mdat` 同一条原生 seek 路径。

> **DoS 边界**：`Info`/`Tracks` 须整元素先缓冲后解析，受 `max_retained_bytes` 封顶；`TrackEntry`/`Video`
> 子元素迭代被已缓冲载荷字节数（游标边界）限死，无需额外 `max_tracks`（沿用 `parser_for` 不向格式
> 解析器传 `Limits` 的约定）。

---

## 5. 解析与正确性（`formats/ebml.rs`）

### 5.1 元素 ID 常量

```
EBML_HEADER   = 0x1A45DFA3   DOCTYPE       = 0x4282
SEGMENT       = 0x18538067   VOID          = 0xEC
INFO          = 0x1549A966   SEEKHEAD      = 0x114D9B74
  TIMESTAMP_SCALE = 0x2AD7B1   CUES        = 0x1C53BB6B
  DURATION        = 0x4489     CLUSTER     = 0x1F43B675
  DATE_UTC        = 0x4461     TAGS        = 0x1254C367
TRACKS        = 0x1654AE6B
  TRACK_ENTRY     = 0xAE
    VIDEO         = 0xE0
      PIXEL_WIDTH  = 0xB0
      PIXEL_HEIGHT = 0xBA
```

> `TimestampScale` 在旧文件中名为 `TimecodeScale`，**元素 ID 相同**（0x2AD7B1），仅命名变化，无需特判。

### 5.2 `parse_info`（已缓冲 `Info` 载荷）

`iter_child_elements` 遍历，收集 `timestamp_scale: Option<u64>`、`duration_raw: Option<f64>`、
`date_ns: Option<i64>`（顺序可任意，先收集后换算）：
- `TIMESTAMP_SCALE` → `read_uint`；`DURATION` → `read_float`；`DATE_UTC` → `read_int`。

换算（**隔离的 f64、守卫式转换**——float 只触达此一值，绝不进入 model 层）：
```
scale = timestamp_scale.unwrap_or(1_000_000)
若 scale == 0 → 无 duration，记 UnrecognizedValue
若 duration_raw 存在：
    若 !d.is_finite() 或 d < 0.0 → 无 duration，记 UnrecognizedValue
    ms_f = d × scale as f64 ÷ 1e6
    若 ms_f < 0.0 或 ms_f > u64::MAX as f64 → 无 duration，记 UnrecognizedValue
    否则 duration_ms = ms_f as u64
若 date_ns 存在 → created = datetime_from_matroska_epoch(date_ns)
```

### 5.3 `datetime_from_matroska_epoch(date_ns: i64) -> DateTimeParts`

```
secs = date_ns.div_euclid(1_000_000_000)
days = secs.div_euclid(86_400) + MATROSKA_EPOCH_DAYS_AFTER_UNIX   // 11_323
tod  = secs.rem_euclid(86_400)
(year, month, day) = civil::civil_from_days(days)
{ year, month, day, hour: tod/3600, minute: (tod%3600)/60, second: tod%60, tz_offset_min: Some(0) }
```
`div_euclid`/`rem_euclid` 正确处理 2001 年前（负 `date_ns`）。锚点：`date_ns = 0` → `2001-01-01T00:00:00Z`
（11_323 天 = unix 秒 978_307_200，已核验）。

### 5.4 `parse_tracks`（已缓冲 `Tracks` 载荷）

`iter_child_elements` 遍历 `TRACK_ENTRY`；对每个 entry，子遍历找 `VIDEO`，其内找 `PIXEL_WIDTH`/`PIXEL_HEIGHT`
（`read_uint`）。取**首个**两者均非零的 entry（音频轨无 `Video` 子元素，天然跳过）。

### 5.5 字段优先级与错误处理

- **优先级**：与 BMFF 完全一致——容器 `Field::Width/Height/Duration/Created` → `Collector` → `finalize`；
  容器值覆盖 EXIF 派生值；`normalize.rs` 零改动。MKV/WebM 无 EXIF/XMP，故 `raw` 两桶为空。
- **错误处理**（不臆造、不 panic、全程 `checked_*`）：
  - 截断的 `Info`/`Tracks`（声明 size > 实际）→ 解析器索要整元素，驱动到 EOF 记 `Truncated`。
  - `scale == 0` / `Duration` 非有限/为负 / 溢出 → 不产出 duration，记 `UnrecognizedValue`。
  - 畸形子元素（长度自洽性破坏）→ `iter_child_elements` 停止遍历，已抽取结果保留。
  - 未知大小媒体先于 Info+Tracks 出现 → 记 `UnreachableSection`、干净 `Done`。

---

## 6. `probe` 改动

抬高 `PROBE_MAX` → 64（编译期断言相应改为 `>= 64`）。魔数 `1A 45 DF A3` 在 `[0..4]` 时：
在已缓冲头部内定位 `DocType`(0x4282)，读其字符串 → `"webm"` → `Webm`，否则（含 `"matroska"`）→ `Mkv`。
- 魔数匹配但 `DocType` 尚不可见 **且** `buf.len() < PROBE_MAX` → 返回 `Unknown`（push 继续缓冲）。
- `buf.len() >= PROBE_MAX` 仍无 `DocType` → 默认 `Mkv`（给出确定答案）。

`probe` 保持「纯缓冲函数 + `>=PROBE_MAX` 内部截止」语义，故四适配器一致：slice/blocking/seek 首次即见
≥64 B（DocType 必现）；push 累积至同一截止点。`parser_for`：`Mkv | Webm => EbmlParser::new()`。

> 抬高 `PROBE_MAX` 对其它格式无害——JPEG/PNG/… 在前 12 B 即判定；`PROBE_MAX` 仅决定「判 `Unknown`
> 失败前等待多少字节」。极短（<64 B）且无 DocType 的畸形 EBML → 四适配器一致 `Err(UnrecognizedFormat)`。

---

## 7. 测试策略

全部**合成 fixture**（仓库不引入二进制样本），延续既有风格。

- **`containers/ebml.rs` `mod tests`**：
  - `read_elem_id`：1/2/4 字节 ID（含 `0xB0`→(0xB0,1)、`0x4282`→(0x4282,2)、`0x1A45DFA3`→(…,4)）；
    首字节 0 / 长度 >4 → None。
  - `read_elem_size`：定长（剥标记位取值）、未知大小（全 1 → None size）、截断 → None。
  - `read_uint`/`read_int`（符号扩展）/`read_float`（f32/f64/0/非法长度）。
  - `iter_child_elements`：正常多子元素；声明长度越界 → 停止、不产出残缺项。
- **`formats/ebml.rs` `mod tests`**：
  - `parse_info`：f32 与 f64 `Duration` × 默认/显式 `TimestampScale` → 正确 `duration_ms`；`DateUTC` → `created`；
    `scale=0`/`Duration` NaN/Inf/负/溢出 → 无 duration + `UnrecognizedValue`。
  - `datetime_from_matroska_epoch`：`0 → 2001-01-01T00:00:00 tz=Some(0)`；`+86_400e9 → 次日`；`+3_661e9 → 01:01:01`；
    负值（2001 前）正确。
  - `parse_tracks`：视频轨 `PixelWidth/Height`；音频轨（无 `Video`）跳过；取首个非零。
  - `pull_walk`：跳过 EBML 头 → 下钻 `Segment` → 跳过 `Cluster` → 解析 `Info`+`Tracks` → 集齐后 `Done`；
    未知大小 `Cluster` 先于 Info → `UnreachableSection` + `Done`；截断 `Info` → `Truncated`。
  - 端到端：WebM（`DocType "webm"`）与 MKV（`"matroska"`）各构造 `EBML头 + Segment{Info, Tracks, Cluster}`
    → `drive_slice`+`finalize` → `unified.{width,height,duration_ms,created}` 与 `format`。
- **`tests/differential.rs`**：WebM + MKV fixture 跑 `assert_all_equal`，验证 slice/blocking/seek/push 四路
  逐字段一致；含 **Cluster >8192 B**（行使 read_seek 原生 seek/skip 分支）与**未知大小 Segment** 变体。
- **收尾验证**：`cargo test` 全绿；`cargo build -p omni-meta-core --no-default-features`（no_std 不破）；
  `cargo clippy --all-targets -- -D warnings` 清零。

---

## 8. 决策记录（brainstorm 结论）

1. **MKV/WebM 区分** → 抬高 `PROBE_MAX` 读 `DocType`，产出两个 `FileFormat` 变体。镜像 BMFF 的
   ftyp-brand 先例；权威、保真。代价：触及四适配器共享的 probe 窗口契约（已论证安全）。
2. **未知大小处理范围** → 下钻 `Segment`、缓冲 Info/Tracks、跳过定长元素、**遇未知大小媒体（在集齐
   Info+Tracks 前）即干净停止**。镜像 BMFF「跳 mdat、缓冲 moov」。覆盖一切常规文件；不为罕见的
   未知大小 Cluster 前置场景构建元素层级下钻表。
3. **float `Duration`** → 隔离 f64、守卫式转换（NaN/Inf/负/`scale==0`/溢出 → 无 duration + 警告）；
   float 只触达单一值、绝不进入 model 层。忠实于 Matroska 规范（`Duration` 本是 IEEE float）。
4. **`Tags` / SeekHead** → **不做**。`Tags`/`Cues` 在尾部、需 SeekHead 随机访问或穿越全部 Cluster，
   与「前向线性走盒、遇未知大小媒体即停」的选择冲突；且 `Info > Duration` 已是不需 SeekHead 的权威
   整体时长。留作独立 SeekHead 里程碑（届时新增 `RawTags` 容器桶承载 `SimpleTag`，并同样惠及 BMFF
   `udta`/`ilst`）。
5. **fuzz 范围** → 合成畸形单测锁定不变量；`cargo-fuzz` 独立横切里程碑另立。

### 未来 Unified/raw 候选（本切片记录、不实现）

- **GPS**（推荐的下一里程碑）：两来源已在 `raw` 层就绪——EXIF GPS IFD（`IfdKind::Gps`，
  JPEG/PNG/WebP/HEIF）+ XMP `exif:GPSLatitude/Longitude`。缺的只是 `normalize.rs` 投影（GPS 有理数 +
  半球 ref → 十进制 `Gps { lat, lon, alt }`）。**纯 normalize 切片，零 EBML 依赖**，是干净的高价值续作。
- **`video_codec`/`audio_codec`**：EBML `CodecID`(0x86) 是一个来源；第二来源需 BMFF `stsd` sample-entry
  fourcc（尚未解析）。达 ≥2 来源后再纳入。
- **Matroska `Tags` → `RawTags` 容器桶**：随 SeekHead 里程碑落地（见决策 4）。

---

## 9. 完成定义（里程碑 C）

- MKV/WebM 文件能从 EBML 树抽出 `width`/`height`（Tracks/Video）、`duration_ms`（Info/Duration ×
  TimestampScale）、`created`（Info/DateUTC，2001 UTC）。
- `probe` 经 `DocType` 区分 `FileFormat::Mkv` / `FileFormat::Webm`，四适配器一致。
- `scale=0`/`Duration` 非有限/溢出/截断/未知大小媒体/越界均产出恰当警告或干净缺失、**绝不 panic**。
- 四适配器对同一 MKV/WebM 输入逐字段一致（含 Cluster >8192 B 的 seek 路径、未知大小 Segment）。
- `duration_ms` 升至 ≥2 格式来源（BMFF + EBML）；Unified **无新字段**。
- `civil_from_days` 提取为共享 `civil` 模块，BMFF 改调之、行为不变。
- no_std 构建与 clippy 均干净。
