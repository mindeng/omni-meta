# omni-meta Roadmap

**活文档** · 最近更新 2026-06-17（testing-hardening 完成：no_std CI 裸机真证 + exiftool 核对的黄金样本）· 维护者随进度勾选
基准设计：[`docs/superpowers/specs/2026-06-14-omni-meta-design.md`](superpowers/specs/2026-06-14-omni-meta-design.md)

> 本文档替代原设计 §11 的线性 phase 表——实际推进中适配器被提前完成、各纵切片
> 顺序有调整，这里是唯一权威的进度与排序来源。

---

## 1. 当前状态快照

四层架构（adapters / engine / formats+containers / codecs+model）已立骨架并跑通端到端。

### 已完成 ✅

| 模块 | 内容 | 关键提交 |
|---|---|---|
| 核心指令机 | `Demand` / `Event` / `MetaParser` / `PullResult` / `Limits`（含 `max_ifds`） | 骨架 |
| 驱动 | `Driver` 指令循环 + Payload→codec 分派 | 骨架 |
| 探测 | `probe` 魔数嗅探 + `parser_for` 分派（JPEG/PNG/WebP/GIF） | `38789fe` |
| **EXIF codec** | TIFF/IFD 解析、泛化值读取（SHORT/LONG/RATIONAL/SRATIONAL/UNDEFINED/数组）、sub-IFD/IFD1 扁平工作队列遍历（visited 防环 + `max_ifds` 封顶）、GPS | `a601593` `4e213fc` `741b81e` |
| **XMP codec** | 非校验式扫描（属性形式/元素形式/`rdf:li`/实体），`max_depth` + `max_payload_bytes` 上界 | `8e8b752` `65da498` |
| 格式：JPEG | SOF 维度 + APP1/Exif Payload | `f4c6b15` |
| 格式：PNG | IHDR 维度 + eXIf + iTXt-XMP | `598d2b6` |
| 格式：WebP | VP8X/VP8/VP8L 维度 + EXIF + XMP | `4cf39f4` |
| 格式：GIF | LSD 维度 + XMP 应用扩展 | `7caf63e` |
| normalize | raw→Unified 投影（仅 Primary IFD）、容器维度优先、XMP 回退 | `d7d279f` `619df68` |
| **容器：ISO-BMFF (A1)** | `read_box_header`/`full_box_vf`/`iter_child_boxes` box 结构层（显式迭代、边界安全）；`ftyp` brand→`FileFormat::{Heif,Avif,Mp4,Mov}` 探测 | `e1bc699` `8682600` |
| **格式：HEIF/AVIF (A2)** | `meta` 下钻 `iinf`/`iloc`→EXIF/XMP item（construction_method 0=mdat 绝对偏移 SeekTo / 1=idat 内联）、`ispe` 维度（`pitm`/`ipma` + 单 ispe 兜底），复用现有 EXIF/XMP codec；method2/越界/截断→警告不 panic | `d8cd745` |
| **格式：MP4/MOV (A3)** | `moov` 整盒入窗解析：`mvhd`→`duration_ms`（duration/timescale→ms，u128 防溢出）/`created`（1904 UTC 秒→`DateTimeParts`），逐 `trak`/`tkhd`→维度（16.16 定点取整，首个非零轨胜出）；`created` 增 EXIF 第二来源（DateTimeOriginal/DateTime + OffsetTime 解析）；`timescale=0`/溢出/`creation=0`/截断/嵌套越界均警告或干净缺失、不 panic | `58b5e06`…`eec7f44` |
| **适配器（4 条）** | `read_slice` / `push` / `read_blocking` / `read_seek` | — |
| 测试基座 | 四适配器差分一致性（含完整 HEIC meta+mdat 的 SeekTo 抽取）+ 各 codec/格式单测 | `535fb90` `d4f5b42` `d4dccd4` |
| **QuickTime 容器标签** | `RawTags.container`（mdta 文本键 / udta `©`-atoms / focal length）+ `software`/`creator` 投影（容器>EXIF>XMP，≥2 来源） | 本次分支 |
| **Stripper (F)** | JPEG/PNG/WebP 剥离 EXIF/XMP/IPTC，默认保留 ICC/orientation（最小 EXIF 合成）；`strip_slice`/`strip_blocking`；`StripOptions::aggressive()` 全删；slice↔blocking 字节级一致 + fuzz target | 本次分支 |

### 当前 Unified 字段

`width` · `height` · `orientation` · `camera_make` · `camera_model` · `duration_ms` · `created` · `gps` · `software` · `creator`（均 `Option`）
> 受控增长原则：每个新字段需 **≥2 种格式来源**才纳入。
> A2 起 `width`/`height` 增 HEIF/AVIF `ispe` 来源（第 5 个格式来源）。
> A3 起新增 `created`（BMFF moov 1904 UTC + EXIF DateTimeOriginal/DateTime，≥2 来源满足；`DateTimeParts`
> 带可选时区：moov=`Some(0)` UTC，EXIF 无 OffsetTime 时 `None`）与 `duration_ms`（BMFF moov，毫秒；
> 第二来源待 EBML 里程碑 C 补齐）。
> C 起 `duration_ms` 增 EBML（MKV/WebM `Info > Duration × TimestampScale`）第二来源；`created` 增 EBML `DateUTC`（2001 UTC）第三来源。`width`/`height` 增 EBML `Video PixelWidth/Height`（第 6 来源）。
> GPS 里程碑起新增 `gps`（`Gps { lat_e7, lon_e7, alt_mm }`，E7/mm 整数表示），来源：EXIF GPS IFD（d/m/s 有理数）+ XMP `exif:GPS*` 回退 + 视频 udta `©xyz`/`loci` + QuickTime moov/meta mdta `location.ISO6709`，≥3 来源满足。
> `camera_make`/`camera_model` 增 QuickTime mdta（首次覆盖视频来源）。`created` 增 QuickTime mdta `creationdate`（ISO 8601 带时区，优先于 mvhd 1904 UTC）。
> QuickTime 容器标签里程碑起新增 `software`（EXIF 0x0131 + XMP `xmp:CreatorTool` + 容器 mdta software / udta `©swr`，≥2 来源）与 `creator`（EXIF 0x013B Artist + XMP `dc:creator` + 容器 mdta author / udta `©aut`，≥2 来源），优先级容器>EXIF>XMP；并在 raw 层新增 `RawTags.container`（QuickTime mdta 文本键 + udta `©`-atoms + focal length 35mm；二进制 covr 留待可选 Phase 3）。

### 尚未开始 ⬜

IPTC codec · ICC 摘要 · TIFF 顶层格式 · async/tokio 适配器 · `video_codec`/`audio_codec` 等 Unified 字段扩展 · `cargo-fuzz`（横切，各容器/codec）

---

## 2. IPTC 决策备忘（关键排序依据）

**事实**：IPTC 有两种形态，区分后才能正确排序——

| 载体 | 本质 | 出现在哪 | 本库现状 |
|---|---|---|---|
| **IPTC-IIM**（传统二进制） | 8BIM/IRB 二进制记录 | **JPEG**（APP13 / `8BIM` / resource `0x0404`）、**TIFF**（tag `0x83BB`）、PSD | 未支持；JPEG 解析器只认 APP1，APP13 被 Skip |
| **IPTC Core/Ext**（现代） | 就是 **XMP**（`Iptc4xmpCore/Ext` 命名空间） | 任何可放 XMP 的格式 | **已覆盖**（XMP codec 原样收 raw） |

**结论**：
- 现代 IPTC（XMP 形态）**已经能拿到**，落在 `RawTags.xmp`。
- 传统 IPTC-IIM 在本库当前四格式里**只有 JPEG 一家**有标准载体；BMFF 系（HEIF/AVIF/MP4/MOV）也几乎不用 IIM。
- 因此 IPTC-IIM **凑不齐"≥2 格式来源"**，短期内只能停在 raw 层（`RawTags.iptc`），**进不了 Unified**——除非同时把 TIFF 顶层格式做了（IIM 第二来源）。
- 它是 raw-only 增量、不阻塞任何模块，**可随时插入，ROI 由使用场景决定**。

**排序判据**（按主用场景选）：

| 场景 | 优先 | 说明 |
|---|---|---|
| 隐私剥离 / 通用图片工具 | **BMFF** | IPTC 几乎用不上 |
| 视频元数据（时长/编解码） | **BMFF + EBML** | 与 IPTC 无关 |
| 专业摄影 / 图库 / 新闻供稿 | **IPTC-IIM（+ TIFF）** | caption/credit/keywords 刚需，常在 JPEG/TIFF |
| 通用性 / 覆盖面最大化 | **BMFF** | 一次解锁 4 个现代格式，杠杆最高 |

> **默认推荐：先 BMFF**（覆盖面与架构验证价值最高；IPTC 作为可随时插入的 raw-only 小增量）。
> 若主场景转向专业图库/新闻，则把 **里程碑 B（IPTC+TIFF）提前到 A 之前**。

---

## 3. 推荐里程碑顺序

每个里程碑都是**可独立测试、可合并的纵切片**，完成时跑四适配器差分 + 单测 + no_std 构建。

### 里程碑 A — ISO-BMFF 容器（HEIF/AVIF/MP4/MOV）

**为什么先做**：一个共享 box 读取器解锁 4 个高价值现代格式；是 sans-io seek 降级设计中
唯一真正考验"向前到达 `moov`/`meta`"的场景，早做早暴露 Driver 缺陷。

**A1（基座，✅ 完成）** — 计划 `plans/2026-06-15-omni-meta-bmff-foundation.md`
- [x] `containers/isobmff.rs`：显式迭代 box 遍历（非递归，`iter_child_boxes`），`checked_*` 偏移、边界安全
- [x] `ftyp` brand 探测接入 `probe`（`heic`/`avif`/`mp4`/`mov`/`isom`…）→ 扩展 `FileFormat`

**A2（HEIF/AVIF meta 抽取，✅ 完成）** — 设计 `specs/2026-06-15-omni-meta-bmff-heif-meta-design.md` / 计划 `plans/2026-06-15-omni-meta-bmff-heif-meta.md`
- [x] HEIF/AVIF：`meta` box 内 `iinf`/`iloc` 定位 EXIF / XMP item → 复用现有 EXIF/XMP codec（method 0=mdat / 1=idat）
- [x] `ispe` 维度（`pitm`/`ipma` 关联 + 单 ispe 兜底）→ `Event::Field`（`width`/`height` 第 5 来源）
- [x] 两阶段 `BmffParser`（Walk 找 meta → Extract `SeekTo` 抽取）+ 两处 Driver 守卫修复（空窗口=EOF、相邻零间隔 SeekTo）
- [x] 四适配器差分（完整 HEIC meta+mdat）+ 截断/越界/method2 警告路径单测

**A3（MP4/MOV moov，✅ 完成）** — 设计 `specs/2026-06-15-omni-meta-bmff-moov-design.md` / 计划 `plans/2026-06-15-omni-meta-bmff-moov.md`
- [x] MP4/MOV：`moov` 维度（`tkhd` 16.16 定点）+ 时长（`mvhd` duration/timescale→ms）+ 创建时间（`mvhd` 1904 UTC）→ `Event::Field`
- [x] 新增 Unified 字段：`duration_ms`（BMFF moov；EBML 里程碑 C 补第二来源）、`created`（BMFF moov + EXIF ≥2 满足）；`DateTimeParts` 带可选时区化解「EXIF 本地无时区 vs BMFF 1904 UTC 秒」（moov=`Some(0)`、EXIF=`None` 或 OffsetTime 解析值）
- [x] box 嵌套/截断/越界 **合成畸形单测**（截断 moov、mvhd 溢出、嵌套越界、`timescale=0`、声明 size 超界 → 永不 panic / 不超 `Limits`）；`cargo-fuzz` 作为独立横切里程碑另立（见 §4）

### 里程碑 B — IPTC-IIM codec（可提前，见 §2）

- [ ] `codecs/iptc.rs`：IIM 记录解析（dataset record 2:xx），`max_tags` 上界
- [ ] `PayloadKind::Iptc` + JPEG APP13 `8BIM`/`0x0404` 识别（`jpeg.rs` 现只认 APP1）
- [ ] `model::IptcRecord` + `RawTags.iptc` 落地
- [ ] （可选）**TIFF 顶层格式** + tag `0x83BB` → 给 IIM 第二来源，可把 caption/credit 投影进 Unified
- [ ] 四适配器差分

### 里程碑 C — EBML 容器（MKV/WebM）✅ 完成 — 设计 `specs/2026-06-16-omni-meta-ebml-design.md` / 计划 `plans/2026-06-16-omni-meta-ebml.md`

- [x] `containers/ebml.rs`：vint 元素 ID/size（保留/剥离标记位、未知大小）+ 元素头/子元素显式迭代 + 大端 uint/int/float
- [x] `formats/ebml.rs`：前向走盒（跳 EBML 头/下钻 Segment 不缓冲/缓冲 Info·Tracks/遇未知大小媒体即停）
- [x] `Info`→`duration_ms`（Duration×TimestampScale，隔离 f64 守卫）/`created`（DateUTC 2001 UTC）；`Tracks`→`width`/`height`（首个视频轨 PixelWidth/Height）
- [x] `probe` 经 `DocType` 区分 `FileFormat::Mkv`/`Webm`（PROBE_MAX→64）；复用里程碑 A 的 `duration_ms`/`created`
- [x] 四适配器差分（WebM/MKV，含大 Void seek + 未知大小 Segment）+ 合成畸形单测（截断/未知大小/越界永不 panic）

### 里程碑 D — async 适配器（feature = `tokio`）

- [ ] `read_async` / `read_async_seek`：`Driver` 薄封装，零格式逻辑重复
- [ ] 接入四适配器差分（升级为五路一致性）

### 里程碑 E — ICC 摘要 codec

- [ ] `codecs/icc.rs`：只取摘要（color space / profile description），不解全 profile
- [ ] JPEG APP2 多段拼接、PNG `iCCP`、BMFF `colr` box

### 里程碑 F — Stripper（唯一"写"路径）✅ 完成 — 设计 `specs/2026-06-17-omni-meta-stripper-design.md` / 计划 `plans/2026-06-17-omni-meta-stripper.md`

- [x] `strip.rs`：sans-io 重写状态机，复用容器读取器，丢弃 EXIF/XMP/IPTC/ICC
- [x] `strip_blocking` + `StripReport`
- [x] 优先 JPEG/PNG/WebP（隐私场景最常用）；box 类作为 stretch

---

## 4. 横切待办（贯穿各里程碑）

- [ ] **Unified 受控增长**：`created`（BMFF+EXIF）/ `duration_ms`（BMFF+EBML，**C 起达 ≥2 来源**）/ `gps`（EXIF GPS IFD + XMP + 视频 ©xyz/loci/QuickTime mdta，**已投影，≥3 来源**）✅ / `video_codec` / `audio_codec` 等随来源达到 ≥2 时纳入
- [ ] **`Value` 枚举**：按需补 `U64`/`I64` 等（当前为 v1 子集）
- [x] **fuzz**：cargo-fuzz harness（独立 `fuzz/` workspace）——6 target（differential/read_slice_bounded/isobmff/ebml/exif/xmp）+ 计数全局分配器（不超 Limits tripwire）+ 复用 fixtures 的种子语料。见 `fuzz/README.md`、设计 `specs/2026-06-17-cargo-fuzz-design.md`、计划 `plans/2026-06-17-cargo-fuzz.md`。首轮暴露并修复 3 个缺陷（seek 越尾误判、前向越尾警告 KIND 不一致、近 u64::MAX skip 的 offset 溢出 panic）。differential oracle 口径：`unified`/`raw`/`format` 严格相等，`warnings` 为 best-effort 不跨适配器比对（slice 随机访问 vs 流式前向只读在边界报告不完整有本质差异）。
- [x] **no_std CI**：GitHub Actions 裸机 target（`thumbv7em-none-eabi`）构建 core+facade；全套门禁（fmt/clippy `-D warnings`/test/no_std/fuzz-build）。见 `.github/workflows/ci.yml`。**裸机真证当场暴露并修复 2 个真实缺陷**：facade `omni-meta` 缺 `#![no_std]` opt-out + 其 `Cargo.toml` 对 core 依赖未 `default-features = false`（host 构建因隐式有 std 而误绿，裸机才暴露）。
- [x] **黄金样本**：真实小样本（ffmpeg 生成）+ **exiftool 独立核对**的期望（Unified 子集 + raw 标签子集，破同源偏差）。8 样本覆盖 JPEG(EXIF+GPS)/PNG/GIF/WebP/MP4/MOV/MKV/WebM；每个跑四适配器一致 + 对外部真相锚定。见 `omni-meta-fixtures/samples/`（`regen.sh` + `README.md` 缺口登记）、`omni-meta-fixtures/src/golden.rs`、`omni-meta/tests/golden.rs`。HEIC/AVIF 因本机 ffmpeg 无 HEIF 复用器未纳入（合成 fixture 兜底）。
- [x] **PNG tEXt/zTXt/iTXt 文本关键字** — 读取侧把 `tEXt`/非XMP-`iTXt`/`zTXt` 收入 `RawTags.text`（`TextTag{keyword, value: TextValue}`，四变体自描述编码+压缩态）；注册关键字投影：`Author`→creator / `Software`→software（末位兜底）、新增 `title`/`description`/`copyright`、`Creation Time`→created（ISO/RFC1123/裸日期）。压缩块（zTXt/压缩 iTXt）保留原始字节、**不解压不报 warning**（解压留未来 feature-gated `omni-meta-inflate`）。keyword 强制 ≤79 防 slice 路径超大分配。**Strip 侧本就默认全删文本块（含 PII）**——此前 ROADMAP 称「strip 盲区」为失实，已纠正并补 PII 删除+幂等测试。设计 `specs/2026-06-17-png-text-keywords-design.md` / 计划 `plans/2026-06-17-png-text-keywords.md`。
- [ ] **待评估：视频朝向（orientation）** — 解析 `tkhd` 变换矩阵（当前 `parse_tkhd` 跳过矩阵区、只取末 8 字节维度）→ 推导旋转 → 投影 `Unified.orientation`（图像 EXIF orientation 作第二来源，可凑 ≥2）。与 QuickTime `video-orientation` 键是同一语义的两个来源，需一并评估优先级。
- [ ] **待评估：anchor/保留机制落地**（基准设计 §锚点/保留机制，至今未实现）。当前 `Demand` 仅 `NeedBytes/Skip/SeekTo/Done`，流式无「解析器声明回看 → 驱动从锚点起保留字节」能力，故**后向 seek 到已弃字节**在流式不可达（`UnreachableSection`）而 slice 全缓冲可达——fuzz 已证此为前向只读流式 vs 随机访问的**本质差异**（畸形 BMFF `iloc` 后向 `SeekTo`）。现状以「oracle 不跨适配器比 warnings + 提取元数据仍一致」消化，**未阻塞**。落地 anchor 的触发条件：①出现需在保留窗口内**回看提取**元数据的真实格式（设计举例 AVI `idx1`）；或②跨「可 seek vs 不可 seek」源的**严格 warning 一致**成为硬需求。届时需扩 `Demand`（anchor/release）+ StreamDriver 保留下界管理（受 `max_retained_bytes` 约束）+ 复核各 parser。
- [x] **容器元数据投影收敛** — make/model/created 现由 normalize 从 `RawTags.container` 统一解释（容器 > EXIF/XMP 优先级）；结构头字段（`tkhd`/`mvhd`/`ispe`/IHDR/EBML 维度、mvhd/EBML created、mvhd duration）经 `StructuralFields` 流入 normalize。GPS 为成文例外：其优先级阶梯交错二进制源与文本源（©xyz > mdta-ISO6709 > loci），整体在 parser 解析后作为 `StructuralFields.gps` 传入 normalize。见设计 `specs/2026-06-19-container-projection-convergence-design.md` / 计划 `plans/2026-06-19-container-projection-convergence.md`。

---

## 5. 不变量（任何里程碑都不得破坏）

- `#![forbid(unsafe_code)]` 全库
- 容器/IFD 遍历一律**显式栈迭代**，非递归
- 所有偏移/长度 `checked_*`，溢出 → `Warning` 跳过，不 panic
- 顶层 API 永返回 `Metadata`（best-effort + `warnings`），仅"格式识别不了/源 I/O 错"才 `Err`
- 新增格式/codec 必须通过**全部适配器差分一致性**
- 缺失即 `None`，**绝不臆造**
