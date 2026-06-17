# cargo-fuzz 鲁棒性 harness 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 omni-meta 的 7 格式 / 2 容器 / 2 codec 建立 cargo-fuzz harness，把「永不 panic / 不超 Limits / 不死循环 / 四适配器一致」从人工断言变成模糊器可证伪的运行时性质。

**Architecture:** 方案 C 混合——(1) 公共 API `differential` target 复用四适配器一致性 oracle；(2) `read_slice_bounded` target + 计数全局分配器把 Limits 变 tripwire；(3) `omni-meta-core` 的 `__fuzzing` 特性暴露四个薄包装，给两容器两 codec 深度 target。fixtures 抽到共享 crate `omni-meta-fixtures`，被差分测试与 fuzz 种子生成器共用。

**Tech Stack:** Rust（edition 2024，workspace）、`cargo-fuzz 0.13.1` + `libfuzzer-sys`（nightly）、`std::alloc::GlobalAlloc`。

**关联** 设计 `docs/superpowers/specs/2026-06-17-cargo-fuzz-design.md`；ROADMAP §4 fuzz checkbox、§5 不变量。分支 `fuzz-harness`（已建）。

---

## 关键事实（实现时依赖的已核对接口）

- 公共入口（`omni_meta::`）：`read_slice(&[u8], Options) -> Result<Metadata, Error>`、`read_blocking<R: Read>(R, Options)`、`read_seek<R: Read+Seek>(R, Options)`、`PushParser::new(Options)` + `.feed(&[u8]) -> Result<Outcome, Error>` + `.finish()`。`Options { limits: Limits }`（`Default`）。`Metadata: PartialEq`。
- `Limits` 字段全 `pub`，可 const 构造：`max_payload_bytes/max_retained_bytes: usize`、`max_depth: u16`、`max_tags/max_ifds/max_total_alloc: usize`。
- codec 入口（`pub fn`，crate 内可达）：`codecs::exif::decode(tiff: &[u8], out: &mut Vec<ExifTag>, warnings: &mut Vec<Warning>, limits: &Limits)`；`codecs::xmp::decode(packet: &[u8], out: &mut Vec<XmpProperty>, warnings: &mut Vec<Warning>, limits: &Limits)`。
- 驱动：`driver::drive_slice(buf: &[u8], parser: &mut dyn MetaParser, limits: Limits) -> Collector`（`pub`）；`driver::finalize(col: Collector, format: FileFormat) -> Metadata`（`pub(crate)`，crate 内可达）。
- 容器解析器构造：`formats::bmff::BmffParser::with_limits(limits)`、`formats::ebml::EbmlParser::new()`（均 `pub(crate)`，crate 内可达）。
- `RawTags { exif: Vec<ExifTag>, xmp: Vec<XmpProperty>, container: Vec<ContainerTag> }`；驱动里 `container.len()` 被显式封顶在 `limits.max_tags`。
- `差分测试` 现有断言逻辑：`omni-meta/tests/differential.rs` 的 `assert_all_equal`（四适配器全 Ok 且 `Metadata` 相等，或全 Err）。

---

## 文件结构

```
omni-meta/                       # workspace 根
├── Cargo.toml                   # 修改：members 增 omni-meta-fixtures；exclude=["fuzz"]
├── omni-meta-core/
│   ├── Cargo.toml               # 修改：新增 __fuzzing 特性
│   └── src/lib.rs               # 修改：新增 #[cfg(feature="__fuzzing")] pub mod __fuzzing
├── omni-meta/
│   ├── Cargo.toml               # 修改：dev-dependencies 增 omni-meta-fixtures
│   └── tests/differential.rs    # 修改：删除 builder/oracle，改 use omni_meta_fixtures::*
├── omni-meta-fixtures/          # 【新】builder + oracle 共享 crate（std，test/fuzz 专用）
│   ├── Cargo.toml
│   └── src/lib.rs
└── fuzz/                        # 【新】独立 workspace（empty [workspace]），nightly-only
    ├── Cargo.toml
    ├── .gitignore
    ├── README.md
    ├── src/lib.rs               # AllocCounter + FuzzAlloc(GlobalAlloc) + FUZZ_LIMITS
    ├── src/bin/seeds.rs         # 种子语料生成器
    └── fuzz_targets/
        ├── differential.rs
        ├── read_slice_bounded.rs
        ├── isobmff.rs
        ├── ebml.rs
        ├── exif.rs
        └── xmp.rs
```

---

## Task 1: 建立 `omni-meta-fixtures` crate 骨架

**Files:**
- Create: `omni-meta-fixtures/Cargo.toml`
- Create: `omni-meta-fixtures/src/lib.rs`
- Modify: `Cargo.toml`（workspace 根）

- [ ] **Step 1: 写 fixtures crate 的 Cargo.toml**

Create `omni-meta-fixtures/Cargo.toml`:

```toml
[package]
name = "omni-meta-fixtures"
version = "0.1.0"
edition = "2024"

[lib]
name = "omni_meta_fixtures"
path = "src/lib.rs"

# oracle 需要四适配器入口（含 std read_blocking/read_seek）。此 crate 仅供 test/fuzz 使用。
[dependencies]
omni-meta = { path = "../omni-meta" }
```

- [ ] **Step 2: 写最小 lib.rs（占位，后续 Task 2 填充）**

Create `omni-meta-fixtures/src/lib.rs`:

```rust
//! omni-meta 测试/模糊共享 fixtures：纯字节构造器 + 四适配器一致性 oracle。
//! 差分集成测试与 fuzz 种子生成器共用，单一真相源（DRY）。

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
```

- [ ] **Step 3: 把 fixtures 加入 workspace members，并排除 fuzz**

Modify `Cargo.toml`（根），替换 `[workspace]` 段为：

```toml
[workspace]
resolver = "2"
members = ["omni-meta-core", "omni-meta", "omni-meta-fixtures"]
exclude = ["fuzz"]
```

- [ ] **Step 4: 验证编译**

Run: `cargo build -p omni-meta-fixtures`
Expected: 编译通过（生成 `omni_meta_fixtures` rlib）。

- [ ] **Step 5: 提交**

```bash
git add Cargo.toml omni-meta-fixtures/
git commit -m "chore(fixtures): 新增 omni-meta-fixtures crate 骨架（builder/oracle 共享源）"
```

---

## Task 2: 迁移 builder + oracle 到 fixtures，重接差分测试

把 `omni-meta/tests/differential.rs` 内的纯字节 builder 与四适配器 oracle 搬到 fixtures crate（单一真相源），并新增可返回结果的 oracle API（供自测「一致/全错」两支）与按类别的种子语料函数。差分测试改为消费 fixtures，行为不变——既有 `#[test]` 全绿即回归保证。

**Files:**
- Modify: `omni-meta-fixtures/src/lib.rs`
- Modify: `omni-meta/Cargo.toml`
- Modify: `omni-meta/tests/differential.rs`

- [ ] **Step 1: 把全部纯 builder 函数逐字移入 fixtures 并改 `pub`**

从 `omni-meta/tests/differential.rs` 把以下**返回 `Vec<u8>` 的自由函数**逐字剪切到 `omni-meta-fixtures/src/lib.rs`，给每个加 `pub`，函数体一字不改（它们只用 `Vec`/`extend_from_slice`/字节字面量，无 `omni_meta`/`Cursor` 依赖）。按当前行号定位：

```
make_tiff(@6) wrap_jpeg(@25) fixture_plain(@46) fixture_large_nonmeta(@51)
fixture_huge_nonmeta(@62) fixture_truncated(@71) fixture_with_sof(@141)
png_chunk(@167) fixture_png(@176) riff_chunk(@207) fixture_webp(@218)
fixture_webp_vp8l(@247) fixture_gif(@275) fixture_png_compressed_itxt(@302)
make_tiff_subifd(@330) fixture_exif_subifd(@355) make_tiff_gps_list(@375)
make_tiff_thumbnail(@400) wrap_jpeg_tiff(@421) bmff_box(@445) bmff_infe(@453)
bmff_ispe(@466) bmff_meta(@473) fixture_bmff_heic(@518) mp4_mvhd_v0(@552)
mp4_tkhd_v0(@561) fixture_bmff_mp4(@576) fixture_bmff_mp4_moov_after_mdat(@595)
mp4_mvhd_v1(@626) fixture_bmff_mp4_v1(@635) make_tiff_datetime_original(@659)
ebml_elem(@691) ebml_video_track(@701) ebml_info(@709) ebml_header(@717)
fixture_ebml(@724) fixture_ebml_unknown_size_segment(@740)
qt_meta_with_keys_local(@773) build_mov_with_gps_and_mdta(@815)
build_jpeg_with_gps_ifd(@855) qt_meta_typed(@950) fixture_bmff_mp4_container_tags(@985)
```

在 `omni-meta-fixtures/src/lib.rs` 顶部加（builder 用到的导入）：

```rust
use std::vec::Vec;
```

> 注：被其它 builder 调用的辅助函数（`wrap_jpeg`/`png_chunk`/`riff_chunk`/`bmff_box`/`ebml_elem`/`make_tiff*` 等）一并改 `pub`，差分测试 glob 导入后调用关系不变。函数之间互调无需改 `crate::` 前缀（同模块）。

- [ ] **Step 2: 在 fixtures 中加入可结果化的 oracle + 现有断言包装**

把 `push_drive(@80)` 与 `assert_all_equal(@93)` 从 differential.rs 移到 fixtures，并重构 `assert_all_equal` 委托给新增的 `adapters_outcome`（返回结果而非直接 assert，便于自测）。在 `omni-meta-fixtures/src/lib.rs` 追加：

```rust
use omni_meta::{read_blocking, read_seek, read_slice, Error, Metadata, Options, Outcome, PushParser};
use std::io::Cursor;

/// 四适配器对同一输入的裁决。
#[derive(Debug)]
pub enum Agreement {
    /// 全部 Ok 且 Metadata 逐字段相等。
    Agree(Metadata),
    /// 全部 Err（格式不可识别等）。
    AllErr,
    /// 适配器间出现分歧——附人类可读原因（违反核心契约）。
    Disagree(String),
}

/// 把 bytes 喂给 push 适配器（分块 chunk），返回最终结果。
pub fn push_drive(bytes: &[u8], opts: Options, chunk: usize) -> Result<Metadata, Error> {
    let mut p = PushParser::new(opts);
    let mut i = 0;
    while i < bytes.len() {
        let end = (i + chunk).min(bytes.len());
        if let Outcome::Done = p.feed(&bytes[i..end])? {
            return p.finish();
        }
        i = end;
    }
    p.finish()
}

/// 跑全部四适配器（push 用多种分块），判定一致性。永不 panic——分歧以 Disagree 返回。
pub fn adapters_outcome(bytes: &[u8]) -> Agreement {
    let slice = read_slice(bytes, Options::default());
    let blocking = read_blocking(bytes, Options::default());
    let seek = read_seek(Cursor::new(bytes), Options::default());
    match &slice {
        Ok(w) => {
            if blocking.as_ref() != Ok(w) {
                return Agreement::Disagree(format!("blocking vs slice: {blocking:?}"));
            }
            if seek.as_ref() != Ok(w) {
                return Agreement::Disagree(format!("seek vs slice: {seek:?}"));
            }
            for chunk in [1usize, 3, 7, bytes.len().max(1)] {
                match push_drive(bytes, Options::default(), chunk) {
                    Ok(got) if &got == w => {}
                    other => {
                        return Agreement::Disagree(format!("push chunk={chunk}: {other:?}"));
                    }
                }
            }
            Agreement::Agree(w.clone())
        }
        Err(_) => {
            if blocking.is_err()
                && seek.is_err()
                && push_drive(bytes, Options::default(), 1).is_err()
            {
                Agreement::AllErr
            } else {
                Agreement::Disagree(format!(
                    "slice Err 但他者非全 Err: blocking={blocking:?} seek={seek:?}"
                ))
            }
        }
    }
}

/// 现有差分测试的断言入口：分歧即 panic（保持原行为）。
pub fn assert_all_equal(bytes: &[u8]) {
    if let Agreement::Disagree(why) = adapters_outcome(bytes) {
        panic!("adapter disagreement: {why}");
    }
}
```

- [ ] **Step 3: 加入按类别种子语料函数**

在 `omni-meta-fixtures/src/lib.rs` 追加（供 fuzz seeds 生成器分发到各 target 的 corpus 目录）：

```rust
/// 完整文件级种子：喂 differential / read_slice_bounded / probe 全链路。
pub fn file_corpus() -> Vec<(&'static str, Vec<u8>)> {
    let mut v = Vec::new();
    v.push(("jpeg_plain", fixture_plain()));
    v.push(("jpeg_sof", fixture_with_sof()));
    v.push(("jpeg_truncated", fixture_truncated()));
    v.push(("jpeg_gps_ifd", build_jpeg_with_gps_ifd()));
    v.push(("png", fixture_png()));
    v.push(("png_itxt", fixture_png_compressed_itxt()));
    v.push(("webp", fixture_webp()));
    v.push(("webp_vp8l", fixture_webp_vp8l()));
    v.push(("gif", fixture_gif()));
    for (n, b) in bmff_corpus() {
        v.push((n, b));
    }
    for (n, b) in ebml_corpus() {
        v.push((n, b));
    }
    v
}

/// 容器（BMFF）种子。
pub fn bmff_corpus() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("heic", fixture_bmff_heic()),
        ("mp4", fixture_bmff_mp4()),
        ("mp4_v1", fixture_bmff_mp4_v1()),
        ("mp4_moov_after_mdat", fixture_bmff_mp4_moov_after_mdat()),
        ("mp4_container_tags", fixture_bmff_mp4_container_tags()),
        ("mov_gps_mdta", build_mov_with_gps_and_mdta()),
    ]
}

/// 容器（EBML）种子。
pub fn ebml_corpus() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("webm", fixture_ebml(b"webm")),
        ("matroska", fixture_ebml(b"matroska")),
        ("unknown_size_segment", fixture_ebml_unknown_size_segment()),
    ]
}

/// EXIF codec 种子（裸 TIFF 字节，即 "Exif\0\0" 之后的内容）。
pub fn tiff_corpus() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("tiff_basic", make_tiff()),
        ("tiff_subifd", make_tiff_subifd()),
        ("tiff_gps_list", make_tiff_gps_list()),
        ("tiff_thumbnail", make_tiff_thumbnail()),
        ("tiff_datetime_original", make_tiff_datetime_original()),
    ]
}

/// XMP codec 种子（RDF/XML 包字节）。
pub fn xmp_corpus() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "xmp_attr",
            br#"<?xpacket?><rdf:Description xmlns:tiff="ns" tiff:Make="Acme" tiff:Model="X1"/>"#
                .to_vec(),
        ),
        (
            "xmp_elem",
            br#"<rdf:Description><dc:creator><rdf:Seq><rdf:li>Jane</rdf:li></rdf:Seq></dc:creator></rdf:Description>"#
                .to_vec(),
        ),
    ]
}
```

- [ ] **Step 4: 重接 differential.rs**

修改 `omni-meta/tests/differential.rs`：删除已迁移的全部 builder 函数、`push_drive`、`assert_all_equal`（Step 1/2 列出的）。把文件顶部的

```rust
use omni_meta::{read_blocking, read_seek, read_slice, Metadata, Options, Outcome, PushParser};
use std::io::Cursor;
```

替换为：

```rust
use omni_meta_fixtures::*;
```

保留所有 `#[test] fn differential_*` 与 `gps_*` / `mov_container_*` 测试函数本身（它们调用的 builder/`assert_all_equal` 现由 glob 导入提供）。`read_slice` 仍被少数测试直接调用（如 `gps_mov_mdta_consistent_across_adapters`）——这些测试改用 `omni_meta_fixtures` 重导出，或在文件内显式 `use omni_meta::{read_slice, Options};`。在文件顶部 `use omni_meta_fixtures::*;` 下补一行：

```rust
use omni_meta::{read_slice, Options};
```

- [ ] **Step 5: fixtures 加入 dev-dependency**

Modify `omni-meta/Cargo.toml`，在末尾追加：

```toml
[dev-dependencies]
omni-meta-fixtures = { path = "../omni-meta-fixtures" }
```

> 注：这形成 `omni-meta --(dev)--> omni-meta-fixtures --> omni-meta` 的依赖环。Cargo 允许经 dev-dependency 的环，正常 `cargo build` 图无环，仅 test 图成环——合法。

- [ ] **Step 6: 跑差分测试确认行为不变**

Run: `cargo test -p omni-meta --test differential`
Expected: 全部既有差分用例 PASS（数量与迁移前一致，0 失败）。

- [ ] **Step 7: 跑全量测试确保无回归**

Run: `cargo test`
Expected: 全 workspace 测试 PASS。

- [ ] **Step 8: 提交**

```bash
git add omni-meta-fixtures/src/lib.rs omni-meta/Cargo.toml omni-meta/tests/differential.rs
git commit -m "refactor(fixtures): builder+oracle 迁入 omni-meta-fixtures，差分测试消费共享源"
```

---

## Task 3: `omni-meta-core` 新增 `__fuzzing` 特性与四包装入口

**Files:**
- Modify: `omni-meta-core/Cargo.toml`
- Modify: `omni-meta-core/src/lib.rs`

- [ ] **Step 1: 声明 `__fuzzing` 特性**

Modify `omni-meta-core/Cargo.toml` 的 `[features]` 段为：

```toml
[features]
default = ["std"]
std = []
# 内部模糊专用：暴露解析器入口的薄包装。非 semver 稳定面，不进 default，不进文档。
__fuzzing = []
```

- [ ] **Step 2: 写失败的特性门控单测**

在 `omni-meta-core/src/lib.rs` 末尾、`smoke` 模块之后追加：

```rust
#[cfg(all(test, feature = "__fuzzing"))]
mod fuzzing_api_tests {
    use crate::limits::Limits;

    #[test]
    fn decode_exif_wrapper_runs_and_is_bounded() {
        // 裸 TIFF：II + 42 + IFD0@8，count=1，一条 Make(0x010F) ASCII="A\0"
        let tiff: &[u8] = &[
            b'I', b'I', 42, 0, 8, 0, 0, 0, // header
            1, 0, // IFD0 count=1
            0x0F, 0x01, 2, 0, 2, 0, 0, 0, b'A', 0, 0, 0, // Make ASCII cnt=2 inline "A\0"
            0, 0, 0, 0, // next IFD = 0
        ];
        let (tags, warns) = crate::__fuzzing::decode_exif(tiff, &Limits::default());
        assert!(tags.len() <= Limits::default().max_tags);
        let _ = warns;
    }

    #[test]
    fn decode_xmp_wrapper_runs() {
        let (props, _w) = crate::__fuzzing::decode_xmp(
            br#"<rdf:Description xmlns:tiff="n" tiff:Make="Acme"/>"#,
            &Limits::default(),
        );
        assert!(props.iter().any(|p| p.name == "Make"));
    }

    #[test]
    fn drive_bmff_wrapper_runs_on_garbage_without_panic() {
        let m = crate::__fuzzing::drive_bmff(&[0u8; 32], Limits::default());
        assert!(m.raw.container.len() <= Limits::default().max_tags);
    }

    #[test]
    fn drive_ebml_wrapper_runs_on_garbage_without_panic() {
        let _ = crate::__fuzzing::drive_ebml(&[0u8; 32], Limits::default());
    }
}
```

- [ ] **Step 3: 跑测试确认失败（模块未定义）**

Run: `cargo test -p omni-meta-core --features __fuzzing fuzzing_api_tests`
Expected: 编译失败 —— `crate::__fuzzing` 未定义。

- [ ] **Step 4: 实现 `__fuzzing` 模块**

在 `omni-meta-core/src/lib.rs` 的 `pub use ...` 块之后、`#[cfg(test)] mod smoke` 之前插入：

```rust
/// 模糊专用入口（薄包装）。仅在 `__fuzzing` 特性下编译；`#[doc(hidden)]` 且
/// 双下划线命名，明确「内部、非 semver 稳定」。绕过 probe，强制走指定解析路径。
#[cfg(feature = "__fuzzing")]
#[doc(hidden)]
pub mod __fuzzing {
    use alloc::vec::Vec;

    use crate::limits::Limits;
    use crate::model::{ExifTag, FileFormat, Metadata, Warning, XmpProperty};

    /// 直接在裸 TIFF 字节上跑 EXIF codec。
    pub fn decode_exif(tiff: &[u8], limits: &Limits) -> (Vec<ExifTag>, Vec<Warning>) {
        let mut out = Vec::new();
        let mut warnings = Vec::new();
        crate::codecs::exif::decode(tiff, &mut out, &mut warnings, limits);
        (out, warnings)
    }

    /// 直接在 XMP 包字节上跑 XMP codec。
    pub fn decode_xmp(packet: &[u8], limits: &Limits) -> (Vec<XmpProperty>, Vec<Warning>) {
        let mut out = Vec::new();
        let mut warnings = Vec::new();
        crate::codecs::xmp::decode(packet, &mut out, &mut warnings, limits);
        (out, warnings)
    }

    /// 强制以 BMFF 解析器在 slice 上驱动到底，投影为 Metadata。
    pub fn drive_bmff(data: &[u8], limits: Limits) -> Metadata {
        let mut parser = crate::formats::bmff::BmffParser::with_limits(limits);
        let col = crate::driver::drive_slice(data, &mut parser, limits);
        crate::driver::finalize(col, FileFormat::Mp4)
    }

    /// 强制以 EBML 解析器在 slice 上驱动到底，投影为 Metadata。
    pub fn drive_ebml(data: &[u8], limits: Limits) -> Metadata {
        let mut parser = crate::formats::ebml::EbmlParser::new();
        let col = crate::driver::drive_slice(data, &mut parser, limits);
        crate::driver::finalize(col, FileFormat::Mkv)
    }
}
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p omni-meta-core --features __fuzzing fuzzing_api_tests`
Expected: 4 测试 PASS。

- [ ] **Step 6: 确认默认/no_std 构建不受影响**

Run: `cargo build -p omni-meta-core && cargo build -p omni-meta-core --no-default-features && cargo build -p omni-meta --no-default-features`
Expected: 三条均成功（`__fuzzing` 关闭时公共 API 面不变）。

- [ ] **Step 7: 提交**

```bash
git add omni-meta-core/Cargo.toml omni-meta-core/src/lib.rs
git commit -m "feat(core): __fuzzing 特性暴露 exif/xmp/bmff/ebml 解析入口薄包装"
```

---

## Task 4: 建立 `fuzz/` 独立 workspace + 计数分配器

**Files:**
- Create: `fuzz/Cargo.toml`
- Create: `fuzz/.gitignore`
- Create: `fuzz/src/lib.rs`

- [ ] **Step 1: 写 fuzz crate 的 Cargo.toml（独立 workspace）**

Create `fuzz/Cargo.toml`:

```toml
[package]
name = "omni-meta-fuzz"
version = "0.0.0"
edition = "2024"
publish = false

[package.metadata]
cargo-fuzz = true

# 空表 → fuzz 自成 workspace 根，与主 stable/no_std workspace 隔离。
[workspace]

[dependencies]
libfuzzer-sys = "0.4"
omni-meta = { path = "../omni-meta", features = ["std", "__fuzzing"] }
omni-meta-fixtures = { path = "../omni-meta-fixtures" }

[[bin]]
name = "differential"
path = "fuzz_targets/differential.rs"
test = false
doc = false
bench = false

[[bin]]
name = "read_slice_bounded"
path = "fuzz_targets/read_slice_bounded.rs"
test = false
doc = false
bench = false

[[bin]]
name = "isobmff"
path = "fuzz_targets/isobmff.rs"
test = false
doc = false
bench = false

[[bin]]
name = "ebml"
path = "fuzz_targets/ebml.rs"
test = false
doc = false
bench = false

[[bin]]
name = "exif"
path = "fuzz_targets/exif.rs"
test = false
doc = false
bench = false

[[bin]]
name = "xmp"
path = "fuzz_targets/xmp.rs"
test = false
doc = false
bench = false
```

> `omni-meta` 重导出 `omni_meta_core::*`，故 `__fuzzing` 是 core 的特性。在 facade 暴露需 core 特性可经 facade 传递——见 Step 2。

- [ ] **Step 2: 让 facade 传递 `__fuzzing` 特性**

Modify `omni-meta/Cargo.toml` 的 `[features]` 段为：

```toml
[features]
default = ["std"]
std = ["omni-meta-core/std"]
__fuzzing = ["omni-meta-core/__fuzzing"]
```

确认 `omni_meta::__fuzzing` 可达：facade 的 `pub use omni_meta_core::*;` 会重导出 `__fuzzing` 模块（特性开启时）。

- [ ] **Step 3: 写 fuzz/.gitignore**

Create `fuzz/.gitignore`:

```
target/
corpus/
artifacts/
coverage/
```

- [ ] **Step 4: 写 AllocCounter 的失败单测**

Create `fuzz/src/lib.rs`:

```rust
//! fuzz 共享：可证伪的分配上界（计数分配器）+ 测试用小 Limits。

use std::sync::atomic::{AtomicUsize, Ordering};

use omni_meta::Limits;

/// 模糊用收紧 Limits：远低于 Default，使分配上界在合理时间内可达。
pub const FUZZ_LIMITS: Limits = Limits {
    max_payload_bytes: 1 << 20,
    max_retained_bytes: 1 << 20,
    max_depth: 16,
    max_tags: 256,
    max_ifds: 8,
    max_total_alloc: 8 << 20,
};

/// 全局分配上界（字节）。远高于 FUZZ_LIMITS.max_total_alloc：合法解析通过，
/// 失控分配触发——返回 null 触发 Rust alloc 错误处理 → abort（libfuzzer 捕获）。
pub const ALLOC_CEILING: usize = 256 * 1024 * 1024;

/// 纯计数逻辑（不真正分配，便于单测，不会 abort）。
pub struct AllocCounter {
    live: AtomicUsize,
    ceiling: usize,
}

impl AllocCounter {
    pub const fn new(ceiling: usize) -> Self {
        Self { live: AtomicUsize::new(0), ceiling }
    }

    /// 预约 n 字节：若会越顶返回 false（不计入），否则计入返回 true。
    pub fn try_add(&self, n: usize) -> bool {
        let mut cur = self.live.load(Ordering::Relaxed);
        loop {
            let next = match cur.checked_add(n) {
                Some(v) if v <= self.ceiling => v,
                _ => return false,
            };
            match self.live.compare_exchange_weak(cur, next, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return true,
                Err(actual) => cur = actual,
            }
        }
    }

    pub fn sub(&self, n: usize) {
        self.live.fetch_sub(n, Ordering::Relaxed);
    }

    pub fn live(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_caps_without_allocating() {
        let c = AllocCounter::new(100);
        assert!(c.try_add(60));
        assert_eq!(c.live(), 60);
        assert!(!c.try_add(60), "越顶预约必须被拒且不计入");
        assert_eq!(c.live(), 60, "被拒预约不得改变 live");
        c.sub(60);
        assert_eq!(c.live(), 0);
        assert!(c.try_add(60), "释放后可再预约");
    }

    #[test]
    fn add_overflow_is_rejected() {
        let c = AllocCounter::new(usize::MAX);
        assert!(c.try_add(usize::MAX));
        assert!(!c.try_add(1), "溢出（checked_add 失败）必须被拒");
    }
}
```

- [ ] **Step 5: 跑 AllocCounter 单测确认通过**

Run: `cd fuzz && cargo +nightly test --lib; cd ..`
Expected: `counts_and_caps_without_allocating`、`add_overflow_is_rejected` PASS。

- [ ] **Step 6: 加入 FuzzAlloc 全局分配器（GlobalAlloc 实现）**

在 `fuzz/src/lib.rs` 的 `AllocCounter` 之后追加：

```rust
use std::alloc::{GlobalAlloc, Layout, System};

/// 全局计数分配器：委托 System，按 ALLOC_CEILING 守上界。越顶 → 返回 null →
/// Rust alloc 错误处理 abort（libfuzzer 记为可复现 crash，带分配栈）。
pub struct FuzzAlloc {
    counter: AllocCounter,
}

impl FuzzAlloc {
    pub const fn new() -> Self {
        Self { counter: AllocCounter::new(ALLOC_CEILING) }
    }
}

unsafe impl GlobalAlloc for FuzzAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !self.counter.try_add(layout.size()) {
            return core::ptr::null_mut();
        }
        let p = unsafe { System.alloc(layout) };
        if p.is_null() {
            self.counter.sub(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        self.counter.sub(layout.size());
    }
}
```

- [ ] **Step 7: 确认 fuzz crate 编译（含全局分配器）**

Run: `cd fuzz && cargo +nightly build --lib; cd ..`
Expected: 编译通过（无 `unsafe` 误用告警阻断）。

- [ ] **Step 8: 提交**

```bash
git add omni-meta/Cargo.toml fuzz/Cargo.toml fuzz/.gitignore fuzz/src/lib.rs
git commit -m "feat(fuzz): 独立 workspace 骨架 + 计数全局分配器（AllocCounter 已自测）"
```

---

## Task 5: 实现六个 fuzz target

每个 target 注册 `FuzzAlloc` 为全局分配器（透明守护所有 target 的分配不变量）。target 体只调入口、丢结果——panic/挂死由 libfuzzer 捕获，越分配上界由 `FuzzAlloc` 捕获。

**Files:**
- Create: `fuzz/fuzz_targets/differential.rs`
- Create: `fuzz/fuzz_targets/read_slice_bounded.rs`
- Create: `fuzz/fuzz_targets/isobmff.rs`
- Create: `fuzz/fuzz_targets/ebml.rs`
- Create: `fuzz/fuzz_targets/exif.rs`
- Create: `fuzz/fuzz_targets/xmp.rs`

- [ ] **Step 1: `differential` target（头牌——四适配器一致性 oracle）**

Create `fuzz/fuzz_targets/differential.rs`:

```rust
#![no_main]

use libfuzzer_sys::fuzz_target;
use omni_meta_fixtures::{adapters_outcome, Agreement};
use omni_meta_fuzz::FuzzAlloc;

#[global_allocator]
static ALLOC: FuzzAlloc = FuzzAlloc::new();

fuzz_target!(|data: &[u8]| {
    // 任意字节经真实 probe→driver 路径过四适配器：分歧即违反核心契约 → panic。
    if let Agreement::Disagree(why) = adapters_outcome(data) {
        panic!("adapter disagreement on {} bytes: {why}", data.len());
    }
});
```

- [ ] **Step 2: `read_slice_bounded` target（Limits tripwire + 产物有界）**

Create `fuzz/fuzz_targets/read_slice_bounded.rs`:

```rust
#![no_main]

use libfuzzer_sys::fuzz_target;
use omni_meta::{read_slice, Options};
use omni_meta_fuzz::{FuzzAlloc, FUZZ_LIMITS};

#[global_allocator]
static ALLOC: FuzzAlloc = FuzzAlloc::new();

fuzz_target!(|data: &[u8]| {
    let opts = Options { limits: FUZZ_LIMITS };
    if let Ok(meta) = read_slice(data, opts) {
        // 容器标签数受 max_tags 显式封顶——产物计数必须落在 Limits 内。
        assert!(
            meta.raw.container.len() <= FUZZ_LIMITS.max_tags,
            "container tags {} 超过 max_tags {}",
            meta.raw.container.len(),
            FUZZ_LIMITS.max_tags
        );
    }
    // 分配上界由 FuzzAlloc 守护：越顶即 abort。
});
```

- [ ] **Step 3: `isobmff` target**

Create `fuzz/fuzz_targets/isobmff.rs`:

```rust
#![no_main]

use libfuzzer_sys::fuzz_target;
use omni_meta::__fuzzing::drive_bmff;
use omni_meta_fuzz::{FuzzAlloc, FUZZ_LIMITS};

#[global_allocator]
static ALLOC: FuzzAlloc = FuzzAlloc::new();

fuzz_target!(|data: &[u8]| {
    let meta = drive_bmff(data, FUZZ_LIMITS);
    assert!(meta.raw.container.len() <= FUZZ_LIMITS.max_tags);
});
```

- [ ] **Step 4: `ebml` target**

Create `fuzz/fuzz_targets/ebml.rs`:

```rust
#![no_main]

use libfuzzer_sys::fuzz_target;
use omni_meta::__fuzzing::drive_ebml;
use omni_meta_fuzz::{FuzzAlloc, FUZZ_LIMITS};

#[global_allocator]
static ALLOC: FuzzAlloc = FuzzAlloc::new();

fuzz_target!(|data: &[u8]| {
    let meta = drive_ebml(data, FUZZ_LIMITS);
    assert!(meta.raw.container.len() <= FUZZ_LIMITS.max_tags);
});
```

- [ ] **Step 5: `exif` target**

Create `fuzz/fuzz_targets/exif.rs`:

```rust
#![no_main]

use libfuzzer_sys::fuzz_target;
use omni_meta::__fuzzing::decode_exif;
use omni_meta_fuzz::{FuzzAlloc, FUZZ_LIMITS};

#[global_allocator]
static ALLOC: FuzzAlloc = FuzzAlloc::new();

fuzz_target!(|data: &[u8]| {
    let (tags, _warns) = decode_exif(data, &FUZZ_LIMITS);
    assert!(tags.len() <= FUZZ_LIMITS.max_tags);
});
```

- [ ] **Step 6: `xmp` target**

Create `fuzz/fuzz_targets/xmp.rs`:

```rust
#![no_main]

use libfuzzer_sys::fuzz_target;
use omni_meta::__fuzzing::decode_xmp;
use omni_meta_fuzz::{FuzzAlloc, FUZZ_LIMITS};

#[global_allocator]
static ALLOC: FuzzAlloc = FuzzAlloc::new();

fuzz_target!(|data: &[u8]| {
    let (props, _warns) = decode_xmp(data, &FUZZ_LIMITS);
    assert!(props.len() <= FUZZ_LIMITS.max_tags);
});
```

- [ ] **Step 7: 构建全部 target**

Run: `cd fuzz && cargo +nightly fuzz build; cd ..`
Expected: 6 个 target 全部编译链接成功（libfuzzer instrumentation）。

> 若 `xmp`/`exif` 的 `props.len() <= max_tags` 断言因 codec 当前未对 `out` 长度封顶而可能失败：先用极短 `-runs` 冒烟（Task 6）确认；若确有超限，这是 fuzz 暴露的真实「不超 Limits」缺口，按 §收尾原则记录为跟踪项（修 codec 或调整断言为「分配上界守护」），不在本 target 内静默放宽。

- [ ] **Step 8: 提交**

```bash
git add fuzz/fuzz_targets/
git commit -m "feat(fuzz): 六 target（differential/bounded/isobmff/ebml/exif/xmp）"
```

---

## Task 6: 种子语料生成器 + 冒烟模糊

**Files:**
- Create: `fuzz/src/bin/seeds.rs`

- [ ] **Step 1: 写种子生成器**

Create `fuzz/src/bin/seeds.rs`:

```rust
//! 种子语料生成器：把 omni-meta-fixtures 的命名 fixture 写入各 target 的 corpus 目录，
//! 使模糊器从「近合法」输入起步。用法：`cargo +nightly run --bin seeds`。

use std::fs;
use std::path::Path;

fn write_seeds(target: &str, seeds: &[(&'static str, Vec<u8>)]) {
    let dir = Path::new("corpus").join(target);
    fs::create_dir_all(&dir).expect("create corpus dir");
    for (name, bytes) in seeds {
        let path = dir.join(format!("{name}.bin"));
        fs::write(&path, bytes).expect("write seed");
    }
    println!("{target}: {} 个种子 → {}", seeds.len(), dir.display());
}

fn main() {
    use omni_meta_fixtures as f;
    let files = f::file_corpus();
    write_seeds("differential", &files);
    write_seeds("read_slice_bounded", &files);
    write_seeds("isobmff", &f::bmff_corpus());
    write_seeds("ebml", &f::ebml_corpus());
    write_seeds("exif", &f::tiff_corpus());
    write_seeds("xmp", &f::xmp_corpus());
}
```

- [ ] **Step 2: 生成种子语料**

Run: `cd fuzz && cargo +nightly run --bin seeds; cd ..`
Expected: 打印各 target 种子数；`fuzz/corpus/<target>/*.bin` 落地（`differential`/`read_slice_bounded` 各含 jpeg/png/webp/gif/bmff/ebml 共 ~18 个；`exif` 5；`xmp` 2；`isobmff` 6；`ebml` 3）。

- [ ] **Step 3: 逐 target 冒烟模糊（短跑，验证不立即崩）**

Run（逐个）：
```bash
cd fuzz
for t in differential read_slice_bounded isobmff ebml exif xmp; do
  echo "=== $t ==="; cargo +nightly fuzz run "$t" -- -runs=20000 -max_total_time=30 || break
done
cd ..
```
Expected: 每个 target 跑完 `-runs`/超时退出，**无 crash artifact**。若出现 crash：libfuzzer 在 `fuzz/artifacts/<target>/` 写复现样本——按 §收尾原则处理（修复缺陷或记录跟踪项），复现命令 `cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<file>`。

- [ ] **Step 4: 提交种子生成器（corpus/artifacts 已被 .gitignore 忽略）**

```bash
git add fuzz/src/bin/seeds.rs
git commit -m "feat(fuzz): 种子语料生成器（复用 fixtures，分发到各 target corpus）"
```

---

## Task 7: 文档 + roadmap 勾选 + 终审

**Files:**
- Create: `fuzz/README.md`
- Modify: `docs/ROADMAP.md`

- [ ] **Step 1: 写 fuzz/README.md**

Create `fuzz/README.md`:

```markdown
# omni-meta fuzz harness

cargo-fuzz（libfuzzer）鲁棒性套件。验证 §5 不变量：任意字节下永不 panic /
不超 Limits / 不死循环；并跨四适配器校验一致性。需 nightly 工具链。

## 前置
- `rustup toolchain install nightly`
- `cargo install cargo-fuzz`

## target
| target | 入口 | 性质 |
|---|---|---|
| `differential` | 公共 API 四适配器 oracle | 全 Ok 且相等，或全 Err（分歧即 panic） |
| `read_slice_bounded` | `read_slice` + 计数分配器 | 不越分配上界；容器标签 ≤ max_tags |
| `isobmff` | `__fuzzing::drive_bmff` | BMFF 走盒不 panic/有界 |
| `ebml` | `__fuzzing::drive_ebml` | EBML 走元素不 panic/有界 |
| `exif` | `__fuzzing::decode_exif` | TIFF/IFD codec 有界 |
| `xmp` | `__fuzzing::decode_xmp` | XMP 扫描有界 |

全部 target 经 `FuzzAlloc` 全局分配器守护分配上界（`ALLOC_CEILING`）。

## 跑法
```bash
cargo +nightly run --bin seeds          # 生成种子语料（首次/更新 fixtures 后）
cargo +nightly fuzz run differential    # 跑某 target
cargo +nightly fuzz run differential -- -max_total_time=60   # 限时
```

## 复现与最小化
```bash
cargo +nightly fuzz run <target> artifacts/<target>/<crash-file>   # 复现
cargo +nightly fuzz tmin <target> artifacts/<target>/<crash-file>  # 最小化输入
cargo +nightly fuzz cmin <target>                                  # 最小化语料
```

## CI 接入点（尚未接线）
生产硬化支柱 2（CI）将以 `cargo +nightly fuzz run <t> -- -runs=N -max_total_time=T`
做短时冒烟。本目录已就绪。
```

- [ ] **Step 2: 勾选 ROADMAP fuzz 项**

Modify `docs/ROADMAP.md` §4，把

```
- [ ] **fuzz**：每个新容器/codec 接 `cargo-fuzz`，断言永不 panic / 不超 `Limits` / 不死循环
```

改为

```
- [x] **fuzz**：cargo-fuzz harness（独立 `fuzz/` workspace）——6 target（differential/read_slice_bounded/isobmff/ebml/exif/xmp）+ 计数全局分配器（不超 Limits tripwire）+ 复用 fixtures 的种子语料。见 `fuzz/README.md`、设计 `specs/2026-06-17-cargo-fuzz-design.md`
```

- [ ] **Step 3: 终审——全量测试 + 默认/no_std 构建 + fuzz 构建**

Run:
```bash
cargo test
cargo build --no-default-features -p omni-meta-core
cargo build --no-default-features -p omni-meta
cd fuzz && cargo +nightly fuzz build && cd ..
```
Expected: 测试全绿；两条 no_std 构建成功（`__fuzzing` 关闭、公共面不变）；6 target 构建成功。

- [ ] **Step 4: 提交**

```bash
git add fuzz/README.md docs/ROADMAP.md
git commit -m "docs(fuzz): README + roadmap 勾选 fuzz 支柱完成"
```

---

## Self-Review 备注（已核对）

- **Spec 覆盖**：§2 布局→Task 1/4；§3 `__fuzzing`→Task 3；§4 六 target→Task 5；§5 计数分配器→Task 4；§6 fixtures+种子→Task 2/6；§7 自测（AllocCounter Task 4 Step 4、oracle 结果化 API `adapters_outcome` Task 2 Step 2）；§8 文档→Task 7。§7 「植入分歧被抓」降级为「`adapters_outcome` 覆盖 Agree（良性 fixture，经差分测试）+ AllErr（不可识别字节）两支」——诚实可达，不伪造 buggy 适配器。
- **类型一致**：`adapters_outcome -> Agreement`、`FuzzAlloc::new()`、`FUZZ_LIMITS`、`AllocCounter::{try_add,sub,live}`、`__fuzzing::{decode_exif,decode_xmp,drive_bmff,drive_ebml}` 在定义与使用处签名一致。
- **不变量**：`unsafe impl GlobalAlloc` 仅在 `fuzz/` crate；主库 `#![forbid(unsafe_code)]` 不破。fixtures 纯安全。
- **已知风险**：`exif`/`xmp` target 的 `len() <= max_tags` 断言取决于 codec 是否对 `out` 封顶；Task 5 Step 7 已标注——若 fuzz 暴露超限即记为真实缺口跟踪，不静默放宽（符合 §收尾原则）。dev-dependency 依赖环（omni-meta↔fixtures）已在 Task 2 Step 5 标注为 cargo 合法。
```
