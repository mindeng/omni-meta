# EventParser：push 式 raw 条目流 + 提前终止 设计

**状态**：设计已确认，待写 plan
**日期**：2026-06-23
**基准设计**：[`2026-06-14-omni-meta-design.md`](2026-06-14-omni-meta-design.md)（sans-io 四层架构）
**关联**：扩展现有适配器层（`read_slice` / `PushParser` / `read_blocking` / `read_seek`）

---

## 1. 动机与用例

应用层希望在**喂数据的同时**挨个拿到解析出来的 raw 元数据条目，并在**拿到所需条目后提前终止**解析，省掉文件尾部的喂入/解析。

典型场景：只想确认某图是否含 GPS、或只取 `created`，不必把整文件喂完、也不必等 `finish()` 投影出完整 `Metadata`。

现状缺口：现有四适配器全是「喂到底 → 返回完整 `Metadata`」。`MetaParser::pull()` 本就增量产出 `Event`，但被 `Collector` **内部吞掉**，应用层看不到中间事件、无法提前终止。本设计把这条已有的内部事件流「开口」给应用层。

---

## 2. 确定的决策（brainstorm 收敛结果）

| 维度 | 决策 | 理由 |
|---|---|---|
| 条目层级 | **raw 级**（pre-normalize） | 提前终止语义自洽：已产出条目都是确定值，不受后续来源覆盖；Unified 跨来源裁决无法在流中提前定论 |
| 控制模型 | **push**（应用喂字节、拿回条目） | 应用掌控字节源；与现有 `PushParser` 同源 |
| 终止机制 | **手动停**（本期）；兴趣集（仅备忘，见 §8） | 手动停零额外机制即满足核心用例 |
| async | **不做**（归 ROADMAP 程碑 D） | push 本质同步；async 只是薄包装，单独里程碑 |
| 交付形态 | **`drain` 迭代器**（形态 C） | 最贴合「迭代器」心智，feed/消费职责分离 |
| 存储/借用 | **借用、feed 边界**（方案 B） | 零拷贝、零依赖、最小侵入；跨 feed 留存某条→`.clone()` 那一条 |
| `finish()` | **保留**（严格超集） | `EventParser` 是 `PushParser` 超集：跑到底亦可拿完整 `Metadata` |

---

## 3. 公开 API

新增 `omni-meta-core/src/adapters/event.rs`，经 crate 根 `pub use` 暴露 `EventParser` 与 `MetaItem`。`no_std + alloc`，与 `PushParser` 同层。

### 3.1 `MetaItem<'a>`

```rust
/// 流式产出的 raw 元数据条目，按文档顺序交付。借用 EventParser 内部存储，
/// 有效期至下一次 `feed`/`drain`（见 §5 借用约束）。要跨 feed 留存请 `.clone()`。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaItem<'a> {
    Exif(&'a ExifTag),
    Xmp(&'a XmpProperty),
    Container(&'a ContainerTag),
    Text(&'a TextTag),
    // 结构标量用离散变体，避免把内部 `Field` enum 固化成公开面
    Width(u32),
    Height(u32),
    Duration(u64),
    Created(DateTimeParts),
    Gps(Gps),
}
```

- 引用变体（Exif/Xmp/Container/Text）借用 Collector 既有 typed vecs → **零拷贝**（尤其省掉 XMP/Container/Text 里的 `String`/`Value` 分配）。
- 标量变体（Width/…/Gps）为 `Copy`/小结构，内联在顺序日志槽位里。
- `#[non_exhaustive]`：后续新增条目种类（如 IPTC、ICC）不破坏调用方。
- **warnings 不进条目流**（避免噪声），仍由 `finish()` 或独立访问取（见 §3.2）。

### 3.2 `EventParser`

```rust
pub struct EventParser { /* 见 §4 */ }

impl EventParser {
    /// 与 `PushParser::new` 同签名。
    pub fn new(opts: Options) -> Self;

    /// 喂一块字节（可空，仅推进）。内部跑 codec、把本轮新条目按文档序记入顺序日志。
    /// 返回 `Outcome`（Need/SkipHint/Done），语义同 `PushParser::feed`。
    /// 格式不可识别 → `Err(UnrecognizedFormat)`。
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Outcome, Error>;

    /// 取走自上次 `drain` 以来新产出的条目，按文档顺序。`break` 即提前终止。
    /// 返回的迭代器借用 `&mut self`：必须 drop 后才能再 `feed`（编译期强制）。
    pub fn drain(&mut self) -> impl Iterator<Item = MetaItem<'_>> + '_;

    /// 调用者已自行向前 seek n 字节后推进逻辑位置（同 `PushParser::skip`）。
    pub fn skip(&mut self, n: u64);

    /// 收尾，返回完整 normalize 后的 `Metadata`（同 `PushParser::finish`）。
    pub fn finish(self) -> Result<Metadata, Error>;
}
```

### 3.3 使用示例

```rust
let mut p = EventParser::new(Options::default());
let mut found = None;
'outer: loop {
    let outcome = p.feed(chunk)?;     // 推进 + 跑 codec
    for item in p.drain() {           // 文档顺序挨个取
        if let MetaItem::Gps(g) = item {
            found = Some(*g);          // 标量直接 Copy；引用变体则 .clone()
            break 'outer;             // 提前终止：drop p
        }
    }
    if outcome == Outcome::Done { break; }
}
// 若没提前停，可拿完整 Metadata：
// let meta = p.finish()?;
```

---

## 4. 实现要点（最小侵入）

核心原则：**Collector 现有 typed vecs 仍是唯一拥有方**，`finalize`/`normalize`/`PushParser` **一行不改**；只新增「可选的顺序日志」记录文档序并供 `drain` 解析借用。

### 4.1 顺序日志（order log）

```rust
// 内部：轻量槽位。引用类记「在对应 vec 的下标」；标量类内联值。
enum Slot {
    Exif(u32), Xmp(u32), Container(u32), Text(u32),
    Width(u32), Height(u32), Duration(u64), Created(DateTimeParts), Gps(Gps),
}
```

- `Collector` 增 `order: Option<Vec<Slot>>` 字段。
  - `PushParser` / `drive_slice` 路径建 `None` → **零行为、零开销变化**。
  - `EventParser` 路径建 `Some(Vec::new())`。
- `Collector::handle` 在 `order.is_some()` 时，**额外**把当前事件记入顺序日志：
  - `Event::Payload{Exif}`：codec 解码后向 `self.exif` 追加了 N 条 → 记录 `Slot::Exif(idx)` × N（idx 为追加后的各下标）。
  - `Event::Payload{Xmp}`：同理记 `Slot::Xmp(idx)`。
  - `Event::ContainerTag` / `Event::Text`：push 到对应 vec 后记 `Slot::Container/Text(idx)`。
  - `Event::Field(Width/Height/Duration/Created/Gps)`：**仅在被接受时**记录（与现有「首个非 None 胜出」一致），标量内联进 `Slot`。
  - `Event::Warning`：不入顺序日志。
- 文档顺序天然成立：单次 `pull` 的事件 Vec 即文档序，跨 `pull` 随文件前进；`order` 按 `handle` 调用序追加。

### 4.2 `StreamDriver` 透传

- `EventParser` 持有 `StreamDriver`（同 `PushParser`），其 `Collector` 以 `order = Some(..)` 构造。
  - 需要 `StreamDriver::new_streaming(parser, limits)` 或在 `new` 上加 `streaming: bool` 参数；建议加私有构造函数避免污染现有 `new`。
- `EventParser` 维护 `emitted: usize` 游标。
- `drain` 实现（借用安全的关键：先用 `&mut` 推进游标，再交出 `&self` 借用，单次 drain 内 vecs 不增长）：

```rust
pub fn drain(&mut self) -> impl Iterator<Item = MetaItem<'_>> + '_ {
    let col = self.driver.collector_ref();   // &Collector
    let order = col.order.as_deref().unwrap_or(&[]);
    let start = self.emitted;
    self.emitted = order.len();              // 先推进游标（&mut self 字段写）
    // 再返回借 &col 的迭代器，range 固定为 start..len
    order[start..].iter().map(move |slot| resolve(col, slot))
}
```
  > 借用细节：`self.emitted = ...` 的 `&mut` 写在返回迭代器前完成；返回的迭代器只借 `&self`（reborrow）。`StreamDriver` 需暴露 `collector_ref(&self) -> &Collector`（pub(crate)）。

- `resolve(col, slot) -> MetaItem`：按 Slot 种类借用对应 vec 元素或返回内联标量。

### 4.3 `finish`

`EventParser::finish` 直接复用 `StreamDriver::finish() -> Collector` + 现有 `finalize(col, format)`。顺序日志在 finish 时随 Collector 丢弃，不参与 normalize。`finish()` 结果须与同输入的 `PushParser::finish()` **byte-equal**。

---

## 5. 借用约束（务必文档化）

数据由 Collector **全程持有**，但 `feed` 会向 typed vecs `push` → `Vec` 可能 realloc → 旧 `&ExifTag` 悬垂。故借用检查器强制：**持有任何 `MetaItem<'a>` 时不可调用 `feed`**。

- 对「扫描 → 够了就停」用例无影响：循环体内看完即停或继续（自然先 drop drain 迭代器再 feed）。
- 要跨 feed 留存某条：`.clone()` 那一条（标量变体本就 `Copy`）。
- 这是借用规则防 realloc，**不是数据被 drop**。若未来出现「跨 feed 批量留存引用」的真实需求，可升级到稳定地址 + 内部可变存储（`elsa::FrozenVec` 或自建 chunked arena，`feed(&self)`），但需引入依赖/改造 Collector，当前 ROI 不足，不做。

---

## 6. 不变量遵守（基准设计 §不变量）

- `#![forbid(unsafe_code)]`：`drain` 借用纯安全 Rust，无 unsafe。
- best-effort：`feed`/`finish` 仅在「格式识别不了」`Err`；解析错落 `warnings`、不 panic。
- 缺失即 `None`，绝不臆造：流式只产出真实解析到的条目。
- DoS 上界：`order` 日志条目数 ≤ typed vecs 总条目数，本就受 `Limits`（`max_tags` / `max_ifds` 等）封顶；无新增无界增长面。
- 顺序日志 `Slot` 用 `u32` 下标：typed vecs 受 `Limits` 封顶远小于 `u32::MAX`，安全。

---

## 7. 测试（扩展四适配器差分）

1. **差分一致性**：`EventParser` 跑到底、drain 出的全部条目重组为 `RawTags`+结构标量，须等于 `read_slice` 的对应产物（raw 层 + 结构字段相等）。
2. **finish 等价**：`EventParser::finish()` 对同输入须与 `PushParser::finish()` byte-equal（覆盖各格式 fixture）。
3. **文档顺序**：构造含多类条目（EXIF + XMP + Container + 结构）的 fixture，断言 drain 序列与文档序一致。
4. **提前终止**：首个 GPS 条目后停，断言不 panic、未喂完文件、已得该条目。
5. **chunk 不变性**：不同 chunk 大小（1/3/7/全量）drain 出的条目序列一致（沿用现有 push 测法）。
6. **借用边界**：编译期负向测试（trybuild 或文档注释示例）确认「持有 item 时 feed」不通过编译——可选，至少在 doc 注释里说明。
7. **no_std 构建**：`event.rs` 在裸机 target 编译通过（纳入现有 no_std CI 门禁）。
8. **fuzz**（可选增量）：差分 fuzz target 增加 EventParser 路径，与 read_slice 的 raw 层比对。

---

## 8. 兴趣集（仅备忘，不在本期）

后续若要「省解析」，可加声明式兴趣集：

- `Options` 增可选 `Interest`（如「只要 `Gps + Created`」）。
- **便宜档**：driver 在每条目后检查兴趣是否集齐，集齐即自动发 `Outcome::Done`，省掉文件尾部喂入/解析。能少读字节但不跳文件内部无关段。
- **深档（待评估）**：把兴趣下沉到各格式 parser，令其对无关 box/段发 `Demand::Skip`。格式相关、改动深，单独评估。
- 兴趣表达在语义层（Gps/Created），需映射到 raw 条目种类 + 结构字段；映射关系格式相关，是深档主要复杂点。

此节仅备忘，本期 plan 不含。

---

## 9. 交付范围（本期）

- 新增 `omni-meta-core/src/adapters/event.rs`：`EventParser` + `MetaItem`。
- `Collector` 增 `order: Option<Vec<Slot>>` + `handle` 顺序日志记录（`None` 路径零变化）。
- `StreamDriver` 增 `collector_ref` + streaming 构造。
- crate 根 `pub use` 暴露 `EventParser` / `MetaItem`。
- 测试 §7（1–5、7 必做；6、8 可选）。
- ROADMAP 勾选/登记。

**不在本期**：async（程碑 D）、兴趣集（§8）、稳定地址存储（方案 C）。
