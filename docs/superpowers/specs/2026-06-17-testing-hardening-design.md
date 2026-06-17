# testing-hardening 设计：no_std CI + 黄金样本

**日期** 2026-06-17 · **状态** 已批准，待实现
**关联**：ROADMAP §4 横切待办「no_std CI」「黄金样本」；基准设计 §5 不变量。

---

## 1. 动机

当前测试基座有两个缺口：

1. **no_std 无人守门**。`omni-meta-core/src/lib.rs:1` 已是 `#![cfg_attr(not(feature = "std"), no_std)]` +
   `extern crate alloc`，`--no-default-features` 本机构建通过——但仓库**没有任何 CI**（`.github/` 不存在）。
   哪天有人不慎引入 `std::`，no_std 即悄然破裂、无人察觉。

2. **同源偏差**。现有 fixtures（`omni-meta-fixtures/src/lib.rs`）全是**合成字节构造器**，断言的是
   「四适配器互相一致」。但构造器与解析器可能**同源一起错**：构造器写错某字节布局，解析器以同样错误
   理解，四适配器仍一致、测试仍绿。缺少**独立真相源**锚定。

本设计一次性补齐两者，且**纯测试基座增量，不碰任何解析器/格式逻辑**。

---

## 2. 关键不变量：CI 无外部工具依赖

黄金样本文件是**提交进仓库的二进制**（经 `include_bytes!` 内嵌），期望值是**提交进仓库的 Rust 常量**。
`ffmpeg`/`exiftool` **仅在本机造样本/重生样本时使用**；CI 跑测试时完全不需要它们——CI 全程纯 Rust、自洽、可离线。

这是整个设计的支点：黄金样本的「真实性」来自生成期的独立工具，但「可复跑性」来自把产物固化进仓库。

---

## 3. A 部分：黄金样本

### 3.1 样本生成（本机一次性 + 可重生）

- 脚本 `omni-meta-fixtures/samples/regen.sh`：
  - 用 `ffmpeg` 造各格式容器骨架；用 `exiftool` 写入真实 EXIF / XMP / GPS 标签。
  - 提交进仓库供审计与重生，**不进 CI**、不被任何测试调用。
  - 力求每个样本最小体积（图片目标几 KB，视频容器尽量裁到最小可解析帧）。
  - provenance：全部自生成 → 无第三方版权问题。

- 覆盖（每个解析器家族 ≥1）：

  | 样本 | 格式 | 锚定的 Unified 字段 |
  |---|---|---|
  | `jpeg_exif_gps` | JPEG | width/height/make/model/orientation/created/**gps** |
  | `png_exif` | PNG | width/height + eXIf/XMP 来源 |
  | `gif_xmp` | GIF | width/height + XMP |
  | `webp_exif` | WebP | width/height + EXIF/XMP |
  | `mp4` | MP4 | width/height/duration_ms/created |
  | `mov` | MOV | width/height/duration_ms/created（含 mdta 来源若适用） |
  | `mkv` | Matroska | width/height/duration_ms/created |
  | `webm` | WebM | width/height/duration_ms |
  | `heic`/`avif` | HEIF/AVIF | **best-effort**：能编则加，否则见 §3.5 |

### 3.2 期望值（exiftool 独立裁定）

- 每个样本生成后用 `exiftool` 读出真值，**人工**译成期望 `Unified` 字段 + `FileFormat`。
- 期望值写成 Rust 常量，与样本字节并置于 `omni-meta-fixtures`：

  ```rust
  pub struct GoldenSample {
      pub name: &'static str,
      pub bytes: &'static [u8],      // include_bytes!("samples/xxx.jpg")
      pub format: FileFormat,
      pub unified: Unified,          // exiftool 派生的期望，仅置已知字段
  }
  pub fn golden_corpus() -> Vec<GoldenSample> { /* ... */ }
  ```

- `omni-meta-fixtures/samples/README.md` 逐文件登记：生成命令、`exiftool` 核对读数、对应期望 `Unified` 值。

### 3.3 断言（`omni-meta/tests/golden.rs`）

对每个 `GoldenSample`：

1. **四适配器一致性**：`assert_all_equal(sample.bytes)`——把**真实文件**纳入差分语料
   （顺带扩大既有 oracle 的覆盖面，真实字节布局比合成器更接近线上）。
2. **独立真相锚定**：`read_slice(bytes).unified` 的**每个期望字段**严格等于 `sample.unified`
   对应字段，且 `format == sample.format`。

### 3.4 断言粒度与口径

- 只断言 **`unified` 字段 + `format`**——即受控增长的 Unified 契约。
- **不**断言 `raw` 全量：真实文件 raw 标签体积大、随生成工具版本漂移、易脆。
  （四适配器间 raw 仍由 §3.3 第 1 步的 `assert_all_equal` 保证一致——那是跨适配器口径，
  不是对外部真相的锚定。）
- **不**断言 `warnings`：best-effort 诊断，与既有 oracle 口径一致。
- 期望 `Unified` 只填**已知确定**的字段；不确定的字段留 `None`，但若 omni-meta 在该字段
  产出了**非 None 且 exiftool 也证实有值**，应回填期望（即期望表随认知完善）。

### 3.5 冲突与缺口处理（spec 硬规定）

- **exiftool 与 omni-meta 字段冲突**：这正是黄金样本要抓的缺陷。处理 = **视为 omni-meta 潜在 bug，
  停下来报告**；**不得**擅自把期望值改成 omni-meta 的输出来「让测试变绿」。冲突需经人裁定根因后再决定
  改实现还是改期望（exiftool 也可能误读边缘情况）。
- **HEIC/AVIF 编不出**（本机 ffmpeg 无对应编码器）：标记为**已知缺口**写进
  `samples/README.md`，**不阻塞合并**；保留现有合成 HEIC fixture（`fixture_bmff_heic`）兜底。

---

## 4. B 部分：no_std CI + 全套门禁

### 4.1 `.github/workflows/ci.yml`

触发：`push` 到 `main` + `pull_request`。三个 job：

1. **test**（`ubuntu-latest`, stable）
   - `cargo fmt --all --check`
   - `cargo clippy --all-targets --all-features -- -D warnings`
   - `cargo test --all`（含 §3 黄金样本测试）

2. **no_std**（stable + 裸机 target `thumbv7em-none-eabi`）
   - `rustup target add thumbv7em-none-eabi`
   - `cargo build -p omni-meta-core --no-default-features --target thumbv7em-none-eabi`
   - `cargo build -p omni-meta --no-default-features --target thumbv7em-none-eabi`
   - 依据：两个 crate 是**库**，只用 `core` + `alloc`；裸机 target 无 std 可链，是 no_std 的金标准证明。
     库构建不链接二进制，故无需定义 panic handler / global allocator。

3. **fuzz-build**（nightly）
   - 装 nightly + `cargo install cargo-fuzz`（缓存）
   - `cd fuzz && cargo +nightly fuzz build`——**仅编译防腐**（fuzz 是独立 workspace，需 nightly）。

### 4.2 工具与缓存

- 装链：`dtolnay/rust-toolchain`（stable / nightly / 带 target）。
- 缓存：`Swatinem/rust-cache`（每 job 分键）。

### 4.3 与本地一致性

CI 的 test/no_std 命令必须与本地可跑命令一一对应，便于本地先验（无需 push 才发现红）。

---

## 5. 边界（本次不做）

- 不碰任何解析器 / 格式 / codec 逻辑——纯测试与 CI 基座。
- 不引入快照库（如 `insta`）：期望值用手写 Rust 常量，零新依赖，契合 core 零依赖的取向。
- 不做 async/tokio 第五适配器（里程碑 D，另立）。
- 黄金样本不追求穷举每字段，只锚定每家族的代表性 Unified 字段；后续可增量补。

---

## 6. 验收标准

- [ ] `.github/workflows/ci.yml` 三 job 在 CI 上全绿。
- [ ] `cargo build -p omni-meta-core --no-default-features --target thumbv7em-none-eabi` 通过。
- [ ] `omni-meta-fixtures/samples/` 含各家族真实样本 + `regen.sh` + `README.md`（provenance + exiftool 期望）。
- [ ] `golden_corpus()` 暴露样本；`omni-meta/tests/golden.rs` 对每个样本跑四适配器一致 + Unified/format 锚定，全绿。
- [ ] HEIC/AVIF 若未纳入，README 明确记录缺口与原因。
- [ ] ROADMAP §4 勾选「no_std CI」「黄金样本」。
- [ ] 全程未改动任何解析器/格式逻辑（diff 仅涉及 fixtures/tests/CI/docs）。
