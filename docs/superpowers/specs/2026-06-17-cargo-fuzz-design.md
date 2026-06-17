# cargo-fuzz 鲁棒性 harness — 设计

**日期** 2026-06-17 · **里程碑** 生产硬化 · 支柱 1/3（fuzz）
**方案** C 混合（公共 API oracle 广度 + `__fuzzing` 逐解析器深度）
**关联** ROADMAP §4「fuzz：每个新容器/codec 接 cargo-fuzz，断言永不 panic / 不超 Limits / 不死循环」、§5 不变量

---

## 1. 目标与非目标

### 目标
为现有 7 格式（JPEG/PNG/WebP/GIF + HEIF/AVIF/MP4/MOV/MKV/WebM）/ 2 容器（ISO-BMFF、EBML）/
2 codec（EXIF、XMP）建立持续可跑的 fuzz harness，把 §5 三条核心不变量从「人工断言」变成
「模糊器可证伪的运行时性质」：

1. **永不 panic** —— 任意字节序列下不得 panic / 溢出 abort（libfuzzer 自动捕获）。
2. **不超 Limits / 不 OOM** —— 解析期实时分配不得越过上界（计数分配器 tripwire）。
3. **不死循环 / 必终止** —— 任意输入有界时间内返回（libfuzzer 超时捕获）。

附加高价值性质（复用现有 oracle）：

4. **四适配器一致** —— 同一字节经 `read_slice`/`read_blocking`/`read_seek`/`PushParser` 必须
   全体 `Ok` 且 `Metadata` 逐字段相等，或全体 `Err`（对抗性输入下的适配器分歧探测）。

### 非目标（YAGNI）
- CI 接线（生产硬化支柱 2，独立 spec）。
- 真实文件黄金样本（支柱 3，独立 spec）。
- `arbitrary` 派生的类型化输入 / 结构感知变异器（v1 用「任意字节 + 结构化种子语料」即可）。
- OSS-Fuzz / 持续模糊基础设施。

---

## 2. 架构与布局

新增 `fuzz/` crate，遵循 cargo-fuzz 约定，**自成独立 workspace**（`fuzz/Cargo.toml` 内置空
`[workspace]` 表），将 nightly-only、std-only 的模糊工具从主 stable / `no_std` workspace 中隔离。

```
omni-meta/                  ← 现有工作区根（stable，no_std 友好）
├── omni-meta-core/         ← 新增 __fuzzing 特性
├── omni-meta/              ← facade
├── omni-meta-fixtures/     ← 【新】共享字节构造器（dev/seed only）
└── fuzz/                   ← 【新】独立 workspace，nightly + libfuzzer
    ├── Cargo.toml          ← 空 [workspace]；依赖 omni-meta (features=std,__fuzzing) + libfuzzer-sys
    ├── src/
    │   ├── lib.rs          ← CountingAlloc + oracle 助手 + 共享 harness
    │   └── bin/seeds.rs    ← 种子语料生成器（消费 omni-meta-fixtures，写入 corpus/<target>/）
    └── fuzz_targets/
        ├── differential.rs
        ├── read_slice_bounded.rs
        ├── isobmff.rs
        ├── ebml.rs
        ├── exif.rs
        └── xmp.rs
```

工具链已就绪：nightly 已装、`cargo-fuzz 0.13.1` 已装。

---

## 3. `__fuzzing` 特性（深度暴露）

`omni-meta-core` 新增 `__fuzzing` 特性（默认关闭，双下划线 + `#[doc(hidden)]` 标记「内部、
非 semver 稳定」）。开启后 `#[doc(hidden)] pub` 暴露四个**薄包装入口**，使逐解析器 target 能
绕过 `probe` 直接强制走某条解析路径（即便任意字节本不会被 probe 路由到该格式）：

| 包装入口（拟名，签名于计划期定稿） | 包装对象 | 形态 |
|---|---|---|
| `fuzz_decode_exif(data: &[u8], limits: &Limits)` | `codecs::exif::decode` | 直接喂 payload 字节 |
| `fuzz_decode_xmp(data: &[u8], limits: &Limits)` | `codecs::xmp::decode` | 直接喂 payload 字节 |
| `fuzz_drive_bmff(data: &[u8], limits: &Limits)` | 构造 BMFF 解析器 + `drive_slice` | 强制走 box 遍历 |
| `fuzz_drive_ebml(data: &[u8], limits: &Limits)` | 构造 EBML 解析器 + `drive_slice` | 强制走 element 遍历 |

codec 入口现为 `decode(data, &mut sink, &mut warnings, &limits)`，包装内部自备 sink/warnings 接收。
容器无单一「解析全部」入口（遍历逻辑在 `formats::bmff`/`formats::ebml`），故包装为
「构造该格式解析器 → `drive_slice` 跑完」。

**semver 保证**：特性关闭时公共 API 面与现状完全一致；该特性不进 `default`，不进文档。

---

## 4. 六个 target

| target | 路径 | 断言 |
|---|---|---|
| `differential` | 公共 API | 上抬 `assert_all_equal`：slice/blocking/seek/push(块 1,3,7,full) 全体一致（`Eq`）或全体 `Err` |
| `read_slice_bounded` | 公共 API + alloc harness | 不越分配天花板；产物计数 ≤ `Limits` |
| `isobmff` | `__fuzzing::fuzz_drive_bmff` | box 遍历不 panic/不挂、有界 |
| `ebml` | `__fuzzing::fuzz_drive_ebml` | vint/element 遍历不 panic/不挂、有界 |
| `exif` | `__fuzzing::fuzz_decode_exif` | TIFF/IFD codec 有界 |
| `xmp` | `__fuzzing::fuzz_decode_xmp` | 扫描器有界 |

`differential` 是头牌——经真实 `probe→driver` 路径，同时捕获 panic **与**适配器分歧，复用既有
oracle，几近零新增逻辑。四个 `__fuzzing` target 在「任意字节难以触达的深层结构」上买深度。

所有 target 统一用一个**较小的测试用 `Limits`**（远低于 `Default`，使分配天花板能在合理时间触发），
具体数值于计划期定。

---

## 5. 有界分配 harness（不超 Limits / 不 OOM 的操作化定义）

`fuzz/src/lib.rs` 提供 `CountingAlloc`：包装 `std::alloc::System`，用原子计数器追踪在用字节；
当一次 `alloc` 将跨越天花板（设为略高于测试 `Limits.max_total_alloc`，使合法解析通过、失控分配触发）
时调用 `abort()` → libfuzzer 捕获为可复现 artifact。

**决策（a，已定）**：注册为 fuzz crate 的 `#[global_allocator]`，**对全部 target 生效**——每个
target 都透明地守护分配不变量，不止 `read_slice_bounded`。

`read_slice_bounded` 额外断言产物计数有界：`RawTags` 各篮子（exif/xmp/container 标签向量）长度
≤ `limits.max_tags` 等模型可见的上界（具体字段于计划期对照 `model.rs` 落定）。

---

## 6. 种子语料与 DRY fixtures

`differential.rs` 当前内联的字节构造器（`make_tiff`/`wrap_jpeg`/BMFF/EBML builders 等）抽取到
新 crate `omni-meta-fixtures`（dev/seed 专用，纯构造、不依赖 fuzz）。

**决策（b，已定）**：抽取为共享 crate（DRY、单一真相源），而非在 `fuzz/` 复制种子生成器。

- `differential.rs` 改为 `dev-dependency` 消费 `omni-meta-fixtures`（行为等价，仅搬家）。
- `fuzz/src/bin/seeds.rs` 消费同一 crate，把各 fixture 字节写入 `fuzz/corpus/<target>/`，
  使模糊器从「近合法」输入起步，迅速钻入深层分支。

---

## 7. harness 自测（harness 也是代码）

可测逻辑放在主 crate / fixtures crate 做单测（不在 fuzz bin 内，bin 仅薄壳）：

- `CountingAlloc`：计数正确；跨越天花板时触发（用受控分配验证 tripwire）。
- 上抬的 oracle 助手：对已知良性 fixture 判一致；对**故意植入的分歧**（test cfg 下）判失败——
  证明 oracle 真能证伪，而非恒真。

遵循 TDD：先为 `CountingAlloc` 与 oracle 助手写失败测试，再实现。

---

## 8. 文档与（推迟的）CI 钩子

- `fuzz/README.md`：`cargo +nightly fuzz run differential` 跑法、corpus 位置、artifact 复现
  （`cargo +nightly fuzz run <t> <artifact>`）与最小化（`cargo +nightly fuzz cmin`/`tmin`）。
- 完成时勾选 ROADMAP §4 fuzz checkbox。
- CI smoke job（`-max_total_time=…` 短跑）**仅标注接入点，不在本 spec 范围**（CI 是独立支柱）。

---

## 9. 完成判据（DoD）

- [ ] `fuzz/` 独立 workspace 建立，6 个 target 编译通过（`cargo +nightly fuzz build`）。
- [ ] `omni-meta-core` `__fuzzing` 特性暴露四包装入口；**默认关闭时**公共 API/semver 面不变
      （`cargo build`、`cargo build --no-default-features` 仍绿）。
- [ ] `omni-meta-fixtures` crate 抽取完成；`differential.rs` 改用它且**全部既有测试仍绿**。
- [ ] `CountingAlloc` 注册为全局分配器；自测覆盖计数与触发。
- [ ] oracle 助手自测覆盖「一致通过」与「植入分歧被抓」。
- [ ] 每个 target 能在其种子语料上跑通短时模糊（`-runs=<N>` 冒烟），不立即 panic。
- [ ] 任何模糊期发现的 panic/越界/超 Limits 缺陷：要么修复，要么记录为 artifact + 跟踪项
      （fuzz 大概率会暴露真实 bug——这是预期收益，不是失败）。
- [ ] `fuzz/README.md` 写就；ROADMAP §4 勾选。

---

## 10. 不变量遵从（§5）

本 harness 不改任何解析逻辑，纯增测试设施；`#![forbid(unsafe_code)]` 在主库保持。
`CountingAlloc` 实现 `GlobalAlloc` 需 `unsafe`——但它**只存在于 `fuzz/` crate**，不进
`omni-meta`/`omni-meta-core`，主库 forbid 不破。fixtures crate 纯安全构造。
