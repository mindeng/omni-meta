# A2：HEIF/AVIF `meta` box 元数据抽取（设计）

**日期** 2026-06-15 · **里程碑** A 的 A2 切片 · 上游基座见 A1
计划：[`docs/superpowers/plans/2026-06-15-omni-meta-bmff-foundation.md`](../plans/2026-06-15-omni-meta-bmff-foundation.md)
基准设计：[`docs/superpowers/specs/2026-06-14-omni-meta-design.md`](2026-06-14-omni-meta-design.md)

---

## 1. 目标与范围

让 `BmffParser` 从 HEIF/AVIF 的 `meta` box 抽取元数据，复用既有 EXIF/XMP codec，并接入图像维度。

**纳入：**
- **仅 `FileFormat::Heif` / `FileFormat::Avif`**。`Mp4` / `Mov` 保持 A1 行为（校验 ftyp → `Done`），`moov` 下钻留 A3。
- **EXIF 抽取**：`iinf` 中 `item_type == "Exif"` 的 item → 经 `iloc` 定位 → 剥 `tiff_header_offset` 前缀 → 发 `PayloadKind::Exif`，交现有 `codecs::exif`。
- **XMP 抽取**：`iinf` 中 `item_type == "mime"` 且 content_type 为 `application/rdf+xml` 的 item → `iloc` 定位 → 原样发 `PayloadKind::Xmp`，交现有 `codecs::xmp`。
- **维度**：`iprp/ipco/ispe` + 主 item（`pitm`/`ipma`）→ `Field::Width` / `Field::Height`。
- **`iloc` construction_method**：`0`（绝对文件偏移，指向 `mdat`）+ `1`（`idat` 内联）。`2`（item 间接引用）与未知方法 → `UnreachableSection` 警告并跳过该 item。

**不纳入（→ A3）：**
- MP4/MOV `moov` / `mvhd` / `tkhd` → 维度 / `duration` / `created`。
- 新 Unified 字段 `duration`、`created`。理由：`created` 需「≥2 格式来源」，A2 的 `meta` box 不含创建时间，仅能从 EXIF 单一来源拿到 `DateTimeOriginal`/`DateTime`（自 JPEG 即有），不构成新增容器原生来源；BMFF 原生 creation_time 在 `moov`（A3）。A2 把 EXIF 抽进 `raw.exif` 即为 A3 铺路。
- `grid` / 派生 item 的拼接维度。

**Unified 改动面**：仅 `width` / `height`（经 ispe，给二者增加第 5 个格式来源）。其余一律停在 raw 层。

---

## 2. 模块边界

| 文件 | 职责 | 动作 |
|---|---|---|
| `containers/isobmff.rs` | 通用结构层（A3 `moov` 共用）：现有 `read_box_header` + 新增 `full_box_vf`（读 FullBox 的 version/flags）+ `iter_child_boxes`（安全遍历兄弟 box，越界即停的迭代器） | Modify |
| `formats/bmff.rs` | HEIF 语义层：item 模型、解析 `meta` 子盒、构建抽取目标、`SeekTo` 抽取状态机 | Modify（A1 骨架扩写） |
| `formats/bmff.rs` `mod tests` | 合成 HEIC fixture 单测（meta+mdat / idat / 截断 / 缺 meta / method2） | Modify |
| `tests/differential.rs` | 完整 HEIC fixture 跑四适配器一致性 | Modify |

**职责切分原则**：`isobmff.rs` 只回答「BMFF 怎么排布」（box 头、FullBox 头、子盒遍历——格式无关、A3 复用）；`bmff.rs` 回答「HEIF 元数据什么含义」（哪个 item 是 EXIF/XMP、tiff 前缀、主 item 维度选择）。若 `bmff.rs` 增长过大，再拆 `formats/bmff/{mod,meta}.rs`。

---

## 3. 状态机（核心，sans-io）

`BmffParser` 持两阶段状态。沿用既有 `MetaParser::pull` 契约：解析器从 `iloc` 算出**绝对文件偏移**，用 `Demand::SeekTo(绝对偏移)` 让 driver 定位；下一次 `pull` 的窗口起点即该偏移。

### Phase `Walk`（顶层 box 扫描）
从偏移 0 逐个顶层 box 推进：
- 读 8/16 字节头（`read_box_header`）。
- 非 `meta` box：`consumed = header_len`，`Demand::Skip(payload_len)` 跳过盒体——**绝不缓冲 `mdat`**。
- `size0`（延伸至 EOF）或走到 EOF 仍非 `meta`：`Done`（无事件，与 A1 一致）。
- 命中 `meta`：`Demand::NeedBytes(meta.total_size)`，要求整个 meta box 入窗（meta 通常 < 64 KB）。
  - 若 `meta.total_size` 超 `max_retained_bytes`：driver 现有「保留缓冲超限」守卫报 `UnreachableSection` 并 `Done`，无需 A2 额外处理。

### 解析 `meta`（窗口含完整 meta box 时一次性执行）
在 `isobmff::iter_child_boxes` 上遍历 meta 的子盒（meta 自身是 FullBox，先 `full_box_vf` 跳 4 字节 version/flags）：
- **`iinf`**：FullBox。读 entry_count（仅作循环上界，迭代被载荷字节数限死），逐 `infe`（version 2/3）取 `item_ID` + `item_type`(4cc) + （`mime` 时）content_type 字符串 → 建 `item_ID → (item_type, content_type?)`。
- **`iloc`**：FullBox。读 offset/length/base_offset/index 字段位宽（version 0/1/2 语义），逐 item 取 `item_ID` + `construction_method` + extents `[(data_reference_index, extent_offset, extent_length)]`，叠加 `base_offset` → 绝对偏移。
- **`iprp` → `ipco`/`ipma`** + **`pitm`**：取主 item 维度。`ispe`（FullBox）载荷 = version/flags(4) + image_width(u32 BE) + image_height(u32 BE)。
  - 选择：`pitm` → 主 item_ID；`ipma` → 该 item 关联的属性序号（1-based，索引进 `ipco` 子盒有序列表）；取其中的 `ispe`。
  - 兜底：`pitm`/`ipma` 缺失或未关联到 ispe，但 `ipco` 恰好只有一个 `ispe` → 直接用（覆盖单图 HEIC/AVIF 常态）。
- **`idat`**：记录其在窗口内的数据切片（供 construction_method 1 的 item 取数据）。

产出：
1. **维度 `Field` 事件**（立即发，`Width`/`Height`）。
2. **method-1（idat 内联）item 的 `Payload`**：数据就在 `idat` 切片内（当前窗口的子切片），立即发。
3. **method-0 目标列表** `Target { offset: u64, length: u64, kind: PayloadKind, strip_exif_prefix: bool }`，**按 `offset` 升序排序**。

### Phase `Extract`（走 method-0 目标）
- 完成 meta 解析的那次 `pull`：`consumed = meta_box_len`，发上述 Field/idat 事件，`demand = SeekTo(targets[0].offset)`（无目标则 `Done`）。
- 后续每次 `pull`（窗口起点 = `targets[i].offset`）：
  - 窗口 < `length` → `NeedBytes(length)`，`consumed = 0`。
  - 够了 → 发 `Payload`：
    - **EXIF**：读首 4 字节 BE `tiff_header_offset = N`，数据自 `4 + N` 起；容错：若随后以 `Exif\0\0` 开头再剥 6 字节；剩余即裸 TIFF（`codecs::exif` 期望 `II`/`MM` 开头）。
    - **XMP**：`item[..length]` 原样。
  - `consumed = length`，`i += 1`；`i < len` → `SeekTo(targets[i].offset)`，否则 `Done`。
- 升序排序保证多数 `SeekTo` 为前向 → slice/blocking/seek/push 四路皆可。偶发后向（offset 重叠）在 push 上由 driver 报 `UnreachableSection`（罕见、可接受的降级）。

---

## 4. 数据流与正确性

- **维度优先级**：ispe 走容器 `Field` 路径，`finalize` 中容器维度覆盖 EXIF/XMP 维度 → ispe 胜出，与 PNG/WebP 行为一致，`normalize` 零改动。
- **EXIF/XMP**：经现有 codec → `raw.exif`/`raw.xmp`，`normalize` 投影 `camera_make`/`camera_model`/`orientation` 等，全部复用。
- **错误处理**（不臆造、不 panic、全程 `checked_*`）：
  - 截断的 meta / extent → driver `Truncated`。
  - 越界 extent、construction_method 2/未知 → `UnreachableSection` 警告，跳过该 item，其余照常抽取。
  - 畸形 box 头 / 子盒长度自洽性破坏 → `iter_child_boxes` 停止遍历，已抽取结果保留。
- **DoS 边界**：整个 `meta` box 必须先缓冲才解析，受 `max_retained_bytes` 封顶（超限 → driver `UnreachableSection`）；`iinf`/`iloc` 的 entry_count 仅作循环上界，实际迭代被 box 载荷字节数（游标边界）限死——声明巨大 count 也只会在载荷耗尽时停止，分配不会超过盒体大小。故无需额外的 `max_items`（`parser_for` 不向格式解析器传 `Limits`，沿用此约定）。

---

## 5. 测试策略

全部用**合成 fixture**（仓库不引入二进制样本），延续既有 codec/格式单测风格。

- **bmff `mod tests`**：
  - 合成 HEIC：`ftyp` + `meta{ hdlr, pitm, iinf(Exif + mime), iloc(method0 → mdat), iprp/ipco/ispe }` + `mdat{ TIFF, XMP }` → 断言 `raw.exif` 标签、`raw.xmp` 属性、`unified.width/height`。
  - idat 变体：`iloc(method1)` 指向 meta 内 `idat` → 同样抽到 EXIF/XMP。
  - 缺 `meta`（只有 ftyp + mdat）→ 干净 `Done`、无事件、无警告。
  - 截断 meta（声明 size 大于实际）→ `Truncated`。
  - construction_method 2 → `UnreachableSection` 警告且不 panic。
  - 单图兜底：无 `ipma` 关联但 `ipco` 仅一个 `ispe` → 维度仍取到。
- **`tests/differential.rs`**：完整 HEIC fixture（含 meta + mdat）跑 `assert_all_equal`，验证 slice/blocking/seek/push 四路逐字段一致。
- **收尾验证**：`cargo test` 全绿；`cargo build -p omni-meta-core --no-default-features`（no_std 不破）；`cargo clippy --all-targets -- -D warnings` 清零。

---

## 6. 完成定义（A2）

- HEIF/AVIF 文件能从 `meta` 抽出 EXIF（→ camera_make 等）与 XMP（→ raw），并经 ispe 拿到 `width`/`height`。
- construction_method 0/1 均可定位；method 2/越界/截断均产出恰当警告、绝不 panic。
- 四适配器对同一 HEIC 输入逐字段一致。
- no_std 构建与 clippy 均干净。
- Unified 仅 `width`/`height` 变动；`created`/`duration` 未引入。
