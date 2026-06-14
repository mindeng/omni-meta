# omni-meta 适配器阶段实现计划（blocking / seek / push + workspace 拆分）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把单 crate 拆成 `omni-meta-core`（no_std 零依赖 sans-io）+ `omni-meta`（std facade），将 `JpegParser` 增量化，新增流式 `StreamDriver` 与 `PushParser` / `read_blocking` / `read_seek` 三个适配器，并以跨适配器差分测试证明四条路径产出一致。

**Architecture:** 一个流式引擎（`StreamDriver`，自有增长缓冲）+ push 原语（`PushParser`），`read_blocking` / `read_seek` 是 push 之上的薄循环；`read_slice` 保留现有零拷贝 `drive_slice` 路径。四路共享同一 parser / codecs / normalize，差分测试守门。

**Tech Stack:** Rust edition 2024，workspace 两 crate，`no_std + alloc`（core）+ std（facade），零第三方依赖，`#![forbid(unsafe_code)]`。

**规范来源:** `docs/superpowers/specs/2026-06-15-omni-meta-adapters-design.md`（全部章节）；上游 `docs/superpowers/specs/2026-06-14-omni-meta-design.md` §5/§7/§9/§11。

**本计划不含:** async/tokio 适配器、新格式/codec/容器、Stripper、JPEG 之外的回跳实战（向后降级仅以假解析器单测覆盖）。

---

## Task 1: workspace 纯重构（拆两 crate，现有测试保持绿）

把现有单 crate 原样拆成 `omni-meta-core`（核心）+ `omni-meta`（facade）。**不改任何逻辑**，只移动文件 + 新增 manifest + facade re-export。验收标准：`cargo test` 全绿、`cargo build -p omni-meta-core --no-default-features` 通过、公开路径 `omni_meta::read_slice` 等不变。

**Files:**
- Create: `Cargo.toml`（workspace 根，替换现有）
- Create: `omni-meta-core/Cargo.toml`
- Move: `src/` → `omni-meta-core/src/`（git mv，含 lib.rs 及全部模块）
- Create: `omni-meta/Cargo.toml`
- Create: `omni-meta/src/lib.rs`（facade）
- Move: `tests/` → `omni-meta/tests/`（git mv）

- [ ] **Step 1: 移动核心源码与测试到子目录**

```bash
cd /home/min/dev/omni-meta
mkdir -p omni-meta-core omni-meta
git mv src omni-meta-core/src
git mv tests omni-meta/tests
```

- [ ] **Step 2: 写 workspace 根 Cargo.toml**

覆盖 `Cargo.toml` 全文：

```toml
[workspace]
resolver = "2"
members = ["omni-meta-core", "omni-meta"]
```

- [ ] **Step 3: 写 `omni-meta-core/Cargo.toml`**

```toml
[package]
name = "omni-meta-core"
version = "0.1.0"
edition = "2024"

[lib]
name = "omni_meta_core"
path = "src/lib.rs"

[features]
default = ["std"]
std = []

[dependencies]
```

- [ ] **Step 4: 写 `omni-meta/Cargo.toml`**

```toml
[package]
name = "omni-meta"
version = "0.1.0"
edition = "2024"

[lib]
name = "omni_meta"
path = "src/lib.rs"

[features]
default = ["std"]
std = ["omni-meta-core/std"]

[dependencies]
omni-meta-core = { path = "../omni-meta-core" }
```

- [ ] **Step 5: 写 `omni-meta/src/lib.rs`（facade）**

```rust
//! omni-meta：batteries-included facade。
//! 重导出 omni-meta-core 的全部公开面，并在 std 下提供 I/O 适配器。
#![forbid(unsafe_code)]

pub use omni_meta_core::*;
```

`omni-meta-core/src/lib.rs` 保持现状不动（它已是 `pub(crate)` 模块 + 精选 `pub use` 的核心面）。

- [ ] **Step 6: 验证整工作区测试通过**

Run: `cargo test`
Expected: 全绿（core 的单元测试 + facade 的 `read_slice_jpeg` 集成测试）。`read_slice_jpeg.rs` 用 `use omni_meta::{...}`，经 facade glob 重导出可见。

- [ ] **Step 7: 验证 core 的 no_std 构建**

Run: `cargo build -p omni-meta-core --no-default-features`
Expected: 编译成功（纯 `no_std + alloc`）。

- [ ] **Step 8: clippy 全绿**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 无告警。

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor: 拆分为 omni-meta-core + omni-meta workspace 两 crate"
```

---

## Task 2: `JpegParser` 增量化 + `drive_slice` NeedBytes 语义对齐

把 `JpegParser` 从"整块缓冲单次 pull"改为可恢复状态机：窗口不足发 `NeedBytes`，非元数据段发 `Skip`，SOS/EOI 发 `Done`。同步修正 `drive_slice` 的 `NeedBytes` 处理，让其按 `start + consumed` 计算截断 offset 并在窗口足够时续跑（保证 slice 路径在新增量 parser 下仍正确）。

**契约（关键）:** parser 只在 `input.len() < 所需` 时返回 `NeedBytes(n)`，其中 `n` 是"自 `consumed` 之后的新窗口起点起所需的字节数"；窗口足够时必须推进，绝不空转。

**Files:**
- Modify: `omni-meta-core/src/formats/jpeg.rs`（重写解析逻辑 + 测试）
- Modify: `omni-meta-core/src/driver.rs`（`drive_slice` 的 `NeedBytes` 分支 + 新增 slice 截断 offset 测试）
- Modify: `omni-meta-core/src/adapters/slice.rs`（`JpegParser` → `JpegParser::new()`）

- [ ] **Step 1: 写失败测试（jpeg 增量行为）**

在 `omni-meta-core/src/formats/jpeg.rs` 的 `#[cfg(test)] mod tests` 内**追加**以下测试（保留现有 `emits_exif_payload` / `non_jpeg_emits_nothing`，但把其中 `JpegParser` 的构造改为 `JpegParser::new()`）：

```rust
    /// 截断在 APP1 段体中间：窗口不足应发 NeedBytes 而非静默 Done。
    #[test]
    fn truncated_app1_requests_more_bytes() {
        // SOI + APP1(声明 len=20，但只给 4 字节 body)
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
        j.extend_from_slice(&20u16.to_be_bytes()); // 段长 20 → body 18
        j.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]); // 只有 4 字节 body
        let mut p = JpegParser::new();
        let res = p.pull(&j);
        match res.demand {
            Demand::NeedBytes(_) => {}
            other => panic!("expected NeedBytes, got {other:?}"),
        }
        assert!(res.events.is_empty());
    }

    /// 非元数据段（APP0/JFIF）应发 Skip 跳过段体，consumed 指向段体起点。
    #[test]
    fn non_metadata_segment_emits_skip() {
        // SOI + APP0(len=8 → body 6)
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE0]);
        j.extend_from_slice(&8u16.to_be_bytes());
        j.extend_from_slice(&[1, 2, 3, 4, 5, 6]); // 6 字节 body
        let mut p = JpegParser::new();
        let res = p.pull(&j);
        assert_eq!(res.demand, Demand::Skip(6));
        // consumed = SOI(2) + 段头(marker2 + len2 = 4) = 6，指向 body 起点
        assert_eq!(res.consumed, 6);
        assert!(res.events.is_empty());
    }

    /// 跨多次 pull 拼出 APP0(skip) → APP1(payload) → EOI(done)。
    #[test]
    fn resumes_across_pulls() {
        let tiff = [0xAAu8, 0xBB, 0xCC];
        let mut app1: Vec<u8> = Vec::new();
        app1.extend_from_slice(b"Exif\0\0");
        app1.extend_from_slice(&tiff);
        let app1_len = (app1.len() + 2) as u16;

        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]); // SOI
        j.extend_from_slice(&[0xFF, 0xE0]); // APP0
        j.extend_from_slice(&8u16.to_be_bytes());
        j.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        j.extend_from_slice(&[0xFF, 0xE1]); // APP1
        j.extend_from_slice(&app1_len.to_be_bytes());
        j.extend_from_slice(&app1);
        j.extend_from_slice(&[0xFF, 0xD9]); // EOI

        let mut p = JpegParser::new();
        // pull #1：到 APP0 段头 → Skip(6)
        let r1 = p.pull(&j);
        assert_eq!(r1.demand, Demand::Skip(6));
        let mut pos = r1.consumed + 6; // 模拟 driver 跳过段体
        // pull #2：APP1 → payload，随后 EOI → Done
        let r2 = p.pull(&j[pos..]);
        assert_eq!(r2.demand, Demand::Done);
        assert_eq!(r2.events.len(), 1);
        match &r2.events[0] {
            Event::Payload { kind, data } => {
                assert_eq!(*kind, PayloadKind::Exif);
                assert_eq!(*data, &[0xAA, 0xBB, 0xCC][..]);
            }
            _ => panic!("expected payload"),
        }
        let _ = &mut pos;
    }
```

- [ ] **Step 2: 运行测试验证失败**

Run: `cargo test -p omni-meta-core --lib formats`
Expected: FAIL（`JpegParser::new` 不存在 / 旧逻辑不发 `NeedBytes`/`Skip`）。

- [ ] **Step 3: 重写 `JpegParser`**

把 `omni-meta-core/src/formats/jpeg.rs` 顶部到 `#[cfg(test)]` 之前的全部内容替换为：

```rust
//! JPEG 段遍历（增量状态机）：SOI 起逐段推进。
//! 元数据段（APP1/Exif）整段入窗后发 Payload；非元数据段发 Skip 让驱动跳过
//! （可 Seek 源借此原生 seek 省 I/O）；SOS/EOI 发 Done。窗口不足发 NeedBytes。
//! 契约：仅在 input.len() < 所需 时发 NeedBytes(n)，n 相对 consumed 之后的新窗口起点。

use alloc::vec::Vec;

use crate::demand::{Demand, Event, MetaParser, PayloadKind, PullResult};

#[derive(Debug, Default)]
pub struct JpegParser {
    saw_soi: bool,
    done: bool,
}

impl JpegParser {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MetaParser for JpegParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        let mut events: Vec<Event<'a>> = Vec::new();
        if self.done {
            return PullResult { demand: Demand::Done, consumed: 0, events };
        }

        let mut pos = 0usize;
        if !self.saw_soi {
            if input.len() < 2 {
                return PullResult { demand: Demand::NeedBytes(2), consumed: 0, events };
            }
            if input[0] != 0xFF || input[1] != 0xD8 {
                self.done = true; // 非 JPEG：best-effort 收尾
                return PullResult { demand: Demand::Done, consumed: 0, events };
            }
            self.saw_soi = true;
            pos = 2;
        }

        loop {
            let rest = &input[pos..];
            // 段以 0xFF + 码字开头，码字前可有重复 0xFF 填充字节。
            if rest.is_empty() {
                return PullResult { demand: Demand::NeedBytes(2), consumed: pos, events };
            }
            if rest[0] != 0xFF {
                self.done = true; // 畸形：停止，已收集照常返回
                return PullResult { demand: Demand::Done, consumed: pos, events };
            }
            let mut i = 1;
            while i < rest.len() && rest[i] == 0xFF {
                i += 1;
            }
            if i >= rest.len() {
                // 还差码字字节
                return PullResult { demand: Demand::NeedBytes(i + 1), consumed: pos, events };
            }
            let marker = rest[i];
            let after = i + 1; // rest 内：码字之后

            match marker {
                0xD9 | 0xDA => {
                    // EOI / SOS：元数据到此为止
                    self.done = true;
                    return PullResult { demand: Demand::Done, consumed: pos + after, events };
                }
                0x01 | 0xD0..=0xD7 => {
                    // TEM / RSTn：无长度字段
                    pos += after;
                    continue;
                }
                _ => {
                    if rest.len() < after + 2 {
                        return PullResult { demand: Demand::NeedBytes(after + 2), consumed: pos, events };
                    }
                    let len = u16::from_be_bytes([rest[after], rest[after + 1]]) as usize;
                    if len < 2 {
                        self.done = true; // 畸形长度
                        return PullResult { demand: Demand::Done, consumed: pos, events };
                    }
                    let body_len = len - 2;
                    let body_start = after + 2; // rest 内 body 起点
                    let seg_total = body_start + body_len; // rest 内段尾

                    if marker == 0xE1 {
                        // APP1：需整段入窗才能判定并发出
                        if rest.len() < seg_total {
                            return PullResult { demand: Demand::NeedBytes(seg_total), consumed: pos, events };
                        }
                        let body = &rest[body_start..seg_total];
                        if body.starts_with(b"Exif\0\0") {
                            events.push(Event::Payload { kind: PayloadKind::Exif, data: &body[6..] });
                        }
                        pos += seg_total;
                        continue;
                    } else {
                        // 非元数据段：跳过段体（消费段头，Skip body_len）
                        return PullResult {
                            demand: Demand::Skip(body_len as u64),
                            consumed: pos + body_start,
                            events,
                        };
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 4: 把现有两个测试里的 `JpegParser` 构造改为 `JpegParser::new()`**

在 `#[cfg(test)] mod tests` 的 `emits_exif_payload` 与 `non_jpeg_emits_nothing` 中：

```rust
// 旧：let mut p = JpegParser;
// 新：
let mut p = JpegParser::new();
```

- [ ] **Step 5: 修正 `adapters/slice.rs` 的构造点**

`omni-meta-core/src/adapters/slice.rs`：

```rust
// 旧：let mut parser = JpegParser;
// 新：
let mut parser = JpegParser::new();
```

- [ ] **Step 6: 对齐 `drive_slice` 的 `NeedBytes` 分支**

`omni-meta-core/src/driver.rs` 内 `drive_slice` 的 `Demand::NeedBytes(_)` 分支，替换为按 `start + consumed` 计算截断 offset、且窗口足够时续跑：

```rust
            Demand::NeedBytes(n) => {
                // 截断点 = 解析器卡住的绝对位置（slice 永不丢弃前缀 → start 即绝对）。
                let stuck = start.saturating_add(res.consumed);
                let avail = buf.len().saturating_sub(stuck);
                if avail >= n && stuck > start {
                    // 已有足够字节且有推进 → 续跑（增量 parser 的正常路径）。
                    pos = stuck;
                } else {
                    // 字节确实不够（slice 给的是全量剩余）→ 截断。
                    col.warnings.push(Warning { offset: stuck as u64, kind: WarnKind::Truncated });
                    break;
                }
            }
```

- [ ] **Step 7: 新增 slice 截断 offset 测试**

在 `omni-meta-core/src/driver.rs` 的 `#[cfg(test)] mod tests` 内追加：

```rust
    #[test]
    fn slice_truncated_app1_warns_with_offset() {
        // SOI + APP1(声明 len=20) 但 body 截断
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
        j.extend_from_slice(&20u16.to_be_bytes());
        j.extend_from_slice(&[0xAA, 0xBB]); // body 不足
        let mut parser = crate::formats::jpeg::JpegParser::new();
        let col = drive_slice(&j, &mut parser, Limits::default());
        assert_eq!(col.warnings.len(), 1);
        assert_eq!(col.warnings[0].kind, WarnKind::Truncated);
        // 卡在 APP1 段起点（SOI 之后）= 偏移 2
        assert_eq!(col.warnings[0].offset, 2);
    }
```

并把现有 `drives_jpeg_into_exif_collector` 里的 `crate::formats::jpeg::JpegParser`（单元构造）改为 `JpegParser::new()`：

```rust
// 旧：let mut parser = crate::formats::jpeg::JpegParser;
// 新：
let mut parser = crate::formats::jpeg::JpegParser::new();
```

- [ ] **Step 8: 运行测试验证通过**

Run: `cargo test -p omni-meta-core`
Expected: 全绿（jpeg 增量 3 新测试 + 现有 2 个 + driver 截断 offset 新测试 + 现有 driver/其它测试）。

- [ ] **Step 9: 验证 facade 集成测试仍绿 + clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: 全绿、无告警（`read_slice_jpeg` 证明增量化后 slice 输出不变）。

- [ ] **Step 10: Commit**

```bash
git add -A
git commit -m "feat: JpegParser 增量化（NeedBytes/Skip/Done）并对齐 drive_slice 截断语义"
```

---

## Task 3: `StreamDriver` 流式引擎 + `Outcome` + `finalize` + `Error::Io`

新增自有增长缓冲的流式驱动 `StreamDriver`、对外结果枚举 `Outcome`、收尾助手 `finalize`（并让 `read_slice` 复用以 DRY），以及顶层 `Error::Io` 变体。`StreamDriver` 复用现有 `Collector` / codecs / normalize，实现 §5 三级 Skip/seek 降级与截断处理。本任务用假解析器单测驱动逻辑（不接 I/O）。

**Files:**
- Modify: `omni-meta-core/src/error.rs`（新增 `Io` 变体 + Display + 测试）
- Modify: `omni-meta-core/src/driver.rs`（新增 `Outcome`、`StreamDriver`、`finalize` + 测试）
- Modify: `omni-meta-core/src/adapters/slice.rs`（`read_slice` 改用 `finalize`）

- [ ] **Step 1: 写失败测试（Error::Io Display）**

`omni-meta-core/src/error.rs` 的 `#[cfg(test)] mod tests` 内追加：

```rust
    #[test]
    fn io_display_renders() {
        assert_eq!(alloc::format!("{}", Error::Io), "i/o error");
    }
```

- [ ] **Step 2: 实现 Error::Io**

`omni-meta-core/src/error.rs`：枚举加变体、Display 加分支。

```rust
pub enum Error {
    /// 连容器格式都无法识别。
    UnrecognizedFormat,
    /// I/O 源直接报错。v1 不保留底层 io::Error 细节（best-effort）。
    Io,
}
```

```rust
        match self {
            Error::UnrecognizedFormat => f.write_str("unrecognized file format"),
            Error::Io => f.write_str("i/o error"),
        }
```

- [ ] **Step 3: 写失败测试（StreamDriver 逻辑，假解析器）**

`omni-meta-core/src/driver.rs` 的 `#[cfg(test)] mod tests` 内追加（复用已有的 `Script` 假解析器；如 `Script` 当前在 tests 模块内，则直接用）：

```rust
    use crate::model::FileFormat;

    /// 把若干 chunk 依次 feed 进 StreamDriver，返回最终 Collector。
    fn run_stream(chunks: &[&[u8]], parser: alloc::boxed::Box<dyn MetaParser>) -> Collector {
        let mut d = StreamDriver::new(parser, Limits::default());
        for c in chunks {
            let _ = d.feed(c);
        }
        d.finish()
    }

    #[test]
    fn stream_drives_jpeg_in_one_chunk() {
        let j = make_jpeg_with_exif();
        let col = run_stream(&[&j], alloc::boxed::Box::new(crate::formats::jpeg::JpegParser::new()));
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
        assert_eq!(col.exif.len(), 2);
    }

    #[test]
    fn stream_drives_jpeg_byte_by_byte() {
        let j = make_jpeg_with_exif();
        let chunks: Vec<&[u8]> = j.chunks(1).collect();
        let col = run_stream(&chunks, alloc::boxed::Box::new(crate::formats::jpeg::JpegParser::new()));
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
        assert_eq!(col.exif.len(), 2);
    }

    #[test]
    fn stream_skip_outcome_then_seek_external() {
        // 用 Script：先 Skip(100)，再 Done。模拟可 Seek 适配器：feed 少量后用 skip_external 抵扣。
        let mut d = StreamDriver::new(
            alloc::boxed::Box::new(Script { steps: vec![Demand::Skip(100)], i: 0 }),
            Limits::default(),
        );
        // 喂 4 字节触发首个 pull → Script 立即 Skip(100)，driver 吞掉这 4 字节，剩余 skip。
        match d.feed(&[0u8; 4]) {
            Outcome::SkipHint(k) => assert!(k > 0 && k <= 100),
            other => panic!("expected SkipHint, got {other:?}"),
        }
        // 适配器自行 seek 了剩余 k 字节：
        if let Outcome::SkipHint(k) = d.feed(&[]) {
            d.skip_external(k);
        }
        let col = d.finish();
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
    }

    #[test]
    fn stream_truncated_app1_warns_truncated() {
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
        j.extend_from_slice(&20u16.to_be_bytes());
        j.extend_from_slice(&[0xAA, 0xBB]);
        let chunks: Vec<&[u8]> = j.chunks(1).collect();
        let col = run_stream(&chunks, alloc::boxed::Box::new(crate::formats::jpeg::JpegParser::new()));
        assert_eq!(col.warnings.len(), 1);
        assert_eq!(col.warnings[0].kind, WarnKind::Truncated);
        assert_eq!(col.warnings[0].offset, 2);
    }

    #[test]
    fn stream_seekto_backward_beyond_retained_warns() {
        // SeekTo(0) 在丢弃前缀后属于"早于保留下界"→ UnreachableSection。
        let mut d = StreamDriver::new(
            alloc::boxed::Box::new(Script { steps: vec![Demand::Skip(4), Demand::SeekTo(0)], i: 0 }),
            Limits::default(),
        );
        let _ = d.feed(&[0u8; 8]);
        let _ = d.feed(&[]);
        let col = d.finish();
        assert!(col.warnings.iter().any(|w| w.kind == WarnKind::UnreachableSection));
    }

    fn make_jpeg_with_exif() -> Vec<u8> {
        let tiff = make_tiff();
        let mut seg_body: Vec<u8> = Vec::new();
        seg_body.extend_from_slice(b"Exif\0\0");
        seg_body.extend_from_slice(&tiff);
        let len = (seg_body.len() + 2) as u16;
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
        j.extend_from_slice(&len.to_be_bytes());
        j.extend_from_slice(&seg_body);
        j.extend_from_slice(&[0xFF, 0xD9]);
        j
    }

    #[test]
    fn finalize_projects_unified() {
        let j = make_jpeg_with_exif();
        let mut parser = crate::formats::jpeg::JpegParser::new();
        let col = drive_slice(&j, &mut parser, Limits::default());
        let meta = finalize(col, FileFormat::Jpeg);
        assert_eq!(meta.format, FileFormat::Jpeg);
        assert_eq!(meta.unified.orientation, Some(crate::model::Orientation::Rotate90));
        assert_eq!(meta.unified.camera_make.as_deref(), Some("Acme"));
        assert_eq!(meta.raw.exif.len(), 2);
    }
```

> 若现有 `make_tiff()` 已在 tests 模块内（Task 1 迁移自原 driver.rs），直接复用；否则从 `drives_jpeg_into_exif_collector` 旁的 `make_tiff` 复用。

- [ ] **Step 4: 运行测试验证失败**

Run: `cargo test -p omni-meta-core --lib driver`
Expected: FAIL（`StreamDriver` / `Outcome` / `finalize` 未定义）。

- [ ] **Step 5: 实现 `Outcome` + `StreamDriver` + `finalize`**

`omni-meta-core/src/driver.rs`：在文件顶部 `use` 之后、`Collector` 定义之后追加。先确保 `use` 含：

```rust
use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::codecs;
use crate::demand::{Demand, Event, MetaParser, PayloadKind};
use crate::limits::Limits;
use crate::model::{ExifTag, FileFormat, Metadata, RawTags, WarnKind, Warning};
use crate::normalize::normalize;
```

然后追加：

```rust
/// 流式适配器与解析引擎之间的结果。`Need`/`SkipHint` 的数值都是"还需多少字节"
/// / "还需向前跳多少字节"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 需要再补至少 n 字节才能继续。
    Need(usize),
    /// 建议向前跳过 n 字节：能 seek 就 seek + skip_external(n)，不能就照常 feed（driver 吞掉）。
    SkipHint(u64),
    /// 解析完成。
    Done,
}

/// 收尾：把 Collector 投影为统一模型，组装 Metadata。read_slice 与 push 路径共用。
pub(crate) fn finalize(col: Collector, format: FileFormat) -> Metadata {
    let raw = RawTags { exif: col.exif };
    let mut warnings = col.warnings;
    let unified = normalize(&raw, &mut warnings);
    Metadata { unified, raw, warnings, format }
}

/// 流式驱动：自有增长缓冲 + parser + Collector。被 PushParser/blocking/seek 复用。
pub(crate) struct StreamDriver {
    buf: Vec<u8>,
    cursor: usize, // buf 内已消费偏移
    parser: Box<dyn MetaParser>,
    collector: Collector,
    skip_remaining: u64,
    pos_base: u64, // buf[0] 的绝对文件偏移
    done: bool,
    eof: bool,
    max_retained: usize,
}

impl StreamDriver {
    pub(crate) fn new(parser: Box<dyn MetaParser>, limits: Limits) -> Self {
        let max_retained = limits.max_retained_bytes;
        Self {
            buf: Vec::new(),
            cursor: 0,
            parser,
            collector: Collector { exif: Vec::new(), warnings: Vec::new(), limits },
            skip_remaining: 0,
            pos_base: 0,
            done: false,
            eof: false,
            max_retained,
        }
    }

    /// 追加一块字节并推进，返回下一步 Outcome。chunk 可为空（仅推进）。
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Outcome {
        if !chunk.is_empty() {
            self.buf.extend_from_slice(chunk);
        }
        self.drive()
    }

    /// 调用者已自行向前跳 n 字节（源级 seek）后，扣减逻辑待跳量。
    pub(crate) fn skip_external(&mut self, n: u64) {
        let take = n.min(self.skip_remaining);
        self.skip_remaining -= take;
        self.pos_base = self.pos_base.saturating_add(take);
    }

    /// 收尾：若未 Done，置 eof 再驱动一次以记录截断/不可达；返回 Collector。
    pub(crate) fn finish(mut self) -> Collector {
        if !self.done {
            self.eof = true;
            let _ = self.drive();
        }
        self.collector
    }

    fn drop_consumed(&mut self) {
        if self.cursor > 0 {
            self.buf.drain(..self.cursor);
            self.pos_base = self.pos_base.saturating_add(self.cursor as u64);
            self.cursor = 0;
        }
    }

    fn drive(&mut self) -> Outcome {
        if self.done {
            return Outcome::Done;
        }
        // 防卡死：单次 drive 内的循环上界（远大于正常段数）。
        let mut budget = self.buf.len().saturating_mul(2).saturating_add(1024);
        loop {
            if budget == 0 {
                self.collector.warnings.push(Warning {
                    offset: self.pos_base + self.cursor as u64,
                    kind: WarnKind::UnreachableSection,
                });
                self.done = true;
                return Outcome::Done;
            }
            budget -= 1;

            // 1) 先用缓冲字节抵扣在途 skip。
            if self.skip_remaining > 0 {
                let avail = (self.buf.len() - self.cursor) as u64;
                let take = avail.min(self.skip_remaining);
                self.cursor += take as usize;
                self.skip_remaining -= take;
                self.drop_consumed();
                if self.skip_remaining > 0 {
                    if self.eof {
                        // 跳越文件尾：该段不可达（与 drive_slice Skip 越界对齐）。
                        self.collector.warnings.push(Warning {
                            offset: self.pos_base + self.cursor as u64 + self.skip_remaining,
                            kind: WarnKind::UnreachableSection,
                        });
                        self.done = true;
                        return Outcome::Done;
                    }
                    return Outcome::SkipHint(self.skip_remaining);
                }
            }

            // DoS 上界：等待巨型段体导致缓冲超限。
            if self.buf.len() - self.cursor > self.max_retained {
                self.collector.warnings.push(Warning {
                    offset: self.pos_base + self.cursor as u64,
                    kind: WarnKind::UnreachableSection,
                });
                self.done = true;
                return Outcome::Done;
            }

            // 2) 拉解析器（拆分字段借用：parser &mut 与 buf & 互不相干）。
            let (demand, consumed) = {
                let Self { buf, cursor, parser, collector, .. } = self;
                let window = &buf[*cursor..];
                let res = parser.pull(window);
                for ev in res.events {
                    collector.handle(ev);
                }
                (res.demand, res.consumed)
            };

            match demand {
                Demand::Done => {
                    self.cursor += consumed;
                    self.drop_consumed();
                    self.done = true;
                    return Outcome::Done;
                }
                Demand::NeedBytes(n) => {
                    self.cursor += consumed;
                    self.drop_consumed();
                    let avail = self.buf.len() - self.cursor;
                    if avail >= n {
                        if consumed == 0 {
                            // 零前进且已有足够字节 → 解析器违约，防卡死收尾。
                            self.collector.warnings.push(Warning {
                                offset: self.pos_base + self.cursor as u64,
                                kind: WarnKind::Truncated,
                            });
                            self.done = true;
                            return Outcome::Done;
                        }
                        continue; // 已够，续跑
                    }
                    if self.eof {
                        self.collector.warnings.push(Warning {
                            offset: self.pos_base + self.cursor as u64,
                            kind: WarnKind::Truncated,
                        });
                        self.done = true;
                        return Outcome::Done;
                    }
                    return Outcome::Need(n - avail);
                }
                Demand::Skip(k) => {
                    self.cursor += consumed;
                    self.drop_consumed();
                    self.skip_remaining = k;
                    if k == 0 && consumed == 0 {
                        // 零前进 Skip(0) → 防卡死。
                        self.collector.warnings.push(Warning {
                            offset: self.pos_base + self.cursor as u64,
                            kind: WarnKind::Truncated,
                        });
                        self.done = true;
                        return Outcome::Done;
                    }
                    continue; // 回到顶部抵扣 skip
                }
                Demand::SeekTo(p) => {
                    self.cursor += consumed;
                    let abs = self.pos_base + self.cursor as u64;
                    if p >= abs {
                        self.skip_remaining = p - abs;
                        self.drop_consumed();
                        if self.skip_remaining == 0 {
                            // 零前进 SeekTo 当前位置 → 防卡死。
                            self.collector.warnings.push(Warning {
                                offset: abs,
                                kind: WarnKind::Truncated,
                            });
                            self.done = true;
                            return Outcome::Done;
                        }
                        continue;
                    } else if p >= self.pos_base {
                        // 落在保留缓冲内 → cursor 回移。
                        self.cursor = (p - self.pos_base) as usize;
                        continue;
                    } else {
                        // 早于保留下界且字节已弃 → 不可达。
                        self.collector.warnings.push(Warning {
                            offset: p,
                            kind: WarnKind::UnreachableSection,
                        });
                        self.done = true;
                        return Outcome::Done;
                    }
                }
            }
        }
    }
}
```

> 注：`Collector` 字段 `exif`/`warnings` 为 `pub`、`limits` 私有但同模块可构造，`StreamDriver` 与 `finalize` 均在 `driver.rs` 内，故可直接构造/读取。

- [ ] **Step 6: `read_slice` 改用 `finalize`（DRY）**

`omni-meta-core/src/adapters/slice.rs` 的 `FileFormat::Jpeg` 分支体替换为：

```rust
        FileFormat::Jpeg => {
            let mut parser = JpegParser::new();
            let col = drive_slice(buf, &mut parser, opts.limits);
            Ok(crate::driver::finalize(col, FileFormat::Jpeg))
        }
```

并清理不再用到的 `use`（`RawTags` / `normalize` 若仅此处用则删；保留 `Metadata` / `FileFormat` / `Error` 等仍需者）。以 clippy 为准。

- [ ] **Step 7: 运行测试验证通过**

Run: `cargo test -p omni-meta-core`
Expected: 全绿（含 7 个 StreamDriver/finalize 新测试）。

- [ ] **Step 8: facade 测试 + no_std + clippy**

Run: `cargo test && cargo build -p omni-meta-core --no-default-features && cargo clippy --all-targets -- -D warnings`
Expected: 全绿、no_std 编译成功、无告警。

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "feat: StreamDriver 流式引擎 + Outcome + finalize + Error::Io"
```

---

## Task 4: `PushParser`（no_std push 适配器 + 延迟探测）

公开 push 适配器，持有 `StreamDriver`，并负责"凑够字节才探测格式"。`feed`/`skip`/`finish` 对外暴露 `Outcome`。本任务把 `PushParser` / `Outcome` 通过 core lib 公开（facade 经 glob 自动再导出）。

**API 微调（相对 spec §7）:** `finish` 返回 `Result<Metadata, Error>`——以覆盖"不足 2 字节即 EOF / 非 JPEG"在 finish 时才暴露的 `UnrecognizedFormat`，与 `read_slice` 的 Err 语义对齐。

**Files:**
- Create: `omni-meta-core/src/adapters/push.rs`
- Modify: `omni-meta-core/src/adapters/mod.rs`（加 `pub mod push;`）
- Modify: `omni-meta-core/src/lib.rs`（`pub use adapters::push::PushParser; pub use driver::Outcome;`）

- [ ] **Step 1: 写失败测试（push 适配器）**

Create `omni-meta-core/src/adapters/push.rs`（先只放测试，实现在 Step 3 补；为能编译，测试引用的符号将在 Step 3 定义）：

```rust
//! read_push：调用者掌握主动权的 push 适配器（no_std 亦可用）。

// （实现见下方 Step 3）

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::slice::{read_slice, Options};
    use alloc::vec::Vec;

    fn make_tiff() -> Vec<u8> {
        let mut t: Vec<u8> = Vec::new();
        t.extend_from_slice(b"II");
        t.extend_from_slice(&42u16.to_le_bytes());
        t.extend_from_slice(&8u32.to_le_bytes());
        t.extend_from_slice(&2u16.to_le_bytes());
        t.extend_from_slice(&0x010Fu16.to_le_bytes());
        t.extend_from_slice(&2u16.to_le_bytes());
        t.extend_from_slice(&5u32.to_le_bytes());
        t.extend_from_slice(&38u32.to_le_bytes());
        t.extend_from_slice(&0x0112u16.to_le_bytes());
        t.extend_from_slice(&3u16.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&6u32.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(b"Acme\0");
        t
    }

    fn jpeg_with_exif() -> Vec<u8> {
        let tiff = make_tiff();
        let mut seg_body: Vec<u8> = Vec::new();
        seg_body.extend_from_slice(b"Exif\0\0");
        seg_body.extend_from_slice(&tiff);
        let len = (seg_body.len() + 2) as u16;
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
        j.extend_from_slice(&len.to_be_bytes());
        j.extend_from_slice(&seg_body);
        j.extend_from_slice(&[0xFF, 0xD9]);
        j
    }

    /// 以固定 chunk 大小喂完整字节，忽略 SkipHint（driver 内部吞掉）。
    fn push_drive(bytes: &[u8], opts: Options, chunk: usize) -> Result<crate::model::Metadata, crate::error::Error> {
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

    #[test]
    fn push_matches_slice_various_chunks() {
        let j = jpeg_with_exif();
        let want = read_slice(&j, Options::default()).unwrap();
        for chunk in [1usize, 3, 7, j.len()] {
            let got = push_drive(&j, Options::default(), chunk).unwrap();
            assert_eq!(got, want, "chunk={chunk}");
        }
    }

    #[test]
    fn push_unrecognized_errors() {
        let r = push_drive(&[0x00, 0x01, 0x02], Options::default(), 1);
        assert!(r.is_err());
    }

    #[test]
    fn push_skip_via_caller_seek_equivalent() {
        // 含非元数据段的 JPEG：调用者响应 SkipHint 自行 seek + skip。
        let tiff = make_tiff();
        let mut app1: Vec<u8> = Vec::new();
        app1.extend_from_slice(b"Exif\0\0");
        app1.extend_from_slice(&tiff);
        let app1_len = (app1.len() + 2) as u16;
        // 大的非元数据段 APP0（body 100 字节）放在 APP1 之前
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]);
        j.extend_from_slice(&[0xFF, 0xE0]);
        j.extend_from_slice(&102u16.to_be_bytes()); // body 100
        j.extend_from_slice(&[0u8; 100]);
        j.extend_from_slice(&[0xFF, 0xE1]);
        j.extend_from_slice(&app1_len.to_be_bytes());
        j.extend_from_slice(&app1);
        j.extend_from_slice(&[0xFF, 0xD9]);

        let want = read_slice(&j, Options::default()).unwrap();

        // 模拟 seek：维护一个"源游标"，SkipHint 时直接前移并 skip。
        let mut p = PushParser::new(Options::default());
        let mut src = 0usize;
        let mut outcome = p.feed(&[]).unwrap();
        loop {
            match outcome {
                Outcome::Done => break,
                Outcome::SkipHint(k) => {
                    src += k as usize; // 源级 seek 前移
                    p.skip(k);
                    outcome = p.feed(&[]).unwrap();
                }
                Outcome::Need(_) => {
                    if src >= j.len() {
                        break;
                    }
                    let end = (src + 4).min(j.len());
                    let chunk = &j[src..end];
                    src = end;
                    outcome = p.feed(chunk).unwrap();
                }
            }
        }
        let got = p.finish().unwrap();
        assert_eq!(got, want);
    }
}
```

- [ ] **Step 2: 声明模块并运行测试验证失败**

`omni-meta-core/src/adapters/mod.rs` 加：

```rust
pub mod push;
```

`omni-meta-core/src/lib.rs` 的 `pub use` 区加：

```rust
pub use adapters::push::PushParser;
pub use driver::Outcome;
```

Run: `cargo test -p omni-meta-core --lib push`
Expected: FAIL（`PushParser` 未定义）。

- [ ] **Step 3: 实现 `PushParser`**

在 `omni-meta-core/src/adapters/push.rs` 顶部（模块文档注释之后、`#[cfg(test)]` 之前）插入：

```rust
use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::adapters::slice::Options;
use crate::driver::{finalize, Outcome, StreamDriver};
use crate::error::Error;
use crate::formats::jpeg::JpegParser;
use crate::model::{FileFormat, Metadata};
use crate::probe::probe;

/// push 适配器：调用者反复 `feed` 字节、按 `Outcome` 决定下一步，最后 `finish`。
/// 探测格式需要前 2 字节；在凑齐前 `feed` 累积到内部预缓冲。
pub struct PushParser {
    limits_opts: Options,
    pre: Vec<u8>,
    driver: Option<StreamDriver>,
    format: FileFormat,
    failed: bool,
}

const PROBE_MIN: usize = 2;

impl PushParser {
    pub fn new(opts: Options) -> Self {
        Self {
            limits_opts: opts,
            pre: Vec::new(),
            driver: None,
            format: FileFormat::Unknown,
            failed: false,
        }
    }

    /// 喂入一块字节（可为空，仅推进），返回下一步 `Outcome`。
    /// 一旦判定格式不可识别，返回 `Err(UnrecognizedFormat)`。
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Outcome, Error> {
        if self.failed {
            return Err(Error::UnrecognizedFormat);
        }
        if let Some(d) = self.driver.as_mut() {
            return Ok(d.feed(chunk));
        }
        // 仍在探测前：累积。
        self.pre.extend_from_slice(chunk);
        if self.pre.len() < PROBE_MIN {
            return Ok(Outcome::Need(PROBE_MIN - self.pre.len()));
        }
        self.start_driver()
    }

    /// 调用者已自行向前跳 n 字节后，推进解析器逻辑位置。
    pub fn skip(&mut self, n: u64) {
        if let Some(d) = self.driver.as_mut() {
            d.skip_external(n);
        }
    }

    /// 收尾，返回 Metadata；从未识别出格式则 Err(UnrecognizedFormat)。
    pub fn finish(mut self) -> Result<Metadata, Error> {
        if self.failed {
            return Err(Error::UnrecognizedFormat);
        }
        if self.driver.is_none() {
            // EOF 前未凑够/未识别：用现有 pre 末次探测。
            let _ = self.start_driver();
            if self.failed || self.driver.is_none() {
                return Err(Error::UnrecognizedFormat);
            }
        }
        let driver = self.driver.take().unwrap();
        let col = driver.finish();
        Ok(finalize(col, self.format))
    }

    /// 用已累积的 `pre` 探测并建驱动；不可识别则置 failed。
    fn start_driver(&mut self) -> Result<Outcome, Error> {
        match probe(&self.pre) {
            FileFormat::Jpeg => {
                self.format = FileFormat::Jpeg;
                let mut d = StreamDriver::new(Box::new(JpegParser::new()), self.limits_opts.limits);
                let pre = core::mem::take(&mut self.pre);
                let outcome = d.feed(&pre);
                self.driver = Some(d);
                Ok(outcome)
            }
            FileFormat::Unknown => {
                self.failed = true;
                Err(Error::UnrecognizedFormat)
            }
        }
    }
}
```

> `Options` 字段名为 `limits`（见 slice.rs）。此处 `self.limits_opts.limits` 取出 `Limits`。`StreamDriver` / `finalize` / `Outcome` 已在 Task 3 于 `driver.rs` 定义为 `pub(crate)`，同 crate 可见。

- [ ] **Step 4: 运行测试验证通过**

Run: `cargo test -p omni-meta-core --lib push`
Expected: PASS（3 个：含 chunk 矩阵与 seek 等价）。

- [ ] **Step 5: 全量 + no_std + clippy**

Run: `cargo test && cargo build -p omni-meta-core --no-default-features && cargo clippy --all-targets -- -D warnings`
Expected: 全绿、no_std 成功、无告警。

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: PushParser push 适配器（延迟探测 + feed/skip/finish）"
```

---

## Task 5: `read_blocking` / `read_seek`（facade std 适配器）

在 facade crate 实现两个同步适配器，都是 `PushParser` 之上的薄循环：`read_blocking` 忽略 `SkipHint`（照常喂，driver 内部吞掉）；`read_seek` 在 `SkipHint` 时原生 `seek(Current(n))` + `skip(n)` 省 I/O。

**Files:**
- Create: `omni-meta/src/adapters/mod.rs`
- Create: `omni-meta/src/adapters/blocking.rs`
- Create: `omni-meta/src/adapters/seek.rs`
- Modify: `omni-meta/src/lib.rs`（在 std 下声明 adapters 并 re-export）

- [ ] **Step 1: 写失败测试（facade 集成测试，走公开 API）**

Create `omni-meta/tests/blocking_seek_jpeg.rs`：

```rust
//! blocking / seek 适配器端到端 + 与 read_slice 一致。

use omni_meta::{read_blocking, read_seek, read_slice, Options, Orientation};
use std::io::Cursor;

fn make_tiff() -> Vec<u8> {
    let mut t: Vec<u8> = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    t.extend_from_slice(&2u16.to_le_bytes());
    t.extend_from_slice(&0x010Fu16.to_le_bytes());
    t.extend_from_slice(&2u16.to_le_bytes());
    t.extend_from_slice(&5u32.to_le_bytes());
    t.extend_from_slice(&38u32.to_le_bytes());
    t.extend_from_slice(&0x0112u16.to_le_bytes());
    t.extend_from_slice(&3u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&6u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    t.extend_from_slice(b"Acme\0");
    t
}

fn jpeg_with_exif() -> Vec<u8> {
    let tiff = make_tiff();
    let mut seg_body: Vec<u8> = Vec::new();
    seg_body.extend_from_slice(b"Exif\0\0");
    seg_body.extend_from_slice(&tiff);
    let len = (seg_body.len() + 2) as u16;
    let mut j: Vec<u8> = Vec::new();
    j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
    j.extend_from_slice(&len.to_be_bytes());
    j.extend_from_slice(&seg_body);
    j.extend_from_slice(&[0xFF, 0xD9]);
    j
}

#[test]
fn blocking_extracts_fields() {
    let j = jpeg_with_exif();
    let meta = read_blocking(&j[..], Options::default()).expect("parse");
    assert_eq!(meta.unified.camera_make.as_deref(), Some("Acme"));
    assert_eq!(meta.unified.orientation, Some(Orientation::Rotate90));
}

#[test]
fn seek_matches_slice() {
    let j = jpeg_with_exif();
    let want = read_slice(&j, Options::default()).unwrap();
    let got = read_seek(Cursor::new(&j), Options::default()).unwrap();
    assert_eq!(got, want);
}

#[test]
fn blocking_unrecognized_errors() {
    assert!(read_blocking(&[0u8, 1, 2][..], Options::default()).is_err());
}
```

- [ ] **Step 2: 运行测试验证失败**

Run: `cargo test -p omni-meta --test blocking_seek_jpeg`
Expected: FAIL（`read_blocking` / `read_seek` 未导出）。

- [ ] **Step 3: 实现 blocking 适配器**

Create `omni-meta/src/adapters/blocking.rs`：

```rust
//! read_blocking：仅顺序读源（管道/网络流等）。忽略 SkipHint——照常喂，
//! StreamDriver 内部把待跳字节吞掉。

use std::io::Read;

use omni_meta_core::{Error, Metadata, Options, Outcome, PushParser};

const CHUNK: usize = 8192;

pub fn read_blocking<R: Read>(mut r: R, opts: Options) -> Result<Metadata, Error> {
    let mut p = PushParser::new(opts);
    let mut buf = [0u8; CHUNK];
    loop {
        let n = r.read(&mut buf).map_err(|_| Error::Io)?;
        if n == 0 {
            break; // EOF
        }
        if let Outcome::Done = p.feed(&buf[..n])? {
            return p.finish();
        }
    }
    p.finish()
}
```

- [ ] **Step 4: 实现 seek 适配器**

Create `omni-meta/src/adapters/seek.rs`：

```rust
//! read_seek：可 Seek 源。SkipHint 时原生向前 seek 省 I/O（不读跳过的字节）。

use std::io::{Read, Seek, SeekFrom};

use omni_meta_core::{Error, Metadata, Options, Outcome, PushParser};

const CHUNK: usize = 8192;

pub fn read_seek<R: Read + Seek>(mut r: R, opts: Options) -> Result<Metadata, Error> {
    let mut p = PushParser::new(opts);
    let mut buf = [0u8; CHUNK];
    let mut outcome = p.feed(&[])?; // 取得首个需求（探测需 2 字节）
    loop {
        match outcome {
            Outcome::Done => break,
            Outcome::SkipHint(k) => {
                // 巨量跳跃用 i64 可能溢出：超界则回退为读弃（照常 feed）。
                match i64::try_from(k) {
                    Ok(off) => {
                        r.seek(SeekFrom::Current(off)).map_err(|_| Error::Io)?;
                        p.skip(k);
                        outcome = p.feed(&[])?;
                    }
                    Err(_) => {
                        let n = r.read(&mut buf).map_err(|_| Error::Io)?;
                        if n == 0 {
                            break;
                        }
                        outcome = p.feed(&buf[..n])?;
                    }
                }
            }
            Outcome::Need(_) => {
                let n = r.read(&mut buf).map_err(|_| Error::Io)?;
                if n == 0 {
                    break;
                }
                outcome = p.feed(&buf[..n])?;
            }
        }
    }
    p.finish()
}
```

- [ ] **Step 5: 声明模块并 re-export**

Create `omni-meta/src/adapters/mod.rs`：

```rust
pub mod blocking;
pub mod seek;
```

把 `omni-meta/src/lib.rs` 替换为：

```rust
//! omni-meta：batteries-included facade。
//! 重导出 omni-meta-core 的全部公开面，并在 std 下提供 I/O 适配器。
#![forbid(unsafe_code)]

pub use omni_meta_core::*;

#[cfg(feature = "std")]
mod adapters;
#[cfg(feature = "std")]
pub use adapters::blocking::read_blocking;
#[cfg(feature = "std")]
pub use adapters::seek::read_seek;
```

- [ ] **Step 6: 运行测试验证通过**

Run: `cargo test -p omni-meta --test blocking_seek_jpeg`
Expected: PASS（3 个）。

- [ ] **Step 7: 全量 + clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: 全绿、无告警。

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat: read_blocking / read_seek 同步适配器（PushParser 之上）"
```

---

## Task 6: 跨适配器差分测试（slice == blocking == seek == push）

最终正确性守门：同一字节经四条路径产出逐字段相同的 `Metadata`，覆盖 fixture × chunk-size 矩阵。

**Files:**
- Create: `omni-meta/tests/differential.rs`

- [ ] **Step 1: 写差分测试**

Create `omni-meta/tests/differential.rs`：

```rust
//! 差分测试：read_slice / read_blocking / read_seek / push 对同一输入逐字段一致。

use omni_meta::{read_blocking, read_seek, read_slice, Metadata, Options, Outcome, PushParser};
use std::io::Cursor;

fn make_tiff() -> Vec<u8> {
    let mut t: Vec<u8> = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    t.extend_from_slice(&2u16.to_le_bytes());
    t.extend_from_slice(&0x010Fu16.to_le_bytes());
    t.extend_from_slice(&2u16.to_le_bytes());
    t.extend_from_slice(&5u32.to_le_bytes());
    t.extend_from_slice(&38u32.to_le_bytes());
    t.extend_from_slice(&0x0112u16.to_le_bytes());
    t.extend_from_slice(&3u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&6u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    t.extend_from_slice(b"Acme\0");
    t
}

fn wrap_jpeg(pre_segments: &[u8], with_exif: bool, eoi: bool) -> Vec<u8> {
    let mut j: Vec<u8> = Vec::new();
    j.extend_from_slice(&[0xFF, 0xD8]); // SOI
    j.extend_from_slice(pre_segments);
    if with_exif {
        let tiff = make_tiff();
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(b"Exif\0\0");
        body.extend_from_slice(&tiff);
        let len = (body.len() + 2) as u16;
        j.extend_from_slice(&[0xFF, 0xE1]);
        j.extend_from_slice(&len.to_be_bytes());
        j.extend_from_slice(&body);
    }
    if eoi {
        j.extend_from_slice(&[0xFF, 0xD9]);
    }
    j
}

/// EXIF-first 的常规 JPEG。
fn fixture_plain() -> Vec<u8> {
    wrap_jpeg(&[], true, true)
}

/// APP1 之前有大的非元数据段（行使 Skip）。
fn fixture_large_nonmeta() -> Vec<u8> {
    let mut app0: Vec<u8> = Vec::new();
    app0.extend_from_slice(&[0xFF, 0xE0]);
    app0.extend_from_slice(&202u16.to_be_bytes()); // body 200
    app0.extend_from_slice(&[0u8; 200]);
    wrap_jpeg(&app0, true, true)
}

/// 截断在 APP1 段体中间（声明 len 远大于实际）。
fn fixture_truncated() -> Vec<u8> {
    let mut j: Vec<u8> = Vec::new();
    j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
    j.extend_from_slice(&200u16.to_be_bytes());
    j.extend_from_slice(b"Exif\0\0");
    j.extend_from_slice(&[0xAA, 0xBB]); // body 严重不足
    j
}

fn push_drive(bytes: &[u8], opts: Options, chunk: usize) -> Result<Metadata, omni_meta::Error> {
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

fn assert_all_equal(bytes: &[u8]) {
    let want = read_slice(bytes, Options::default());
    let blocking = read_blocking(bytes, Options::default());
    let seek = read_seek(Cursor::new(bytes), Options::default());
    match &want {
        Ok(w) => {
            assert_eq!(blocking.as_ref().unwrap(), w, "blocking vs slice");
            assert_eq!(seek.as_ref().unwrap(), w, "seek vs slice");
            for chunk in [1usize, 3, 7, bytes.len().max(1)] {
                let got = push_drive(bytes, Options::default(), chunk).unwrap();
                assert_eq!(&got, w, "push chunk={chunk} vs slice");
            }
        }
        Err(_) => {
            assert!(blocking.is_err(), "blocking should also err");
            assert!(seek.is_err(), "seek should also err");
            assert!(push_drive(bytes, Options::default(), 1).is_err(), "push should also err");
        }
    }
}

#[test]
fn differential_plain() {
    assert_all_equal(&fixture_plain());
}

#[test]
fn differential_large_nonmeta() {
    assert_all_equal(&fixture_large_nonmeta());
}

#[test]
fn differential_truncated() {
    assert_all_equal(&fixture_truncated());
}

#[test]
fn differential_unrecognized() {
    assert_all_equal(&[0x00, 0x01, 0x02, 0x03]);
}
```

- [ ] **Step 2: 运行差分测试**

Run: `cargo test -p omni-meta --test differential`
Expected: PASS（4 个）。若 `differential_truncated` 因 warning offset 在某条路径不一致而失败，按"绝对卡住位置"统一 offset 口径（drive_slice 用 `start + consumed`，StreamDriver 用 `pos_base + cursor + consumed`），二者应得同值；据失败信息定位并修正对应分支。

- [ ] **Step 3: 全量 + no_std + clippy 总验收**

Run: `cargo test && cargo build -p omni-meta-core --no-default-features && cargo clippy --all-targets -- -D warnings`
Expected: 全绿、no_std 成功、无告警。

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "test: 四适配器差分测试（slice/blocking/seek/push 逐字段一致）"
```

---

## 自查（spec 覆盖与一致性）

- **§1/§2 范围与决策**：blocking+seek+push、真流式+JpegParser 增量化、现拆 workspace、slice 保零拷贝——Task 1–6 全覆盖；async 明确不含 ✅
- **§3 架构（一引擎三薄封装 + push 原语）**：StreamDriver(Task3) + PushParser(Task4) + blocking/seek 薄循环(Task5) + slice 不动 ✅
- **§4 workspace 布局**：core/facade 两 crate、core 无条件 forbid+no_std-able、facade glob re-export、差分测试落 facade——Task1/Task6 ✅
- **§5 JpegParser 增量化（NeedBytes/Skip/Done、契约、畸形 best-effort）**：Task2 ✅
- **§6 StreamDriver 字节管理 + 三级降级 + push API + std 签名**：Task3（前向 Skip/缓冲内回跳/越界 Warning、截断、DoS 上界、防卡死）+ Task4（Outcome/feed/skip/finish）+ Task5（Read/Read+Seek 签名）✅
- **§7 差分测试 fixture × chunk 矩阵 + 单元 TDD + no_std 构建**：Task6 矩阵 + 各任务单测 + 每任务 `--no-default-features` 校验 ✅
- **§8 安全**：两 crate forbid(unsafe)、缓冲增长 max_retained 上界、checked、防卡死预算、错误姿态一致——Task1/3 ✅
- **§9 实现顺序**：六步 = Task1–6，逐步可测可合并 ✅

**类型/签名一致性核对**：`Outcome{Need(usize),SkipHint(u64),Done}` 跨 driver/push/blocking/seek 一致；`PushParser::{new,feed→Result<Outcome>,skip(u64),finish→Result<Metadata,Error>}` 跨 Task4/5/6 一致；`StreamDriver::{new(Box<dyn MetaParser>,Limits),feed(&[u8])→Outcome,skip_external(u64),finish→Collector}` 跨 Task3/4 一致；`finalize(Collector,FileFormat)→Metadata` 跨 Task3 read_slice 与 Task4 push 一致；`Error::{UnrecognizedFormat,Io}` 跨 core/facade 一致；`Options.limits` 字段名一致；`MetaParser::pull` 契约（NeedBytes 仅在不足时发、consumed 相对窗口起点）在 Task2 parser 与 Task2/3 driver 双方一致。
