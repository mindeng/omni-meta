# A3：MP4/MOV `moov` 元数据抽取 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `BmffParser` 从 MP4/MOV 的 `moov` box 抽取维度（tkhd）、时长（mvhd duration/timescale）、创建时间（mvhd creation_time），并把 `duration_ms` / `created` 两个新 Unified 字段以受控增长方式纳入。

**Architecture:** 沿用 A2 的「Walk 走盒 → 命中目标盒整盒入窗 → 一次性解析发 Field」框架，新增 `moov` 分支（深度 2 显式迭代 `moov→trak→tkhd`，复用 A1 的 `iter_child_boxes`/`full_box_vf`/`read_uint_be`，结构层零新增 API）。新增 `DateTimeParts` 类型同时承载有/无时区两种语义（BMFF=UTC `Some(0)`、EXIF=`None` 或 OffsetTime 解析值）。容器维度/created 经 `Field` 事件入 `Collector`，`finalize` 中覆盖 EXIF 派生值，`normalize` 补 EXIF 第二来源。

**Tech Stack:** Rust，`#![no_std]` + `alloc`，`#![forbid(unsafe_code)]`，零依赖；纯整数民用历法换算（Howard Hinnant `civil_from_days`），全程 `checked_*`、不 panic。

**设计依据：** [`docs/superpowers/specs/2026-06-15-omni-meta-bmff-moov-design.md`](../specs/2026-06-15-omni-meta-bmff-moov-design.md)

**运行测试约定：** 所有 `cargo` 命令在仓库根 `/home/min/dev/omni-meta` 执行。单测跑核心 crate：`cargo test -p omni-meta-core`；差分测试在 `omni-meta`：`cargo test -p omni-meta`。

---

## File Structure

| 文件 | 责任 | 本计划动作 |
|---|---|---|
| `omni-meta-core/src/model.rs` | 数据模型：`DateTimeParts`、`Field::Duration/Created`、`Unified.duration_ms/created` | Modify (Task 1) |
| `omni-meta-core/src/lib.rs` | 导出 `DateTimeParts` | Modify (Task 1) |
| `omni-meta-core/src/driver.rs` | `Collector` 收集容器 Duration/Created Field + `finalize` 覆盖 | Modify (Task 2) |
| `omni-meta-core/src/normalize.rs` | EXIF 日期 → `Unified.created` 回退来源 | Modify (Task 3) |
| `omni-meta-core/src/formats/bmff.rs` | `civil`/`parse_mvhd`/`parse_tkhd`/`parse_moov` + `pull_walk` moov 分支 | Modify (Tasks 4–9) |
| `omni-meta/tests/differential.rs` | MP4 四适配器一致性 fixture | Modify (Task 10) |
| `docs/ROADMAP.md` | 勾选 A3 | Modify (Task 11) |

---

## Task 1: 数据模型 — `DateTimeParts`、`Field` 变体、`Unified` 字段

**Files:**
- Modify: `omni-meta-core/src/model.rs`
- Modify: `omni-meta-core/src/lib.rs:26-29`（`pub use model::{…}` 列表）

- [ ] **Step 1: Write the failing test**

加到 `omni-meta-core/src/model.rs` 的 `mod tests`（文件末 `}` 之前）：

```rust
    #[test]
    fn datetime_parts_construct_and_eq() {
        let a = DateTimeParts { year: 1970, month: 1, day: 1, hour: 0, minute: 0, second: 0, tz_offset_min: Some(0) };
        let b = DateTimeParts { year: 1970, month: 1, day: 1, hour: 0, minute: 0, second: 0, tz_offset_min: None };
        assert_eq!(a.tz_offset_min, Some(0)); // BMFF: UTC
        assert_eq!(b.tz_offset_min, None);    // EXIF 本地: 无时区
        assert_ne!(a, b);
    }

    #[test]
    fn field_has_duration_and_created() {
        let d = Field::Duration(1_501_500);
        assert_eq!(d, Field::Duration(1_501_500));
        let c = Field::Created(DateTimeParts {
            year: 2018, month: 1, day: 1, hour: 0, minute: 0, second: 0, tz_offset_min: Some(0),
        });
        assert_ne!(c, Field::Created(DateTimeParts {
            year: 2019, month: 1, day: 1, hour: 0, minute: 0, second: 0, tz_offset_min: Some(0),
        }));
    }

    #[test]
    fn unified_has_duration_and_created_defaulting_none() {
        let u = Unified::default();
        assert_eq!(u.duration_ms, None);
        assert_eq!(u.created, None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omni-meta-core datetime_parts_construct_and_eq`
Expected: FAIL — `cannot find type DateTimeParts` / `no variant Duration`。

- [ ] **Step 3: Write minimal implementation**

在 `model.rs` 的 `Field` 枚举之前插入 `DateTimeParts`：

```rust
/// 民用时间戳。容器/EXIF 共用的归一时间表示。
/// `tz_offset_min`:
///   None     = 无时区信息（如 EXIF 本地时间，不臆造）
///   Some(0)  = UTC（如 BMFF moov 的 1904 纪元秒）
///   Some(±n) = UTC±n 分钟（如 EXIF OffsetTime "+09:00" → Some(540)）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTimeParts {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub tz_offset_min: Option<i16>,
}
```

把 `Field` 枚举改为：

```rust
/// 容器原生字段（解析器直接从头部读出，不经 codec）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Width(u32),
    Height(u32),
    /// 媒体时长，毫秒。
    Duration(u64),
    /// 创建时间。
    Created(DateTimeParts),
}
```

在 `Unified` 结构体追加两字段（在 `camera_model` 之后）：

```rust
pub struct Unified {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: Option<Orientation>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub duration_ms: Option<u64>,
    pub created: Option<DateTimeParts>,
}
```

在 `lib.rs` 的 `pub use model::{…}` 列表加入 `DateTimeParts`（保持字母序附近即可）：

```rust
pub use model::{
    DateTimeParts, ExifTag, FileFormat, IfdKind, Metadata, Orientation, RawTags, Unified, Value,
    WarnKind, Warning, XmpProperty,
};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p omni-meta-core`
Expected: PASS（含既有测试不回归）。

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/model.rs omni-meta-core/src/lib.rs
git commit -m "feat(model): DateTimeParts + Field::Duration/Created + Unified.duration_ms/created (A3)"
```

---

## Task 2: `Collector` 收集容器 Duration/Created + `finalize` 覆盖

**Files:**
- Modify: `omni-meta-core/src/driver.rs`（`Collector` 结构体 + `handle` + `new`/`drive_slice` 初始化 + `finalize`）

- [ ] **Step 1: Write the failing test**

加到 `driver.rs` 的 `mod tests`（文件末 `}` 之前）。复用既有 `Event`/`Field`/`PullResult` 引入：

```rust
    use crate::model::DateTimeParts;

    /// 发容器 Duration/Created Field（无 EXIF）后 Done。
    struct ContainerTimeEmitter;
    impl MetaParser for ContainerTimeEmitter {
        fn pull<'a>(&mut self, input: &'a [u8]) -> crate::demand::PullResult<'a> {
            use crate::demand::PullResult;
            let events = vec![
                Event::Field(Field::Duration(1_501_500)),
                Event::Field(Field::Created(DateTimeParts {
                    year: 2018, month: 1, day: 1, hour: 12, minute: 0, second: 0, tz_offset_min: Some(0),
                })),
            ];
            PullResult { demand: Demand::Done, consumed: input.len(), events }
        }
    }

    #[test]
    fn collector_records_duration_and_created() {
        let buf = [0u8; 4];
        let mut p = ContainerTimeEmitter;
        let col = drive_slice(&buf, &mut p, Limits::default());
        let meta = finalize(col, FileFormat::Mp4);
        assert_eq!(meta.unified.duration_ms, Some(1_501_500));
        assert_eq!(meta.unified.created.map(|d| d.year), Some(2018));
        assert_eq!(meta.unified.created.and_then(|d| d.tz_offset_min), Some(0));
    }

    /// 容器 Created 覆盖 EXIF 派生 Created：发一个容器 Created + 一段含 DateTime 的 EXIF。
    struct ContainerBeatsExifEmitter;
    impl MetaParser for ContainerBeatsExifEmitter {
        fn pull<'a>(&mut self, input: &'a [u8]) -> crate::demand::PullResult<'a> {
            use crate::demand::PullResult;
            let tiff = make_tiff_with_datetime();
            // data 借用静态不便，这里直接发 Created + 一条 EXIF DateTime via leaked? 用 Field 即可验证覆盖。
            let events = vec![Event::Field(Field::Created(DateTimeParts {
                year: 2018, month: 1, day: 1, hour: 0, minute: 0, second: 0, tz_offset_min: Some(0),
            }))];
            let _ = tiff;
            PullResult { demand: Demand::Done, consumed: input.len(), events }
        }
    }

    #[test]
    fn container_created_present_is_kept() {
        // 仅验证容器 Created 落到 unified（覆盖逻辑在 Task 3 与 EXIF 联调）。
        let buf = [0u8; 4];
        let mut p = ContainerBeatsExifEmitter;
        let col = drive_slice(&buf, &mut p, Limits::default());
        let meta = finalize(col, FileFormat::Mp4);
        assert_eq!(meta.unified.created.map(|d| d.year), Some(2018));
    }

    /// 最小 TIFF：IFD0 含 DateTime(0x0132) ASCII "2003:01:24 09:20:00"。
    fn make_tiff_with_datetime() -> Vec<u8> {
        let mut t: Vec<u8> = Vec::new();
        t.extend_from_slice(b"II");
        t.extend_from_slice(&42u16.to_le_bytes());
        t.extend_from_slice(&8u32.to_le_bytes());
        t.extend_from_slice(&1u16.to_le_bytes()); // 1 entry
        t.extend_from_slice(&0x0132u16.to_le_bytes()); // DateTime
        t.extend_from_slice(&2u16.to_le_bytes()); // ASCII
        t.extend_from_slice(&20u32.to_le_bytes()); // count = 19 chars + NUL
        t.extend_from_slice(&26u32.to_le_bytes()); // 值偏移
        t.extend_from_slice(&0u32.to_le_bytes()); // next IFD
        t.extend_from_slice(b"2003:01:24 09:20:00\0");
        t
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omni-meta-core collector_records_duration_and_created`
Expected: FAIL — `Collector` 无 `duration_ms`/`created` 字段、`handle` 不识别新 Field、`finalize` 不投影。

- [ ] **Step 3: Write minimal implementation**

在 `Collector` 结构体（`driver.rs:14-21`）加两字段：

```rust
pub struct Collector {
    pub exif: Vec<ExifTag>,
    pub xmp: Vec<XmpProperty>,
    pub warnings: Vec<Warning>,
    width: Option<u32>,
    height: Option<u32>,
    duration_ms: Option<u64>,
    created: Option<crate::model::DateTimeParts>,
    limits: Limits,
}
```

在 `Collector::handle` 的 `match ev` 里，`Event::Field(Field::Height(h)) => {…}` 之后加两臂：

```rust
            Event::Field(Field::Duration(ms)) => {
                if self.duration_ms.is_none() {
                    self.duration_ms = Some(ms);
                }
            }
            Event::Field(Field::Created(dt)) => {
                if self.created.is_none() {
                    self.created = Some(dt);
                }
            }
```

`StreamDriver::new`（`driver.rs:94-101`）与 `drive_slice`（`driver.rs:296-303`）两处 `Collector { … }` 初始化各补：

```rust
                width: None,
                height: None,
                duration_ms: None,
                created: None,
                limits,
```

（`drive_slice` 内同理，缩进对应。）

`finalize`（`driver.rs:60-72`）改为同时投影 duration/created（容器值覆盖 normalize 的 EXIF 值）：

```rust
pub(crate) fn finalize(col: Collector, format: FileFormat) -> Metadata {
    let (width, height) = (col.width, col.height);
    let (duration_ms, created) = (col.duration_ms, col.created);
    let raw = RawTags { exif: col.exif, xmp: col.xmp };
    let mut warnings = col.warnings;
    let mut unified = normalize(&raw, &mut warnings);
    if let Some(w) = width {
        unified.width = Some(w);
    }
    if let Some(h) = height {
        unified.height = Some(h);
    }
    if let Some(d) = duration_ms {
        unified.duration_ms = Some(d);
    }
    if let Some(c) = created {
        unified.created = Some(c); // 容器（moov）优先于 EXIF 派生
    }
    Metadata { unified, raw, warnings, format }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p omni-meta-core`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/driver.rs
git commit -m "feat(driver): Collector 收集容器 Duration/Created Field, finalize 覆盖 EXIF (A3)"
```

---

## Task 3: `normalize` — EXIF 日期 → `Unified.created`（DateTimeOriginal/DateTime + OffsetTime）

**Files:**
- Modify: `omni-meta-core/src/normalize.rs`

- [ ] **Step 1: Write the failing test**

加到 `normalize.rs` 的 `mod tests`（文件末 `}` 之前）。先补一个构造 EXIF 标签的辅助：

```rust
    fn exif_tag(ifd: IfdKind, tag: u16, text: &str) -> ExifTag {
        ExifTag { ifd, tag, value: Value::Text(String::from(text)) }
    }

    #[test]
    fn created_from_datetime_original_no_offset_is_naive() {
        let raw = RawTags {
            exif: Vec::from([exif_tag(IfdKind::Exif, 0x9003, "2003:01:24 09:20:00")]),
            xmp: Vec::new(),
        };
        let mut w = Vec::new();
        let u = normalize(&raw, &mut w);
        let c = u.created.expect("created");
        assert_eq!((c.year, c.month, c.day, c.hour, c.minute, c.second), (2003, 1, 24, 9, 20, 0));
        assert_eq!(c.tz_offset_min, None); // 无 OffsetTime → 无时区
    }

    #[test]
    fn created_from_datetime_original_with_offset() {
        let raw = RawTags {
            exif: Vec::from([
                exif_tag(IfdKind::Exif, 0x9003, "2003:01:24 09:20:00"),
                exif_tag(IfdKind::Exif, 0x9011, "+09:00"), // OffsetTimeOriginal
            ]),
            xmp: Vec::new(),
        };
        let mut w = Vec::new();
        let u = normalize(&raw, &mut w);
        assert_eq!(u.created.unwrap().tz_offset_min, Some(540));
    }

    #[test]
    fn created_falls_back_to_ifd0_datetime() {
        let raw = RawTags {
            exif: Vec::from([
                exif_tag(IfdKind::Primary, 0x0132, "1999:12:31 23:59:59"),
                exif_tag(IfdKind::Exif, 0x9010, "-05:00"), // OffsetTime (对应 0x0132)
            ]),
            xmp: Vec::new(),
        };
        let mut w = Vec::new();
        let u = normalize(&raw, &mut w);
        let c = u.created.expect("created");
        assert_eq!((c.year, c.month, c.day), (1999, 12, 31));
        assert_eq!(c.tz_offset_min, Some(-300));
    }

    #[test]
    fn created_original_wins_over_ifd0_datetime() {
        let raw = RawTags {
            exif: Vec::from([
                exif_tag(IfdKind::Primary, 0x0132, "1999:12:31 23:59:59"),
                exif_tag(IfdKind::Exif, 0x9003, "2003:01:24 09:20:00"),
            ]),
            xmp: Vec::new(),
        };
        let mut w = Vec::new();
        let u = normalize(&raw, &mut w);
        assert_eq!(u.created.unwrap().year, 2003); // DateTimeOriginal 优先
    }

    #[test]
    fn created_malformed_is_none() {
        for bad in ["not-a-date", "2003-01-24 09:20:00", "2003:13:40 25:99:99", ""] {
            let raw = RawTags {
                exif: Vec::from([exif_tag(IfdKind::Exif, 0x9003, bad)]),
                xmp: Vec::new(),
            };
            let mut w = Vec::new();
            let u = normalize(&raw, &mut w);
            assert_eq!(u.created, None, "input {bad:?} 应判为无效");
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omni-meta-core created_from_datetime_original_no_offset_is_naive`
Expected: FAIL — `Unified` 无 created 投影逻辑（值恒为 None）。

- [ ] **Step 3: Write minimal implementation**

在 `normalize.rs` 顶部常量区（`const TAG_ORIENTATION` 之后）加 EXIF 日期标签号：

```rust
const TAG_DATETIME: u16 = 0x0132; // IFD0
const TAG_DATETIME_ORIGINAL: u16 = 0x9003; // Exif IFD
const TAG_OFFSET_TIME: u16 = 0x9010; // 对应 0x0132
const TAG_OFFSET_TIME_ORIGINAL: u16 = 0x9011; // 对应 0x9003
```

引入 `DateTimeParts`（修改 `use crate::model::{…}` 行加入 `DateTimeParts`）。在 `normalize` 函数体末尾、`u` 之前插入 created 投影（EXIF 仅作回退来源，容器值在 `finalize` 覆盖）：

```rust
    // created：DateTimeOriginal(Exif IFD 0x9003) 优先，回退 DateTime(IFD0 0x0132)。
    // 时区：默认 None；对应 OffsetTime* 标签存在则解析 "±HH:MM"。
    let find = |ifd: IfdKind, tag: u16| -> Option<&str> {
        raw.exif.iter().find_map(|t| {
            if t.ifd == ifd && t.tag == tag {
                if let Value::Text(s) = &t.value { return Some(s.as_str()); }
            }
            None
        })
    };
    let (dt_str, off_str) = if let Some(s) = find(IfdKind::Exif, TAG_DATETIME_ORIGINAL) {
        (Some(s), find(IfdKind::Exif, TAG_OFFSET_TIME_ORIGINAL))
    } else if let Some(s) = find(IfdKind::Primary, TAG_DATETIME) {
        (Some(s), find(IfdKind::Exif, TAG_OFFSET_TIME))
    } else {
        (None, None)
    };
    if let Some(s) = dt_str {
        if let Some(mut dt) = parse_exif_datetime(s) {
            dt.tz_offset_min = off_str.and_then(parse_exif_offset);
            u.created = Some(dt);
        }
    }
```

在文件 `normalize` 函数之后（`mod tests` 之前）加两个解析辅助：

```rust
/// 解析 EXIF "YYYY:MM:DD HH:MM:SS" → DateTimeParts（tz 由调用方填）。
/// 严格定长定分隔；任一段越界或格式不符 → None（不臆造）。
fn parse_exif_datetime(s: &str) -> Option<DateTimeParts> {
    let b = s.as_bytes();
    if b.len() != 19 || b[4] != b':' || b[7] != b':' || b[10] != b' ' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: core::ops::Range<usize>| -> Option<u32> {
        let mut v = 0u32;
        for &c in &b[r] {
            if !c.is_ascii_digit() { return None; }
            v = v * 10 + u32::from(c - b'0');
        }
        Some(v)
    };
    let year = num(0..4)?;
    let month = num(5..7)?;
    let day = num(8..10)?;
    let hour = num(11..13)?;
    let minute = num(14..16)?;
    let second = num(17..19)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day)
        || hour > 23 || minute > 59 || second > 60
    {
        return None;
    }
    Some(DateTimeParts {
        year: year as u16, month: month as u8, day: day as u8,
        hour: hour as u8, minute: minute as u8, second: second as u8,
        tz_offset_min: None,
    })
}

/// 解析 EXIF OffsetTime "±HH:MM" → 分钟偏移。格式不符 → None。
fn parse_exif_offset(s: &str) -> Option<i16> {
    let b = s.as_bytes();
    if b.len() != 6 || (b[0] != b'+' && b[0] != b'-') || b[3] != b':' {
        return None;
    }
    let two = |i: usize| -> Option<i16> {
        let (h, l) = (b[i], b[i + 1]);
        if !h.is_ascii_digit() || !l.is_ascii_digit() { return None; }
        Some(i16::from((h - b'0') * 10 + (l - b'0')))
    };
    let hh = two(1)?;
    let mm = two(4)?;
    if hh > 23 || mm > 59 { return None; }
    let mag = hh * 60 + mm;
    Some(if b[0] == b'-' { -mag } else { mag })
}
```

> 注：`use` 行需含 `DateTimeParts`，即 `use crate::model::{DateTimeParts, IfdKind, Orientation, RawTags, Unified, Value, WarnKind, Warning};`

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p omni-meta-core normalize`
Expected: PASS（含既有 normalize 测试）。

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/normalize.rs
git commit -m "feat(normalize): EXIF DateTimeOriginal/DateTime + OffsetTime → Unified.created (A3)"
```

---

## Task 4: bmff — 1904 纪元 → 民用历法换算

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`

- [ ] **Step 1: Write the failing test**

加到 `bmff.rs` 的 `mod tests`（文件末闭合 `}` 之前）：

```rust
    #[test]
    fn civil_from_days_known_vectors() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(31), (1970, 2, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(59), (1970, 3, 1)); // 1970 非闰
    }

    #[test]
    fn datetime_from_mp4_epoch_anchor() {
        // 24107 天 = 2_082_844_800 秒后正好是 1970-01-01T00:00:00 UTC。
        let dt = datetime_from_mp4_epoch(2_082_844_800);
        assert_eq!((dt.year, dt.month, dt.day), (1970, 1, 1));
        assert_eq!((dt.hour, dt.minute, dt.second), (0, 0, 0));
        assert_eq!(dt.tz_offset_min, Some(0)); // BMFF 即 UTC
    }

    #[test]
    fn datetime_from_mp4_epoch_offsets() {
        let next_day = datetime_from_mp4_epoch(2_082_844_800 + 86_400);
        assert_eq!((next_day.year, next_day.month, next_day.day), (1970, 1, 2));
        let tod = datetime_from_mp4_epoch(2_082_844_800 + 3_661);
        assert_eq!((tod.hour, tod.minute, tod.second), (1, 1, 1));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omni-meta-core civil_from_days_known_vectors`
Expected: FAIL — `cannot find function civil_from_days`。

- [ ] **Step 3: Write minimal implementation**

在 `bmff.rs` 文件内（建议紧接 `use` 之后、`struct Wanted` 之前），加入引入与函数。文件顶部已 `use crate::model::{Field, WarnKind, Warning}`，改为加入 `DateTimeParts`：

```rust
use crate::model::{DateTimeParts, Field, WarnKind, Warning};
```

新增换算函数：

```rust
/// MP4/MOV 纪元起点（1904-01-01）相对 Unix 纪元（1970-01-01）的天数差。
const MP4_EPOCH_DAYS_BEFORE_UNIX: i64 = 24107;

/// 民用历法：自 1970-01-01 起的天数 → (year, month, day)。
/// Howard Hinnant `civil_from_days` 算法，纯整数、no_std 安全。
fn civil_from_days(days: i64) -> (u16, u8, u8) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8; // [1, 12]
    let year = (y + if m <= 2 { 1 } else { 0 }) as u16;
    (year, m, d)
}

/// MP4/MOV creation_time（自 1904-01-01 00:00:00 UTC 的秒）→ DateTimeParts（UTC）。
fn datetime_from_mp4_epoch(secs: u64) -> DateTimeParts {
    let days = (secs / 86_400) as i64 - MP4_EPOCH_DAYS_BEFORE_UNIX;
    let tod = (secs % 86_400) as u32;
    let (year, month, day) = civil_from_days(days);
    DateTimeParts {
        year,
        month,
        day,
        hour: (tod / 3600) as u8,
        minute: ((tod % 3600) / 60) as u8,
        second: (tod % 60) as u8,
        tz_offset_min: Some(0),
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p omni-meta-core -- civil_from_days datetime_from_mp4_epoch`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat(bmff): 1904 纪元秒 → 民用历法 DateTimeParts 换算 (A3)"
```

---

## Task 5: bmff — `parse_mvhd`（duration_ms + created）

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`

- [ ] **Step 1: Write the failing test**

加到 `bmff.rs` 的 `mod tests`：

```rust
    /// 构造 mvhd 载荷（box 头之后的字节），version 0。
    fn mvhd_v0(creation: u32, timescale: u32, duration: u32) -> Vec<u8> {
        let mut p = alloc::vec![0u8, 0, 0, 0]; // version 0, flags 0
        p.extend_from_slice(&creation.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes()); // modification_time
        p.extend_from_slice(&timescale.to_be_bytes());
        p.extend_from_slice(&duration.to_be_bytes());
        // 其余字段（rate/volume/matrix/...）parse_mvhd 不读，省略。
        p
    }

    fn mvhd_v1(creation: u64, timescale: u32, duration: u64) -> Vec<u8> {
        let mut p = alloc::vec![1u8, 0, 0, 0]; // version 1
        p.extend_from_slice(&creation.to_be_bytes());
        p.extend_from_slice(&0u64.to_be_bytes()); // modification_time
        p.extend_from_slice(&timescale.to_be_bytes());
        p.extend_from_slice(&duration.to_be_bytes());
        p
    }

    #[test]
    fn parse_mvhd_v0_duration_and_created() {
        // timescale 600, duration 900900 → 900900*1000/600 = 1_501_500 ms
        // creation 2_082_844_800 → 1970-01-01
        let m = parse_mvhd(&mvhd_v0(2_082_844_800, 600, 900_900));
        assert_eq!(m.duration_ms, Some(1_501_500));
        assert_eq!(m.created.map(|d| d.year), Some(1970));
        assert!(m.timescale_invalid == false);
    }

    #[test]
    fn parse_mvhd_v1_wide_fields() {
        let m = parse_mvhd(&mvhd_v1(2_082_844_800, 1000, 5000));
        assert_eq!(m.duration_ms, Some(5000));
        assert_eq!(m.created.map(|d| d.year), Some(1970));
    }

    #[test]
    fn parse_mvhd_timescale_zero_no_duration() {
        let m = parse_mvhd(&mvhd_v0(0, 0, 1000));
        assert_eq!(m.duration_ms, None);
        assert!(m.timescale_invalid); // 触发警告标记
    }

    #[test]
    fn parse_mvhd_creation_zero_no_created() {
        let m = parse_mvhd(&mvhd_v0(0, 600, 600));
        assert_eq!(m.created, None); // creation_time==0 视作未设置
        assert_eq!(m.duration_ms, Some(1000));
    }

    #[test]
    fn parse_mvhd_truncated_is_none() {
        let m = parse_mvhd(&[0u8, 0, 0]); // 不足 FullBox 头
        assert_eq!(m.duration_ms, None);
        assert_eq!(m.created, None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omni-meta-core parse_mvhd_v0_duration_and_created`
Expected: FAIL — `cannot find function parse_mvhd` / 类型 `Mvhd` 未定义。

- [ ] **Step 3: Write minimal implementation**

在 `bmff.rs`（`datetime_from_mp4_epoch` 之后）加结构体与解析函数。文件已 `use crate::containers::isobmff::{full_box_vf, iter_child_boxes, read_box_header, read_uint_be}` 与 `ByteCursor/Endian`：

```rust
/// mvhd 解析产物。`timescale_invalid` 标记 timescale==0 或时长溢出，供上层发警告。
#[derive(Default)]
struct Mvhd {
    duration_ms: Option<u64>,
    created: Option<DateTimeParts>,
    timescale_invalid: bool,
}

/// 解析 `mvhd`（MovieHeaderBox）载荷 → 时长 + 创建时间。
fn parse_mvhd(payload: &[u8]) -> Mvhd {
    let mut out = Mvhd::default();
    let (version, _flags) = match full_box_vf(payload) {
        Some(v) => v,
        None => return out,
    };
    let mut cur = ByteCursor::new(payload);
    if cur.seek(4).is_none() {
        return out;
    }
    let (creation, timescale, duration) = if version == 1 {
        let creation = match read_uint_be(&mut cur, 8) { Some(v) => v, None => return out };
        if read_uint_be(&mut cur, 8).is_none() { return out; } // modification_time
        let timescale = match cur.u32(Endian::Big) { Some(v) => v, None => return out };
        let duration = match read_uint_be(&mut cur, 8) { Some(v) => v, None => return out };
        (creation, timescale, duration)
    } else {
        let creation = match cur.u32(Endian::Big) { Some(v) => u64::from(v), None => return out };
        if cur.u32(Endian::Big).is_none() { return out; } // modification_time
        let timescale = match cur.u32(Endian::Big) { Some(v) => v, None => return out };
        let duration = match cur.u32(Endian::Big) { Some(v) => u64::from(v), None => return out };
        (creation, timescale, duration)
    };
    // duration_ms = duration * 1000 / timescale（u128 中间量防溢出）。
    if timescale == 0 {
        out.timescale_invalid = true;
    } else {
        let ms = u128::from(duration) * 1000 / u128::from(timescale);
        match u64::try_from(ms) {
            Ok(v) => out.duration_ms = Some(v),
            Err(_) => out.timescale_invalid = true, // 溢出当作无效，发警告、不臆造
        }
    }
    if creation != 0 {
        out.created = Some(datetime_from_mp4_epoch(creation));
    }
    out
}
```

> `cur.seek(4)` 跳 version/flags 后，version 1 用 `read_uint_be(&mut cur, 8)` 读 u64（A1 已提供 size∈{0,4,8}）。

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p omni-meta-core parse_mvhd`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat(bmff): parse_mvhd → duration_ms + created（v0/v1, timescale=0/溢出/creation=0 容错）(A3)"
```

---

## Task 6: bmff — `parse_tkhd`（16.16 定点维度）

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`

- [ ] **Step 1: Write the failing test**

加到 `bmff.rs` 的 `mod tests`：

```rust
    /// tkhd 载荷（box 头之后），version 0。width/height 为 16.16 定点。
    fn tkhd_v0(w: u32, h: u32) -> Vec<u8> {
        let mut p = alloc::vec![0u8, 0, 0, 7]; // version 0, flags=0x000007
        p.extend_from_slice(&0u32.to_be_bytes()); // creation
        p.extend_from_slice(&0u32.to_be_bytes()); // modification
        p.extend_from_slice(&1u32.to_be_bytes()); // track_ID
        p.extend_from_slice(&0u32.to_be_bytes()); // reserved
        p.extend_from_slice(&0u32.to_be_bytes()); // duration
        p.extend_from_slice(&[0u8; 8]); // reserved[2]
        p.extend_from_slice(&0i16.to_be_bytes()); // layer
        p.extend_from_slice(&0i16.to_be_bytes()); // alternate_group
        p.extend_from_slice(&0i16.to_be_bytes()); // volume
        p.extend_from_slice(&0u16.to_be_bytes()); // reserved
        p.extend_from_slice(&[0u8; 36]); // matrix[9]
        p.extend_from_slice(&(w << 16).to_be_bytes()); // width 16.16
        p.extend_from_slice(&(h << 16).to_be_bytes()); // height 16.16
        p
    }

    #[test]
    fn parse_tkhd_v0_fixed_point_dims() {
        assert_eq!(parse_tkhd(&tkhd_v0(1920, 1080)), Some((1920, 1080)));
    }

    #[test]
    fn parse_tkhd_zero_dims_is_none() {
        // 音频/数据轨 width=height=0 → None（不选作维度来源）。
        assert_eq!(parse_tkhd(&tkhd_v0(0, 0)), None);
    }

    #[test]
    fn parse_tkhd_truncated_is_none() {
        assert_eq!(parse_tkhd(&[0u8, 0, 0, 0, 1, 2]), None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omni-meta-core parse_tkhd_v0_fixed_point_dims`
Expected: FAIL — `cannot find function parse_tkhd`。

- [ ] **Step 3: Write minimal implementation**

在 `bmff.rs`（`parse_mvhd` 之后）加：

```rust
/// 解析 `tkhd`（TrackHeaderBox）载荷 → (width, height) 像素整数。
/// width/height 为载荷末 8 字节的 16.16 定点；按 version 计算偏移以避免误读尾随字节。
/// 任一为 0（音频/数据/提示轨）或截断 → None。
fn parse_tkhd(payload: &[u8]) -> Option<(u32, u32)> {
    let (version, _flags) = full_box_vf(payload)?;
    // version 0: width @76 height @80（载荷 ≥84）；version 1: width @88 height @92（≥96）。
    let woff = if version == 1 { 88 } else { 76 };
    let wfix = read_u32_at(payload, woff)?;
    let hfix = read_u32_at(payload, woff + 4)?;
    let (w, h) = (wfix >> 16, hfix >> 16);
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

/// 从切片指定偏移读大端 u32；越界 → None。
fn read_u32_at(b: &[u8], off: usize) -> Option<u32> {
    let end = off.checked_add(4)?;
    let s = b.get(off..end)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p omni-meta-core parse_tkhd`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat(bmff): parse_tkhd 16.16 定点维度（音频轨 0×0 跳过）(A3)"
```

---

## Task 7: bmff — `parse_moov`（聚合 mvhd + 逐 trak tkhd）

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`

- [ ] **Step 1: Write the failing test**

加到 `bmff.rs` 的 `mod tests`。复用既有 `box_bytes` 辅助（A2 已定义于本 mod）：

```rust
    /// trak{ tkhd }。
    fn trak(tkhd_payload: &[u8]) -> Vec<u8> {
        box_bytes(b"trak", &box_bytes(b"tkhd", tkhd_payload))
    }

    #[test]
    fn parse_moov_picks_video_track_and_time() {
        // moov{ mvhd, trak(audio 0×0), trak(video 1920×1080) }
        let mut moov_p = Vec::new();
        moov_p.extend_from_slice(&box_bytes(b"mvhd", &mvhd_v0(2_082_844_800, 600, 900_900)));
        moov_p.extend_from_slice(&trak(&tkhd_v0(0, 0)));        // 音频轨先出现
        moov_p.extend_from_slice(&trak(&tkhd_v0(1920, 1080)));  // 视频轨
        let info = parse_moov(&moov_p, 0);
        assert_eq!(info.dims, Some((1920, 1080))); // 跳过 0×0，选视频
        assert_eq!(info.duration_ms, Some(1_501_500));
        assert_eq!(info.created.map(|d| d.year), Some(1970));
        assert!(info.warnings.is_empty());
    }

    #[test]
    fn parse_moov_timescale_zero_warns() {
        let mut moov_p = Vec::new();
        moov_p.extend_from_slice(&box_bytes(b"mvhd", &mvhd_v0(0, 0, 1000)));
        let info = parse_moov(&moov_p, 0);
        assert_eq!(info.duration_ms, None);
        assert_eq!(info.warnings.len(), 1);
        assert_eq!(info.warnings[0].kind, WarnKind::UnrecognizedValue);
    }

    #[test]
    fn parse_moov_no_mvhd_no_trak_is_empty() {
        let info = parse_moov(&box_bytes(b"free", &[0u8; 4]), 0);
        assert_eq!(info.dims, None);
        assert_eq!(info.duration_ms, None);
        assert_eq!(info.created, None);
        assert!(info.warnings.is_empty());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omni-meta-core parse_moov_picks_video_track_and_time`
Expected: FAIL — `cannot find function parse_moov` / 类型 `MoovInfo` 未定义。

- [ ] **Step 3: Write minimal implementation**

在 `bmff.rs`（`parse_tkhd`/`read_u32_at` 之后）加：

```rust
/// moov 解析产物。
struct MoovInfo {
    dims: Option<(u32, u32)>,
    duration_ms: Option<u64>,
    created: Option<DateTimeParts>,
    warnings: Vec<Warning>,
}

/// 解析 `moov` 载荷：mvhd → 时长/创建时间；逐 trak → tkhd 取首个非零维度。
/// `moov_abs_base` 仅用于警告偏移保真。深度 2 显式迭代，非递归。
fn parse_moov(moov_payload: &[u8], moov_abs_base: u64) -> MoovInfo {
    let mut info = MoovInfo { dims: None, duration_ms: None, created: None, warnings: Vec::new() };
    for (hdr, p) in iter_child_boxes(moov_payload) {
        match &hdr.kind {
            b"mvhd" => {
                let m = parse_mvhd(p);
                info.duration_ms = m.duration_ms;
                info.created = m.created;
                if m.timescale_invalid {
                    info.warnings.push(Warning { offset: moov_abs_base, kind: WarnKind::UnrecognizedValue });
                }
            }
            b"trak" if info.dims.is_none() => {
                for (thdr, tp) in iter_child_boxes(p) {
                    if &thdr.kind == b"tkhd"
                        && let Some(d) = parse_tkhd(tp)
                    {
                        info.dims = Some(d);
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    info
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p omni-meta-core parse_moov`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat(bmff): parse_moov 聚合 mvhd + 逐 trak tkhd（首个非零维度）(A3)"
```

---

## Task 8: bmff — `pull_walk` 增 `moov` 分支 + 端到端

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`（`pull_walk` 函数）

- [ ] **Step 1: Write the failing test**

加到 `bmff.rs` 的 `mod tests`。复用 `ftyp_heic`/`box_bytes`；新增 mp4 ftyp 与组装：

```rust
    fn ftyp_mp4() -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(b"isom");
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(b"mp42");
        box_bytes(b"ftyp", &p)
    }

    fn mp4_with_moov() -> Vec<u8> {
        let mut moov_p = Vec::new();
        moov_p.extend_from_slice(&box_bytes(b"mvhd", &mvhd_v0(2_082_844_800, 600, 900_900)));
        moov_p.extend_from_slice(&trak(&tkhd_v0(1920, 1080)));
        let mut f = ftyp_mp4();
        f.extend_from_slice(&box_bytes(b"moov", &moov_p));
        f
    }

    #[test]
    fn end_to_end_mp4_moov() {
        let buf = mp4_with_moov();
        let col = crate::driver::drive_slice(&buf, &mut BmffParser::new(), crate::limits::Limits::default());
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Mp4);
        assert!(meta.warnings.is_empty(), "warnings: {:?}", meta.warnings);
        assert_eq!(meta.unified.width, Some(1920));
        assert_eq!(meta.unified.height, Some(1080));
        assert_eq!(meta.unified.duration_ms, Some(1_501_500));
        assert_eq!(meta.unified.created.map(|d| d.year), Some(1970));
        assert_eq!(meta.unified.created.and_then(|d| d.tz_offset_min), Some(0));
    }

    #[test]
    fn end_to_end_mp4_moov_after_mdat() {
        // moov 在 mdat 之后（非 faststart）：walk 须 Skip(mdat) 再缓冲 moov。
        let mut moov_p = Vec::new();
        moov_p.extend_from_slice(&box_bytes(b"mvhd", &mvhd_v0(0, 600, 1200)));
        moov_p.extend_from_slice(&trak(&tkhd_v0(640, 480)));
        let mut f = ftyp_mp4();
        f.extend_from_slice(&box_bytes(b"mdat", &[0u8; 64])); // 大盒被跳过、不缓冲
        f.extend_from_slice(&box_bytes(b"moov", &moov_p));
        let col = crate::driver::drive_slice(&f, &mut BmffParser::new(), crate::limits::Limits::default());
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Mp4);
        assert_eq!(meta.unified.width, Some(640));
        assert_eq!(meta.unified.duration_ms, Some(2000));
        assert_eq!(meta.unified.created, None); // creation_time==0
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omni-meta-core end_to_end_mp4_moov`
Expected: FAIL — `pull_walk` 不识别 `moov`，当作普通盒 `Skip`，无字段产出（width 为 None）。

- [ ] **Step 3: Write minimal implementation**

在 `pull_walk`（`bmff.rs`）中，`if &hdr.kind == b"meta" { … }` 整块**之后、**「非 meta：跳过整盒」注释**之前**，插入 `moov` 分支。与 meta 同款「整盒入窗后解析」，但解析后直接 `Done`（moov 无 Extract 目标）：

```rust
        if &hdr.kind == b"moov" {
            let total = match hdr.total_size {
                Some(t) => t,
                None => {
                    self.done = true;
                    return PullResult { demand: Demand::Done, consumed: 0, events: Vec::new() };
                }
            };
            let need = match usize::try_from(total) {
                Ok(n) => n,
                Err(_) => {
                    self.done = true;
                    return PullResult { demand: Demand::Done, consumed: 0, events: Vec::new() };
                }
            };
            let header_len = hdr.header_len as usize;
            if need < header_len {
                self.done = true;
                return PullResult { demand: Demand::Done, consumed: 0, events: Vec::new() };
            }
            if input.len() < need {
                return PullResult { demand: Demand::NeedBytes(need), consumed: 0, events: Vec::new() };
            }
            let info = parse_moov(&input[header_len..need], self.pos);
            let mut events: Vec<Event<'a>> = Vec::new();
            if let Some((w, h)) = info.dims {
                events.push(Event::Field(Field::Width(w)));
                events.push(Event::Field(Field::Height(h)));
            }
            if let Some(ms) = info.duration_ms {
                events.push(Event::Field(Field::Duration(ms)));
            }
            if let Some(dt) = info.created {
                events.push(Event::Field(Field::Created(dt)));
            }
            for warn in info.warnings {
                events.push(Event::Warning(warn));
            }
            self.done = true;
            return PullResult { demand: Demand::Done, consumed: need, events };
        }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p omni-meta-core end_to_end_mp4`
Expected: PASS（`end_to_end_mp4_moov` 与 `end_to_end_mp4_moov_after_mdat` 均绿）。

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat(bmff): pull_walk 识别 moov, 整盒入窗解析发 Field 后 Done (A3)"
```

---

## Task 9: bmff — 畸形/抗 fuzz 合成单测

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`（`mod tests`）

- [ ] **Step 1: Write the failing test**

加到 `bmff.rs` 的 `mod tests`：

```rust
    #[test]
    fn drive_truncated_moov_warns_truncated() {
        // moov 声明 size=300 但实际仅 20 字节 → driver 到 EOF 记 Truncated，不 panic。
        let mut buf = ftyp_mp4();
        let mut moov = Vec::new();
        moov.extend_from_slice(&300u32.to_be_bytes());
        moov.extend_from_slice(b"moov");
        moov.extend_from_slice(&[0u8; 12]);
        buf.extend_from_slice(&moov);
        let col = crate::driver::drive_slice(&buf, &mut BmffParser::new(), crate::limits::Limits::default());
        assert!(col.warnings.iter().any(|w| w.kind == WarnKind::Truncated));
    }

    #[test]
    fn parse_mvhd_duration_overflow_no_panic() {
        // duration=u64::MAX, timescale=1 → *1000 溢出 u64 → 无 duration、标记无效。
        let m = parse_mvhd(&mvhd_v1(2_082_844_800, 1, u64::MAX));
        assert_eq!(m.duration_ms, None);
        assert!(m.timescale_invalid);
    }

    #[test]
    fn parse_moov_nested_overrun_does_not_panic() {
        // trak 声明子盒长度越界 → iter_child_boxes 停止，不 panic、无维度。
        let mut bad_trak_p = Vec::new();
        bad_trak_p.extend_from_slice(&999u32.to_be_bytes()); // tkhd 声明 999 > 实际
        bad_trak_p.extend_from_slice(b"tkhd");
        bad_trak_p.extend_from_slice(&[0u8; 4]);
        let mut moov_p = Vec::new();
        moov_p.extend_from_slice(&box_bytes(b"trak", &bad_trak_p));
        let info = parse_moov(&moov_p, 0);
        assert_eq!(info.dims, None);
    }

    #[test]
    fn drive_moov_declared_larger_than_file_is_truncated() {
        // 顶层 moov 头声明 size 大于文件剩余 → NeedBytes 到 EOF → Truncated，绝不 panic。
        let mut buf = ftyp_mp4();
        let mut moov = Vec::new();
        moov.extend_from_slice(&100_000u32.to_be_bytes());
        moov.extend_from_slice(b"moov");
        moov.extend_from_slice(&[0u8; 8]);
        buf.extend_from_slice(&moov);
        let col = crate::driver::drive_slice(&buf, &mut BmffParser::new(), crate::limits::Limits::default());
        assert!(col.warnings.iter().any(|w| w.kind == WarnKind::Truncated));
    }
```

- [ ] **Step 2: Run test to verify it fails / passes**

Run: `cargo test -p omni-meta-core -- drive_truncated_moov parse_mvhd_duration_overflow parse_moov_nested_overrun drive_moov_declared_larger`
Expected: 全 PASS（Task 5–8 的实现已覆盖这些路径；此 Task 锁定不变量，不应再改实现）。若任一 FAIL，按 superpowers:systematic-debugging 定位——预期它们直接绿。

- [ ] **Step 3: （仅当上步有 FAIL 时）修实现**

不应发生。若 `parse_mvhd_duration_overflow_no_panic` 失败，复核 Task 5 的 `u64::try_from(ms)` 溢出分支。

- [ ] **Step 4: Run full core test suite**

Run: `cargo test -p omni-meta-core`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "test(bmff): moov 截断/溢出/嵌套越界 合成畸形单测（永不 panic 不变量）(A3)"
```

---

## Task 10: 差分测试 — MP4 四适配器一致性

**Files:**
- Modify: `omni-meta/tests/differential.rs`

- [ ] **Step 1: Write the failing test**

在 `differential.rs` 末尾追加。复用文件内已有的 `bmff_box` 辅助：

```rust
fn mp4_mvhd_v0(creation: u32, timescale: u32, duration: u32) -> Vec<u8> {
    let mut p = vec![0u8, 0, 0, 0];
    p.extend_from_slice(&creation.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&timescale.to_be_bytes());
    p.extend_from_slice(&duration.to_be_bytes());
    p
}

fn mp4_tkhd_v0(w: u32, h: u32) -> Vec<u8> {
    let mut p = vec![0u8, 0, 0, 7];
    p.extend_from_slice(&0u32.to_be_bytes()); // creation
    p.extend_from_slice(&0u32.to_be_bytes()); // modification
    p.extend_from_slice(&1u32.to_be_bytes()); // track_ID
    p.extend_from_slice(&0u32.to_be_bytes()); // reserved
    p.extend_from_slice(&0u32.to_be_bytes()); // duration
    p.extend_from_slice(&[0u8; 8]);           // reserved[2]
    p.extend_from_slice(&[0u8; 8]);           // layer/alt/volume/reserved
    p.extend_from_slice(&[0u8; 36]);          // matrix
    p.extend_from_slice(&(w << 16).to_be_bytes());
    p.extend_from_slice(&(h << 16).to_be_bytes());
    p
}

fn fixture_bmff_mp4() -> Vec<u8> {
    let mut ftyp_p = Vec::new();
    ftyp_p.extend_from_slice(b"isom");
    ftyp_p.extend_from_slice(&0u32.to_be_bytes());
    ftyp_p.extend_from_slice(b"mp42");
    let ftyp = bmff_box(b"ftyp", &ftyp_p);

    let mut moov_p = Vec::new();
    moov_p.extend_from_slice(&bmff_box(b"mvhd", &mp4_mvhd_v0(2_082_844_800, 600, 900_900)));
    moov_p.extend_from_slice(&bmff_box(b"trak", &bmff_box(b"tkhd", &mp4_tkhd_v0(1920, 1080))));
    let moov = bmff_box(b"moov", &moov_p);

    let mut f = Vec::new();
    f.extend_from_slice(&ftyp);
    f.extend_from_slice(&moov);
    f
}

/// moov 在 mdat 之后：行使 read_seek 的 Skip/seek 路径。
fn fixture_bmff_mp4_moov_after_mdat() -> Vec<u8> {
    let mut ftyp_p = Vec::new();
    ftyp_p.extend_from_slice(b"isom");
    ftyp_p.extend_from_slice(&0u32.to_be_bytes());
    ftyp_p.extend_from_slice(b"mp42");
    let ftyp = bmff_box(b"ftyp", &ftyp_p);

    let mut moov_p = Vec::new();
    moov_p.extend_from_slice(&bmff_box(b"mvhd", &mp4_mvhd_v0(0, 1000, 5000)));
    moov_p.extend_from_slice(&bmff_box(b"trak", &bmff_box(b"tkhd", &mp4_tkhd_v0(640, 480))));
    let moov = bmff_box(b"moov", &moov_p);

    let mdat = bmff_box(b"mdat", &[0u8; 10_000]); // >8192 读块，强制 seek 路径

    let mut f = Vec::new();
    f.extend_from_slice(&ftyp);
    f.extend_from_slice(&mdat);
    f.extend_from_slice(&moov);
    f
}

#[test]
fn differential_bmff_mp4() {
    assert_all_equal(&fixture_bmff_mp4());
}

#[test]
fn differential_bmff_mp4_moov_after_mdat() {
    assert_all_equal(&fixture_bmff_mp4_moov_after_mdat());
}
```

- [ ] **Step 2: Run test to verify it fails then passes**

Run: `cargo test -p omni-meta differential_bmff_mp4`
Expected: PASS（实现已在 Task 1–8 完成；此 Task 是跨适配器一致性回归网）。若 FAIL，多半是某适配器在 moov-after-mdat 的 Skip/seek 行为分歧——按 systematic-debugging 定位。

- [ ] **Step 3: Run both crates' full suites**

Run: `cargo test`
Expected: 全 PASS。

- [ ] **Step 4: Commit**

```bash
git add omni-meta/tests/differential.rs
git commit -m "test(differential): MP4 moov 四适配器一致性（含 moov-after-mdat seek 路径）(A3)"
```

---

## Task 11: 收尾验证 + ROADMAP

**Files:**
- Modify: `docs/ROADMAP.md`

- [ ] **Step 1: 全量验证（no_std + clippy + test）**

Run:
```bash
cargo test
cargo build -p omni-meta-core --no-default-features
cargo clippy --all-targets -- -D warnings
```
Expected: 三条全部成功、零 warning。若 clippy 报 `collapsible_if`/`needless_return` 等，按提示清整（参考 A2 的 `style: A2 BMFF clippy 清整` 提交风格）。

- [ ] **Step 2: 更新 ROADMAP**

`docs/ROADMAP.md` 顶部「最近更新」改为标注 A3 完成。把「已完成 ✅」表追加一行：

```markdown
| **格式：MP4/MOV (A3)** | `moov` 下钻：`mvhd`→`duration_ms`/`created`（1904 UTC 秒→DateTimeParts），逐 `trak`/`tkhd`→维度（16.16 定点，首个非零轨）；`timescale=0`/溢出/`creation=0`/截断/嵌套越界均警告或干净缺失、不 panic | （本次提交） |
```

把 §3 里程碑 A 的 A3 三条 `- [ ]` 勾为 `- [x]`，并把第三条改为反映「合成畸形单测落地、cargo-fuzz 另立横切」：

```markdown
**A3（MP4/MOV moov，✅ 完成）** — 设计 `specs/2026-06-15-omni-meta-bmff-moov-design.md` / 计划 `plans/2026-06-15-omni-meta-bmff-moov.md`
- [x] MP4/MOV：`moov` 维度（tkhd）+ 时长（mvhd duration/timescale→ms）+ 创建时间（mvhd 1904 UTC）→ `Event::Field`
- [x] 新增 Unified 字段：`duration_ms`（BMFF moov；EBML 里程碑 C 补第二来源）、`created`（BMFF moov + EXIF ≥2 满足，含 OffsetTime 解析）+ `DateTimeParts` 带可选时区
- [x] box 嵌套/截断/越界 合成畸形单测（cargo-fuzz 作为独立横切里程碑另立，见 §4）
```

「当前 Unified 字段」段补充 `duration_ms` / `created` 与来源计数说明。「尚未开始 ⬜」段从 ISO-BMFF 行移除 MP4/MOV（已完成）。

- [ ] **Step 3: Commit**

```bash
git add docs/ROADMAP.md
git commit -m "docs: ROADMAP 标记 A3 完成（MP4/MOV moov + duration_ms/created）"
```

- [ ] **Step 4: 完成分支整合**

REQUIRED SUB-SKILL: 用 superpowers:finishing-a-development-branch 决定合入方式。用户偏好 **ff-only 合入 main**（见自动记忆 dev-workflow）。

---

## Self-Review（写完即对照 spec 自查）

- **Spec 覆盖**：§1 维度(tkhd)=Task 6/7/8；时长(mvhd)=Task 5；创建时间(mvhd)=Task 5；EXIF 第二来源+OffsetTime=Task 3；DateTimeParts=Task 1；duration_ms/created Unified=Task 1/2；畸形抗性=Task 9；四适配器一致=Task 10；no_std/clippy=Task 11。✅ 全覆盖。
- **类型一致性**：`DateTimeParts` 字段（year:u16, month/day/hour/minute/second:u8, tz_offset_min:Option<i16>）在 Task 1/3/4/5 一致；`Field::Duration(u64)`/`Field::Created(DateTimeParts)` 在 Task 1/2/8 一致；`parse_mvhd`→`Mvhd{duration_ms,created,timescale_invalid}`、`parse_moov`→`MoovInfo{dims,duration_ms,created,warnings}`、`parse_tkhd`→`Option<(u32,u32)>` 在定义处（Task 5/6/7）与调用处（Task 7/8）一致。✅
- **无占位符**：每个改码步骤均含完整代码与确切路径/命令/预期输出。✅
- **TDD/小步**：每 Task = 写测试→见失败→最小实现→见通过→提交。✅

---

## 执行方式

计划已存 `docs/superpowers/plans/2026-06-15-omni-meta-bmff-moov.md`。两种执行选项：

1. **Subagent-Driven（推荐）** — 每 Task 派新 subagent，Task 间两段式审查，快速迭代。
2. **Inline Execution** — 本会话内按 Task 批量执行、检查点审查。

选哪种？
