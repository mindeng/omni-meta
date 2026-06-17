# PNG tEXt/zTXt/iTXt 文本关键字 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 PNG 解析器读取 tEXt/iTXt(非XMP)/zTXt 文本关键字进 `RawTags.text`，并把注册关键字投影到 Unified（creator/software 末位兜底 + 新增 title/description/copyright + Creation Time）。

**Architecture:** 读取侧把 `tEXt`/`zTXt` 纳入 `is_meta` 整块读入；`tEXt`→`TextValue::Latin1`、非 XMP 未压缩 `iTXt`→`Utf8`、压缩块→`CompressedLatin1/CompressedUtf8`（不解压不报 warning）；新增 `Event::Text(TextTag)` 经 Collector 累积、finalize 写入 `RawTags.text`；normalize 用 `png_text` 助手接入既有优先级链作末位兜底。

**Tech Stack:** Rust（no_std + alloc），`#![forbid(unsafe_code)]`，零依赖；现有 sans-io Demand/Event 状态机 + Collector/finalize/normalize 三段式。

**关联设计：** `docs/superpowers/specs/2026-06-17-png-text-keywords-design.md`

**工作目录：** 分支 `feat/png-text-keywords`（已建，spec 已提交 `8fed1cb`）。

---

## 通用约定

- 每个任务结束跑该 crate 的测试，绿后 `git commit`。
- 全程不得引入依赖、不得 `unsafe`、所有偏移/长度 `checked_*`、解析失败丢弃不 panic。
- 测试命令统一在仓库根目录执行。core 单测：`cargo test -p omni-meta-core <名>`。
- 完成全部任务后跑总门禁（见 Task 9）。

---

## Task 1: 数据模型 —— TextTag / TextValue / RawTags.text / Unified 三字段

**Files:**
- Modify: `omni-meta-core/src/model.rs`（在 `XmpProperty` 之后插入新类型；扩展 `RawTags` 与 `Unified`）
- Modify: `omni-meta-core/src/lib.rs:29-32`（`pub use model::{...}` 加 `TextTag, TextValue`）

- [ ] **Step 1: 写失败测试**

在 `omni-meta-core/src/model.rs` 的 `mod tests` 内追加：

```rust
    #[test]
    fn text_tag_constructs_and_value_variants() {
        let t = TextTag {
            keyword: String::from("Author"),
            value: TextValue::Latin1(String::from("Ada")),
        };
        assert_eq!(t.keyword, "Author");
        assert_eq!(t.value, TextValue::Latin1(String::from("Ada")));
        // 四变体可构造且互不相等
        assert_ne!(
            TextValue::Utf8(String::from("x")),
            TextValue::Latin1(String::from("x"))
        );
        assert_ne!(
            TextValue::CompressedLatin1(alloc::vec![1, 2]),
            TextValue::CompressedUtf8(alloc::vec![1, 2])
        );
    }

    #[test]
    fn rawtags_default_has_empty_text() {
        let r = RawTags::default();
        assert!(r.text.is_empty());
    }

    #[test]
    fn unified_has_title_description_copyright_default_none() {
        let u = Unified::default();
        assert!(u.title.is_none());
        assert!(u.description.is_none());
        assert!(u.copyright.is_none());
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p omni-meta-core text_tag_constructs_and_value_variants`
Expected: 编译失败（`TextTag` / `TextValue` 未定义）。

- [ ] **Step 3: 实现类型与字段**

在 `model.rs` 中 `XmpProperty` 定义（约第 128 行）之后插入：

```rust
/// PNG 文本块（tEXt/iTXt/zTXt）的一条 keyword→value。
/// keyword 在四种块里都是明文，故始终可读；value 的载体/编码/压缩状态
/// 由 `TextValue` 单一表达——不单设 source 字段（避免与 value 变体冲突）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextTag {
    pub keyword: String,
    pub value: TextValue,
}

/// 文本值，自描述其编码与压缩状态。
/// 压缩变体仅保留原始压缩字节（本库零依赖、不解压）；上层可按变体决定
/// 解压后用 Latin-1 还是 UTF-8 解码。未来解压走独立 feature-gated crate。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextValue {
    /// tEXt：Latin-1 已逐字节无损映射为 UTF-8 String（永不失败）。
    Latin1(String),
    /// iTXt 未压缩、非 XMP：原生 UTF-8。
    Utf8(String),
    /// zTXt：zlib 压缩字节，未解压；解压后应按 Latin-1 解码。
    CompressedLatin1(Vec<u8>),
    /// 压缩 iTXt：zlib 压缩字节，未解压；解压后应按 UTF-8 解码。
    CompressedUtf8(Vec<u8>),
}
```

在 `RawTags`（约第 150 行）加字段：

```rust
pub struct RawTags {
    pub exif: Vec<ExifTag>,
    pub xmp: Vec<XmpProperty>,
    pub container: Vec<ContainerTag>,
    pub text: Vec<TextTag>,
}
```

在 `Unified`（约第 157 行）末尾加三字段：

```rust
    pub software: Option<String>,
    pub creator: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub copyright: Option<String>,
}
```

在 `lib.rs` 第 29-32 行的 `pub use model::{...}` 列表里加入 `TextTag, TextValue`（保持字母序可读即可）：

```rust
pub use model::{
    ContainerSource, ContainerTag, DateTimeParts, ExifTag, FileFormat, Gps, IfdKind, Metadata,
    Orientation, RawTags, TextTag, TextValue, Unified, Value, WarnKind, Warning, XmpProperty,
};
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p omni-meta-core text_tag_constructs_and_value_variants rawtags_default_has_empty_text unified_has_title_description_copyright_default_none`
Expected: 3 个测试 PASS。

> 注：`driver.rs:108` 的 `RawTags { exif, xmp, container }` 字面量此时会因缺 `text` 字段而编译失败——Task 2 修复。本任务先只跑 model 单测（`--lib` 默认会编译整 crate，故若想隔离，可临时在 `finalize` 的 `RawTags { ... }` 加 `text: Vec::new()`，并在 Task 2 替换为真实值）。为避免中间不可编译，**在本步顺带**把 `driver.rs:108-112` 的字面量改为含 `text: alloc::vec::Vec::new()`：

```rust
    let raw = RawTags {
        exif: col.exif,
        xmp: col.xmp,
        container: col.container,
        text: alloc::vec::Vec::new(), // Task 2 替换为 col.text
    };
```

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/model.rs omni-meta-core/src/lib.rs omni-meta-core/src/driver.rs
git commit -m "feat(model): TextTag/TextValue + RawTags.text + Unified title/description/copyright"
```

---

## Task 2: Event::Text 事件 + Collector 累积 + finalize 写入

**Files:**
- Modify: `omni-meta-core/src/demand.rs:32-42`（`Event` 加 `Text` 变体）
- Modify: `omni-meta-core/src/driver.rs`（`Collector` 加 `text` 字段、`handle` 加分支、`new()` 初始化、`finalize` 写入）

- [ ] **Step 1: 写失败测试**

在 `omni-meta-core/src/driver.rs` 的 `mod tests` 内追加（紧邻其它 `drive_slice` 测试；如需，参考文件内已有的 `FieldXmpEmitter` 风格自造一个发 `Event::Text` 的假解析器）：

```rust
    #[test]
    fn collector_accumulates_text_into_rawtags() {
        use crate::demand::{Demand, Event, MetaParser, PullResult};
        use crate::model::{TextTag, TextValue};

        struct TextEmitter(bool);
        impl MetaParser for TextEmitter {
            fn pull<'a>(&mut self, _input: &'a [u8]) -> PullResult<'a> {
                let events = alloc::vec![Event::Text(TextTag {
                    keyword: alloc::string::String::from("Author"),
                    value: TextValue::Latin1(alloc::string::String::from("Ada")),
                })];
                self.0 = true;
                PullResult { demand: Demand::Done, consumed: 0, events }
            }
        }

        let mut p = TextEmitter(false);
        let col = crate::driver::drive_slice(&[0u8; 4], &mut p, crate::limits::Limits::default());
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
        assert_eq!(meta.raw.text.len(), 1);
        assert_eq!(meta.raw.text[0].keyword, "Author");
    }

    #[test]
    fn collector_caps_text_at_max_tags() {
        use crate::demand::{Demand, Event, MetaParser, PullResult};
        use crate::model::{TextTag, TextValue};

        struct Flood;
        impl MetaParser for Flood {
            fn pull<'a>(&mut self, _input: &'a [u8]) -> PullResult<'a> {
                let mut events = alloc::vec::Vec::new();
                for _ in 0..10 {
                    events.push(Event::Text(TextTag {
                        keyword: alloc::string::String::from("K"),
                        value: TextValue::Utf8(alloc::string::String::from("v")),
                    }));
                }
                PullResult { demand: Demand::Done, consumed: 0, events }
            }
        }
        let limits = crate::limits::Limits { max_tags: 3, ..crate::limits::Limits::default() };
        let mut p = Flood;
        let col = crate::driver::drive_slice(&[0u8; 4], &mut p, limits);
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
        assert_eq!(meta.raw.text.len(), 3);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p omni-meta-core collector_accumulates_text_into_rawtags collector_caps_text_at_max_tags`
Expected: 编译失败（`Event::Text` 不存在）。

- [ ] **Step 3: 实现**

`demand.rs` 在 `Event` 枚举（第 32-42 行）的 `Warning(Warning)` 之前加变体：

```rust
    /// PNG 文本块（tEXt/iTXt 非 XMP/zTXt）keyword→value，原样入 raw.text。
    Text(crate::model::TextTag),
    Warning(Warning),
```

`driver.rs`：
1. `Collector` 结构（第 16-29 行）`container` 之后加 `text: Vec<crate::model::TextTag>,`。
2. `handle`（第 32-88 行）在 `Event::ContainerTag` 分支后加：

```rust
            Event::Text(t) => {
                if self.text.len() < self.limits.max_tags {
                    self.text.push(t);
                }
            }
```

3. `StreamDriver::new` 的 `Collector { ... }` 初始化（第 164-177 行）`container: Vec::new(),` 后加 `text: Vec::new(),`。
4. `drive_slice` 内构造 `Collector` 处同样加 `text: Vec::new(),`（搜索文件内另一处 `container: Vec::new()` 初始化，约第 384 行起）。
5. `finalize`（第 108-112 行）把 Task 1 的占位替换：

```rust
    let raw = RawTags {
        exif: col.exif,
        xmp: col.xmp,
        container: col.container,
        text: col.text,
    };
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p omni-meta-core collector_accumulates_text_into_rawtags collector_caps_text_at_max_tags`
Expected: 2 PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/demand.rs omni-meta-core/src/driver.rs
git commit -m "feat(driver): Event::Text 经 Collector 累积入 RawTags.text（max_tags 封顶）"
```

---

## Task 3: PNG 读取 tEXt（含 keyword 校验助手）

**Files:**
- Modify: `omni-meta-core/src/formats/png.rs`（`is_meta` 纳入 `tEXt`；新增 keyword 切分 + Latin-1 助手；`tEXt` 分支发 `Event::Text`）

- [ ] **Step 1: 写失败测试**

在 `formats/png.rs` 的 `mod tests` 内追加测试与构造助手：

```rust
    fn text_chunk(kw: &[u8], val: &[u8]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(kw);
        d.push(0);
        d.extend_from_slice(val);
        chunk(b"tEXt", &d)
    }

    #[test]
    fn text_chunk_parses_into_rawtext_latin1() {
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(2, 2));
        p.extend_from_slice(&text_chunk(b"Author", b"Ada Lovelace"));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        let col = collect(&p);
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
        assert!(meta.raw.text.iter().any(|t| t.keyword == "Author"
            && t.value == crate::model::TextValue::Latin1("Ada Lovelace".into())));
    }

    #[test]
    fn text_chunk_empty_keyword_or_no_nul_is_dropped_silently() {
        for data in [&b"\0value"[..] /* 空 keyword */, &b"noseparator"[..] /* 无 \0 */] {
            let mut p = Vec::new();
            p.extend_from_slice(&SIG);
            p.extend_from_slice(&ihdr(2, 2));
            p.extend_from_slice(&chunk(b"tEXt", data));
            p.extend_from_slice(&chunk(b"IEND", &[]));
            let col = collect(&p);
            assert!(col.warnings.is_empty(), "畸形 tEXt 应静默丢弃: {data:?}");
            let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
            assert!(meta.raw.text.is_empty());
        }
    }

    #[test]
    fn text_chunk_keyword_too_long_warns_unrecognized() {
        let long_kw = [b'K'; 80]; // >79
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(2, 2));
        p.extend_from_slice(&text_chunk(&long_kw, b"v"));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        let col = collect(&p);
        assert!(col.warnings.iter().any(|w| w.kind == WarnKind::UnrecognizedValue));
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
        assert!(meta.raw.text.is_empty());
    }
```

注：`collect` 返回 `Collector`，其 `warnings`/`text` 字段已 `pub`，可直接断言 `col.warnings`；`col.text` 为 `pub(crate)`（同 crate 测试可见）。若 `col.text` 不可见，改为对 `finalize` 后的 `meta.raw.text` 断言（如上）。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p omni-meta-core text_chunk_parses_into_rawtext_latin1 text_chunk_empty_keyword_or_no_nul_is_dropped_silently text_chunk_keyword_too_long_warns_unrecognized`
Expected: 失败（tEXt 仍被 Skip，raw.text 为空 / 无 warning）。

- [ ] **Step 3: 实现**

在 `formats/png.rs` 顶部 `use` 区补 `TextTag`/`TextValue`/`String`：

```rust
use alloc::string::String;
use alloc::vec::Vec;

use crate::demand::{Demand, Event, MetaParser, PayloadKind, PullResult};
use crate::model::{Field, TextTag, TextValue, WarnKind, Warning};
```

第 76 行 `is_meta` 纳入 `tEXt`：

```rust
            let is_meta =
                ctype == b"IHDR" || ctype == b"eXIf" || ctype == b"iTXt" || ctype == b"tEXt";
```

在 `match ctype` 的 `b"iTXt" => { handle_itxt(...) }` 分支后加（`pos` 是当前 chunk 头在 input 的偏移）：

```rust
                    b"tEXt" => {
                        handle_text(data, pos as u64 + 8, &mut events);
                    }
```

在文件底部（`handle_itxt` 附近）加助手：

```rust
/// keyword 切分结果。
enum KwSplit<'a> {
    /// (keyword, keyword 之后的余下字节)
    Ok(&'a [u8], &'a [u8]),
    /// 无 \0 分隔 或 空 keyword —— 静默丢弃。
    Malformed,
    /// keyword > 79 字节（违反 PNG 规范）—— 调用方应发 UnrecognizedValue。
    TooLong,
}

/// 按首个 \0 切分 keyword；强制 1..=79 字节（PNG 规范）。
fn split_keyword(data: &[u8]) -> KwSplit<'_> {
    let nul = match data.iter().position(|&b| b == 0) {
        Some(p) => p,
        None => return KwSplit::Malformed,
    };
    if nul == 0 {
        return KwSplit::Malformed; // 空 keyword
    }
    if nul > 79 {
        return KwSplit::TooLong;
    }
    KwSplit::Ok(&data[..nul], &data[nul + 1..])
}

/// Latin-1 字节逐个无损映射为 UTF-8 String（永不失败、零依赖）。
fn latin1_to_string(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// 解析 tEXt：keyword\0value，全 Latin-1。`offset` 为该 chunk 数据起点（供 warning）。
fn handle_text<'a>(data: &'a [u8], offset: u64, events: &mut Vec<Event<'a>>) {
    match split_keyword(data) {
        KwSplit::Ok(kw, val) => {
            events.push(Event::Text(TextTag {
                keyword: latin1_to_string(kw),
                value: TextValue::Latin1(latin1_to_string(val)),
            }));
        }
        KwSplit::Malformed => {}
        KwSplit::TooLong => events.push(Event::Warning(Warning {
            offset,
            kind: WarnKind::UnrecognizedValue,
        })),
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p omni-meta-core text_chunk_parses_into_rawtext_latin1 text_chunk_empty_keyword_or_no_nul_is_dropped_silently text_chunk_keyword_too_long_warns_unrecognized`
Expected: 3 PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/formats/png.rs
git commit -m "feat(png): 读取 tEXt → RawTags.text(Latin1)；keyword 校验(空/无NUL丢弃, >79 警告)"
```

---

## Task 4: PNG iTXt 非 XMP + 压缩 iTXt

**Files:**
- Modify: `omni-meta-core/src/formats/png.rs`（改造 `handle_itxt`：非 XMP 未压缩→Utf8，非法 UTF-8→UnrecognizedValue，压缩→CompressedUtf8 不报 warning）

- [ ] **Step 1: 写失败测试**

`mod tests` 内追加。先加一个可设 keyword 的 iTXt 构造助手：

```rust
    fn itxt(keyword: &[u8], compressed: bool, text: &[u8]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(keyword);
        d.push(0); // keyword NUL
        d.push(if compressed { 1 } else { 0 }); // compflag
        d.push(0); // compmethod
        d.push(0); // lang NUL
        d.push(0); // transkw NUL
        d.extend_from_slice(text);
        chunk(b"iTXt", &d)
    }

    #[test]
    fn itxt_non_xmp_uncompressed_parses_utf8() {
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(2, 2));
        p.extend_from_slice(&itxt(b"Description", false, "héllo".as_bytes()));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        let meta = crate::driver::finalize(collect(&p), crate::model::FileFormat::Png);
        assert!(meta.raw.text.iter().any(|t| t.keyword == "Description"
            && t.value == crate::model::TextValue::Utf8("héllo".into())));
    }

    #[test]
    fn itxt_invalid_utf8_warns_unrecognized() {
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(2, 2));
        p.extend_from_slice(&itxt(b"Comment", false, &[0xFF, 0xFE])); // 非法 UTF-8
        p.extend_from_slice(&chunk(b"IEND", &[]));
        let col = collect(&p);
        assert!(col.warnings.iter().any(|w| w.kind == WarnKind::UnrecognizedValue));
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
        assert!(meta.raw.text.is_empty());
    }

    #[test]
    fn itxt_compressed_keeps_bytes_no_warning() {
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(2, 2));
        p.extend_from_slice(&itxt(b"Description", true, &[0x78, 0x9c, 1, 2, 3]));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        let col = collect(&p);
        assert!(!col.warnings.iter().any(|w| w.kind == WarnKind::CompressedChunkSkipped));
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
        assert!(meta.raw.text.iter().any(|t| t.keyword == "Description"
            && matches!(t.value, crate::model::TextValue::CompressedUtf8(_))));
    }

    #[test]
    fn itxt_xmp_still_routes_to_xmp_unchanged() {
        // 复用既有 itxt_xmp 构造器；XMP 路径不应进 raw.text
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(2, 2));
        p.extend_from_slice(&itxt_xmp(br#"<rdf:Description tiff:Make="Acme"/>"#, false));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        let meta = crate::driver::finalize(collect(&p), crate::model::FileFormat::Png);
        assert!(meta.raw.text.is_empty());
        assert!(meta.raw.xmp.iter().any(|x| x.name == "Make"));
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p omni-meta-core itxt_non_xmp_uncompressed_parses_utf8 itxt_invalid_utf8_warns_unrecognized itxt_compressed_keeps_bytes_no_warning itxt_xmp_still_routes_to_xmp_unchanged`
Expected: 前三个失败（现有 `handle_itxt` 对非 XMP 早返回、压缩发 CompressedChunkSkipped）。

- [ ] **Step 3: 实现 —— 重写 `handle_itxt`**

把 `formats/png.rs` 现有 `handle_itxt`（第 135-172 行）整体替换为下面版本。调用点改为传入 offset：把 `b"iTXt" => { handle_itxt(data, &mut events); }` 改为 `b"iTXt" => { handle_itxt(data, pos as u64 + 8, &mut events); }`。

```rust
/// 解析 iTXt 数据。
/// keyword==XML:com.adobe.xmp 且未压缩 → 发 Xmp 载荷（不变）。
/// 其它 keyword：未压缩合法 UTF-8 → Text(Utf8)；非法 UTF-8 → UnrecognizedValue；
/// 压缩 → Text(CompressedUtf8)，不报 warning。`offset` 为 chunk 数据起点。
fn handle_itxt<'a>(data: &'a [u8], offset: u64, events: &mut Vec<Event<'a>>) {
    // 布局：keyword\0 compflag(1) compmethod(1) lang\0 transkw\0 text
    let (kw, after_kw) = match split_keyword(data) {
        KwSplit::Ok(kw, rest) => (kw, rest),
        KwSplit::Malformed => return,
        KwSplit::TooLong => {
            events.push(Event::Warning(Warning {
                offset,
                kind: WarnKind::UnrecognizedValue,
            }));
            return;
        }
    };
    if after_kw.len() < 2 {
        return;
    }
    let compressed = after_kw[0] != 0;
    // 跳过 compflag(1)+compmethod(1)，再跳过 lang\0 与 transkw\0
    let rest = &after_kw[2..];
    let lang_end = match rest.iter().position(|&b| b == 0) {
        Some(p) => p,
        None => return,
    };
    let rest2 = &rest[lang_end + 1..];
    let tk_end = match rest2.iter().position(|&b| b == 0) {
        Some(p) => p,
        None => return,
    };
    let text = &rest2[tk_end + 1..];

    let is_xmp = kw == b"XML:com.adobe.xmp";
    if is_xmp && !compressed {
        events.push(Event::Payload {
            kind: PayloadKind::Xmp,
            data: text,
        });
        return;
    }
    if compressed {
        events.push(Event::Text(TextTag {
            keyword: latin1_to_string(kw),
            value: TextValue::CompressedUtf8(text.to_vec()),
        }));
        return;
    }
    match core::str::from_utf8(text) {
        Ok(s) => events.push(Event::Text(TextTag {
            keyword: latin1_to_string(kw),
            value: TextValue::Utf8(String::from(s)),
        })),
        Err(_) => events.push(Event::Warning(Warning {
            offset,
            kind: WarnKind::UnrecognizedValue,
        })),
    }
}
```

> 说明：keyword 用 `latin1_to_string`——iTXt keyword 规范为 Latin-1（与 tEXt 同）。压缩 XMP iTXt 此处也归入 `CompressedUtf8`（keyword=`XML:com.adobe.xmp`），符合设计「压缩块统一保留字节」。`Vec` 已在 use 中；`to_vec()` 需 `alloc::vec::Vec`（文件已 `use alloc::vec::Vec;`）。

旧测试 `compressed_itxt_warns_and_skips`（断言 `CompressedChunkSkipped`）已与新行为冲突——**删除该测试**（其语义被 `itxt_compressed_keeps_bytes_no_warning` 取代）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p omni-meta-core --lib formats::png`
Expected: 全部 PNG 单测 PASS（含新四测；旧 `compressed_itxt_warns_and_skips` 已移除）。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/formats/png.rs
git commit -m "feat(png): iTXt 非XMP→Text(Utf8/非法UTF8警告)，压缩iTXt→CompressedUtf8(不报警)"
```

---

## Task 5: PNG 读取 zTXt

**Files:**
- Modify: `omni-meta-core/src/formats/png.rs`（`is_meta` 纳入 `zTXt`；新增 `handle_ztxt`）

- [ ] **Step 1: 写失败测试**

`mod tests` 内追加：

```rust
    fn ztxt(keyword: &[u8], zdata: &[u8]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(keyword);
        d.push(0); // keyword NUL
        d.push(0); // compression method
        d.extend_from_slice(zdata);
        chunk(b"zTXt", &d)
    }

    #[test]
    fn ztxt_keeps_compressed_latin1_no_warning() {
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(2, 2));
        p.extend_from_slice(&ztxt(b"Comment", &[0x78, 0x9c, 9, 8, 7]));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        let col = collect(&p);
        assert!(!col.warnings.iter().any(|w| w.kind == WarnKind::CompressedChunkSkipped));
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
        assert!(meta.raw.text.iter().any(|t| t.keyword == "Comment"
            && matches!(t.value, crate::model::TextValue::CompressedLatin1(_))));
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p omni-meta-core ztxt_keeps_compressed_latin1_no_warning`
Expected: 失败（zTXt 当前被 Skip）。

- [ ] **Step 3: 实现**

第 76 行 `is_meta` 再纳入 `zTXt`：

```rust
            let is_meta = ctype == b"IHDR"
                || ctype == b"eXIf"
                || ctype == b"iTXt"
                || ctype == b"tEXt"
                || ctype == b"zTXt";
```

`match ctype` 加分支（在 `b"tEXt"` 之后）：

```rust
                    b"zTXt" => {
                        handle_ztxt(data, &mut events);
                    }
```

底部加助手：

```rust
/// 解析 zTXt：keyword\0 compmethod(1) <zlib 压缩字节>。
/// 保留压缩字节为 CompressedLatin1（本库不解压），不报 warning。
fn handle_ztxt<'a>(data: &'a [u8], events: &mut Vec<Event<'a>>) {
    let (kw, after_kw) = match split_keyword(data) {
        KwSplit::Ok(kw, rest) => (kw, rest),
        // zTXt 的 keyword 同受 1..=79 约束；畸形/超长均直接丢弃（不投影、无价值）。
        KwSplit::Malformed | KwSplit::TooLong => return,
    };
    if after_kw.is_empty() {
        return; // 缺 compression method 字节
    }
    let zdata = &after_kw[1..]; // 跳过 compmethod
    events.push(Event::Text(TextTag {
        keyword: latin1_to_string(kw),
        value: TextValue::CompressedLatin1(zdata.to_vec()),
    }));
}
```

> 说明：zTXt 超长 keyword 这里选择**静默丢弃**（与 tEXt/iTXt 的 TooLong→警告略有不同）——因 zTXt 压缩值本就不投影，超长 keyword 几乎必为畸形，无需噪声。若需与 tEXt 完全一致，可改为发 UnrecognizedValue；当前按设计「压缩块不报 warning」从简。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p omni-meta-core --lib formats::png`
Expected: 全部 PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/formats/png.rs
git commit -m "feat(png): 读取 zTXt → CompressedLatin1(保留字节, 不解压不报警)"
```

---

## Task 6: normalize 投影（creator/software 兜底 + 新字段 + Creation Time）

**Files:**
- Modify: `omni-meta-core/src/normalize.rs`（新增常量、`png_text` 助手、`parse_png_creation_time`、`parse_rfc1123`、接入投影链）

- [ ] **Step 1: 写失败测试**

在 `normalize.rs` 的 `mod tests` 内追加。先准备构造 `RawTags`（含 text）的小助手——参考文件内已有测试用 `RawTags { ..Default::default() }` 风格：

```rust
    use crate::model::{TextTag, TextValue};

    fn raw_with_text(keyword: &str, value: &str) -> RawTags {
        RawTags {
            text: alloc::vec![TextTag {
                keyword: keyword.into(),
                value: TextValue::Latin1(value.into()),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn png_author_projects_creator_as_fallback() {
        let mut w = Vec::new();
        let u = normalize(&raw_with_text("Author", "Ada"), &mut w);
        assert_eq!(u.creator.as_deref(), Some("Ada"));
    }

    #[test]
    fn png_software_projects_software_as_fallback() {
        let mut w = Vec::new();
        let u = normalize(&raw_with_text("Software", "OmniTool"), &mut w);
        assert_eq!(u.software.as_deref(), Some("OmniTool"));
    }

    #[test]
    fn png_new_fields_project() {
        let mut w = Vec::new();
        assert_eq!(normalize(&raw_with_text("Title", "T"), &mut w).title.as_deref(), Some("T"));
        assert_eq!(normalize(&raw_with_text("Description", "D"), &mut w).description.as_deref(), Some("D"));
        assert_eq!(normalize(&raw_with_text("Copyright", "C"), &mut w).copyright.as_deref(), Some("C"));
    }

    #[test]
    fn png_creator_does_not_override_xmp() {
        // XMP dc:creator 存在时，PNG Author 不应覆盖
        let raw = RawTags {
            xmp: alloc::vec![crate::model::XmpProperty {
                prefix: "dc".into(), name: "creator".into(), value: "FromXmp".into(),
            }],
            text: alloc::vec![TextTag { keyword: "Author".into(), value: TextValue::Latin1("FromPng".into()) }],
            ..Default::default()
        };
        let mut w = Vec::new();
        assert_eq!(normalize(&raw, &mut w).creator.as_deref(), Some("FromXmp"));
    }

    #[test]
    fn png_creation_time_iso_rfc1123_baredate() {
        for (input, y, mo, d, h) in [
            ("2021-07-06T09:30:00Z", 2021u16, 7u8, 6u8, 9u8),
            ("Tue, 06 Jul 2021 09:30:00 GMT", 2021, 7, 6, 9),
            ("2021-07-06", 2021, 7, 6, 0),
        ] {
            let mut w = Vec::new();
            let u = normalize(&raw_with_text("Creation Time", input), &mut w);
            let c = u.created.unwrap_or_else(|| panic!("未解析: {input}"));
            assert_eq!((c.year, c.month, c.day, c.hour), (y, mo, d, h), "{input}");
        }
    }

    #[test]
    fn png_creation_time_unparseable_stays_raw_no_warning() {
        let mut w = Vec::new();
        let u = normalize(&raw_with_text("Creation Time", "sometime last summer"), &mut w);
        assert!(u.created.is_none());
        assert!(w.is_empty());
    }

    #[test]
    fn png_compressed_value_not_projected() {
        let raw = RawTags {
            text: alloc::vec![TextTag {
                keyword: "Author".into(),
                value: TextValue::CompressedLatin1(alloc::vec![1, 2, 3]),
            }],
            ..Default::default()
        };
        let mut w = Vec::new();
        assert!(normalize(&raw, &mut w).creator.is_none());
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p omni-meta-core png_author_projects_creator_as_fallback png_new_fields_project png_creation_time_iso_rfc1123_baredate`
Expected: 失败（投影未实现）。

- [ ] **Step 3: 实现**

`normalize.rs` 常量区（第 10-18 行附近）新增：

```rust
const TAG_IMAGE_DESCRIPTION: u16 = 0x010E;
const TAG_COPYRIGHT: u16 = 0x8298;
```

在助手区（`xmp_text` 之后，约第 269 行）新增：

```rust
/// 取 raw.text 中首个匹配 keyword 的**明文**值（Latin1/Utf8）；压缩变体跳过。
fn png_text(raw: &RawTags, keyword: &str) -> Option<alloc::string::String> {
    raw.text.iter().find_map(|t| {
        if t.keyword != keyword {
            return None;
        }
        match &t.value {
            crate::model::TextValue::Latin1(s) | crate::model::TextValue::Utf8(s) => {
                Some(s.clone())
            }
            _ => None,
        }
    })
}

/// 解析 PNG `Creation Time`：依次尝试 ISO 8601 / RFC 1123 / 裸日期 YYYY-MM-DD。
/// 均不匹配 → None（不臆造、不报 warning）。
fn parse_png_creation_time(s: &str) -> Option<DateTimeParts> {
    if let Some(dt) = parse_iso8601(s) {
        return Some(dt);
    }
    if let Some(dt) = parse_rfc1123(s) {
        return Some(dt);
    }
    parse_bare_date(s)
}

/// RFC 1123：`Day, DD Mon YYYY HH:MM:SS GMT`（PNG 规范钦定）。tz 视作 UTC=Some(0)。
fn parse_rfc1123(s: &str) -> Option<DateTimeParts> {
    // 例： "Tue, 06 Jul 2021 09:30:00 GMT"
    let b = s.as_bytes();
    if b.len() != 29 || b[3] != b',' || b[4] != b' ' {
        return None;
    }
    let two = |i: usize| -> Option<u32> {
        let (h, l) = (b[i], b[i + 1]);
        if !h.is_ascii_digit() || !l.is_ascii_digit() {
            return None;
        }
        Some(u32::from((h - b'0') * 10 + (l - b'0')))
    };
    let four = |i: usize| -> Option<u32> {
        let mut v = 0u32;
        for &c in &b[i..i + 4] {
            if !c.is_ascii_digit() {
                return None;
            }
            v = v * 10 + u32::from(c - b'0');
        }
        Some(v)
    };
    let month = match &b[8..11] {
        b"Jan" => 1, b"Feb" => 2, b"Mar" => 3, b"Apr" => 4,
        b"May" => 5, b"Jun" => 6, b"Jul" => 7, b"Aug" => 8,
        b"Sep" => 9, b"Oct" => 10, b"Nov" => 11, b"Dec" => 12,
        _ => return None,
    };
    let day = two(5)?;
    let year = four(12)?;
    let hour = two(17)?;
    let minute = two(20)?;
    let second = two(23)?;
    if !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 || &b[26..29] != b"GMT" {
        return None;
    }
    Some(DateTimeParts {
        year: year as u16,
        month,
        day: day as u8,
        hour: hour as u8,
        minute: minute as u8,
        second: second as u8,
        tz_offset_min: Some(0),
    })
}

/// 裸日期 `YYYY-MM-DD` → 时分秒 00:00:00、tz None。
fn parse_bare_date(s: &str) -> Option<DateTimeParts> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: core::ops::Range<usize>| -> Option<u32> {
        let mut v = 0u32;
        for &c in &b[r] {
            if !c.is_ascii_digit() {
                return None;
            }
            v = v * 10 + u32::from(c - b'0');
        }
        Some(v)
    };
    let year = num(0..4)?;
    let month = num(5..7)?;
    let day = num(8..10)?;
    if year == 0 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(DateTimeParts {
        year: year as u16,
        month: month as u8,
        day: day as u8,
        hour: 0,
        minute: 0,
        second: 0,
        tz_offset_min: None,
    })
}
```

在 `normalize` 函数体内、`software`/`creator` 赋值（第 370-388 行）的 `.or_else` 链尾各追加一项，并在函数返回 `u` 之前加入新字段与 created 兜底：

```rust
    // software：容器 > EXIF > XMP > PNG Software
    u.software = container_text(raw, ContainerSource::QuickTimeMdta, "com.apple.quicktime.software")
        .or_else(|| container_text(raw, ContainerSource::Udta, "©swr"))
        .map(alloc::string::String::from)
        .or_else(|| exif_primary_text(raw, TAG_SOFTWARE))
        .or_else(|| xmp_text(raw, "xmp", "CreatorTool"))
        .or_else(|| png_text(raw, "Software"));
    // creator：容器 > EXIF > XMP > PNG Author
    u.creator = container_text(raw, ContainerSource::QuickTimeMdta, "com.apple.quicktime.author")
        .or_else(|| container_text(raw, ContainerSource::Udta, "©aut"))
        .map(alloc::string::String::from)
        .or_else(|| exif_primary_text(raw, TAG_ARTIST))
        .or_else(|| xmp_text(raw, "dc", "creator"))
        .or_else(|| png_text(raw, "Author"));
    // 新字段：description / copyright / title
    u.description = exif_primary_text(raw, TAG_IMAGE_DESCRIPTION)
        .or_else(|| xmp_text(raw, "dc", "description"))
        .or_else(|| png_text(raw, "Description"));
    u.copyright = exif_primary_text(raw, TAG_COPYRIGHT)
        .or_else(|| xmp_text(raw, "dc", "rights"))
        .or_else(|| png_text(raw, "Copyright"));
    u.title = xmp_text(raw, "dc", "title").or_else(|| png_text(raw, "Title"));
    // created：normalize 内 EXIF 已优先；PNG Creation Time 末位兜底（容器在 finalize 再覆盖）
    if u.created.is_none()
        && let Some(s) = png_text(raw, "Creation Time")
        && let Some(dt) = parse_png_creation_time(&s)
    {
        u.created = Some(dt);
    }
    u
```

> 注：上面 `software`/`creator` 块是对现有代码的**整段替换**（只在链尾加了 `.or_else(|| png_text(...))`），避免重复粘贴；其余为新增。`if let &&` 链式语法与文件内 created 解析处（第 345-350 行）一致，确认 edition 支持（现有代码已用 `if let ... && let ...`）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p omni-meta-core --lib normalize`
Expected: 新测试全 PASS，既有 normalize 测试不回归。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/normalize.rs
git commit -m "feat(normalize): PNG 文本关键字投影(creator/software 兜底 + title/description/copyright + Creation Time)"
```

---

## Task 7: Strip 纠偏 + PII 测试 + ROADMAP 修正

**Files:**
- Modify: `omni-meta-core/src/strip/png.rs`（`classify` 死枝加注释；新增 PII 删除测试）
- Modify: `docs/ROADMAP.md:150`（纠正 strip 失实表述 + 勾选）

- [ ] **Step 1: 写失败测试（实为锁定现有正确行为）**

`strip/png.rs` 的 `mod tests` 内追加：

```rust
    #[test]
    fn default_strips_text_chunks_with_pii() {
        let mut p = alloc::vec::Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(4, 4));
        // tEXt Author=PII + zTXt Comment=<压缩字节>
        let mut te = alloc::vec::Vec::new();
        te.extend_from_slice(b"Author");
        te.push(0);
        te.extend_from_slice(b"Jane Secret");
        p.extend_from_slice(&chunk(b"tEXt", &te));
        let mut zt = alloc::vec::Vec::new();
        zt.extend_from_slice(b"Comment");
        zt.push(0);
        zt.push(0); // compmethod
        zt.extend_from_slice(&[0x78, 0x9c, 1, 2, 3]);
        p.extend_from_slice(&chunk(b"zTXt", &zt));
        p.extend_from_slice(&chunk(b"IDAT", &[1, 2, 3, 4]));
        p.extend_from_slice(&chunk(b"IEND", &[]));

        let (out, report) = run(&p, StripOptions::default());
        assert!(!out.windows(6).any(|w| w == b"Author"), "tEXt Author 应被剥离");
        assert!(!out.windows(11).any(|w| w == b"Jane Secret"), "PII 值应被剥离");
        assert!(!out.windows(4).any(|w| w == b"tEXt"));
        assert!(!out.windows(4).any(|w| w == b"zTXt"));
        assert!(report.removed.contains(RemovedKind::Other));
        // 幂等
        let (again, _) = run(&out, StripOptions::default());
        assert_eq!(out, again);
    }
```

- [ ] **Step 2: 跑测试**

Run: `cargo test -p omni-meta-core default_strips_text_chunks_with_pii`
Expected: **PASS**（strip 行为本就正确）。若失败则说明 strip 有真问题，转 systematic-debugging。

- [ ] **Step 3: 加死枝注释**

`strip/png.rs` 的 `classify`（第 108-114 行）改为：

```rust
        b"iTXt" | b"tEXt" | b"zTXt" => {
            // 注：XMP 仅以 iTXt 承载，tEXt/zTXt 永不命中下面的 XMP 分支（死枝，保留以统一形态）。
            if data.starts_with(b"XML:com.adobe.xmp") {
                Some((RemovedKind::Xmp, false))
            } else {
                Some((RemovedKind::Other, false))
            }
        }
```

- [ ] **Step 4: 纠正 ROADMAP §4 第 150 行**

把第 150 行整条 `- [ ] **待评估：PNG tEXt/zTXt 注册关键字** ...` 替换为：

```markdown
- [x] **PNG tEXt/zTXt/iTXt 文本关键字** — 读取侧把 `tEXt`/非XMP-`iTXt`/`zTXt` 收入 `RawTags.text`（`TextTag{keyword, value: TextValue}`，四变体自描述编码+压缩态）；注册关键字投影：`Author`→creator / `Software`→software（末位兜底）、新增 `title`/`description`/`copyright`、`Creation Time`→created（ISO/RFC1123/裸日期）。压缩块（zTXt/压缩 iTXt）保留原始字节、**不解压不报 warning**（解压留未来 feature-gated `omni-meta-inflate`）。keyword 强制 ≤79 防 slice 路径超大分配。**Strip 侧本就默认全删文本块（含 PII）**——此前 ROADMAP 称「strip 盲区」为失实，已纠正并补 PII 删除+幂等测试。设计 `specs/2026-06-17-png-text-keywords-design.md` / 计划 `plans/2026-06-17-png-text-keywords.md`。
```

- [ ] **Step 5: 跑测试 + 提交**

Run: `cargo test -p omni-meta-core --lib strip::png`
Expected: PASS。

```bash
git add omni-meta-core/src/strip/png.rs docs/ROADMAP.md
git commit -m "test(strip): 锁定 PNG 文本块默认剥离(含PII)+幂等；注释XMP死枝；纠正ROADMAP §4 失实表述"
```

---

## Task 8: 黄金样本 —— GoldenRawTag::Text + 兑现 tEXt 缺口

**Files:**
- Modify: `omni-meta-fixtures/src/golden.rs`（`GoldenRawTag` 加 `Text` 变体；`png_exif` 更新注释 + 加 raw.text 期望）
- Modify: `omni-meta/tests/golden.rs`（`assert_raw_subset` 加 `Text` 分支）

- [ ] **Step 1: 先确认现有 png_exif.png 实际含哪些文本块**

Run: `strings omni-meta-fixtures/samples/png_exif.png | grep -aiE 'Make|Author|OmniTest|GoldenAuthor' ; python3 -c "import sys;d=open('omni-meta-fixtures/samples/png_exif.png','rb').read();print([d[i+4:i+8] for i in range(8,len(d)-8) if d[i+4:i+8] in (b'tEXt',b'iTXt',b'zTXt',b'eXIf')][:10])"`
Expected: 看到 `tEXt`（exiftool 写的 `Make=OmniTest`）与 `iTXt`（XMP dc:creator）。记下 tEXt 的实际 keyword 与值（设计注释与 `regen.sh:16` 表明是 `Make=OmniTest`）。

> 若实际 tEXt keyword/值与下面假设（`Make`/`OmniTest`）不符，以 Step 1 输出为准调整 Step 3 的期望值。

- [ ] **Step 2: 写失败测试（更新 png_exif 期望）**

在 `golden.rs` 的 `GoldenRawTag`（第 8-24 行）加变体：

```rust
    Text {
        keyword: &'static str,
        value: &'static str, // 仅明文变体（Latin1/Utf8）可断言
    },
```

把 `png_exif()`（第 77-98 行）的注释更新并在 `raw_subset` 追加 Text 期望：

```rust
fn png_exif() -> GoldenSample {
    // exiftool `-Make=OmniTest` 在 PNG 上写的是 `tEXt`（keyword="Make"，组 [PNG]），非 eXIf。
    // 自 PNG 文本关键字里程碑起，omni-meta 读取 tEXt → RawTags.text（此处断言 Make 可读出，
    // 兑现 commit f644533 记录的缺口）。Make 不投影 camera_make（仅 Author/Software/...
    // /Creation Time 投影），故 unified.camera_make 仍缺失。XMP dc:creator 照常解析。
    GoldenSample {
        name: "png_exif",
        bytes: include_bytes!("../samples/png_exif.png"),
        format: FileFormat::Png,
        unified: Unified {
            width: Some(80),
            height: Some(60),
            creator: Some("GoldenAuthor".into()),
            ..Default::default()
        },
        raw_subset: vec![
            GoldenRawTag::Xmp { prefix: "dc", name: "creator", value: "GoldenAuthor" },
            GoldenRawTag::Text { keyword: "Make", value: "OmniTest" },
        ],
    }
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p omni-meta --test golden`
Expected: 编译失败（`assert_raw_subset` 未处理 `Text` 变体）。

- [ ] **Step 4: 实现 assert 分支**

`omni-meta/tests/golden.rs` 的 `assert_raw_subset`（第 33-74 行）`match t` 内加分支：

```rust
            GoldenRawTag::Text { keyword, value } => {
                let hit = raw.text.iter().any(|t| {
                    t.keyword == *keyword
                        && matches!(
                            &t.value,
                            omni_meta::TextValue::Latin1(s) | omni_meta::TextValue::Utf8(s)
                            if s == *value
                        )
                });
                assert!(
                    hit,
                    "[{name}] 缺文本标签 {keyword}={value}\n实际 text={:?}",
                    raw.text
                );
            }
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p omni-meta --test golden`
Expected: `golden_samples_anchor_to_exiftool_truth` PASS（含四适配器一致 `assert_all_equal`，自动覆盖 PNG 含 tEXt 的差分一致性）。

- [ ] **Step 6: 提交**

```bash
git add omni-meta-fixtures/src/golden.rs omni-meta/tests/golden.rs
git commit -m "test(golden): GoldenRawTag::Text + png_exif 断言 tEXt Make 可读出（兑现 f644533 缺口）"
```

---

## Task 9: 全量门禁 + 收尾

**Files:** 无新增；运行验证。

- [ ] **Step 1: 格式与 lint**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: 无 diff、无警告。若 clippy 报新代码问题，就地修复后重跑。

- [ ] **Step 2: 全量测试（含四适配器差分）**

Run: `cargo test --workspace`
Expected: 全绿。重点确认 `omni-meta/tests/differential.rs` 与 `golden` 通过（PNG 文本块在 slice/push/blocking/seek 四路径一致）。

- [ ] **Step 3: no_std 裸机构建防腐**

Run: `cargo build -p omni-meta-core --no-default-features` （若本机装了 `thumbv7em-none-eabi` 可改用 `--target thumbv7em-none-eabi`，与 CI 一致）
Expected: 成功（新代码未引入 std/依赖）。

- [ ] **Step 4: fuzz 构建防腐**

Run: `cargo build --manifest-path fuzz/Cargo.toml` （或仓库既有的 fuzz 构建命令）
Expected: 成功。

- [ ] **Step 5: 最终确认无遗留 TODO/占位，提交收尾（若 Step 1 有 fmt 改动）**

```bash
git add -A
git commit -m "chore: PNG 文本关键字里程碑 fmt/clippy/no_std/fuzz 门禁通过" || echo "无收尾改动"
```

---

## 完成标准（Definition of Done）

- `RawTags.text` 读出 tEXt / 非 XMP 未压缩 iTXt（Utf8）/ 压缩 iTXt（CompressedUtf8）/ zTXt（CompressedLatin1）；XMP iTXt 路径不变。
- keyword 空/无 NUL 静默丢弃；>79 → `UnrecognizedValue`（zTXt 超长静默）。
- 压缩块不发 `CompressedChunkSkipped`；iTXt 非法 UTF-8 → `UnrecognizedValue`。
- Unified：`creator`/`software` PNG 末位兜底；新增 `title`/`description`/`copyright`；`Creation Time`→`created`（ISO/RFC1123/裸日期，末位兜底，不可解析留 raw 不报警）；压缩值不投影。
- Strip 默认剥离全部文本块（含 PII）+ 幂等，测试锁定；ROADMAP §4 纠偏并勾选。
- 黄金样本断言 tEXt `Make` 可读出（兑现 f644533）。
- 全 workspace fmt/clippy/test/no_std/fuzz 门禁通过；四适配器差分一致。
```
