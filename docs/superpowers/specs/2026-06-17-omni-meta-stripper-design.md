# Stripper（元数据剥离）— 设计

**日期** 2026-06-17 · **里程碑** F（Stripper，唯一「写」路径）
**方案** A（自包含 strip walker + 格式无关指令 trait，面向容器复用设计接缝）
**范围** v1：JPEG / PNG / WebP；sans-io 核心 + `strip_slice`（no_std）+ `strip_blocking`（std）
**关联** ROADMAP §3 里程碑 F、§5 不变量、§2 IPTC（APP13 一并剥离）

---

## 1. 目标与非目标

### 目标
- 把 EXIF / XMP / IPTC-IIM（JPEG APP13/8BIM）/ ICC 等可识别元数据从图片中**删除**，产出干净文件。
- 服务隐私剥离 / 通用图片工具场景（ROADMAP §2 默认推荐主场景之一）。
- 验证架构的「写」维度：这是全库第一条写路径；sans-io 重写状态机复用容器读框架的边界安全经验。
- **默认剥离隐私元数据（EXIF/XMP/IPTC），保留渲染必需数据（ICC/orientation）**；可经选项改为「全删」。

### 默认策略：隐私 vs 渲染必需

剥离对象分两类，**默认行为不同**：

| 类别 | 内容 | 隐私? | 默认 |
|---|---|---|---|
| EXIF / XMP / IPTC | GPS、机型、时间、作者、序列号… | 是 | **剥离** |
| ICC | 色彩配置 | 否 | **保留** |
| orientation | 显示旋转 | 否 | **保留** |

理由：ICC / orientation 是**渲染必需、非个人信息**数据。隐私目标靠删 EXIF/XMP/IPTC 即完全达成；默认删掉它们只会让广色域图偏色、旋转图翻车，无任何隐私收益（最小惊讶原则）。本库不重编码像素，故保留 orientation 元数据是唯一保真手段。隐私极端派可用 `StripOptions::aggressive()` 连 ICC/orientation 一起删。

```rust
pub struct StripOptions {
    pub limits: Limits,
    pub keep_icc: bool,          // 默认 true：保留 ICC，避免偏色
    pub keep_orientation: bool,  // 默认 true：保留方向，避免显示翻车
}
impl Default for StripOptions {
    fn default() -> Self { Self { limits: Limits::default(), keep_icc: true, keep_orientation: true } }
}
impl StripOptions {
    /// 隐私极端模式：连 ICC/orientation 一并删除（可能偏色/翻车）。
    pub fn aggressive() -> Self { Self { keep_icc: false, keep_orientation: false, ..Self::default() } }
}
```

> **副作用（务必文档化）**：`keep_orientation` 默认保留 ⇒ 即使源 EXIF 只含 orientation + 隐私项，输出仍残留一个**单 tag 的最小 EXIF**（即「剥离后并非零 EXIF」）。`StripReport.removed` 仍标记 `Exif` 被删（原段确被删、重写为最小段）。`aggressive()` 下输出零 EXIF。

### 非目标（v1）
- BMFF（HEIF/AVIF/MP4/MOV）、EBML（MKV/WebM）、GIF 的剥离——盒树重写 / `iloc` 偏移表重建复杂度高一个数量级，留待后续里程碑。
- 选择性按字段剥离（仅删 GPS 等）——v1 是「按类剥离/保留」，不做字段级。
- push / seek / async 适配器——v1 只 slice + blocking。
- 写回比源更丰富的元数据——除 `keep_orientation` 需合成最小 EXIF 外，不新增任何元数据。

---

## 2. 核心架构

方案 A：strip 逻辑自包含，**不改动**已被 fuzz 首轮硬化的读路径（`formats/`、`containers/`）。strip 走盒器与读解析器并列。

### 为何不复用读解析器（不选 B/C）
- 读 `MetaParser` 的 `Skip(body)` 把「段头+段体」合并消费，恰好丢掉 strip 需要的精确删除边界（**段头也要删**）；读用 `NeedBytes` 窗口逻辑是为读调的。
- 读在 SOS/EOI 处即停（元数据到此为止）；strip 必须**继续走过**压缩图像数据 + EOI，把它们拷到输出。strip 走得比读远。
- 让一个迭代器同时服务读与写，就要回去改硬化的读解析器使其暴露更多信息 → 引入回归风险。三处重叠仅 APPn / chunk 段头解析一小段（约百行），不值得为此耦合。
- **未来 BMFF strip 的复用不经过图片格式的分帧抽象**：BMFF 盒读取器（`containers/isobmff.rs`）已存在且已共享，届时让 BMFF strip planner 直接消费它即可——这与现在图片格式 read↔strip 是否共享分帧正交。因此现在做 C 省不到未来的活，反而徒增风险。我们改为**设计好接缝**（见 §3 指令 trait），让 BMFF 后续无缝接入。

### 模块布局
```
omni-meta-core/src/strip/
  mod.rs        # StripPlanner trait + StripCmd + StripOptions + StripReport
                # + RemovedKind/RemovedKinds + planner_for 分派 + drive_strip_slice 引擎
  jpeg.rs       # JpegStripper
  png.rs        # PngStripper（含 CRC 重算，仅对合成 eXIf 时需要）
  webp.rs       # WebpStripper（含 RIFF filesize 回填）
  exif_synth.rs # 最小 EXIF 合成器（keep_orientation 用）
omni-meta-core/src/adapters/strip_slice.rs   # strip_slice(&[u8], StripOptions) -> Result<(Vec<u8>, StripReport), Error>
omni-meta/src/adapters/strip_blocking.rs     # strip_blocking<R:Read, W:Write>(r, w, StripOptions) -> Result<StripReport, Error>
```

对称性：`strip/` ↔ `formats/`，`strip_slice` ↔ `read_slice`，`strip_blocking` ↔ `read_blocking`。

---

## 3. sans-io 指令 trait（格式无关，面向容器复用）

```rust
/// 单类被删元数据的归类，用于 StripReport 统计。
pub enum RemovedKind { Exif, Xmp, Iptc, Icc, Other }

pub enum StripCmd {
    /// 把输入窗口接下来的 n 字节原样拷到输出。
    Emit(usize),
    /// 丢弃输入窗口接下来的 n 字节（被剥离的元数据），计入报告。
    Drop { len: usize, kind: RemovedKind },
    /// 消费输入窗口接下来的 consume 字节，改写为 `with` 写入输出
    /// （WebP RIFF filesize 回填；keep_orientation 合成段的注入也走此/Insert）。
    Replace { consume: usize, with: Vec<u8> },
    /// 不消费任何输入，向输出注入 `bytes`（合成的最小 EXIF 段）。
    Insert(Vec<u8>),
}

/// 一次 pull 的结果：下一步需求 + 本步消耗输入字节 + 指令序列。
pub struct StripResult {
    pub demand: StripDemand, // Need(usize) / Done
    pub consumed: usize,
    pub cmds: Vec<StripCmd>,
}

pub trait StripPlanner {
    fn pull(&mut self, input: &[u8]) -> StripResult;
}
```

注：`Replace`/`Insert` 持**拥有式** `Vec<u8>`（合成段 ≤ ~64 字节，分配可忽略），避免 `&mut self` 与 `input` 同生命周期的自借用约束——故 trait 无生命周期参数。`consumed` 之和 = `Emit + Drop + Replace.consume` 覆盖的输入字节；`Insert` 不计 `consumed`。

**接缝**：未来 BMFF strip planner 产出同样的 `StripCmd` 流、消费 `containers/isobmff.rs`，无需改动本 trait 或图片格式 walker。

---

## 4. 各格式 walker

### 4.1 JPEG（`strip/jpeg.rs`）
- 逐段遍历（同读解析器的分帧，但语义为 Emit/Drop）：
  - SOI、SOF、DQT/DHT、SOS 之后的熵编码数据、EOI 等**结构/图像段 → Emit**。
  - APP1 且 body `starts_with("Exif\0\0")` → Drop(Exif)。
  - APP1 且 body 为 XMP 命名空间（`http://ns.adobe.com/xap/1.0/\0`）→ Drop(Xmp)。
  - APP13 且 `8BIM`（Photoshop IRB，含 IPTC-IIM resource 0x0404）→ Drop(Iptc)。
  - APP2 且 body `starts_with("ICC_PROFILE\0")` → `keep_icc ? Emit : Drop(Icc)`。
  - 其它 APPn（如 APP0/JFIF）→ Emit（保留，非隐私元数据）。
- `keep_orientation`：见 §5。
- SOS 起的熵编码数据无显式长度——遇 SOS 后**整段 Emit 到文件尾**（到 EOI 及之后），strip 不再解析标记（与读不同，读到 SOS 即停）。

### 4.2 PNG（`strip/png.rs`）
- 逐 chunk 遍历（len/type/data/crc）：
  - `IHDR`、`IDAT`、`PLTE`、`IEND` 等关键/图像 chunk → Emit。
  - `eXIf` → Drop(Exif)。
  - `iTXt`/`tEXt`/`zTXt` 且 keyword 为 XMP（`XML:com.adobe.xmp`）→ Drop(Xmp)；其它文本 chunk → Drop(Other)（含潜在隐私文本注释）。
  - `iCCP` → `keep_icc ? Emit : Drop(Icc)`。
- chunk 删除是整块（len+type+data+crc）删除，PNG 无全局长度字段、无需改别处。

### 4.3 WebP（`strip/webp.rs`）
- 逐 RIFF chunk 遍历（fourcc/size/data/pad）：
  - `VP8 `/`VP8L`/`VP8X`/`ANIM`/`ANMF`/`ALPH` 等图像 chunk → Emit。
  - `EXIF` → Drop(Exif)；`XMP ` → Drop(Xmp)；`ICCP` → `keep_icc ? Emit : Drop(Icc)`。
  - 删除含其后的偶数对齐 pad。
  - **VP8X flags**：删除 EXIF/XMP/ICC chunk 后，应清除 VP8X 头里对应的 EXIF(bit)/XMP(bit)/ICC(bit) 标志位，否则解码器会找不到声明的 chunk。VP8X 头用 `Replace` 改写标志字节。
- **RIFF filesize 回填**：filesize（offset 4）= 4("WEBP") + 保留 chunk 总长。planner 声明需整个 RIFF 区入窗（`Need` 到边界，受 `Limits.max_payload_bytes` 封顶；超限 → `Error`），算出新 filesize 用 `Replace` 改那 4 字节。slice 路径天然全缓冲。

---

## 5. keep_orientation：最小 EXIF 合成（`strip/exif_synth.rs`）

orientation（EXIF tag `0x0112`，SHORT，值 1..=8）藏在 EXIF TIFF 块里，保留它需在删除原 EXIF 后**合成一个只含 Orientation 一条目的最小 TIFF**写回。

### 流程
1. walker 在删除 EXIF 段前，先在该段的 TIFF 内**就地查出** Orientation 值（复用读路径的轻量定位逻辑或内联一个最小 IFD0 扫描，仅找 0x0112）。
2. 若存在且 `keep_orientation`：Drop 原 EXIF 段，再 `Insert`（或 `Replace`）一个合成的最小 EXIF 段。
3. 若不存在 orientation：照常全删，不合成。

### 合成 TIFF 字节（小端 II，固定骨架，约 26 字节）
```
II 2A00 08000000            # 头：little-endian, magic 42, IFD0@8
0100                        # IFD0 count = 1
12 01  03 00  01000000  XX 00 0000   # entry: tag=0x0112, type=SHORT(3), count=1, value=XX(内联)
00000000                    # next IFD = 0
```
- JPEG：包成 APP1，body 前缀 `"Exif\0\0"` + 上述 TIFF，外加 `FFE1` + 2 字节段长。
- PNG：包成 `eXIf` chunk（裸 TIFF），**需重算 CRC**（`strip/png.rs` 含 CRC32）。
- WebP：包成 `EXIF` chunk（裸 TIFF），并在 VP8X flags 保留 EXIF bit；计入 filesize。

合成段位置：放在源 EXIF 原位（JPEG 紧随 SOI/APP0 区；PNG 在 IDAT 前；WebP 在 VP8X 后）。

---

## 6. 写安全契约（与读的 best-effort 相反）

读路径是「尽力解析、部分结果」。**写路径绝不能产出损坏文件**：

1. **边界确定才删**：任何无法安全导航的结构 / 歧义段 → **保留该区字节**（宁可漏删也不删错），并发 `Warning`（`WarnKind` 复用或新增 `StripSkippedAmbiguous`）。
2. **要么干净剥离、要么字节等同源**：输出永远是「合法的已剥离文件」或「与输入字节一致的安全副本」，**绝不输出半成品损坏文件**。
3. **灾难性损坏**（连框架都走不下去 / WebP 超 `max_payload_bytes` 无法回填）→ 返回 `Error`，不写出任何输出。
4. 不变量延续：`#![forbid(unsafe_code)]`、显式栈迭代非递归、所有偏移/长度 `checked_*`、永不 panic、缺失即不合成不臆造。

### 新增 Error 变体
```rust
pub enum Error {
    UnrecognizedFormat,
    Unsupported,   // 新增：已识别格式但 v1 strip 不支持（GIF/HEIF/MP4/MKV…）
    Io,
}
```
`planner_for(fmt)` 仅 Jpeg/Png/Webp 返回 `Some`；其余已识别格式 → `Err(Unsupported)`；`Unknown` → `Err(UnrecognizedFormat)`。

---

## 7. 数据流与适配器

### 引擎 `drive_strip_slice`（core, no_std）
```
probe(buf) → FileFormat
planner_for(fmt, opts) → Box<dyn StripPlanner>（None → Err）
loop:
    res = planner.pull(window)
    for cmd in res.cmds:
        Emit(n)      → out.extend(window[..n])
        Drop{len,k}  → report.bytes_removed += len; report.removed |= k
        Replace{c,w} → out.extend(w)            （跳过 window[..c]）
        Insert(b)    → out.extend(b)
    advance window by res.consumed
    match res.demand { Done → break, Need(_) → slice 下全缓冲，继续 }
→ (out, report)
```

### `strip_slice`（core, no_std）
全缓冲，直接调 `drive_strip_slice`，返回 `(Vec<u8>, StripReport)`。

### `strip_blocking`（omni-meta, std）
**v1 实现**：把输入读入有界缓冲（累计 ≤ `Limits.max_payload_bytes`，超限 → `Error::Io`），再走 `strip_slice` 引擎，最后整块写出 `W`。
- 理由：sans-io 核心（`StripPlanner`）保持纯状态机；blocking 缓冲是适配器实现选择。让 planner 只需处理「整缓冲」一种模式，bug 面最小；WebP filesize 回填天然成立；slice↔blocking 输出**字节级一致**变为平凡真值（两者同走 slice 引擎）。
- **非目标（v1）**：真·窗口流式（常量内存）——需 planner 同时支持增量窗口模式，复杂度与 bug 面显著上升，留待后续优化（届时各 planner 增量化即可，trait 不变）。

### StripReport
```rust
pub struct StripReport {
    pub format: FileFormat,
    pub bytes_removed: u64,
    pub removed: RemovedKinds,  // 位标记集合：exif/xmp/iptc/icc/other
    pub warnings: Vec<Warning>,
}
```

### 公开面（lib.rs `pub use`）
`strip_slice`、`strip_blocking`、`StripOptions`、`StripReport`、`RemovedKind`/`RemovedKinds`、扩展的 `Error`。

---

## 8. 测试策略

- **回环 oracle（默认/隐私模式）**：`strip(file, default)` → `read_slice(stripped)` 断言隐私项（GPS/机型/时间/作者/XMP/IPTC）全清空；`width`/`height` 不变；**ICC 段仍在**、`unified.orientation` **保持**（默认 `keep_*=true`）。
- **回环 oracle（aggressive 模式）**：`strip(file, StripOptions::aggressive())` → 断言**零 EXIF / 零 ICC / 零 orientation**，隐私项亦空。
- **幂等**：`strip(strip(x)) == strip(x)`（字节级）。
- **slice ↔ blocking 字节级一致**：两适配器对同一输入输出 byte-identical（WebP 含 filesize 回填 + VP8X flags）。
- **结构完整**：剥离产物可被读路径重新解析、维度一致；WebP filesize/对齐自洽。
- **keep_orientation 合成正确**：合成 TIFF 能被现有 EXIF codec 解回 orientation 原值（小端 + 各容器封装各一例）。
- **合成畸形**：截断段 / 超长段长 / VP8X 声明 chunk 缺失 → 不 panic、安全输出（保留歧义区或 `Error`，绝不损坏）。
- **fuzz**：新增 `strip` target（接入 `fuzz/` workspace）——`strip(任意输入)` 永不 panic / 不超 `Limits`；不变式：`read(strip(x))` 无隐私元数据残留；输出要么可被 `read_slice` 重解析、要么字节等同输入。
- **no_std**：core strip `--no-default-features` 构建通过。

---

## 9. 不变量（不得破坏）

- `#![forbid(unsafe_code)]` 全库。
- 显式栈迭代，非递归；所有偏移/长度 `checked_*`，溢出 → 保留字节 + Warning，不 panic。
- **写路径绝不输出损坏文件**：歧义保留、灾难 `Error`、要么干净剥离要么字节等同源。
- slice 与 blocking 输出**字节级一致**（新增的 strip 版「适配器差分」）。
- 缺失即不合成、不臆造（`keep_orientation` 仅在源确有 orientation 时合成）。
</content>
</invoke>
