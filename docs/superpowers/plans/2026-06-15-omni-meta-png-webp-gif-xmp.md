# omni-meta 阶段 3 实现计划：PNG / WebP / GIF + XMP codec

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 omni-meta 增加 PNG / WebP / GIF 三个 sans-io 格式解析器与一个非校验式 XMP codec，统一填充容器原生 width/height（含 JPEG SOF），并以跨适配器差分测试守门。

**Architecture:** 沿用现有 sans-io 契约——每个格式解析器实现 `MetaParser::pull`，对调用者发 `Demand`（`NeedBytes`/`Skip`/`Done`）、产 `Event`（`Payload`/`Field`/`Warning`）；`drive_slice`/`StreamDriver` 驱动并把 `Payload` 分派给 codec（`exif`/`xmp`），`Field` 写入 `Collector`，`finalize` 投影为 `Metadata`。三格式各自遍历自身结构，复用 `ByteCursor` 读定长头，不引入共享容器层。

**Tech Stack:** Rust edition 2024，workspace 两 crate（`omni-meta-core` no_std+alloc 零依赖 / `omni-meta` std facade），`#![forbid(unsafe_code)]`。

**规范来源:** `docs/superpowers/specs/2026-06-15-omni-meta-png-webp-gif-xmp-design.md`（全部章节）。

**本计划不含:** IPTC codec、inflate/zlib 解压（压缩块跳过并告警）、XMP 命名空间 URI 解析、ImageMagick `zTXt "Raw profile"` 约定、ICC / 视频容器 / Stripper。

**通用约定:**
- 所有命令在仓库根 `/home/min/dev/omni-meta` 下运行。
- 测试命令：核心包 `cargo test -p omni-meta-core`，facade `cargo test -p omni-meta`，全量 `cargo test`。
- no_std 守门：`cargo build -p omni-meta-core --no-default-features`。
- 每个 parser 的 `pull` 契约（与 `formats/jpeg.rs` 一致）：`consumed` 是本次窗口内已消费字节数；`NeedBytes(n)` 表示"在 `consumed` 之后还需 n 字节可读"；窗口不足时 `consumed` 指向尚未处理结构的起点。

---

## Task 1: 核心类型扩展（model + demand）

**Files:**
- Modify: `omni-meta-core/src/model.rs`
- Modify: `omni-meta-core/src/demand.rs`
- Modify: `omni-meta-core/src/lib.rs`

- [ ] **Step 1: 写失败测试（model 默认值）**

在 `omni-meta-core/src/model.rs` 的 `mod tests` 末尾追加：

```rust
    #[test]
    fn unified_has_dimensions_defaulting_none() {
        let u = Unified::default();
        assert_eq!(u.width, None);
        assert_eq!(u.height, None);
    }

    #[test]
    fn rawtags_has_empty_xmp_by_default() {
        let r = RawTags::default();
        assert!(r.xmp.is_empty());
    }

    #[test]
    fn field_and_xmp_property_construct() {
        let f = Field::Width(1920);
        assert_eq!(f, Field::Width(1920));
        let p = XmpProperty {
            prefix: String::from("tiff"),
            name: String::from("Orientation"),
            value: String::from("1"),
        };
        assert_eq!(p.name, "Orientation");
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p omni-meta-core --lib model 2>&1 | tail -20`
Expected: 编译失败（`Unified` 无 `width`、`RawTags` 无 `xmp`、无 `Field`/`XmpProperty`）。

- [ ] **Step 3: 扩展 model.rs 类型**

在 `omni-meta-core/src/model.rs` 中修改/新增：

`FileFormat` 改为：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileFormat {
    Jpeg,
    Png,
    Webp,
    Gif,
    Unknown,
}
```

在 `Value` 定义之后新增：

```rust
/// 容器原生字段（解析器直接从头部读出，不经 codec）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Width(u32),
    Height(u32),
}

/// 一条 XMP 属性。prefix 为惯用前缀（如 "tiff"），原样保留，不解析命名空间 URI。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmpProperty {
    pub prefix: String,
    pub name: String,
    pub value: String,
}
```

`RawTags` 改为：

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawTags {
    pub exif: Vec<ExifTag>,
    pub xmp: Vec<XmpProperty>,
}
```

`Unified` 改为（width/height 置于最前）：

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unified {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: Option<Orientation>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
}
```

`WarnKind` 增加变体：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarnKind {
    Truncated,
    BadExifHeader,
    UnreachableSection,
    UnrecognizedValue,
    /// 压缩块被跳过（本库零依赖、不解压）。
    CompressedChunkSkipped,
}
```

- [ ] **Step 4: 扩展 demand.rs**

在 `omni-meta-core/src/demand.rs` 中，把 `PayloadKind` 改为：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadKind {
    Exif,
    Xmp,
}
```

在文件顶部 `use` 区把 `Warning` 引入扩展为同时引入 `Field`：

```rust
use crate::model::{Field, Warning};
```

把 `Event` 改为：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<'a> {
    Payload { kind: PayloadKind, data: &'a [u8] },
    /// 容器原生字段（width/height 等）。
    Field(Field),
    Warning(Warning),
}
```

删除 `Event::Warning` 上方的 `#[allow(dead_code)]`（现在会被构造）。`Field` 变体若暂未被构造，给 `Field(Field)` 上方加 `#[allow(dead_code)]`（Task 4 起会构造，可届时移除）。

- [ ] **Step 5: 更新 lib.rs 公开 re-export**

在 `omni-meta-core/src/lib.rs` 的 `pub use model::{...}` 中加入 `XmpProperty`：

```rust
pub use model::{
    ExifTag, FileFormat, Metadata, Orientation, RawTags, Unified, Value, WarnKind, Warning,
    XmpProperty,
};
```

- [ ] **Step 6: 运行测试确认通过**

Run: `cargo test -p omni-meta-core 2>&1 | tail -20`
Expected: 全部通过（现有测试 + 新增 model 测试）。`Field` 的 `#[allow(dead_code)]` 抑制告警。

- [ ] **Step 7: 提交**

```bash
git add omni-meta-core/src/model.rs omni-meta-core/src/demand.rs omni-meta-core/src/lib.rs
git commit -m "feat: 核心类型扩展 (Field/XmpProperty/width/height/PayloadKind::Xmp)"
```

---

## Task 2: XMP codec（`codecs/xmp.rs`）

非校验式扫描器：UTF-8 解码后，先扫属性形式（`prefix:name="value"`），再扫元素形式（`<prefix:name>text</prefix:name>` 与 `rdf:li`）。纯函数，独立可测。

**Files:**
- Create: `omni-meta-core/src/codecs/xmp.rs`
- Modify: `omni-meta-core/src/codecs/mod.rs`

- [ ] **Step 1: 注册模块**

`omni-meta-core/src/codecs/mod.rs` 改为：

```rust
pub mod exif;
pub mod xmp;
```

- [ ] **Step 2: 写失败测试**

创建 `omni-meta-core/src/codecs/xmp.rs`，先只放测试（实现在 Step 4 补）：

```rust
//! XMP（RDF/XML）非校验式扫描：把包扫成 (prefix, name, value) 属性列表。
//! 不解析命名空间 URI，不做 DTD/CDATA；属性形式与元素形式（含 rdf:li）皆覆盖。

use alloc::string::String;
use alloc::vec::Vec;

use crate::limits::Limits;
use crate::model::{WarnKind, Warning, XmpProperty};

#[cfg(test)]
mod tests {
    use super::*;

    fn run(packet: &[u8]) -> (Vec<XmpProperty>, Vec<Warning>) {
        let mut out = Vec::new();
        let mut warns = Vec::new();
        decode(packet, &mut out, &mut warns, &Limits::default());
        (out, warns)
    }

    fn find<'a>(props: &'a [XmpProperty], prefix: &str, name: &str) -> Option<&'a str> {
        props
            .iter()
            .find(|p| p.prefix == prefix && p.name == name)
            .map(|p| p.value.as_str())
    }

    #[test]
    fn attribute_form() {
        let pkt = br#"<rdf:Description rdf:about="" xmlns:tiff="ns" tiff:Make="Acme" tiff:Orientation="6"/>"#;
        let (props, warns) = run(pkt);
        assert!(warns.is_empty());
        assert_eq!(find(&props, "tiff", "Make"), Some("Acme"));
        assert_eq!(find(&props, "tiff", "Orientation"), Some("6"));
        // 结构属性不应出现
        assert!(find(&props, "rdf", "about").is_none());
        assert!(find(&props, "xmlns", "tiff").is_none());
    }

    #[test]
    fn element_form_leaf() {
        let pkt = br#"<rdf:Description><tiff:Model>X100</tiff:Model></rdf:Description>"#;
        let (props, _) = run(pkt);
        assert_eq!(find(&props, "tiff", "Model"), Some("X100"));
    }

    #[test]
    fn rdf_alt_takes_first_li() {
        let pkt = br#"<dc:description><rdf:Alt><rdf:li xml:lang="x-default">hello</rdf:li><rdf:li xml:lang="fr">bonjour</rdf:li></rdf:Alt></dc:description>"#;
        let (props, _) = run(pkt);
        let vals: Vec<&str> = props
            .iter()
            .filter(|p| p.prefix == "dc" && p.name == "description")
            .map(|p| p.value.as_str())
            .collect();
        assert_eq!(vals, vec!["hello"]);
    }

    #[test]
    fn decodes_basic_entities() {
        let pkt = br#"<rdf:Description dc:rights="a &amp; b &lt;c&gt;"/>"#;
        let (props, _) = run(pkt);
        assert_eq!(find(&props, "dc", "rights"), Some("a & b <c>"));
    }

    #[test]
    fn invalid_utf8_warns_and_returns_empty() {
        let (props, warns) = run(&[0xFF, 0xFE, 0x00]);
        assert!(props.is_empty());
        assert_eq!(warns.len(), 1);
        assert_eq!(warns[0].kind, WarnKind::Truncated);
    }

    #[test]
    fn max_tags_caps_output() {
        let pkt = br#"<rdf:Description a:one="1" a:two="2" a:three="3"/>"#;
        let mut out = Vec::new();
        let mut warns = Vec::new();
        let limits = Limits { max_tags: 2, ..Limits::default() };
        decode(pkt, &mut out, &mut warns, &limits);
        assert_eq!(out.len(), 2);
    }
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test -p omni-meta-core --lib codecs::xmp 2>&1 | tail -20`
Expected: 编译失败（`decode` 未定义）。

- [ ] **Step 4: 实现 decode 及辅助函数**

在 `omni-meta-core/src/codecs/xmp.rs` 顶部 `use` 之后、`#[cfg(test)]` 之前插入实现：

```rust
/// 把一段 XMP 包扫成属性列表。无效 UTF-8 → 一条 Truncated 告警后返回。
pub fn decode(
    packet: &[u8],
    out: &mut Vec<XmpProperty>,
    warnings: &mut Vec<Warning>,
    limits: &Limits,
) {
    let text = match core::str::from_utf8(packet) {
        Ok(t) => t,
        Err(_) => {
            warnings.push(Warning { offset: 0, kind: WarnKind::Truncated });
            return;
        }
    };
    scan_attributes(text, out, limits);
    scan_elements(text, out, limits);
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b':' | b'.' | b'_' | b'-')
}

/// 结构性前缀，不作为属性产出。
fn is_structural_prefix(px: &str) -> bool {
    matches!(px, "xmlns" | "rdf" | "xml" | "x")
}

fn split_prefix(qname: &str) -> Option<(&str, &str)> {
    let idx = qname.find(':')?;
    let (px, rest) = qname.split_at(idx);
    let nm = &rest[1..];
    if px.is_empty() || nm.is_empty() {
        return None;
    }
    Some((px, nm))
}

/// 解码五个基本 XML 实体，其余原样保留。
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return String::from(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        if let Some(semi) = tail.find(';') {
            let ent = &tail[1..semi];
            match ent {
                "lt" => out.push('<'),
                "gt" => out.push('>'),
                "amp" => out.push('&'),
                "quot" => out.push('"'),
                "apos" => out.push('\''),
                _ => out.push_str(&tail[..=semi]), // 未知实体原样保留
            }
            rest = &tail[semi + 1..];
        } else {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

fn push_prop(out: &mut Vec<XmpProperty>, limits: &Limits, prefix: &str, name: &str, value: &str) {
    if out.len() >= limits.max_tags {
        return;
    }
    out.push(XmpProperty {
        prefix: String::from(prefix),
        name: String::from(name),
        value: decode_entities(value),
    });
}

/// 扫描所有 `name="value"` / `name='value'` 属性对（任意元素上）。
fn scan_attributes(text: &str, out: &mut Vec<XmpProperty>, limits: &Limits) {
    let b = text.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'=' && (b[i + 1] == b'"' || b[i + 1] == b'\'') {
            let quote = b[i + 1];
            // 名字：跳过 = 前空白，再回退取名字符
            let mut ne = i;
            while ne > 0 && b[ne - 1].is_ascii_whitespace() {
                ne -= 1;
            }
            let mut ns = ne;
            while ns > 0 && is_name_byte(b[ns - 1]) {
                ns -= 1;
            }
            // 值
            let vs = i + 2;
            let mut ve = vs;
            while ve < b.len() && b[ve] != quote {
                ve += 1;
            }
            if ve >= b.len() {
                break;
            }
            if ns < ne {
                let name = &text[ns..ne];
                let value = &text[vs..ve];
                if let Some((px, nm)) = split_prefix(name) {
                    if !is_structural_prefix(px) {
                        push_prop(out, limits, px, nm, value);
                    }
                }
            }
            i = ve + 1;
        } else {
            i += 1;
        }
    }
}

struct Frame<'a> {
    prefix: &'a str,
    name: &'a str,
    is_alt: bool,
    alt_taken: bool,
}

/// 扫描元素形式 `<prefix:name>text</...>` 与 rdf 容器中的 `rdf:li`。
fn scan_elements(text: &str, out: &mut Vec<XmpProperty>, limits: &Limits) {
    let b = text.as_bytes();
    let mut stack: Vec<Frame> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        if i + 1 >= b.len() {
            break;
        }
        // 注释 / PI / 声明：跳到 '>'
        if b[i + 1] == b'!' || b[i + 1] == b'?' {
            i = find_gt(b, i);
            continue;
        }
        // 闭合标签
        if b[i + 1] == b'/' {
            let (px, nm, end) = parse_qname(text, i + 2);
            if let Some(f) = stack.last() {
                if f.prefix == px && f.name == nm {
                    stack.pop();
                }
            }
            i = find_gt(b, end);
            continue;
        }
        // 开始标签
        let (px, nm, after_name) = parse_qname(text, i + 1);
        let gt = find_gt(b, after_name);
        let self_closing = gt > 0 && b[gt - 1] == b'/';
        let content_start = gt + 1; // '>' 之后
        if self_closing || px.is_empty() {
            i = content_start;
            continue;
        }
        // 看内容是否为纯文本叶子
        let mut j = content_start;
        while j < b.len() && b[j] != b'<' {
            j += 1;
        }
        let content = &text[content_start..j.min(b.len())];
        let is_leaf = j < b.len() && !content.trim().is_empty();
        if is_leaf {
            record_leaf(px, nm, content, &mut stack, out, limits);
            i = j; // 后续闭合标签由顶层处理（不匹配栈顶则忽略）
        } else {
            let is_alt = px == "rdf" && nm == "Alt";
            stack.push(Frame { prefix: px, name: nm, is_alt, alt_taken: false });
            i = content_start;
        }
    }
}

fn record_leaf<'a>(
    px: &'a str,
    nm: &'a str,
    content: &str,
    stack: &mut [Frame<'a>],
    out: &mut Vec<XmpProperty>,
    limits: &Limits,
) {
    let val = content.trim();
    if px == "rdf" && nm == "li" {
        // 若直接容器是 rdf:Alt，只取首个
        if let Some(top) = stack.last_mut() {
            if top.is_alt {
                if top.alt_taken {
                    return;
                }
                top.alt_taken = true;
            }
        }
        // 归属到最近的非 rdf 祖先属性名
        if let Some(prop) = stack.iter().rev().find(|f| f.prefix != "rdf") {
            push_prop(out, limits, prop.prefix, prop.name, val);
        }
    } else if px != "rdf" && px != "x" {
        push_prop(out, limits, px, nm, val);
    }
}

/// 从 `start`（'<' 或 '/' 之后）解析限定名，返回 (prefix, name, 名字结束后的索引)。
/// 无前缀时 prefix 为空串。
fn parse_qname(text: &str, start: usize) -> (&str, &str, usize) {
    let b = text.as_bytes();
    let mut e = start;
    while e < b.len() && is_name_byte(b[e]) {
        e += 1;
    }
    let qname = &text[start..e.min(b.len())];
    match split_prefix(qname) {
        Some((px, nm)) => (px, nm, e),
        None => ("", qname, e),
    }
}

/// 返回从 `from` 起第一个 '>' 的索引；找不到则返回 b.len()。
fn find_gt(b: &[u8], from: usize) -> usize {
    let mut k = from;
    while k < b.len() && b[k] != b'>' {
        k += 1;
    }
    k
}
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p omni-meta-core --lib codecs::xmp 2>&1 | tail -20`
Expected: 6 个测试全部 PASS。

- [ ] **Step 6: no_std 守门**

Run: `cargo build -p omni-meta-core --no-default-features 2>&1 | tail -5`
Expected: 编译成功。

- [ ] **Step 7: 提交**

```bash
git add omni-meta-core/src/codecs/xmp.rs omni-meta-core/src/codecs/mod.rs
git commit -m "feat: XMP codec 非校验式扫描 (属性形式/元素形式/rdf:li/实体)"
```

---

## Task 3: 驱动层接入 Field 与 Xmp（`driver.rs`）

`Collector` 收集 xmp/width/height；`handle` 路由 `Payload{Xmp}`→`xmp::decode`、`Field`→记录维度；`finalize` 把维度写入 `Unified`、xmp 写入 `RawTags`。

**Files:**
- Modify: `omni-meta-core/src/driver.rs`

- [ ] **Step 1: 写失败测试**

在 `omni-meta-core/src/driver.rs` 的 `mod tests` 末尾追加（用脚本解析器直接发 Field/Xmp 事件验证收集）：

```rust
    use crate::model::{Field, XmpProperty};
    use crate::demand::PayloadKind;

    /// 一次性发出 Width/Height Field + 一个 XMP 载荷后 Done 的假解析器。
    struct FieldXmpEmitter {
        done: bool,
    }
    impl MetaParser for FieldXmpEmitter {
        fn pull<'a>(&mut self, input: &'a [u8]) -> crate::demand::PullResult<'a> {
            use crate::demand::PullResult;
            self.done = true;
            let events = vec![
                Event::Field(Field::Width(1920)),
                Event::Field(Field::Height(1080)),
                Event::Payload {
                    kind: PayloadKind::Xmp,
                    data: br#"<rdf:Description tiff:Make="Acme"/>"#,
                },
            ];
            PullResult { demand: Demand::Done, consumed: input.len(), events }
        }
    }

    #[test]
    fn collector_records_fields_and_xmp() {
        let buf = [0u8; 4];
        let mut p = FieldXmpEmitter { done: false };
        let col = drive_slice(&buf, &mut p, Limits::default());
        let meta = finalize(col, FileFormat::Png);
        assert_eq!(meta.unified.width, Some(1920));
        assert_eq!(meta.unified.height, Some(1080));
        assert_eq!(
            meta.raw.xmp,
            vec![XmpProperty {
                prefix: String::from("tiff"),
                name: String::from("Make"),
                value: String::from("Acme"),
            }]
        );
    }
```

注：测试用到 `String`，`mod tests` 顶部已 `use alloc::string::String;`？若无则在该测试 `use` 处补 `use alloc::string::String;`（文件测试模块已 `use alloc::vec::Vec;` 与 `use alloc::vec;`）。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p omni-meta-core --lib driver::tests::collector_records_fields_and_xmp 2>&1 | tail -20`
Expected: 编译失败（`Collector` 无 width/height/xmp 字段；`handle` 不识别 Field/Xmp）。

- [ ] **Step 3: 扩展 Collector 与 finalize**

在 `omni-meta-core/src/driver.rs` 顶部 `use` 区把 model 引入扩展：

```rust
use crate::model::{ExifTag, Field, FileFormat, Metadata, RawTags, WarnKind, Warning, XmpProperty};
```

`Collector` 结构改为：

```rust
pub struct Collector {
    pub exif: Vec<ExifTag>,
    pub xmp: Vec<XmpProperty>,
    pub warnings: Vec<Warning>,
    width: Option<u32>,
    height: Option<u32>,
    limits: Limits,
}
```

`Collector::handle` 改为：

```rust
impl Collector {
    fn handle(&mut self, ev: Event<'_>) {
        match ev {
            Event::Payload { kind: PayloadKind::Exif, data } => {
                codecs::exif::decode(data, &mut self.exif, &mut self.warnings, &self.limits);
            }
            Event::Payload { kind: PayloadKind::Xmp, data } => {
                codecs::xmp::decode(data, &mut self.xmp, &mut self.warnings, &self.limits);
            }
            Event::Field(Field::Width(w)) => {
                if self.width.is_none() {
                    self.width = Some(w);
                }
            }
            Event::Field(Field::Height(h)) => {
                if self.height.is_none() {
                    self.height = Some(h);
                }
            }
            Event::Warning(w) => self.warnings.push(w),
        }
    }
}
```

- [ ] **Step 4: 更新所有 Collector 构造点与 finalize**

`finalize` 改为（先取出维度，normalize 后写入）：

```rust
pub(crate) fn finalize(col: Collector, format: FileFormat) -> Metadata {
    let (width, height) = (col.width, col.height);
    let raw = RawTags { exif: col.exif, xmp: col.xmp };
    let mut warnings = col.warnings;
    let mut unified = normalize(&raw, &mut warnings);
    if unified.width.is_none() {
        unified.width = width;
    }
    if unified.height.is_none() {
        unified.height = height;
    }
    Metadata { unified, raw, warnings, format }
}
```

`StreamDriver::new` 中构造 `Collector` 的位置改为：

```rust
            collector: Collector {
                exif: Vec::new(),
                xmp: Vec::new(),
                warnings: Vec::new(),
                width: None,
                height: None,
                limits,
            },
```

`drive_slice` 中构造 `Collector` 的位置改为：

```rust
    let mut col = Collector {
        exif: Vec::new(),
        xmp: Vec::new(),
        warnings: Vec::new(),
        width: None,
        height: None,
        limits,
    };
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p omni-meta-core 2>&1 | tail -20`
Expected: 全部通过（新增 collector 测试 + 现有测试）。

- [ ] **Step 6: 提交**

```bash
git add omni-meta-core/src/driver.rs
git commit -m "feat: 驱动层接入 Field(width/height) 与 Payload::Xmp 分派"
```

---

## Task 4: JPEG SOF 维度（`formats/jpeg.rs`）

SOF 标记（`0xC0..=0xCF` 排除 `0xC4`/`0xC8`/`0xCC`）读 height/width 发 `Field`，随后继续扫描。

**Files:**
- Modify: `omni-meta-core/src/formats/jpeg.rs`

- [ ] **Step 1: 写失败测试**

在 `omni-meta-core/src/formats/jpeg.rs` 的 `mod tests` 末尾追加：

```rust
    /// SOF0 段应发出 Width/Height Field，并继续到 EOI。
    #[test]
    fn sof_emits_dimensions() {
        use crate::demand::Field as _; // 防止未用告警（无副作用）
        // SOI + SOF0(len=17: precision1 + height2 + width2 + ...) + EOI
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]); // SOI
        j.extend_from_slice(&[0xFF, 0xC0]); // SOF0
        // 段长 = 2(len) + 1(precision) + 2(height) + 2(width) + 6(1 组件) = 13
        j.extend_from_slice(&13u16.to_be_bytes());
        j.push(8); // precision
        j.extend_from_slice(&1080u16.to_be_bytes()); // height
        j.extend_from_slice(&1920u16.to_be_bytes()); // width
        j.extend_from_slice(&[1, 0x11, 0]); // 1 个组件
        j.extend_from_slice(&[0xFF, 0xD9]); // EOI

        let mut p = JpegParser::new();
        let res = p.pull(&j);
        let mut w = None;
        let mut h = None;
        for ev in &res.events {
            if let Event::Field(crate::model::Field::Width(x)) = ev {
                w = Some(*x);
            }
            if let Event::Field(crate::model::Field::Height(x)) = ev {
                h = Some(*x);
            }
        }
        assert_eq!(w, Some(1920));
        assert_eq!(h, Some(1080));
    }
```

注：删除上面那行 `use crate::demand::Field as _;`（无此项），直接匹配 `crate::model::Field`。最终测试不需要该 use——请勿包含它。

修正后的测试体首行不写该 use，直接构造字节并断言。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p omni-meta-core --lib formats::jpeg::tests::sof_emits_dimensions 2>&1 | tail -20`
Expected: FAIL（当前 SOF 走默认分支 → Skip，无 Field 事件，w/h 为 None）。

- [ ] **Step 3: 在 jpeg.rs 顶部引入 Field**

把 `omni-meta-core/src/formats/jpeg.rs` 的 use 行改为：

```rust
use crate::demand::{Demand, Event, MetaParser, PayloadKind, PullResult};
use crate::model::Field;
```

- [ ] **Step 4: 处理 SOF 标记**

在 `pull` 的 `match marker` 中，`_ =>` 默认分支**之前**插入新分支（处理 SOF；排除 DHT/JPG/DAC）：

```rust
                0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                    // SOF：读 precision(1) + height(2 BE) + width(2 BE)
                    if rest.len() < after + 2 {
                        return PullResult { demand: Demand::NeedBytes(after + 2), consumed: pos, events };
                    }
                    let len = u16::from_be_bytes([rest[after], rest[after + 1]]) as usize;
                    if len < 2 {
                        self.done = true;
                        return PullResult { demand: Demand::Done, consumed: pos, events };
                    }
                    let body_start = after + 2;
                    // 需要 body 前 5 字节：precision(1)+height(2)+width(2)
                    if rest.len() < body_start + 5 {
                        return PullResult { demand: Demand::NeedBytes(body_start + 5), consumed: pos, events };
                    }
                    let h = u16::from_be_bytes([rest[body_start + 1], rest[body_start + 2]]) as u32;
                    let w = u16::from_be_bytes([rest[body_start + 3], rest[body_start + 4]]) as u32;
                    events.push(Event::Field(Field::Width(w)));
                    events.push(Event::Field(Field::Height(h)));
                    // 跳过整段剩余（消费段头，Skip body）
                    let body_len = len - 2;
                    return PullResult {
                        demand: Demand::Skip(body_len as u64),
                        consumed: pos + body_start,
                        events,
                    };
                }
```

说明：发出 Field 后 `Skip(body_len)` 跳过 SOF 段体，驱动续跑到后续段（含 EOI）。

- [ ] **Step 5: 移除 demand.rs 中 Field 的 dead_code 抑制**

`omni-meta-core/src/demand.rs` 中 `Event::Field(Field)` 上方若有 `#[allow(dead_code)]` 现可删除（已被构造）。

- [ ] **Step 6: 运行测试确认通过**

Run: `cargo test -p omni-meta-core 2>&1 | tail -20`
Expected: 全部通过。

- [ ] **Step 7: 提交**

```bash
git add omni-meta-core/src/formats/jpeg.rs omni-meta-core/src/demand.rs
git commit -m "feat: JpegParser SOF 维度解析 → Field(width/height)"
```

---

## Task 5: 格式分派收口（probe + parser_for）

引入 `PROBE_MAX` 与 `parser_for`，把 slice/push 的分派统一到一处（本任务仅含 JPEG，行为不变）。

**Files:**
- Modify: `omni-meta-core/src/probe.rs`
- Modify: `omni-meta-core/src/adapters/slice.rs`
- Modify: `omni-meta-core/src/adapters/push.rs`

- [ ] **Step 1: 写失败测试（probe 占位 + parser_for 存在）**

在 `omni-meta-core/src/probe.rs` 的 `mod tests` 末尾追加：

```rust
    #[test]
    fn probe_max_covers_signatures() {
        assert!(PROBE_MAX >= 12);
    }

    #[test]
    fn parser_for_jpeg_some_unknown_none() {
        assert!(parser_for(FileFormat::Jpeg).is_some());
        assert!(parser_for(FileFormat::Unknown).is_none());
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p omni-meta-core --lib probe 2>&1 | tail -20`
Expected: 编译失败（无 `PROBE_MAX`/`parser_for`）。

- [ ] **Step 3: 扩展 probe.rs**

把 `omni-meta-core/src/probe.rs` 顶部改为：

```rust
//! 魔数嗅探 + 格式→解析器分派。本阶段识别 JPEG/PNG/WebP/GIF。

use alloc::boxed::Box;

use crate::demand::MetaParser;
use crate::model::FileFormat;

/// 各格式签名最长字节数（WebP "RIFF"+4+"WEBP" = 12）。
pub(crate) const PROBE_MAX: usize = 12;
```

`probe` 函数保持仅 JPEG（PNG/WebP/GIF 在各自任务加分支），暂不改。

在 `probe` 函数之后新增 `parser_for`：

```rust
/// 把已探测的格式映射到对应解析器。Unknown / 尚未实现的格式 → None。
pub(crate) fn parser_for(fmt: FileFormat) -> Option<Box<dyn MetaParser>> {
    match fmt {
        FileFormat::Jpeg => Some(Box::new(crate::formats::jpeg::JpegParser::new())),
        _ => None,
    }
}
```

- [ ] **Step 4: 重构 slice.rs 使用 parser_for**

把 `omni-meta-core/src/adapters/slice.rs` 的 `read_slice` 改为：

```rust
//! read_slice：全内存/零拷贝随机访问适配器。

use crate::driver::{drive_slice, finalize};
use crate::error::Error;
use crate::limits::Limits;
use crate::model::Metadata;
use crate::probe::{parser_for, probe};

/// 解析选项。
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    pub limits: Limits,
}

/// 从一整块内存缓冲解析元数据。无法识别格式时返回 Err。
pub fn read_slice(buf: &[u8], opts: Options) -> Result<Metadata, Error> {
    let fmt = probe(buf);
    match parser_for(fmt.clone()) {
        Some(mut parser) => {
            let col = drive_slice(buf, parser.as_mut(), opts.limits);
            Ok(finalize(col, fmt))
        }
        None => Err(Error::UnrecognizedFormat),
    }
}
```

（`finalize` 现需对 slice 公开；它已是 `pub(crate)`，直接 `use`。删除原 `FileFormat`/`JpegParser` 引入。）

- [ ] **Step 5: 重构 push.rs 使用 parser_for + PROBE_MAX**

把 `omni-meta-core/src/adapters/push.rs` 顶部 use 与探测逻辑改为：

use 区改为：

```rust
use alloc::vec::Vec;

use crate::adapters::slice::Options;
use crate::driver::{finalize, Outcome, StreamDriver};
use crate::error::Error;
use crate::model::{FileFormat, Metadata};
use crate::probe::{parser_for, probe, PROBE_MAX};
```

删除常量 `const PROBE_MIN: usize = 2;`。

`feed` 改为：

```rust
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Outcome, Error> {
        if self.failed {
            return Err(Error::UnrecognizedFormat);
        }
        if let Some(d) = self.driver.as_mut() {
            return Ok(d.feed(chunk));
        }
        self.pre.extend_from_slice(chunk);
        let fmt = probe(&self.pre);
        if fmt == FileFormat::Unknown {
            if self.pre.len() >= PROBE_MAX {
                self.failed = true;
                return Err(Error::UnrecognizedFormat);
            }
            return Ok(Outcome::Need(PROBE_MAX - self.pre.len()));
        }
        self.start_driver(fmt)
    }
```

`finish` 中调用 `start_driver` 处改为传入末次探测结果：

```rust
    pub fn finish(mut self) -> Result<Metadata, Error> {
        if self.failed {
            return Err(Error::UnrecognizedFormat);
        }
        if self.driver.is_none() {
            let fmt = probe(&self.pre);
            let _ = self.start_driver(fmt);
            if self.failed || self.driver.is_none() {
                return Err(Error::UnrecognizedFormat);
            }
        }
        let driver = self.driver.take().unwrap();
        let col = driver.finish();
        Ok(finalize(col, self.format))
    }
```

`start_driver` 改为接收已探测格式、用 `parser_for`：

```rust
    /// 用已探测格式建驱动；不可识别则置 failed。
    fn start_driver(&mut self, fmt: FileFormat) -> Result<Outcome, Error> {
        match parser_for(fmt.clone()) {
            Some(parser) => {
                self.format = fmt;
                let mut d = StreamDriver::new(parser, self.limits_opts.limits);
                let pre = core::mem::take(&mut self.pre);
                let outcome = d.feed(&pre);
                self.driver = Some(d);
                Ok(outcome)
            }
            None => {
                self.failed = true;
                Err(Error::UnrecognizedFormat)
            }
        }
    }
```

（`StreamDriver::new` 已接收 `Box<dyn MetaParser>`，`parser_for` 直接给出，无需再 `Box::new`。删除原 `use alloc::boxed::Box;` 若不再使用。）

- [ ] **Step 6: 运行测试确认通过**

Run: `cargo test -p omni-meta-core 2>&1 | tail -20`
Expected: 全部通过（probe 新测试 + 现有 push/slice 测试不变）。

- [ ] **Step 7: facade 与 no_std 守门**

Run: `cargo test -p omni-meta 2>&1 | tail -10 && cargo build -p omni-meta-core --no-default-features 2>&1 | tail -3`
Expected: 全绿、no_std 编译成功。

- [ ] **Step 8: 提交**

```bash
git add omni-meta-core/src/probe.rs omni-meta-core/src/adapters/slice.rs omni-meta-core/src/adapters/push.rs
git commit -m "refactor: 格式分派收口到 probe::parser_for + PROBE_MAX"
```

---

## Task 6: PNG 解析器（`formats/png.rs`）

签名 + chunk 遍历：IHDR→维度、eXIf→EXIF、iTXt(XMP,未压缩)→XMP、IEND→Done，其余 Skip。

**Files:**
- Create: `omni-meta-core/src/formats/png.rs`
- Modify: `omni-meta-core/src/formats/mod.rs`
- Modify: `omni-meta-core/src/probe.rs`
- Test: `omni-meta/tests/differential.rs`

- [ ] **Step 1: 注册模块**

`omni-meta-core/src/formats/mod.rs` 改为：

```rust
pub mod jpeg;
pub mod png;
```

- [ ] **Step 2: 写失败测试（PNG 单测）**

创建 `omni-meta-core/src/formats/png.rs`，先放头部 + 测试：

```rust
//! PNG chunk 遍历（增量状态机）：8 字节签名后逐 chunk 推进。
//! IHDR 发 Width/Height；eXIf 发 Exif 载荷；iTXt(XML:com.adobe.xmp，未压缩)发 Xmp 载荷；
//! 压缩文本块（flag=1）告警并跳过；IEND 发 Done；其余 chunk Skip(len+crc)。

use alloc::vec::Vec;

use crate::demand::{Demand, Event, MetaParser, PayloadKind, PullResult};
use crate::model::{Field, WarnKind, Warning};

const SIG: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

#[cfg(test)]
mod tests {
    use super::*;

    /// 拼一个 PNG chunk：len(4 BE) + type(4) + data + crc(4，置 0)。
    fn chunk(ctype: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend_from_slice(&(data.len() as u32).to_be_bytes());
        c.extend_from_slice(ctype);
        c.extend_from_slice(data);
        c.extend_from_slice(&[0, 0, 0, 0]); // crc 不校验
        c
    }

    fn ihdr(w: u32, h: u32) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&w.to_be_bytes());
        d.extend_from_slice(&h.to_be_bytes());
        d.extend_from_slice(&[8, 6, 0, 0, 0]); // bitdepth/colortype/...
        chunk(b"IHDR", &d)
    }

    fn itxt_xmp(packet: &[u8], compressed: bool) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(b"XML:com.adobe.xmp");
        d.push(0); // keyword NUL
        d.push(if compressed { 1 } else { 0 }); // compression flag
        d.push(0); // compression method
        d.push(0); // language tag NUL
        d.push(0); // translated keyword NUL
        d.extend_from_slice(packet);
        chunk(b"iTXt", &d)
    }

    fn full_png() -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(1920, 1080));
        p.extend_from_slice(&chunk(b"eXIf", &[0xAA, 0xBB, 0xCC])); // 占位 TIFF
        p.extend_from_slice(&itxt_xmp(br#"<rdf:Description tiff:Make="Acme"/>"#, false));
        p.extend_from_slice(&chunk(b"IDAT", &[1, 2, 3, 4]));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        p
    }

    fn collect(buf: &[u8]) -> crate::driver::Collector {
        let mut p = PngParser::new();
        crate::driver::drive_slice(buf, &mut p, crate::limits::Limits::default())
    }

    #[test]
    fn extracts_dimensions_exif_xmp() {
        let col = collect(&full_png());
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
        // eXIf 载荷被送入 exif::decode（占位 TIFF → BadExifHeader? 不，3 字节非 II/MM → 告警）
        // 为避免 EXIF 解码噪声，这里只断言 XMP 与维度经由 finalize。
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Png);
        assert_eq!(meta.unified.width, Some(1920));
        assert_eq!(meta.unified.height, Some(1080));
        assert!(meta.raw.xmp.iter().any(|x| x.prefix == "tiff" && x.name == "Make" && x.value == "Acme"));
    }

    #[test]
    fn compressed_itxt_warns_and_skips() {
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(2, 2));
        p.extend_from_slice(&itxt_xmp(b"ignored", true)); // compressed
        p.extend_from_slice(&chunk(b"IEND", &[]));
        let col = collect(&p);
        assert!(col.warnings.iter().any(|w| w.kind == WarnKind::CompressedChunkSkipped));
        assert!(col.xmp.is_empty());
    }

    #[test]
    fn non_png_signature_done_no_events() {
        let mut p = PngParser::new();
        let res = p.pull(&[0u8; 8]);
        assert_eq!(res.demand, Demand::Done);
        assert!(res.events.is_empty());
    }

    #[test]
    fn truncated_chunk_requests_more() {
        // 签名 + 声称 len=100 的 eXIf，但数据不足
        let mut p = Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&100u32.to_be_bytes());
        p.extend_from_slice(b"eXIf");
        p.extend_from_slice(&[1, 2, 3]); // 远不足 100
        let mut parser = PngParser::new();
        let res = parser.pull(&p);
        assert!(matches!(res.demand, Demand::NeedBytes(_)));
    }
}
```

注：为避免 `eXIf` 占位 3 字节触发 EXIF 头告警污染 `extracts_dimensions_exif_xmp` 的 `warnings.is_empty()` 断言，把该断言改为允许仅 `BadExifHeader`：将 `assert!(col.warnings.is_empty(), ...)` 替换为
`assert!(col.warnings.iter().all(|w| w.kind == WarnKind::BadExifHeader), "warnings: {:?}", col.warnings);`

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test -p omni-meta-core --lib formats::png 2>&1 | tail -20`
Expected: 编译失败（`PngParser` 未定义）。

- [ ] **Step 4: 实现 PngParser**

在 `omni-meta-core/src/formats/png.rs` 的 `const SIG` 之后、`#[cfg(test)]` 之前插入：

```rust
#[derive(Debug, Default)]
pub struct PngParser {
    saw_sig: bool,
    done: bool,
}

impl PngParser {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MetaParser for PngParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        let mut events: Vec<Event<'a>> = Vec::new();
        if self.done {
            return PullResult { demand: Demand::Done, consumed: 0, events };
        }
        let mut pos = 0usize;
        if !self.saw_sig {
            if input.len() < 8 {
                return PullResult { demand: Demand::NeedBytes(8), consumed: 0, events };
            }
            if input[..8] != SIG {
                self.done = true;
                return PullResult { demand: Demand::Done, consumed: 0, events };
            }
            self.saw_sig = true;
            pos = 8;
        }

        loop {
            let rest = &input[pos..];
            if rest.len() < 8 {
                return PullResult { demand: Demand::NeedBytes(8), consumed: pos, events };
            }
            let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            let ctype = &rest[4..8];

            if ctype == b"IEND" {
                self.done = true;
                return PullResult { demand: Demand::Done, consumed: pos + 8, events };
            }

            let is_meta = ctype == b"IHDR" || ctype == b"eXIf" || ctype == b"iTXt";
            if is_meta {
                // 须整读 header(8)+data(len)+crc(4)
                let need = match 8usize.checked_add(len).and_then(|v| v.checked_add(4)) {
                    Some(v) => v,
                    None => {
                        // 长度溢出 → 当作不可读，跳过数据+crc
                        self.done = true;
                        return PullResult { demand: Demand::Done, consumed: pos, events };
                    }
                };
                if rest.len() < need {
                    return PullResult { demand: Demand::NeedBytes(need), consumed: pos, events };
                }
                let data = &rest[8..8 + len];
                match ctype {
                    b"IHDR" => {
                        if data.len() >= 8 {
                            let w = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                            let h = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
                            events.push(Event::Field(Field::Width(w)));
                            events.push(Event::Field(Field::Height(h)));
                        }
                    }
                    b"eXIf" => {
                        events.push(Event::Payload { kind: PayloadKind::Exif, data });
                    }
                    b"iTXt" => {
                        handle_itxt(data, pos, &mut events);
                    }
                    _ => {}
                }
                pos += need; // 跳过 crc 一并消费
                continue;
            }

            // 可跳过 chunk：消费 8 字节头，Skip(data + crc)
            let skip = (len as u64).saturating_add(4);
            return PullResult { demand: Demand::Skip(skip), consumed: pos + 8, events };
        }
    }
}

/// 解析 iTXt 数据；仅当 keyword 为 XMP 且未压缩时发 Xmp 载荷，压缩则告警。
fn handle_itxt<'a>(data: &'a [u8], chunk_pos: usize, events: &mut Vec<Event<'a>>) {
    // keyword\0 compflag(1) compmethod(1) lang\0 transkw\0 text
    let kw_end = match data.iter().position(|&b| b == 0) {
        Some(p) => p,
        None => return,
    };
    if &data[..kw_end] != b"XML:com.adobe.xmp" {
        return;
    }
    let after_kw = &data[kw_end + 1..];
    if after_kw.len() < 2 {
        return;
    }
    let compressed = after_kw[0] != 0;
    if compressed {
        events.push(Event::Warning(Warning {
            offset: chunk_pos as u64,
            kind: WarnKind::CompressedChunkSkipped,
        }));
        return;
    }
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
    events.push(Event::Payload { kind: PayloadKind::Xmp, data: text });
}
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p omni-meta-core --lib formats::png 2>&1 | tail -20`
Expected: 4 个 PNG 测试 PASS。

- [ ] **Step 6: 接入 probe 与 parser_for**

`omni-meta-core/src/probe.rs` 的 `probe` 函数改为（在 JPEG 判定后加 PNG）：

```rust
pub fn probe(buf: &[u8]) -> FileFormat {
    if buf.len() >= 2 && buf[0] == 0xFF && buf[1] == 0xD8 {
        return FileFormat::Jpeg;
    }
    if buf.len() >= 8
        && buf[..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
    {
        return FileFormat::Png;
    }
    FileFormat::Unknown
}
```

`parser_for` 加 PNG 分支：

```rust
        FileFormat::Png => Some(Box::new(crate::formats::png::PngParser::new())),
```

在 `probe.rs` 的 `mod tests` 加：

```rust
    #[test]
    fn detects_png_signature() {
        let sig = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        assert_eq!(probe(&sig), FileFormat::Png);
        assert!(parser_for(FileFormat::Png).is_some());
    }
```

- [ ] **Step 7: 加 PNG 差分测试**

在 `omni-meta/tests/differential.rs` 末尾追加：

```rust
fn png_chunk(ctype: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(&(data.len() as u32).to_be_bytes());
    c.extend_from_slice(ctype);
    c.extend_from_slice(data);
    c.extend_from_slice(&[0, 0, 0, 0]);
    c
}

fn fixture_png() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
    // IHDR 1920x1080
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&1920u32.to_be_bytes());
    ihdr.extend_from_slice(&1080u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    p.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
    // eXIf：完整 TIFF（复用 make_tiff）
    p.extend_from_slice(&png_chunk(b"eXIf", &make_tiff()));
    // iTXt XMP（未压缩）
    let mut itxt = Vec::new();
    itxt.extend_from_slice(b"XML:com.adobe.xmp");
    itxt.push(0);
    itxt.push(0);
    itxt.push(0);
    itxt.push(0);
    itxt.push(0);
    itxt.extend_from_slice(br#"<rdf:Description tiff:Make="Acme"/>"#);
    p.extend_from_slice(&png_chunk(b"iTXt", &itxt));
    p.extend_from_slice(&png_chunk(b"IDAT", &[1, 2, 3, 4]));
    p.extend_from_slice(&png_chunk(b"IEND", &[]));
    p
}

#[test]
fn differential_png() {
    assert_all_equal(&fixture_png());
}
```

- [ ] **Step 8: 运行全部测试**

Run: `cargo test 2>&1 | tail -25`
Expected: 全绿，含 `differential_png`（slice/blocking/seek/push 四路一致）。

- [ ] **Step 9: no_std 守门 + 提交**

```bash
cargo build -p omni-meta-core --no-default-features 2>&1 | tail -3
git add omni-meta-core/src/formats/png.rs omni-meta-core/src/formats/mod.rs omni-meta-core/src/probe.rs omni-meta/tests/differential.rs
git commit -m "feat: PNG 解析器 (IHDR 维度 + eXIf + iTXt-XMP) + 差分测试"
```

---

## Task 7: WebP 解析器（`formats/webp.rs`）

RIFF 遍历：VP8X/VP8/VP8L→维度、EXIF→EXIF、`XMP `→XMP，其余 Skip（偶数对齐）。

**Files:**
- Create: `omni-meta-core/src/formats/webp.rs`
- Modify: `omni-meta-core/src/formats/mod.rs`
- Modify: `omni-meta-core/src/probe.rs`
- Test: `omni-meta/tests/differential.rs`

- [ ] **Step 1: 注册模块**

`omni-meta-core/src/formats/mod.rs` 加 `pub mod webp;`。

- [ ] **Step 2: 写失败测试**

创建 `omni-meta-core/src/formats/webp.rs`，先放头部 + 测试：

```rust
//! WebP（RIFF）chunk 遍历：RIFF/WEBP 头后逐 chunk 推进。
//! VP8X/VP8/VP8L 发维度；EXIF 发 Exif 载荷；"XMP " 发 Xmp 载荷；其余 Skip。
//! 每 chunk 前进 size + (size & 1)（RIFF 偶数对齐）。

use alloc::vec::Vec;

use crate::demand::{Demand, Event, MetaParser, PayloadKind, PullResult};
use crate::model::Field;

#[cfg(test)]
mod tests {
    use super::*;

    fn riff_chunk(fourcc: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend_from_slice(fourcc);
        c.extend_from_slice(&(data.len() as u32).to_le_bytes());
        c.extend_from_slice(data);
        if data.len() % 2 == 1 {
            c.push(0); // 偶数对齐
        }
        c
    }

    fn vp8x_data(w: u32, h: u32) -> Vec<u8> {
        let mut d = vec![0u8; 10];
        // d[0]=flags，d[1..4]=reserved；width-1 @4..7，height-1 @7..10（u24 LE）
        let wm1 = w - 1;
        let hm1 = h - 1;
        d[4] = (wm1 & 0xFF) as u8;
        d[5] = ((wm1 >> 8) & 0xFF) as u8;
        d[6] = ((wm1 >> 16) & 0xFF) as u8;
        d[7] = (hm1 & 0xFF) as u8;
        d[8] = ((hm1 >> 8) & 0xFF) as u8;
        d[9] = ((hm1 >> 16) & 0xFF) as u8;
        d
    }

    fn fixture(extra: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(b"WEBP");
        body.extend_from_slice(&riff_chunk(b"VP8X", &vp8x_data(640, 480)));
        body.extend_from_slice(extra);
        let mut f = Vec::new();
        f.extend_from_slice(b"RIFF");
        f.extend_from_slice(&(body.len() as u32).to_le_bytes());
        f.extend_from_slice(&body);
        f
    }

    fn collect(buf: &[u8]) -> crate::driver::Collector {
        let mut p = WebpParser::new();
        crate::driver::drive_slice(buf, &mut p, crate::limits::Limits::default())
    }

    #[test]
    fn vp8x_dimensions_and_xmp() {
        let xmp = riff_chunk(b"XMP ", br#"<rdf:Description tiff:Make="Acme"/>"#);
        let buf = fixture(&xmp);
        let col = collect(&buf);
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Webp);
        assert_eq!(meta.unified.width, Some(640));
        assert_eq!(meta.unified.height, Some(480));
        assert!(meta.raw.xmp.iter().any(|x| x.name == "Make" && x.value == "Acme"));
    }

    #[test]
    fn exif_chunk_emitted() {
        // EXIF chunk 带完整 TIFF
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&0u16.to_le_bytes()); // 0 entries
        tiff.extend_from_slice(&0u32.to_le_bytes());
        let exif = riff_chunk(b"EXIF", &tiff);
        let buf = fixture(&exif);
        let col = collect(&buf);
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
    }

    #[test]
    fn non_webp_done_no_events() {
        let mut p = WebpParser::new();
        let res = p.pull(b"RIFF\0\0\0\0XXXX");
        assert_eq!(res.demand, Demand::Done);
        assert!(res.events.is_empty());
    }
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test -p omni-meta-core --lib formats::webp 2>&1 | tail -20`
Expected: 编译失败（`WebpParser` 未定义）。

- [ ] **Step 4: 实现 WebpParser**

在 `omni-meta-core/src/formats/webp.rs` 头部 use 之后、`#[cfg(test)]` 之前插入：

```rust
#[derive(Debug, Default)]
pub struct WebpParser {
    saw_header: bool,
    done: bool,
}

impl WebpParser {
    pub fn new() -> Self {
        Self::default()
    }
}

fn u24_le(b: &[u8]) -> u32 {
    (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16)
}

impl MetaParser for WebpParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        let mut events: Vec<Event<'a>> = Vec::new();
        if self.done {
            return PullResult { demand: Demand::Done, consumed: 0, events };
        }
        let mut pos = 0usize;
        if !self.saw_header {
            if input.len() < 12 {
                return PullResult { demand: Demand::NeedBytes(12), consumed: 0, events };
            }
            if &input[0..4] != b"RIFF" || &input[8..12] != b"WEBP" {
                self.done = true;
                return PullResult { demand: Demand::Done, consumed: 0, events };
            }
            self.saw_header = true;
            pos = 12;
        }

        loop {
            let rest = &input[pos..];
            if rest.len() < 8 {
                return PullResult { demand: Demand::NeedBytes(8), consumed: pos, events };
            }
            let fourcc = &rest[0..4];
            let size = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
            let pad = size & 1;

            // 维度 chunk：只需小前缀即可读出，读后 Skip 整个 data+pad。
            let dim_prefix = match fourcc {
                b"VP8X" => Some(10usize),
                b"VP8 " => Some(10usize),
                b"VP8L" => Some(5usize),
                _ => None,
            };
            if let Some(prefix) = dim_prefix {
                let need = 8 + prefix.min(size);
                if rest.len() < need {
                    return PullResult { demand: Demand::NeedBytes(need), consumed: pos, events };
                }
                let data = &rest[8..8 + prefix.min(size)];
                read_dimensions(fourcc, data, &mut events);
                let skip = (size as u64).saturating_add(pad as u64);
                return PullResult { demand: Demand::Skip(skip), consumed: pos + 8, events };
            }

            // 元数据 chunk：须整读 data。
            if fourcc == b"EXIF" || fourcc == b"XMP " {
                let need = match 8usize.checked_add(size) {
                    Some(v) => v,
                    None => {
                        self.done = true;
                        return PullResult { demand: Demand::Done, consumed: pos, events };
                    }
                };
                if rest.len() < need {
                    return PullResult { demand: Demand::NeedBytes(need), consumed: pos, events };
                }
                let mut data = &rest[8..8 + size];
                let kind = if fourcc == b"EXIF" {
                    // 容错可选 "Exif\0\0" 前缀
                    if data.starts_with(b"Exif\0\0") {
                        data = &data[6..];
                    }
                    PayloadKind::Exif
                } else {
                    PayloadKind::Xmp
                };
                events.push(Event::Payload { kind, data });
                // 跳过对齐填充（pad 为 0 或 1）
                let skip = pad as u64;
                if skip > 0 {
                    return PullResult { demand: Demand::Skip(skip), consumed: pos + need, events };
                }
                pos += need;
                continue;
            }

            // 其他 chunk：消费 8 字节头，Skip(data + pad)。
            let skip = (size as u64).saturating_add(pad as u64);
            return PullResult { demand: Demand::Skip(skip), consumed: pos + 8, events };
        }
    }
}

fn read_dimensions<'a>(fourcc: &[u8], data: &[u8], events: &mut Vec<Event<'a>>) {
    match fourcc {
        b"VP8X" if data.len() >= 10 => {
            let w = u24_le(&data[4..7]) + 1;
            let h = u24_le(&data[7..10]) + 1;
            events.push(Event::Field(Field::Width(w)));
            events.push(Event::Field(Field::Height(h)));
        }
        b"VP8 " if data.len() >= 10 => {
            // 关键帧起始码 0x9d 0x01 0x2a 在 data[3..6]
            if data[3] == 0x9d && data[4] == 0x01 && data[5] == 0x2a {
                let w = (u16::from_le_bytes([data[6], data[7]]) & 0x3FFF) as u32;
                let h = (u16::from_le_bytes([data[8], data[9]]) & 0x3FFF) as u32;
                events.push(Event::Field(Field::Width(w)));
                events.push(Event::Field(Field::Height(h)));
            }
        }
        b"VP8L" if data.len() >= 5 && data[0] == 0x2f => {
            let bits = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
            let w = (bits & 0x3FFF) + 1;
            let h = ((bits >> 14) & 0x3FFF) + 1;
            events.push(Event::Field(Field::Width(w)));
            events.push(Event::Field(Field::Height(h)));
        }
        _ => {}
    }
}
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p omni-meta-core --lib formats::webp 2>&1 | tail -20`
Expected: 3 个 WebP 测试 PASS。

- [ ] **Step 6: 接入 probe 与 parser_for**

`probe` 加 WebP 分支（在 PNG 之后）：

```rust
    if buf.len() >= 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"WEBP" {
        return FileFormat::Webp;
    }
```

`parser_for` 加：

```rust
        FileFormat::Webp => Some(Box::new(crate::formats::webp::WebpParser::new())),
```

`probe.rs` 测试加：

```rust
    #[test]
    fn detects_webp_signature() {
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(b"WEBP");
        assert_eq!(probe(&b), FileFormat::Webp);
        assert!(parser_for(FileFormat::Webp).is_some());
    }
```

（`probe.rs` 测试若未 `use alloc::vec::Vec;`，在 `mod tests` 顶部补 `use alloc::vec::Vec;`。）

- [ ] **Step 7: 加 WebP 差分测试**

在 `omni-meta/tests/differential.rs` 末尾追加：

```rust
fn riff_chunk(fourcc: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(fourcc);
    c.extend_from_slice(&(data.len() as u32).to_le_bytes());
    c.extend_from_slice(data);
    if data.len() % 2 == 1 {
        c.push(0);
    }
    c
}

fn fixture_webp() -> Vec<u8> {
    // VP8X 640x480
    let mut vp8x = vec![0u8; 10];
    let (wm1, hm1) = (639u32, 479u32);
    vp8x[4] = (wm1 & 0xFF) as u8;
    vp8x[5] = ((wm1 >> 8) & 0xFF) as u8;
    vp8x[6] = ((wm1 >> 16) & 0xFF) as u8;
    vp8x[7] = (hm1 & 0xFF) as u8;
    vp8x[8] = ((hm1 >> 8) & 0xFF) as u8;
    vp8x[9] = ((hm1 >> 16) & 0xFF) as u8;

    let mut body = Vec::new();
    body.extend_from_slice(b"WEBP");
    body.extend_from_slice(&riff_chunk(b"VP8X", &vp8x));
    body.extend_from_slice(&riff_chunk(b"EXIF", &make_tiff()));
    body.extend_from_slice(&riff_chunk(b"XMP ", br#"<rdf:Description tiff:Make="Acme"/>"#));

    let mut f = Vec::new();
    f.extend_from_slice(b"RIFF");
    f.extend_from_slice(&(body.len() as u32).to_le_bytes());
    f.extend_from_slice(&body);
    f
}

#[test]
fn differential_webp() {
    assert_all_equal(&fixture_webp());
}
```

- [ ] **Step 8: 运行全部测试 + no_std + 提交**

```bash
cargo test 2>&1 | tail -25
cargo build -p omni-meta-core --no-default-features 2>&1 | tail -3
git add omni-meta-core/src/formats/webp.rs omni-meta-core/src/formats/mod.rs omni-meta-core/src/probe.rs omni-meta/tests/differential.rs
git commit -m "feat: WebP 解析器 (VP8X/VP8/VP8L 维度 + EXIF + XMP) + 差分测试"
```
Expected: 全绿含 `differential_webp`。

---

## Task 8: GIF 解析器（`formats/gif.rs`）

Header + LSD→维度；block 循环：图像/扩展走 sub-block 跳过，Application Extension `XMP DataXMP`→捕获 XMP，Trailer→Done。

**Files:**
- Create: `omni-meta-core/src/formats/gif.rs`
- Modify: `omni-meta-core/src/formats/mod.rs`
- Modify: `omni-meta-core/src/probe.rs`
- Test: `omni-meta/tests/differential.rs`

- [ ] **Step 1: 注册模块**

`omni-meta-core/src/formats/mod.rs` 加 `pub mod gif;`。

- [ ] **Step 2: 写失败测试**

创建 `omni-meta-core/src/formats/gif.rs`，先放头部 + 测试：

```rust
//! GIF block 遍历：header(6)+LSD(7) 后逐 block 推进。
//! LSD 发维度；图像/普通扩展走 sub-block 跳过；Application Extension "XMP DataXMP"
//! 捕获 XMP 包（裸字节直到魔数尾的 0x00 终止）；Trailer 0x3B 发 Done。

use alloc::vec::Vec;

use crate::demand::{Demand, Event, MetaParser, PayloadKind, PullResult};
use crate::model::{Field, WarnKind, Warning};

#[cfg(test)]
mod tests {
    use super::*;

    fn header_lsd(w: u16, h: u16, gct: bool) -> Vec<u8> {
        let mut g = Vec::new();
        g.extend_from_slice(b"GIF89a");
        g.extend_from_slice(&w.to_le_bytes());
        g.extend_from_slice(&h.to_le_bytes());
        g.push(if gct { 0x80 } else { 0x00 }); // packed：无 GCT
        g.push(0); // bg
        g.push(0); // aspect
        g
    }

    /// XMP Application Extension：0x21 0xFF 0x0B "XMP Data" "XMP" + 包 + 魔数尾(以 0x00 结束)。
    fn xmp_app_ext(packet: &[u8]) -> Vec<u8> {
        let mut e = Vec::new();
        e.push(0x21);
        e.push(0xFF);
        e.push(0x0B);
        e.extend_from_slice(b"XMP DataXMP");
        e.extend_from_slice(packet);
        // 魔数尾：0x01,0xFF,0xFE,...,0x00（递降）。最末 0x00 为终止符。
        e.push(0x01);
        for v in (0u8..=0xFF).rev() {
            e.push(v);
        }
        e
    }

    fn collect(buf: &[u8]) -> crate::driver::Collector {
        let mut p = GifParser::new();
        crate::driver::drive_slice(buf, &mut p, crate::limits::Limits::default())
    }

    #[test]
    fn lsd_dimensions_and_xmp() {
        let mut g = header_lsd(800, 600, false);
        g.extend_from_slice(&xmp_app_ext(br#"<rdf:Description tiff:Make="Acme"/>"#));
        g.push(0x3B); // trailer
        let col = collect(&g);
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Gif);
        assert_eq!(meta.unified.width, Some(800));
        assert_eq!(meta.unified.height, Some(600));
        assert!(meta.raw.xmp.iter().any(|x| x.name == "Make" && x.value == "Acme"));
    }

    #[test]
    fn skips_comment_extension() {
        let mut g = header_lsd(2, 2, false);
        // 注释扩展 0x21 0xFE + sub-block("hi") + 终止 0x00
        g.push(0x21);
        g.push(0xFE);
        g.push(2);
        g.extend_from_slice(b"hi");
        g.push(0x00);
        g.push(0x3B);
        let col = collect(&g);
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
        assert!(col.xmp.is_empty());
    }

    #[test]
    fn non_gif_done_no_events() {
        let mut p = GifParser::new();
        let res = p.pull(b"NOTAGIFFFFFFF");
        assert_eq!(res.demand, Demand::Done);
        assert!(res.events.is_empty());
    }
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test -p omni-meta-core --lib formats::gif 2>&1 | tail -20`
Expected: 编译失败（`GifParser` 未定义）。

- [ ] **Step 4: 实现 GifParser**

GIF 的 sub-block 链跨 pull，需显式状态。在头部 use 之后、`#[cfg(test)]` 之前插入：

```rust
#[derive(Debug, PartialEq, Eq)]
enum State {
    Header,
    Block,     // 期待引导字节
    SubBlocks, // 跳过模式：走 sub-block 链至 0x00
}

#[derive(Debug)]
pub struct GifParser {
    state: State,
    done: bool,
}

impl Default for GifParser {
    fn default() -> Self {
        Self { state: State::Header, done: false }
    }
}

impl GifParser {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MetaParser for GifParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        let mut events: Vec<Event<'a>> = Vec::new();
        if self.done {
            return PullResult { demand: Demand::Done, consumed: 0, events };
        }
        let mut pos = 0usize;

        if self.state == State::Header {
            if input.len() < 13 {
                return PullResult { demand: Demand::NeedBytes(13), consumed: 0, events };
            }
            let sig = &input[0..6];
            if sig != b"GIF87a" && sig != b"GIF89a" {
                self.done = true;
                return PullResult { demand: Demand::Done, consumed: 0, events };
            }
            let w = u16::from_le_bytes([input[6], input[7]]) as u32;
            let h = u16::from_le_bytes([input[8], input[9]]) as u32;
            events.push(Event::Field(Field::Width(w)));
            events.push(Event::Field(Field::Height(h)));
            let packed = input[10];
            self.state = State::Block;
            pos = 13;
            if packed & 0x80 != 0 {
                // 跳过 Global Color Table：3 * 2^((packed&7)+1)
                let gct = 3usize * (1usize << ((packed & 0x07) + 1));
                return PullResult { demand: Demand::Skip(gct as u64), consumed: pos, events };
            }
        }

        loop {
            match self.state {
                State::Header => unreachable!(),
                State::SubBlocks => {
                    let rest = &input[pos..];
                    if rest.is_empty() {
                        return PullResult { demand: Demand::NeedBytes(1), consumed: pos, events };
                    }
                    let len = rest[0] as usize;
                    if len == 0 {
                        // 链终止，回到 Block
                        self.state = State::Block;
                        pos += 1;
                        continue;
                    }
                    // 跳过长度字节 + len 数据
                    return PullResult {
                        demand: Demand::Skip(len as u64),
                        consumed: pos + 1,
                        events,
                    };
                }
                State::Block => {
                    let rest = &input[pos..];
                    if rest.is_empty() {
                        return PullResult { demand: Demand::NeedBytes(1), consumed: pos, events };
                    }
                    match rest[0] {
                        0x3B => {
                            // Trailer
                            self.done = true;
                            return PullResult { demand: Demand::Done, consumed: pos + 1, events };
                        }
                        0x2C => {
                            // 图像描述符：需 10 字节(1 引导 + 9 描述符)读 packed
                            if rest.len() < 10 {
                                return PullResult { demand: Demand::NeedBytes(10), consumed: pos, events };
                            }
                            let packed = rest[9];
                            let lct = if packed & 0x80 != 0 {
                                3usize * (1usize << ((packed & 0x07) + 1))
                            } else {
                                0
                            };
                            // 消费 10 字节描述符；跳过 LCT + 1 字节 LZW 最小码长；转 SubBlocks
                            self.state = State::SubBlocks;
                            let skip = (lct as u64) + 1;
                            return PullResult { demand: Demand::Skip(skip), consumed: pos + 10, events };
                        }
                        0x21 => {
                            // 扩展：需第 2 字节 label
                            if rest.len() < 2 {
                                return PullResult { demand: Demand::NeedBytes(2), consumed: pos, events };
                            }
                            let label = rest[1];
                            if label == 0xFF {
                                // Application Extension：需 block size 字节 + 11 字节 id
                                if rest.len() < 3 + 11 {
                                    return PullResult { demand: Demand::NeedBytes(3 + 11), consumed: pos, events };
                                }
                                let id = &rest[3..3 + 11];
                                if id == b"XMP DataXMP" {
                                    return capture_gif_xmp(input, pos, events);
                                }
                                // 非 XMP 应用扩展：消费 0x21 0xFF size(1) id(11)，转 SubBlocks
                                self.state = State::SubBlocks;
                                return PullResult { demand: Demand::Skip(0), consumed: pos + 3 + 11, events };
                            }
                            // 其他扩展（注释/图形控制/纯文本）：消费 0x21 label，转 SubBlocks
                            self.state = State::SubBlocks;
                            return PullResult { demand: Demand::Skip(0), consumed: pos + 2, events };
                        }
                        _ => {
                            // 畸形引导字节：best-effort 收尾
                            self.done = true;
                            return PullResult { demand: Demand::Done, consumed: pos, events };
                        }
                    }
                }
            }
        }
    }
}

/// 捕获 GIF Application Extension 中的 XMP 包。从 14 字节头之后起，包文本到魔数尾；
/// 魔数尾以 0x00 终止（XMP 为 XML 文本，不含 0x00，故首个 0x00 即终止符）。
fn capture_gif_xmp<'a>(
    input: &'a [u8],
    pos: usize,
    mut events: Vec<Event<'a>>,
) -> PullResult<'a> {
    let header = pos + 3 + 11; // 0x21 0xFF size id(11)
    let rest = &input[header..];
    // 找首个 0x00（魔数尾终止符）
    match rest.iter().position(|&b| b == 0) {
        Some(zero) => {
            // 包文本 = header..(魔数尾起点)。魔数尾以 0x01 0xFF 0xFE... 开头；
            // 若存在则截断到其前，否则取到 0x00 前。
            let magic = find_magic(rest);
            let pkt_end = magic.unwrap_or(zero);
            let packet = &rest[..pkt_end];
            events.push(Event::Payload { kind: PayloadKind::Xmp, data: packet });
            // 消费到 0x00 终止符（含）；之后回到 Block——由调用方状态机处理。
            // 这里直接 Skip 0 并把消费定位到 0x00 之后，状态置 Block。
            // 注：调用者已把 self.state 留在 Block 之外；用 Done? 不——需续解析。
            // 采用：消费 header + zero + 1，返回 Skip(0) 续跑 Block。
            PullResult {
                demand: Demand::Skip(0),
                consumed: header + zero + 1,
                events,
            }
        }
        None => {
            // 窗口内未见终止符：请求更多（上界由 driver max_retained 兜底）。
            PullResult {
                demand: Demand::NeedBytes(rest.len() + 1),
                consumed: pos,
                events,
            }
        }
    }
}

/// 找魔数尾起点（子序列 0x01 0xFF 0xFE）。
fn find_magic(b: &[u8]) -> Option<usize> {
    if b.len() < 3 {
        return None;
    }
    (0..=b.len() - 3).find(|&k| b[k] == 0x01 && b[k + 1] == 0xFF && b[k + 2] == 0xFE)
}
```

注意状态机细节：`capture_gif_xmp` 返回后需让解析器回到 `State::Block`。由于该函数是从 `State::Block` 分支调用且 `self` 不在函数内，**改为**：在 `State::Block` 的 `0xFF` + `XMP DataXMP` 分支里先 `self.state = State::Block;`（保持）再 `return capture_gif_xmp(...)`——`capture_gif_xmp` 已 `Skip(0)` 续跑，状态留在 `Block` 正确。请确保该分支不误置 `SubBlocks`。

另：`Warning`/`WarnKind` 已在 use 引入；若 `capture_gif_xmp` 的 `None` 分支最终经 driver EOF → driver 自动记 `Truncated`，无需在此手动告警。

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p omni-meta-core --lib formats::gif 2>&1 | tail -30`
Expected: 3 个 GIF 测试 PASS。若 XMP 捕获边界有偏差，对照 `xmp_app_ext` fixture 调试 `capture_gif_xmp`。

- [ ] **Step 6: 接入 probe 与 parser_for**

`probe` 加 GIF（在 WebP 之后）：

```rust
    if buf.len() >= 6 && (&buf[0..6] == b"GIF87a" || &buf[0..6] == b"GIF89a") {
        return FileFormat::Gif;
    }
```

`parser_for` 加：

```rust
        FileFormat::Gif => Some(Box::new(crate::formats::gif::GifParser::new())),
```

`probe.rs` 测试加：

```rust
    #[test]
    fn detects_gif_signature() {
        assert_eq!(probe(b"GIF89a\0\0\0\0\0\0\0"), FileFormat::Gif);
        assert_eq!(probe(b"GIF87a\0\0\0\0\0\0\0"), FileFormat::Gif);
        assert!(parser_for(FileFormat::Gif).is_some());
    }
```

- [ ] **Step 7: 加 GIF 差分测试**

在 `omni-meta/tests/differential.rs` 末尾追加：

```rust
fn fixture_gif() -> Vec<u8> {
    let mut g = Vec::new();
    g.extend_from_slice(b"GIF89a");
    g.extend_from_slice(&800u16.to_le_bytes());
    g.extend_from_slice(&600u16.to_le_bytes());
    g.push(0x00); // 无 GCT
    g.push(0);
    g.push(0);
    // XMP Application Extension
    g.push(0x21);
    g.push(0xFF);
    g.push(0x0B);
    g.extend_from_slice(b"XMP DataXMP");
    g.extend_from_slice(br#"<rdf:Description tiff:Make="Acme"/>"#);
    g.push(0x01);
    for v in (0u8..=0xFFu8).rev() {
        g.push(v);
    }
    g.push(0x3B); // trailer
    g
}

#[test]
fn differential_gif() {
    assert_all_equal(&fixture_gif());
}
```

- [ ] **Step 8: 运行全部测试 + no_std + 提交**

```bash
cargo test 2>&1 | tail -30
cargo build -p omni-meta-core --no-default-features 2>&1 | tail -3
git add omni-meta-core/src/formats/gif.rs omni-meta-core/src/formats/mod.rs omni-meta-core/src/probe.rs omni-meta/tests/differential.rs
git commit -m "feat: GIF 解析器 (LSD 维度 + XMP 应用扩展) + 差分测试"
```
Expected: 全绿含 `differential_gif`。

---

## Task 9: normalize XMP 投影（`normalize.rs`）

把若干 XMP 属性回退投影到 Unified（EXIF 优先，只填 None 槽）。

**Files:**
- Modify: `omni-meta-core/src/normalize.rs`

- [ ] **Step 1: 写失败测试**

在 `omni-meta-core/src/normalize.rs` 的 `mod tests` 末尾追加：

```rust
    use crate::model::XmpProperty;

    fn xmp(prefix: &str, name: &str, value: &str) -> XmpProperty {
        XmpProperty {
            prefix: String::from(prefix),
            name: String::from(name),
            value: String::from(value),
        }
    }

    #[test]
    fn xmp_fills_when_exif_absent() {
        let raw = RawTags {
            exif: Vec::new(),
            xmp: Vec::from([
                xmp("tiff", "Make", "XmpMake"),
                xmp("tiff", "Orientation", "6"),
            ]),
        };
        let mut warnings = Vec::new();
        let u = normalize(&raw, &mut warnings);
        assert_eq!(u.camera_make.as_deref(), Some("XmpMake"));
        assert_eq!(u.orientation, Some(Orientation::Rotate90));
    }

    #[test]
    fn exif_wins_over_xmp() {
        let raw = RawTags {
            exif: Vec::from([ExifTag {
                ifd: 0,
                tag: 0x010F,
                value: Value::Text(String::from("ExifMake")),
            }]),
            xmp: Vec::from([xmp("tiff", "Make", "XmpMake")]),
        };
        let mut warnings = Vec::new();
        let u = normalize(&raw, &mut warnings);
        assert_eq!(u.camera_make.as_deref(), Some("ExifMake"));
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p omni-meta-core --lib normalize 2>&1 | tail -20`
Expected: FAIL（`normalize` 不读 xmp，camera_make 为 None）。

- [ ] **Step 3: 在 normalize 末尾加 XMP 回退投影**

把 `omni-meta-core/src/normalize.rs` 的 `normalize` 函数改为（EXIF 循环后追加 XMP 回退；`u` 在返回前补全）：

在现有 `for t in &raw.exif { ... }` 循环之后、`u` 之前插入：

```rust
    // XMP 回退：仅填 EXIF 未提供的槽。
    for p in &raw.xmp {
        match (p.prefix.as_str(), p.name.as_str()) {
            ("tiff", "Make") if u.camera_make.is_none() => {
                u.camera_make = Some(p.value.clone());
            }
            ("tiff", "Model") if u.camera_model.is_none() => {
                u.camera_model = Some(p.value.clone());
            }
            ("tiff", "Orientation") if u.orientation.is_none() => {
                if let Ok(v) = p.value.parse::<u16>() {
                    if let Some(o) = Orientation::from_u16(v) {
                        u.orientation = Some(o);
                    }
                }
            }
            ("tiff", "ImageWidth") if u.width.is_none() => {
                if let Ok(v) = p.value.parse::<u32>() {
                    u.width = Some(v);
                }
            }
            ("tiff", "ImageLength") if u.height.is_none() => {
                if let Ok(v) = p.value.parse::<u32>() {
                    u.height = Some(v);
                }
            }
            _ => {}
        }
    }
```

注：width/height 的容器级权威值在 `finalize` 中于 `normalize` 之后才写入；为保证"容器优先于 XMP"，本函数只在 `u.width/height` 为 None 时填 XMP，而 `finalize` 也只在 None 时填容器值——两者皆 None 时 XMP 先填、容器值随后不覆盖。**这会让 XMP 维度优先于容器维度，违背规范"容器优先"。** 故改为：在 `finalize` 中容器维度**覆盖** XMP 维度。修改 `finalize`（driver.rs）相应两行为无条件赋值（若容器有值）：

```rust
    if let Some(w) = width {
        unified.width = Some(w);
    }
    if let Some(h) = height {
        unified.height = Some(h);
    }
```

（即容器维度存在则覆盖 normalize 填的 XMP 维度，符合"容器优先"。）

- [ ] **Step 4: 同步修改 driver.rs finalize 的维度写入**

按上一步说明，把 `omni-meta-core/src/driver.rs` 中 `finalize` 的维度写入两段改为无条件覆盖（容器有值即覆盖）：

```rust
    if let Some(w) = width {
        unified.width = Some(w);
    }
    if let Some(h) = height {
        unified.height = Some(h);
    }
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p omni-meta-core 2>&1 | tail -20`
Expected: 全部通过（normalize 两测试 + 现有；维度覆盖语义不破坏已有 PNG/WebP/GIF 维度断言，因 fixture 无 XMP 维度冲突）。

- [ ] **Step 6: 提交**

```bash
git add omni-meta-core/src/normalize.rs omni-meta-core/src/driver.rs
git commit -m "feat: normalize XMP 回退投影 (EXIF 优先, 容器维度覆盖 XMP)"
```

---

## Task 10: 终局验证

全量测试 + no_std + facade 公开面核对。

**Files:**
- 仅验证（必要时微调 re-export）

- [ ] **Step 1: 全量测试**

Run: `cargo test 2>&1 | tail -40`
Expected: 全绿，含 `differential_png` / `differential_webp` / `differential_gif` 与既有 JPEG 差分。

- [ ] **Step 2: no_std 构建**

Run: `cargo build -p omni-meta-core --no-default-features 2>&1 | tail -5`
Expected: 成功，零告警。

- [ ] **Step 3: 核对公开面**

Run: `cargo build 2>&1 | tail -5`
确认 `omni_meta::XmpProperty`、`FileFormat::{Png,Webp,Gif}` 经 facade `pub use omni_meta_core::*` 可见（facade 无需改动）。

- [ ] **Step 4: clippy（若可用）**

Run: `cargo clippy --all-targets 2>&1 | tail -20`
Expected: 无 error；修掉新代码引入的 warning。

- [ ] **Step 5: 最终提交（如有微调）**

```bash
git add -A
git commit -m "chore: 阶段3 终局验证与微调" || echo "无改动"
```

---

## Self-Review 记录

- **规范覆盖**：§3 类型→T1；§8 XMP codec→T2；§3 driver→T3；§7 JPEG SOF→T4；§9 probe/分派→T5；§4 PNG→T6；§5 WebP→T7；§6 GIF→T8；§9 normalize→T9；§11 测试贯穿各任务差分 + T10。IPTC/inflate/URI 解析按 §1 推迟，无对应任务（正确）。
- **类型一致**：`Field::{Width,Height}`、`XmpProperty{prefix,name,value}`、`PayloadKind::Xmp`、`parser_for`/`PROBE_MAX` 全程一致使用。
- **占位扫描**：无 TBD/TODO；每个改码步骤含完整代码。
