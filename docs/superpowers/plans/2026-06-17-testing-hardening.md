# testing-hardening 实现计划（no_std CI + 黄金样本）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 omni-meta 立 GitHub Actions CI（含裸机 no_std 真证 + 全套门禁），并引入 exiftool 独立核对的真实黄金样本，打破合成 fixture 的同源偏差。

**Architecture:** 纯测试/CI 基座增量，不碰任何解析器/格式/codec 逻辑。黄金样本文件与期望值常量均入库，CI 跑测试时零外部工具依赖；ffmpeg/exiftool 只在本机 `regen.sh` 造样本时用。Unified 与 raw 均做**子集锚定**（断言显式列出的字段/标签，容忍额外项）。

**Tech Stack:** Rust（edition 2024，stable + nightly），GitHub Actions，`thumbv7em-none-eabi` 裸机 target，ffmpeg + exiftool（仅样本生成期）。

**关联 spec:** `docs/superpowers/specs/2026-06-17-testing-hardening-design.md`

---

## 文件结构

| 文件 | 职责 | 动作 |
|---|---|---|
| `omni-meta-core/src/lib.rs` | 公开 API 重导出 | 修改：加 `Gps, ContainerSource` |
| `.github/workflows/ci.yml` | CI 三 job | 创建 |
| `omni-meta-fixtures/samples/regen.sh` | 样本生成脚本（本机，不进 CI） | 创建 |
| `omni-meta-fixtures/samples/*.{jpg,png,gif,webp,mp4,mov,mkv,webm}` | 真实样本二进制 | 创建（脚本产出） |
| `omni-meta-fixtures/samples/README.md` | provenance + exiftool 期望登记 | 创建 |
| `omni-meta-fixtures/src/golden.rs` | `GoldenSample`/`GoldenRawTag`/`golden_corpus()` | 创建 |
| `omni-meta-fixtures/src/lib.rs` | 挂 `mod golden;` 重导出 | 修改 |
| `omni-meta/tests/golden.rs` | 黄金样本断言（四适配器一致 + Unified/raw 子集锚定） | 创建 |
| `docs/ROADMAP.md` | 勾选两项横切待办 | 修改 |

**任务依赖顺序：** Task 1（类型重导出，解锁命名）→ Task 2（CI，独立）→ Task 3/4（造样本）→ Task 5（harness + 类型 + 图片样本断言）→ Task 6（视频样本断言）→ Task 7（README）→ Task 8（ROADMAP + 终验）。Task 2 与 3/4/5… 无代码耦合，subagent 可并行，但建议顺序执行以便审查。

---

## Task 1: 公开重导出 `Gps` 与 `ContainerSource`

黄金样本期望值需命名 `Gps`（构造 `Unified.gps`）与 `ContainerSource`（断言容器标签）。二者已是 `model.rs` 中的 `pub` 类型，只是未经 facade 重导出。

**Files:**
- Modify: `omni-meta-core/src/lib.rs:30-33`

- [ ] **Step 1: 写失败测试**

在 `omni-meta-core/src/lib.rs` 末尾的 `mod smoke` 内追加：

```rust
    #[test]
    fn gps_and_container_source_are_reexported() {
        // 经 crate 根路径可命名 → 证明已重导出（编译期即验证）。
        let _g: crate::Gps = crate::Gps { lat_e7: 0, lon_e7: 0, alt_mm: None };
        let _s: crate::ContainerSource = crate::ContainerSource::QuickTimeMdta;
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p omni-meta-core gps_and_container_source_are_reexported 2>&1 | tail -20`
Expected: 编译错误 `no Gps in the root` / `no ContainerSource in the root`。

- [ ] **Step 3: 加重导出**

把 `omni-meta-core/src/lib.rs` 的

```rust
pub use model::{
    DateTimeParts, ExifTag, FileFormat, IfdKind, Metadata, Orientation, RawTags, Unified, Value,
    WarnKind, Warning, XmpProperty,
};
```

改为

```rust
pub use model::{
    ContainerSource, ContainerTag, DateTimeParts, ExifTag, FileFormat, Gps, IfdKind, Metadata,
    Orientation, RawTags, Unified, Value, WarnKind, Warning, XmpProperty,
};
```

（顺带导出 `ContainerTag`，与 `ContainerSource` 配套，便于后续断言容器标签。）

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p omni-meta-core gps_and_container_source_are_reexported 2>&1 | tail -20`
Expected: PASS（1 passed）。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/lib.rs
git commit -m "feat(api): 重导出 Gps/ContainerTag/ContainerSource（黄金样本期望值需命名）"
```

---

## Task 2: GitHub Actions CI（test / no_std / fuzz-build 三 job）

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: 写 workflow**

创建 `.github/workflows/ci.yml`：

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - name: fmt
        run: cargo fmt --all --check
      - name: clippy
        run: cargo clippy --all-targets --all-features -- -D warnings
      - name: test
        run: cargo test --all

  no_std:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: thumbv7em-none-eabi
      - uses: Swatinem/rust-cache@v2
      - name: build core (no_std, bare-metal)
        run: cargo build -p omni-meta-core --no-default-features --target thumbv7em-none-eabi
      - name: build facade (no_std, bare-metal)
        run: cargo build -p omni-meta --no-default-features --target thumbv7em-none-eabi

  fuzz-build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
      - uses: Swatinem/rust-cache@v2
        with:
          workspaces: fuzz
      - name: install cargo-fuzz
        run: cargo install cargo-fuzz
      - name: build fuzz targets
        run: cd fuzz && cargo +nightly fuzz build
```

- [ ] **Step 2: 本机验证 test job 的命令逐条可过**

Run:
```bash
cargo fmt --all --check && \
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -5 && \
cargo test --all 2>&1 | tail -15
```
Expected: fmt 无输出（已格式化）；clippy 无 warning 退出 0；test 全 pass。
> 若 clippy 报现存告警，**不在本任务修复**（超范围）——记录下来、与用户确认；本任务只确保新增 CI 命令本身正确。

- [ ] **Step 3: 本机验证 no_std bare-metal 构建**

Run:
```bash
rustup target add thumbv7em-none-eabi && \
cargo build -p omni-meta-core --no-default-features --target thumbv7em-none-eabi 2>&1 | tail -5 && \
cargo build -p omni-meta --no-default-features --target thumbv7em-none-eabi 2>&1 | tail -5
```
Expected: 两条均 `Finished`。
> 若失败且根因是库里有 `std::` 泄漏，那正是 no_std CI 要抓的真实缺陷——停下报告，**不擅自改解析器**绕过。

- [ ] **Step 4: 提交**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: GitHub Actions 三 job（test+clippy / 裸机 no_std / fuzz 构建防腐）"
```

---

## Task 3: 样本生成脚本 + 图片样本（JPEG/PNG/GIF/WebP）

用 ffmpeg 造最小图片骨架，exiftool 写**确定性**元数据（值由 `-metadata`/exiftool 参数注入，故先验已知），再用 exiftool 读回核对。所有注入值见下，作为后续期望常量的真相。

**Files:**
- Create: `omni-meta-fixtures/samples/regen.sh`
- Create（脚本产出）: `omni-meta-fixtures/samples/{jpeg_exif_gps.jpg,png_exif.png,gif_xmp.gif,webp_exif.webp}`

**注入的确定性元数据（期望真相）：**

| 样本 | 尺寸 | 注入标签 |
|---|---|---|
| jpeg_exif_gps.jpg | 64×48 | Make=`OmniTest`, Model=`GoldenCam`, Orientation=6(Rotate90), DateTimeOriginal=`2020:01:02 03:04:05`, GPSLatitude=35.5 N, GPSLongitude=139.5 E |
| png_exif.png | 80×60 | Make=`OmniTest`, XMP-dc:Creator=`GoldenAuthor` |
| gif_xmp.gif | 48×32 | XMP-dc:Creator=`GoldenAuthor` |
| webp_exif.webp | 72×54 | Make=`OmniTest` |

- [ ] **Step 1: 写 regen.sh（图片部分）**

创建 `omni-meta-fixtures/samples/regen.sh`（含 shebang + `set -euo pipefail`）：

```bash
#!/usr/bin/env bash
# 重生黄金样本。本机运行，需 ffmpeg + exiftool；产物入库、CI 不调用。
# 所有元数据值是确定性注入的（见 README.md），exiftool 读回仅作交叉核对。
set -euo pipefail
cd "$(dirname "$0")"

# ---- JPEG: 64x48 + EXIF(Make/Model/Orientation/DateTimeOriginal) + GPS ----
ffmpeg -y -f lavfi -i color=c=red:s=64x48 -frames:v 1 jpeg_exif_gps.jpg
exiftool -overwrite_original \
  -Make=OmniTest -Model=GoldenCam -Orientation#=6 \
  -DateTimeOriginal="2020:01:02 03:04:05" \
  -GPSLatitudeRef=N -GPSLatitude=35.5 \
  -GPSLongitudeRef=E -GPSLongitude=139.5 \
  jpeg_exif_gps.jpg

# ---- PNG: 80x60 + eXIf(Make) + XMP(dc:creator) ----
ffmpeg -y -f lavfi -i color=c=green:s=80x60 -frames:v 1 png_exif.png
exiftool -overwrite_original -Make=OmniTest -XMP-dc:Creator=GoldenAuthor png_exif.png

# ---- GIF: 48x32 + XMP(dc:creator) ----
ffmpeg -y -f lavfi -i color=c=blue:s=48x32 -frames:v 1 gif_xmp.gif
exiftool -overwrite_original -XMP-dc:Creator=GoldenAuthor gif_xmp.gif

# ---- WebP: 72x54 + EXIF(Make) ----
ffmpeg -y -f lavfi -i color=c=yellow:s=72x54 -frames:v 1 -c:v libwebp webp_exif.webp
exiftool -overwrite_original -Make=OmniTest webp_exif.webp

echo "图片样本已生成。"
```

- [ ] **Step 2: 运行脚本生成图片样本**

Run: `bash omni-meta-fixtures/samples/regen.sh 2>&1 | tail -20`
Expected: 末行 `图片样本已生成。`，四个文件存在。
> 若 `libwebp` 缺失致 WebP 失败：从脚本暂移 WebP 段、在 README 记 WebP 缺口，继续（不阻塞）。

- [ ] **Step 3: exiftool 读回核对（确认注入值落地）**

Run:
```bash
cd omni-meta-fixtures/samples
exiftool -s -Make -Model -Orientation -DateTimeOriginal -GPSLatitude -GPSLongitude -ImageWidth -ImageHeight jpeg_exif_gps.jpg
exiftool -s -Make -Creator -ImageWidth -ImageHeight png_exif.png
exiftool -s -Creator -ImageWidth -ImageHeight gif_xmp.gif
exiftool -s -Make -ImageWidth -ImageHeight webp_exif.webp
cd -
```
Expected: 各值与上表一致（GPS 以度显示 35.5 / 139.5；尺寸与注入一致）。
> 记下实际读数——Task 5/7 的期望常量与 README 以此为准。任何与注入值的偏差先查 exiftool 写入是否成功。

- [ ] **Step 4: 快速确认 omni-meta 能解析（不崩、能出格式）**

Run:
```bash
cargo run -q -p omni-meta --example dump 2>/dev/null || \
cat > /tmp/omni_probe.rs <<'EOF'
fn main() {
    for p in ["jpeg_exif_gps.jpg","png_exif.png","gif_xmp.gif","webp_exif.webp"] {
        let path = format!("omni-meta-fixtures/samples/{p}");
        match std::fs::read(&path) {
            Ok(b) => { let m = omni_meta::read_slice(&b, omni_meta::Options::default()); println!("{p}: {:?}", m.map(|m| (m.format, m.unified.width, m.unified.height))); }
            Err(e) => println!("{p}: read err {e}"),
        }
    }
}
EOF
echo "（无 example 时，Task 5 的测试将覆盖解析验证）"
```
Expected: 至少不报错；正式断言留给 Task 5。此步仅烟雾确认文件可被读取。
> 本步是探查性的，可跳过——真正的解析验证在 Task 5 的黄金测试里。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-fixtures/samples/regen.sh omni-meta-fixtures/samples/*.jpg omni-meta-fixtures/samples/*.png omni-meta-fixtures/samples/*.gif omni-meta-fixtures/samples/*.webp
git commit -m "test(golden): regen.sh + 图片真实样本（JPEG/PNG/GIF/WebP，确定性注入元数据）"
```

---

## Task 4: 视频样本（MP4/MOV/MKV/WebM）+ HEIC best-effort

ffmpeg 造精确时长/尺寸的最小视频容器。用 `testsrc duration=2 rate=25`（恰 50 帧）令 mvhd duration/timescale 给出确定 2000ms；`-metadata creation_time` 固定创建时间。

**Files:**
- Modify: `omni-meta-fixtures/samples/regen.sh`（追加视频段）
- Create（脚本产出）: `omni-meta-fixtures/samples/{mp4.mp4,mov.mov,mkv.mkv,webm.webm}`（+ 可选 `heic.heic`）

**注入的确定性元数据：**

| 样本 | 尺寸 | 时长 | creation_time |
|---|---|---|---|
| mp4.mp4 | 64×48 | 2000 ms | 2020-01-02T03:04:05Z |
| mov.mov | 64×48 | 2000 ms | 2020-01-02T03:04:05Z |
| mkv.mkv | 64×48 | 2000 ms | 2020-01-02T03:04:05Z |
| webm.webm | 64×48 | 2000 ms | （WebM 不强求 created） |

- [ ] **Step 1: 追加视频段到 regen.sh**

在 `regen.sh` 的 `echo "图片样本已生成。"` **之前**插入：

```bash
# ---- 视频：精确 2.000s（testsrc 50 帧 @25fps）、64x48、固定创建时间 ----
VID_FILTER="testsrc=duration=2:size=64x48:rate=25"
CT="2020-01-02T03:04:05Z"

# MP4（H.264）
ffmpeg -y -f lavfi -i "$VID_FILTER" -c:v libx264 -pix_fmt yuv420p \
  -metadata creation_time="$CT" mp4.mp4
# MOV（H.264，brand qt）
ffmpeg -y -f lavfi -i "$VID_FILTER" -c:v libx264 -pix_fmt yuv420p -f mov \
  -metadata creation_time="$CT" mov.mov
# MKV（H.264）
ffmpeg -y -f lavfi -i "$VID_FILTER" -c:v libx264 -pix_fmt yuv420p \
  -metadata creation_time="$CT" mkv.mkv
# WebM（VP9，最小）
ffmpeg -y -f lavfi -i "$VID_FILTER" -c:v libvpx-vp9 -b:v 50k webm.webm

# HEIC（best-effort：编码器常缺，失败不致命）
ffmpeg -y -f lavfi -i color=c=red:s=64x48 -frames:v 1 -c:v libx265 heic.heic 2>/dev/null \
  && echo "HEIC 已生成。" || echo "HEIC 跳过（无 libx265/heif 复用器）——见 README 缺口。"
```

- [ ] **Step 2: 运行脚本（生成全部样本）**

Run: `bash omni-meta-fixtures/samples/regen.sh 2>&1 | tail -25`
Expected: 末尾 `图片样本已生成。`；视频四文件存在；HEIC 行二选一打印。
> ffmpeg 缺某编码器（libx264/libvpx-vp9）时该格式失败——从脚本暂移该段、README 记缺口，继续。

- [ ] **Step 3: exiftool 核对视频尺寸/时长/创建时间**

Run:
```bash
cd omni-meta-fixtures/samples
for f in mp4.mp4 mov.mov mkv.mkv webm.webm; do
  echo "== $f =="; exiftool -s -ImageWidth -ImageHeight -Duration -CreateDate -MediaCreateDate "$f"
done
cd -
```
Expected: 各 64×48；Duration ≈ 2.00 s；CreateDate ≈ 2020:01:02 03:04:05。
> **关键**：记下 exiftool 实际 Duration。Task 6 期望 `duration_ms=2000`。若某容器 exiftool 报非 2.00s（如 2.04s），说明该容器头时长非精确 2000ms——则该样本 Task 6 **不固定 duration_ms**（留 None 不断言），并在 README 记原因。created 同理：能精确则固定，否则不固定。

- [ ] **Step 4: 提交**

```bash
git add omni-meta-fixtures/samples/regen.sh omni-meta-fixtures/samples/*.mp4 omni-meta-fixtures/samples/*.mov omni-meta-fixtures/samples/*.mkv omni-meta-fixtures/samples/*.webm
[ -f omni-meta-fixtures/samples/heic.heic ] && git add omni-meta-fixtures/samples/heic.heic
git commit -m "test(golden): 视频真实样本（MP4/MOV/MKV/WebM，精确时长+固定创建时间；HEIC best-effort）"
```

---

## Task 5: 黄金 harness + 类型 + 图片样本断言

定义 `GoldenSample`/`GoldenRawTag` 与 `golden_corpus()`（先放图片样本），写 `golden.rs` 断言：四适配器一致 + Unified 子集 + raw 子集。

**Files:**
- Create: `omni-meta-fixtures/src/golden.rs`
- Modify: `omni-meta-fixtures/src/lib.rs`（顶部加 `mod golden; pub use golden::*;`）
- Create: `omni-meta/tests/golden.rs`

- [ ] **Step 1: 写 golden.rs 类型与图片样本语料**

创建 `omni-meta-fixtures/src/golden.rs`。**期望值以 Task 3 Step 3 的 exiftool 实际读数为准**（下方按注入值预填；若读数有别，改这里）：

```rust
//! 黄金样本：真实文件 + exiftool 独立核对的期望（Unified 子集 + raw 标签子集）。
//! 文件由 `samples/regen.sh` 生成；期望值是确定性注入并经 exiftool 读回核对的真相。

use omni_meta::{ContainerSource, DateTimeParts, FileFormat, Gps, IfdKind, Unified, Value};

/// 一条 raw 标签期望（断言「存在且值相等」，容忍额外标签）。
#[derive(Debug, Clone)]
pub enum GoldenRawTag {
    Exif { ifd: IfdKind, tag: u16, value: Value },
    Xmp { prefix: &'static str, name: &'static str, value: &'static str },
    Container { source: ContainerSource, key: &'static str, value: Value },
}

/// 一个黄金样本：真实字节 + 期望格式 + 期望 Unified 子集（None 字段=不约束）+ raw 子集。
pub struct GoldenSample {
    pub name: &'static str,
    pub bytes: &'static [u8],
    pub format: FileFormat,
    pub unified: Unified,
    pub raw_subset: Vec<GoldenRawTag>,
}

fn jpeg_exif_gps() -> GoldenSample {
    GoldenSample {
        name: "jpeg_exif_gps",
        bytes: include_bytes!("../samples/jpeg_exif_gps.jpg"),
        format: FileFormat::Jpeg,
        unified: Unified {
            width: Some(64),
            height: Some(48),
            orientation: Some(omni_meta::Orientation::Rotate90),
            camera_make: Some("OmniTest".into()),
            camera_model: Some("GoldenCam".into()),
            created: Some(DateTimeParts {
                year: 2020, month: 1, day: 2, hour: 3, minute: 4, second: 5,
                tz_offset_min: None, // EXIF DateTimeOriginal 无 OffsetTime → 不臆造时区
            }),
            gps: Some(Gps { lat_e7: 355_000_000, lon_e7: 1_395_000_000, alt_mm: None }),
            ..Default::default()
        },
        raw_subset: vec![
            GoldenRawTag::Exif { ifd: IfdKind::Primary, tag: 0x010F, value: Value::Text("OmniTest".into()) },
            GoldenRawTag::Exif { ifd: IfdKind::Primary, tag: 0x0110, value: Value::Text("GoldenCam".into()) },
        ],
    }
}

fn png_exif() -> GoldenSample {
    GoldenSample {
        name: "png_exif",
        bytes: include_bytes!("../samples/png_exif.png"),
        format: FileFormat::Png,
        unified: Unified { width: Some(80), height: Some(60), camera_make: Some("OmniTest".into()), ..Default::default() },
        raw_subset: vec![
            GoldenRawTag::Exif { ifd: IfdKind::Primary, tag: 0x010F, value: Value::Text("OmniTest".into()) },
            GoldenRawTag::Xmp { prefix: "dc", name: "creator", value: "GoldenAuthor" },
        ],
    }
}

fn gif_xmp() -> GoldenSample {
    GoldenSample {
        name: "gif_xmp",
        bytes: include_bytes!("../samples/gif_xmp.gif"),
        format: FileFormat::Gif,
        unified: Unified { width: Some(48), height: Some(32), ..Default::default() },
        raw_subset: vec![
            GoldenRawTag::Xmp { prefix: "dc", name: "creator", value: "GoldenAuthor" },
        ],
    }
}

fn webp_exif() -> GoldenSample {
    GoldenSample {
        name: "webp_exif",
        bytes: include_bytes!("../samples/webp_exif.webp"),
        format: FileFormat::Webp,
        unified: Unified { width: Some(72), height: Some(54), camera_make: Some("OmniTest".into()), ..Default::default() },
        raw_subset: vec![
            GoldenRawTag::Exif { ifd: IfdKind::Primary, tag: 0x010F, value: Value::Text("OmniTest".into()) },
        ],
    }
}

/// 全部黄金样本。视频样本在 Task 6 追加。
pub fn golden_corpus() -> Vec<GoldenSample> {
    vec![jpeg_exif_gps(), png_exif(), gif_xmp(), webp_exif()]
}
```

> 注：`raw_subset` 的 XMP 期望 `prefix`/`name` 须匹配 omni-meta 解析出的实际 `XmpProperty.prefix`/`.name`（dc:creator 常解析为 prefix=`dc` name=`creator`；Task 5 Step 5 若不符，按实际读数改）。`Orientation` 在样本里用全限定 `omni_meta::Orientation::Rotate90`，无需导入。

- [ ] **Step 2: 挂模块并重导出**

修改 `omni-meta-fixtures/src/lib.rs`，在文件顶部 `use` 区之后加：

```rust
mod golden;
pub use golden::{golden_corpus, GoldenRawTag, GoldenSample};
```

- [ ] **Step 3: 写 golden.rs 集成测试**

创建 `omni-meta/tests/golden.rs`：

```rust
//! 黄金样本测试：真实文件 → 四适配器一致 + Unified 子集锚定 + raw 子集锚定（exiftool 真相）。

use omni_meta::{read_slice, Options, RawTags, Unified};
use omni_meta_fixtures::{assert_all_equal, golden_corpus, GoldenRawTag};

/// Unified 子集断言：仅校验 expected 中为 Some 的字段，其余不约束。
fn assert_unified_subset(name: &str, exp: &Unified, got: &Unified) {
    macro_rules! chk {
        ($f:ident) => {
            if let Some(ref e) = exp.$f {
                assert_eq!(got.$f.as_ref(), Some(e), "[{name}] unified.{} 不符", stringify!($f));
            }
        };
    }
    chk!(width); chk!(height); chk!(orientation); chk!(camera_make);
    chk!(camera_model); chk!(duration_ms); chk!(created); chk!(gps);
    chk!(software); chk!(creator);
}

/// raw 子集断言：每个期望标签须在 raw 中存在且值相等。
fn assert_raw_subset(name: &str, exp: &[GoldenRawTag], raw: &RawTags) {
    for t in exp {
        match t {
            GoldenRawTag::Exif { ifd, tag, value } => {
                let hit = raw.exif.iter().any(|e| e.ifd == *ifd && e.tag == *tag && &e.value == value);
                assert!(hit, "[{name}] 缺 EXIF 标签 ifd={ifd:?} tag={tag:#06x} value={value:?}\n实际 exif={:?}", raw.exif);
            }
            GoldenRawTag::Xmp { prefix, name: pname, value } => {
                let hit = raw.xmp.iter().any(|p| p.prefix == *prefix && p.name == *pname && p.value == *value);
                assert!(hit, "[{name}] 缺 XMP {prefix}:{pname}={value}\n实际 xmp={:?}", raw.xmp);
            }
            GoldenRawTag::Container { source, key, value } => {
                let hit = raw.container.iter().any(|c| c.source == *source && c.key == *key && &c.value == value);
                assert!(hit, "[{name}] 缺容器标签 {source:?} {key}={value:?}\n实际 container={:?}", raw.container);
            }
        }
    }
}

#[test]
fn golden_samples_anchor_to_exiftool_truth() {
    for s in golden_corpus() {
        // ① 四适配器对真实字节逐字段一致（顺带纳入差分语料）。
        assert_all_equal(s.bytes);
        // ② 解析并锚定到 exiftool 真相。
        let m = read_slice(s.bytes, Options::default())
            .unwrap_or_else(|e| panic!("[{}] read_slice 失败: {e:?}", s.name));
        assert_eq!(m.format, s.format, "[{}] format 不符", s.name);
        assert_unified_subset(s.name, &s.unified, &m.unified);
        assert_raw_subset(s.name, &s.raw_subset, &m.raw);
    }
}
```

- [ ] **Step 4: 跑测试**

Run: `cargo test -p omni-meta --test golden 2>&1 | tail -40`
Expected: PASS。

- [ ] **Step 5: 处理失败 = 抓 bug，不擅改期望**

若断言失败，按三类分流：
- **同源偏差性失败**（omni-meta 解出的值与 exiftool 注入值**矛盾**）= **真实缺陷**，停下报告，**不得**把期望改成 omni-meta 的输出来变绿。
- **表示性差异**（如 XMP prefix/name 大小写、GPS 方向号符号、orientation 枚举映射、EXIF tag 号写错）：用 exiftool 复核哪个对。若是**期望常量写法**写错而 omni-meta 与 exiftool 一致，则修期望写法。
- **设计性未投影**（某字段 exiftool 有值，但 omni-meta **按设计不投影**——如 Unified 字段需 ≥2 来源、或该格式不走某来源）：这既非缺陷也非写错。**放松该字段 pin 为 None（不约束）**，并在 README「已知缺口/说明」记一行原因。判断依据：查 normalize/格式解析是否本就不产出该字段。
- 把每次判定与处置写进 commit message / 给用户的小结。

- [ ] **Step 6: 提交**

```bash
git add omni-meta-fixtures/src/golden.rs omni-meta-fixtures/src/lib.rs omni-meta/tests/golden.rs
git commit -m "test(golden): harness + 图片样本断言（四适配器一致 + Unified/raw 子集锚定 exiftool）"
```

---

## Task 6: 视频样本断言

把 MP4/MOV/MKV/WebM 加入 `golden_corpus()`。duration_ms/created **仅在 Task 4 Step 3 确认 exiftool 报精确值时固定**，否则留 None 不约束。

**Files:**
- Modify: `omni-meta-fixtures/src/golden.rs`

- [ ] **Step 1: 加视频样本构造函数**

在 `golden.rs` 的 `golden_corpus` 之前加（**duration_ms/created 按 Task 4 Step 3 实测决定是否填**；下方按精确 2000ms 预填）：

```rust
fn mp4() -> GoldenSample {
    GoldenSample {
        name: "mp4",
        bytes: include_bytes!("../samples/mp4.mp4"),
        format: FileFormat::Mp4,
        unified: Unified {
            width: Some(64), height: Some(48),
            duration_ms: Some(2000), // 若 Task4 实测非精确 2000 → 改 None
            created: Some(DateTimeParts { year: 2020, month: 1, day: 2, hour: 3, minute: 4, second: 5, tz_offset_min: Some(0) }),
            ..Default::default()
        },
        raw_subset: vec![],
    }
}

fn mov() -> GoldenSample {
    GoldenSample {
        name: "mov",
        bytes: include_bytes!("../samples/mov.mov"),
        format: FileFormat::Mov,
        unified: Unified {
            width: Some(64), height: Some(48),
            duration_ms: Some(2000),
            created: Some(DateTimeParts { year: 2020, month: 1, day: 2, hour: 3, minute: 4, second: 5, tz_offset_min: Some(0) }),
            ..Default::default()
        },
        raw_subset: vec![],
    }
}

fn mkv() -> GoldenSample {
    GoldenSample {
        name: "mkv",
        bytes: include_bytes!("../samples/mkv.mkv"),
        format: FileFormat::Mkv,
        unified: Unified { width: Some(64), height: Some(48), duration_ms: Some(2000), ..Default::default() },
        raw_subset: vec![],
    }
}

fn webm() -> GoldenSample {
    GoldenSample {
        name: "webm",
        bytes: include_bytes!("../samples/webm.webm"),
        format: FileFormat::Webm,
        unified: Unified { width: Some(64), height: Some(48), duration_ms: Some(2000), ..Default::default() },
        raw_subset: vec![],
    }
}
```

- [ ] **Step 2: 扩 golden_corpus**

把 `golden_corpus` 的返回改为：

```rust
pub fn golden_corpus() -> Vec<GoldenSample> {
    vec![
        jpeg_exif_gps(), png_exif(), gif_xmp(), webp_exif(),
        mp4(), mov(), mkv(), webm(),
    ]
}
```

- [ ] **Step 3: 跑测试**

Run: `cargo test -p omni-meta --test golden 2>&1 | tail -40`
Expected: PASS。
> duration_ms/created 失败且根因是容器头非精确值 → 把该样本对应字段改 None（不约束）并在 README 记原因；**非** omni-meta 缺陷时不上报为 bug。若 omni-meta 算的 ms 与 exiftool 的 Duration 矛盾，则为缺陷、上报。

- [ ] **Step 4: 提交**

```bash
git add omni-meta-fixtures/src/golden.rs
git commit -m "test(golden): 视频样本断言（MP4/MOV/MKV/WebM 尺寸+时长+创建时间锚定）"
```

---

## Task 7: 样本 provenance README

**Files:**
- Create: `omni-meta-fixtures/samples/README.md`

- [ ] **Step 1: 写 README**

创建 `omni-meta-fixtures/samples/README.md`，逐文件登记。模板（**期望值列按 Task 3/4 的 exiftool 实测填**）：

```markdown
# 黄金样本

真实小文件，用于以 **exiftool 独立核对**的期望值锚定 omni-meta，打破合成 fixture 的同源偏差。
全部由 `regen.sh` 用 ffmpeg + exiftool 自生成 → 无第三方版权。CI **不**调用本目录脚本；
测试经 `include_bytes!` 读取已入库的二进制，零外部工具依赖。

## 重生

    bash regen.sh    # 需 ffmpeg + exiftool

## 样本与 exiftool 核对的期望值

| 文件 | 格式 | 尺寸 | 关键期望（exiftool 核对） |
|---|---|---|---|
| jpeg_exif_gps.jpg | JPEG | 64×48 | Make=OmniTest, Model=GoldenCam, Orientation=Rotate90, DateTimeOriginal=2020:01:02 03:04:05, GPS=35.5N/139.5E |
| png_exif.png | PNG | 80×60 | Make=OmniTest, dc:creator=GoldenAuthor |
| gif_xmp.gif | GIF | 48×32 | dc:creator=GoldenAuthor |
| webp_exif.webp | WebP | 72×54 | Make=OmniTest |
| mp4.mp4 | MP4 | 64×48 | duration≈2000ms, created=2020-01-02T03:04:05Z |
| mov.mov | MOV | 64×48 | duration≈2000ms, created=2020-01-02T03:04:05Z |
| mkv.mkv | Matroska | 64×48 | duration≈2000ms |
| webm.webm | WebM | 64×48 | duration≈2000ms |

## 已知缺口

- HEIC/AVIF：本机 ffmpeg <填实际情况：是否有 libx265/heif 复用器>。<未生成则说明；现有合成 fixture `fixture_bmff_heic` 兜底。>
- <若某视频 duration/created 未达精确值而未在测试中固定，在此记录原因。>
- <若 WebP 因 libwebp 缺失未生成，在此记录。>
```

> 把所有 `<...>` 占位换成 Task 3/4 的实际结果——README 不得留尖括号占位。

- [ ] **Step 2: 提交**

```bash
git add omni-meta-fixtures/samples/README.md
git commit -m "docs(golden): 样本 provenance + exiftool 核对期望 + 已知缺口登记"
```

---

## Task 8: ROADMAP 勾选 + 终验

**Files:**
- Modify: `docs/ROADMAP.md`

- [ ] **Step 1: 勾选两项横切待办**

在 `docs/ROADMAP.md` §4 横切待办：
- 把 `- [ ] **no_std CI**：每个里程碑验证 `--no-default-features`` 改为
  `- [x] **no_std CI**：GitHub Actions 裸机 target（thumbv7em-none-eabi）构建 core+facade；全套门禁（fmt/clippy/test/no_std/fuzz-build）。见 `.github/workflows/ci.yml`。`
- 把 `- [ ] **黄金样本**：真实小样本 + 期望 `Metadata` 快照` 改为
  `- [x] **黄金样本**：真实小样本（ffmpeg 生成）+ exiftool 独立核对的期望（Unified 子集 + raw 标签子集，破同源偏差）。见 `omni-meta-fixtures/samples/`、`omni-meta/tests/golden.rs`。`

并更新文档首行「最近更新」日期为 2026-06-17。

- [ ] **Step 2: 全套终验（对齐 CI 三 job）**

Run:
```bash
cargo fmt --all --check && \
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3 && \
cargo test --all 2>&1 | tail -10 && \
cargo build -p omni-meta-core --no-default-features --target thumbv7em-none-eabi 2>&1 | tail -2 && \
cargo build -p omni-meta --no-default-features --target thumbv7em-none-eabi 2>&1 | tail -2
```
Expected: fmt 无输出；clippy 退出 0 无 warning；test 全 pass（含 golden）；两条裸机构建 `Finished`。

- [ ] **Step 3: 提交**

```bash
git add docs/ROADMAP.md
git commit -m "docs(roadmap): 勾选 no_std CI + 黄金样本（testing-hardening 完成）"
```

---

## 验收对照（实现完成后逐条核对 spec §6）

- [ ] `.github/workflows/ci.yml` 三 job 存在且本机对应命令全绿。
- [ ] 裸机 `thumbv7em-none-eabi` 构建 core + facade 通过。
- [ ] `samples/` 含各家族真实样本 + `regen.sh` + `README.md`（provenance + exiftool 期望 + 缺口）。
- [ ] `golden_corpus()` 暴露样本；`golden.rs` 跑四适配器一致 + Unified 子集 + raw 子集锚定，全绿。
- [ ] 每样本 `raw_subset` ≥1 个 exiftool 核对过的关键标签（图片含 EXIF/XMP；GPS 样本另由 Unified.gps 锚定）。
- [ ] HEIC/AVIF 若未纳入，README 记缺口。
- [ ] ROADMAP §4 两项已勾选。
- [ ] diff 仅涉 fixtures/tests/CI/docs + 一处 API 重导出；未改任何解析器/格式/codec 逻辑。
```
