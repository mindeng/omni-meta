# XMP sidecar 合并：解析后注入 `.xmp` 旁挂文件 设计

**状态**：设计已确认，待写 plan
**日期**：2026-06-28
**基准设计**：[`2026-06-14-omni-meta-design.md`](2026-06-14-omni-meta-design.md)（sans-io 四层架构）
**关联**：扩展 `Metadata`（后处理 method）+ `RawTags`（新增 xmp_sidecar 列）+ `normalize`（新增 sidecar 优先级档）

---

## 1. 动机与用例

Apple Photos 导出时，用户加的 title/caption/creator/copyright/keywords 不写进图像字节，而是落在**旁挂 `.xmp` sidecar 文件**（`Export IPTC as XMP` 选项）。sidecar 本质是一段裸 XMP 包（RDF/XML，可能带 `<?xpacket?>` 包裹），属**现代 IPTC（IPTC Core，XMP 形态）**，不是传统 IPTC-IIM。

> 与 ROADMAP §2 的衔接：现代 IPTC = XMP，本库 XMP codec 已能解析；缺的只是「sidecar 作为独立文件，如何并入主图结果」这一步。传统 IPTC-IIM（里程碑 B）与本设计无关、不被本设计触碰。

现状缺口：四适配器都只吃**单个**字节缓冲，没有「把另一段 XMP 折进已解析结果」的入口。

### 关键约束（定死，不可议）

- **库从不碰文件系统**（no_std + sans-io）。`foo.jpg → foo.xmp` 的**发现/路径推导不归库管**，调用方负责读好 sidecar 字节再传入。API 只接受 `&[u8]`。

---

## 2. 确定的决策（brainstorm 收敛结果）

| 维度 | 决策 | 理由 |
|---|---|---|
| 入口形状 | **解析后 method**：`Metadata::with_xmp_sidecar(self, &[u8], Limits) -> Self` | 否决「`Options` 注入」：注入会造成**时间耦合**（sidecar 必须在解析图像那刻就位）+ 概念混淆 + 流式路径别扭。后处理 method 解耦时间、借用仅限单次调用、对 slice/blocking/push 三入口一视同仁 |
| sidecar 字节生命周期 | **仅借用一次调用** | method 返回 owned `Metadata`，无任何长留借用 |
| raw 落点 | **`RawTags` 单设 `xmp_sidecar: Vec<XmpProperty>`** 列（不与内嵌 `xmp` 混） | 留出处（provenance）；让 normalize 能给 sidecar 定**独立优先级档**。否决「同一 vec append」（丢出处、优先级不可控） |
| 重规整能力 | **`Metadata` 内留 `pub(crate) structural: StructuralFields` 快照** | merge 后需重跑 `normalize` 才能让新 XMP 影响 Unified；normalize 需 `StructuralFields`，`finalize` 时已折进 Unified。留快照使 merge 结果与「一次到位」**字节级一致**（呼应 strip slice↔blocking 一致的库规约）。`pub(crate)` 保持 `StructuralFields` 为内部类型，公开面不增 |
| 优先级（技术字段） | 内嵌 > sidecar | make/model/orientation/dims/created/gps：相机直出更权威，sidecar 仅作 XMP 档兜底 |
| 优先级（描述字段） | **sidecar > 内嵌** | title/description/creator/copyright：sidecar 是用户主动写的意图，应压过内嵌 |
| keywords | **暂停在 raw 层**（不进 Unified） | Unified 无 keywords 字段，且受「≥2 格式来源」铁律约束（当前仅 XMP `dc:subject` 一源）。调用方自 `raw.xmp_sidecar` 取 |
| 幂等/可重入 | merge 可叠加（多次调用累积进 xmp_sidecar 后整体重规整） | 语义清晰；同名属性按 vec 顺序首胜 |

---

## 3. 公开 API

```rust
impl Metadata {
    /// 把一段 `.xmp` sidecar 字节折进已解析结果，返回更新后的 Metadata。
    ///
    /// sidecar 经 XMP codec 解析后落 `raw.xmp_sidecar`，随后基于
    /// 保留的 `structural` 快照重跑 normalize；新增告警追加进 `warnings`。
    /// `packet` 仅在本次调用内借用。空/无效 UTF-8 → 一条 Truncated 告警，
    /// Unified 不变（与 XMP codec 既有失败语义一致）。
    pub fn with_xmp_sidecar(self, packet: &[u8], limits: Limits) -> Self;
}
```

经 crate 根已暴露的 `Metadata` 自然可见；无新增 `pub use`。三入口（`read_slice` / `read_blocking` / `PushParser::finish`）皆产出 `Metadata`，统一适用。

调用方视角：
```rust
let mut meta = read_slice(image, opts)?;
if let Some(xmp) = load_sidecar_if_any() {       // 晚到也行，不必重解图像
    meta = meta.with_xmp_sidecar(&xmp, opts.limits);
}
```

---

## 4. 数据模型变更

### 4.1 `RawTags` 新增一列

```rust
pub struct RawTags {
    pub exif: Vec<ExifTag>,
    pub xmp: Vec<XmpProperty>,
    pub xmp_sidecar: Vec<XmpProperty>,  // 新增：旁挂 .xmp 来源，与内嵌 xmp 分列
    pub container: Vec<ContainerTag>,
    pub text: Vec<TextTag>,
}
```
`Default` 派生自动补空 vec，既有构造点（`finalize`）无需改值，仅结构补字段。

### 4.2 `Metadata` 保留 structural 快照

```rust
pub struct Metadata {
    pub unified: Unified,
    pub raw: RawTags,
    pub warnings: Vec<Warning>,
    pub format: FileFormat,
    pub(crate) structural: StructuralFields,  // 新增：重规整所需的结构来源快照（内部辅助）
}
```
- `structural` 取 **`pub(crate)`**：仅 crate 内 `with_xmp_sidecar` 读取，外部无需访问（其内容已全在 `Unified` 暴露）。因此 `StructuralFields` **保持 `pub(crate)`**，无需提为 pub、无需 crate 根 `pub use`——公开类型面不增。其字段已 `pub`，派生 `Clone, Default`。
- `finalize(col, format)` 改为在投影后把 `structural` 一并存入返回的 `Metadata`（当前它构造完 `structural` 即丢弃，改为 move 进结果）。

> **破坏性**：`Metadata` 新增 `pub(crate)` 字段使**外部 crate 无法再用 struct-literal 构造** `Metadata`（任一私有字段即禁用字面量语法）。工作区内 `omni-meta-fixtures` 的测试辅助 `meta_with_warnings` 正是字面量构造——需改造。对策：给 `Metadata` 派生 `Default`（连带 `FileFormat` 派生 `Default`，默认 `Unknown`），外部构造改走 `Metadata::default()` + 字段赋值（`structural` 留默认）。此法保留 `pub(crate)` 的小公开面，`Default` 为标准 trait、无 bespoke API。库 pre-1.0，可接受。

---

## 5. normalize 优先级变更

新增私有 helper（对称于既有 `xmp_text`）：
```rust
fn xmp_sidecar_text(raw: &RawTags, prefix: &str, name: &str) -> Option<String>; // 扫 raw.xmp_sidecar
```

在 `normalize()` 各字段的 `.or_else()` 链中按下表插档（首胜语义 → 插入位置即优先级）：

| 字段类别 | 字段 | sidecar 插入位置 |
|---|---|---|
| 描述（sidecar 胜） | `title` | **链首**（`xmp_sidecar_text(dc,title)` → 既有 xmp dc:title → png Title） |
| | `description` | **链首**（sidecar dc:description → EXIF 0x010E → xmp → png） |
| | `creator` | **链首**（sidecar dc:creator → 容器 → EXIF Artist → xmp → png） |
| | `copyright` | **链首**（sidecar dc:rights → EXIF 0x8298 → xmp → png） |
| 技术（内嵌胜） | `camera_make`/`model` | 末位 XMP 档之后（sidecar tiff:Make/Model 仅当内嵌全缺时兜底） |
| | `orientation`/`width`/`height` | 同上，作内嵌 XMP 之后的兜底（扫 `raw.xmp_sidecar` 的 tiff:*） |
| | `gps` | 仅当 EXIF GPS IFD + 内嵌 XMP `exif:GPS*` 全缺时，尝试 sidecar `exif:GPS*` 回退（复用 `gps_from_xmp`，泛化扫 sidecar 属性切片） |
| | `created` | **本期不做**：sidecar `exif:DateTimeOriginal` 是 ISO-8601（`YYYY-MM-DDT…`），与 EXIF `parse_exif_datetime` 的 `YYYY:MM:DD…` 不同，需独立 ISO 解析器，单列后续增量 |

> 注：描述字段「sidecar 压过 EXIF 内嵌」是有意为之——用户在 Photos 里写的 caption 应胜过相机刻进 EXIF 的 ImageDescription。

`keywords`：**不新增 Unified 字段**，仅保证 `raw.xmp_sidecar` 内 `dc:subject` 条目被 XMP codec 原样收录，调用方自取。

---

## 6. 范围边界

**做**：sidecar 字节 → `raw.xmp_sidecar` → 重规整描述/技术字段；`Metadata::with_xmp_sidecar` method；structural 快照保留。

**不做**（明确划出）：
- ❌ 文件系统访问 / `.xmp` 路径发现（调用方职责）
- ❌ 传统 IPTC-IIM（里程碑 B，与本设计正交）
- ❌ Unified 新增 `keywords` 字段（受 ≥2 来源铁律阻塞，留 raw）
- ❌ 独立 `FileFormat::Xmp` / 单独解析游离 .xmp 成 `Metadata`（如需，后续薄入口另议）
- ❌ `created` 的 sidecar 回退（需 XMP ISO-8601 datetime 解析器，单列后续增量）
- ❌ 写回 / sidecar 生成（本库只读）

---

## 7. 测试策略

- **一致性（核心）**：构造「图像 + sidecar」，对比 `read_slice(img).with_xmp_sidecar(side)` 与「把 sidecar 的 XMP 预置进 raw 后直接 normalize」两条路径 → Unified **字节级一致**。
- **描述字段 sidecar 胜**：sidecar dc:description 压过 EXIF ImageDescription / 内嵌 xmp。
- **技术字段内嵌胜**：sidecar tiff:Make 不覆盖 EXIF 0x010F；EXIF 缺时 sidecar 兜底生效。
- **provenance**：sidecar 属性落 `raw.xmp_sidecar`、不污染 `raw.xmp`。
- **keywords**：sidecar `dc:subject` 在 raw 可取、Unified 无对应字段。
- **退化**：空 packet / 无效 UTF-8 / 超 `max_payload_bytes` → 一条 Truncated 告警，Unified 不变。
- **幂等叠加**：连续两次 `with_xmp_sidecar` 累积且重规整稳定。
- **no_std 构建**通过。

---

## 8. ROADMAP 登记

新增里程碑（raw-only 小增量，不阻塞他者）：

> **里程碑 H — XMP sidecar 合并**：`Metadata::with_xmp_sidecar` 解析后注入；`RawTags.xmp_sidecar` 列 + `Metadata.structural` 快照；normalize 描述字段 sidecar>内嵌、技术字段内嵌>sidecar；keywords 留 raw。服务 Apple Photos `Export IPTC as XMP` 场景。
