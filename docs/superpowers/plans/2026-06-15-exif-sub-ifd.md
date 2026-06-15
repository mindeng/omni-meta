# EXIF Sub-IFD 支持 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 EXIF 解码跟随 sub-IFD(Exif `0x8769`、GPS `0x8825`、Interop `0xA005`)与 next-IFD(IFD1 缩略图)指针,把这些 IFD 的标签连同正确的来源标识与扩展值类型写入 `raw.exif`。

**Architecture:** 用一个**扁平工作队列**替换原先单次解析 IFD0 的逻辑——队列项为 `(offset, IfdKind)`,`visited` 集合防环,`max_ifds` 封顶 IFD 总数。值读取器泛化为按 EXIF type 计算单元大小、边界安全地解出标量或 `List` 数组。数据止于 `raw.exif`,`normalize` 仅加一处 `Primary` 防护,不新增 `Unified` 字段。

**Tech Stack:** Rust(`no_std` + `alloc`),既有 `ByteCursor`(边界安全字节游标)、`Limits`(分配上界)、cargo test。

**参考规格:** `docs/superpowers/specs/2026-06-15-exif-sub-ifd-design.md`

**通用命令:**
- 跑单个测试:`cargo test -p omni-meta-core <test_name> -- --nocapture`
- 跑某文件全部:`cargo test -p omni-meta-core`
- 跑差分测试:`cargo test -p omni-meta --test differential`
- clippy(本仓库要求干净):`cargo clippy --workspace --all-targets -- -D warnings`

---

## File Structure

- `omni-meta-core/src/model.rs` —— 新增 `IfdKind` 枚举;`ExifTag.ifd` 改类型;`Value` 新增变体。
- `omni-meta-core/src/limits.rs` —— 新增 `max_ifds` 字段。
- `omni-meta-core/src/codecs/exif.rs` —— 工作队列遍历、指针跟随、泛化值读取器。
- `omni-meta-core/src/normalize.rs` —— `Primary` 防护(一行)+ 回归测试。
- `omni-meta/tests/differential.rs` —— sub-IFD 跨适配器一致性 fixture。

---

## Task 1: 引入 `IfdKind` 枚举并迁移 `ExifTag.ifd`

纯重构:把 `ExifTag.ifd: u8`(恒为 0)改为 `IfdKind` 枚举,更新所有现存构造点,保持全绿。无行为变化。

**Files:**
- Modify: `omni-meta-core/src/model.rs`
- Modify: `omni-meta-core/src/codecs/exif.rs`
- Modify: `omni-meta-core/src/normalize.rs`

- [ ] **Step 1: 在 model.rs 增加 `IfdKind` 枚举**

在 `model.rs` 的 `ExifTag` 定义之前插入:

```rust
/// EXIF IFD 来源标识。raw 层据此记录每条标签所属的 IFD。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfdKind {
    Primary,   // IFD0
    Thumbnail, // IFD1（next-IFD 链）
    Exif,      // 0x8769
    Gps,       // 0x8825
    Interop,   // 0xA005
}
```

- [ ] **Step 2: 修改 `ExifTag.ifd` 字段类型**

在 `model.rs` 把:

```rust
pub struct ExifTag {
    pub ifd: u8,
    pub tag: u16,
    pub value: Value,
}
```

改为:

```rust
pub struct ExifTag {
    pub ifd: IfdKind,
    pub tag: u16,
    pub value: Value,
}
```

- [ ] **Step 3: 更新 exif.rs 的构造点与导入**

在 `exif.rs` 顶部把 `use crate::model::{ExifTag, Value, WarnKind, Warning};` 改为:

```rust
use crate::model::{ExifTag, IfdKind, Value, WarnKind, Warning};
```

把 `parse_ifd` 中的 push(当前约 82 行):

```rust
out.push(ExifTag { ifd: 0, tag, value: val });
```

改为:

```rust
out.push(ExifTag { ifd: IfdKind::Primary, tag, value: val });
```

并把 `exif.rs` 测试中两处 `ExifTag { ifd: 0, tag: ..., value: ... }`(`decodes_make_and_orientation` 与 `decodes_big_endian` 各两条断言,共 4 处)的 `ifd: 0` 改为 `ifd: IfdKind::Primary`。`IfdKind` 已经过模块顶部 `use` 引入,测试 `use super::*;` 可见。

- [ ] **Step 4: 更新 normalize.rs 测试构造点**

在 `normalize.rs` 测试模块的 `use crate::model::{ExifTag, XmpProperty};` 改为:

```rust
use crate::model::{ExifTag, IfdKind, XmpProperty};
```

把测试中所有 `ExifTag { ifd: 0, ... }`(`projects_exif_tags_to_unified` 3 处、`unknown_orientation_value_is_dropped_with_warning` 1 处、`exif_wins_over_xmp` 1 处)的 `ifd: 0` 改为 `ifd: IfdKind::Primary`。

- [ ] **Step 5: 编译并跑全套测试,确认全绿**

Run: `cargo test -p omni-meta-core`
Expected: PASS(无行为变化,所有既有测试通过)

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: 无警告

- [ ] **Step 6: Commit**

```bash
git add omni-meta-core/src/model.rs omni-meta-core/src/codecs/exif.rs omni-meta-core/src/normalize.rs
git commit -m "refactor: ExifTag.ifd u8 → IfdKind 枚举 (迁移构造点)"
```

---

## Task 2: 扩展 `Value` 类型并泛化值读取器

新增 `Value` 变体,把 `read_value` 从"仅 ASCII + SHORT cnt==1"泛化为按 EXIF type 解出标量/数组。此任务**不动**遍历逻辑(仍只解 IFD0),通过构造 IFD0 单条目 TIFF 来驱动测试。

**Files:**
- Modify: `omni-meta-core/src/model.rs`
- Modify: `omni-meta-core/src/codecs/exif.rs`

- [ ] **Step 1: 给 `Value` 新增变体**

在 `model.rs` 把:

```rust
pub enum Value {
    U16(u16),
    Text(String),
}
```

改为:

```rust
pub enum Value {
    U16(u16),            // SHORT, cnt==1
    U32(u32),            // LONG,  cnt==1
    Text(String),        // ASCII
    Rational(u32, u32),  // RATIONAL  num/den
    SRational(i32, i32), // SRATIONAL num/den
    Bytes(Vec<u8>),      // BYTE / UNDEFINED
    List(Vec<Value>),    // 任意数值类型 cnt>1（如 GPS lat = 3×Rational）
}
```

- [ ] **Step 2: 写失败测试 —— RATIONAL / LONG / SRATIONAL / UNDEFINED / SHORT 数组 / 未知类型**

在 `exif.rs` 测试模块末尾(`mod tests` 内)加入一个单条目 TIFF 构造器与一组测试:

```rust
/// 构造小端单条目 IFD0 TIFF：外部数据(若有)紧跟 next-IFD 之后,起始偏移 26。
fn tiff_one(tag: u16, typ: u16, cnt: u32, valoff: [u8; 4], external: &[u8]) -> Vec<u8> {
    let mut t: Vec<u8> = Vec::new();
    t.extend_from_slice(b"II"); // 0..2
    t.extend_from_slice(&42u16.to_le_bytes()); // 2..4
    t.extend_from_slice(&8u32.to_le_bytes()); // 4..8 IFD0 偏移
    t.extend_from_slice(&1u16.to_le_bytes()); // 8..10 count=1
    t.extend_from_slice(&tag.to_le_bytes()); // 10..12
    t.extend_from_slice(&typ.to_le_bytes()); // 12..14
    t.extend_from_slice(&cnt.to_le_bytes()); // 14..18
    t.extend_from_slice(&valoff); // 18..22
    t.extend_from_slice(&0u32.to_le_bytes()); // 22..26 next=0
    debug_assert_eq!(t.len(), 26);
    t.extend_from_slice(external); // @26
    t
}

fn decode_one(t: &[u8]) -> (Vec<ExifTag>, Vec<Warning>) {
    let mut out = Vec::new();
    let mut warns = Vec::new();
    decode(t, &mut out, &mut warns, &Limits::default());
    (out, warns)
}

#[test]
fn reads_rational_external() {
    // FNumber(0x829D) RATIONAL cnt=1 → 8 字节外部数据 @26
    let mut ext = Vec::new();
    ext.extend_from_slice(&4u32.to_le_bytes()); // num
    ext.extend_from_slice(&1u32.to_le_bytes()); // den
    let t = tiff_one(0x829D, 5, 1, 26u32.to_le_bytes(), &ext);
    let (out, warns) = decode_one(&t);
    assert!(warns.is_empty(), "warns: {:?}", warns);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].value, Value::Rational(4, 1));
    assert_eq!(out[0].ifd, IfdKind::Primary);
}

#[test]
fn reads_long_inline() {
    // LONG cnt=1 → 4 字节内联
    let t = tiff_one(0x0111, 4, 1, 1234u32.to_le_bytes(), &[]);
    let (out, _) = decode_one(&t);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].value, Value::U32(1234));
}

#[test]
fn reads_srational_external() {
    // SRATIONAL cnt=1 → 8 字节外部
    let mut ext = Vec::new();
    ext.extend_from_slice(&(-3i32).to_le_bytes());
    ext.extend_from_slice(&2i32.to_le_bytes());
    let t = tiff_one(0x9204, 10, 1, 26u32.to_le_bytes(), &ext);
    let (out, _) = decode_one(&t);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].value, Value::SRational(-3, 2));
}

#[test]
fn reads_undefined_as_bytes() {
    // UNDEFINED cnt=4 → 内联 4 字节
    let t = tiff_one(0x9000, 7, 4, *b"0230", &[]);
    let (out, _) = decode_one(&t);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].value, Value::Bytes(Vec::from(b"0230".as_slice())));
}

#[test]
fn reads_short_array_as_list() {
    // SHORT cnt=2 → 内联 4 字节 → List([U16,U16])
    let t = tiff_one(0x0212, 3, 2, [2, 0, 3, 0], &[]);
    let (out, _) = decode_one(&t);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].value, Value::List(Vec::from([Value::U16(2), Value::U16(3)])));
}

#[test]
fn unknown_type_drops_tag() {
    // type=99 未知 → 丢弃,不 panic
    let t = tiff_one(0x0100, 99, 1, [0, 0, 0, 0], &[]);
    let (out, warns) = decode_one(&t);
    assert!(out.is_empty());
    assert!(warns.is_empty());
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p omni-meta-core reads_rational_external`
Expected: FAIL —— 旧 `read_value` 仅处理 type 2/3,RATIONAL 返回 None,断言 `out.len()==1` 失败。

- [ ] **Step 4: 用泛化读取器替换 `read_value`**

在 `exif.rs` 顶部导入补上 `Endian`(若尚未):确认 `use crate::cursor::{ByteCursor, Endian};` 已存在(原文件已有)。

把整个 `read_value` 函数(连同其文档注释)替换为下列实现:

```rust
/// EXIF type → 单元字节数；未知/不支持(含罕见 SLONG type 9)返回 None。
fn unit_size(typ: u16) -> Option<usize> {
    Some(match typ {
        1 | 2 | 7 => 1, // BYTE / ASCII / UNDEFINED
        3 => 2,         // SHORT
        4 => 4,         // LONG
        5 | 10 => 8,    // RATIONAL / SRATIONAL
        _ => return None,
    })
}

fn read_u32_at(e: Endian, b: &[u8]) -> u32 {
    match e {
        Endian::Little => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        Endian::Big => u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
    }
}

/// 解出一条标签的值。失败(越界/未知类型/超上界)返回 None 并丢弃该标签,绝不 panic。
fn read_value(
    tiff: &[u8],
    e: Endian,
    typ: u16,
    cnt: u32,
    valoff: &[u8],
    max_value_bytes: usize,
) -> Option<Value> {
    debug_assert_eq!(valoff.len(), 4);
    let unit = unit_size(typ)?;
    let total = (cnt as usize).checked_mul(unit)?;
    // cnt==0 畸形；total 超上界则丢弃,防止聚合放大。
    if total == 0 || total > max_value_bytes {
        return None;
    }
    // <=4 字节内联于 valoff,否则按偏移取并做边界检查。
    let data: &[u8] = if total <= 4 {
        &valoff[..total]
    } else {
        let off = read_u32_at(e, valoff) as usize;
        let end = off.checked_add(total)?;
        tiff.get(off..end)?
    };
    decode_typed(e, typ, cnt, data)
}

/// 把已定位的字节切片按 type 解成 Value。ASCII→单个 Text,BYTE/UNDEFINED→单个 Bytes,
/// 数值类型 cnt==1→标量,cnt>1→List。
fn decode_typed(e: Endian, typ: u16, cnt: u32, data: &[u8]) -> Option<Value> {
    match typ {
        2 => {
            let nul = data.iter().position(|&b| b == 0).unwrap_or(data.len());
            let s = core::str::from_utf8(&data[..nul]).ok()?;
            Some(Value::Text(String::from(s)))
        }
        1 | 7 => Some(Value::Bytes(Vec::from(data))),
        _ => {
            let n = cnt as usize;
            let mut items: Vec<Value> = Vec::with_capacity(n);
            let mut cur = ByteCursor::new(data);
            for _ in 0..n {
                items.push(read_scalar(&mut cur, e, typ)?);
            }
            if n == 1 {
                items.into_iter().next()
            } else {
                Some(Value::List(items))
            }
        }
    }
}

/// 从游标读一个数值标量(SHORT/LONG/RATIONAL/SRATIONAL)。
fn read_scalar(cur: &mut ByteCursor, e: Endian, typ: u16) -> Option<Value> {
    match typ {
        3 => Some(Value::U16(cur.u16(e)?)),
        4 => Some(Value::U32(cur.u32(e)?)),
        5 => {
            let num = cur.u32(e)?;
            let den = cur.u32(e)?;
            Some(Value::Rational(num, den))
        }
        10 => {
            let num = cur.u32(e)? as i32;
            let den = cur.u32(e)? as i32;
            Some(Value::SRational(num, den))
        }
        _ => None,
    }
}
```

- [ ] **Step 5: 跑测试确认通过(含既有测试不回归)**

Run: `cargo test -p omni-meta-core`
Expected: PASS —— 新增 6 个测试全过;`decodes_make_and_orientation`、`decodes_big_endian`(ASCII→Text、SHORT cnt1→U16)等既有测试仍过。

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: 无警告

- [ ] **Step 6: Commit**

```bash
git add omni-meta-core/src/model.rs omni-meta-core/src/codecs/exif.rs
git commit -m "feat: 泛化 EXIF 值读取器 (LONG/RATIONAL/SRATIONAL/UNDEFINED/数组) + Value 扩展"
```

---

## Task 3: 给 `Limits` 增加 `max_ifds`

**Files:**
- Modify: `omni-meta-core/src/limits.rs`

- [ ] **Step 1: 写失败测试**

在 `limits.rs` 测试模块加入:

```rust
#[test]
fn max_ifds_has_sane_default() {
    let l = Limits::default();
    assert_eq!(l.max_ifds, 16);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p omni-meta-core max_ifds_has_sane_default`
Expected: FAIL —— `Limits` 无 `max_ifds` 字段,编译错误。

- [ ] **Step 3: 增加字段与默认值**

在 `Limits` 结构体加入字段(放在 `max_tags` 之后):

```rust
    pub max_tags: usize,
    /// 单次 EXIF 解码中实际解析的 IFD 总数上限(扁平计数,非嵌套深度)。
    pub max_ifds: usize,
    pub max_total_alloc: usize,
```

在 `Default` 实现里加入(放在 `max_tags` 之后):

```rust
            max_tags: 8192,
            max_ifds: 16,
            max_total_alloc: 128 * 1024 * 1024,
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p omni-meta-core max_ifds_has_sane_default`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/limits.rs
git commit -m "feat: Limits 增加 max_ifds (IFD 总数上界)"
```

---

## Task 4: 工作队列遍历 + 指针跟随 + next-IFD

把 `decode` 改为扁平工作队列,`parse_ifd` 增加 `kind` 与 `queue` 参数;识别 sub-IFD 指针并入队(不作为数据发出);仅 `Primary` 跟随 next-IFD 为 `Thumbnail`。

**Files:**
- Modify: `omni-meta-core/src/codecs/exif.rs`

- [ ] **Step 1: 写失败测试 —— Exif/GPS/IFD1/环/上界/越界指针**

在 `exif.rs` 测试模块末尾加入。先加一个多 IFD 构造器,再加测试:

```rust
/// 小端 TIFF:IFD0 含一个 Exif 指针(0x8769)指向一个 Exif sub-IFD,
/// 后者含 FNumber(0x829D) RATIONAL=4/1。
/// 布局:
///  @0  II,42,IFD0@8
///  @8  IFD0 count=1
///  @10 entry 0x8769 LONG cnt1 val=26
///  @22 next=0
///  @26 Exif IFD count=1
///  @28 entry 0x829D RATIONAL cnt1 val=44
///  @40 next=0
///  @44 num=4,den=1 (8 字节)
fn tiff_with_exif_subifd() -> Vec<u8> {
    let mut t: Vec<u8> = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    // IFD0 @8
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x8769u16.to_le_bytes());
    t.extend_from_slice(&4u16.to_le_bytes()); // LONG
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&26u32.to_le_bytes()); // → Exif IFD @26
    t.extend_from_slice(&0u32.to_le_bytes()); // next=0
    debug_assert_eq!(t.len(), 26);
    // Exif IFD @26
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x829Du16.to_le_bytes());
    t.extend_from_slice(&5u16.to_le_bytes()); // RATIONAL
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&44u32.to_le_bytes()); // → 数据 @44
    t.extend_from_slice(&0u32.to_le_bytes()); // next=0
    debug_assert_eq!(t.len(), 44);
    t.extend_from_slice(&4u32.to_le_bytes()); // num
    t.extend_from_slice(&1u32.to_le_bytes()); // den
    t
}

#[test]
fn follows_exif_subifd_and_tags_ifd() {
    let t = tiff_with_exif_subifd();
    let (out, warns) = decode_one(&t);
    assert!(warns.is_empty(), "warns: {:?}", warns);
    // 指针标签不作为数据发出 → 仅 1 条(FNumber)
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].ifd, IfdKind::Exif);
    assert_eq!(out[0].tag, 0x829D);
    assert_eq!(out[0].value, Value::Rational(4, 1));
}

#[test]
fn follows_gps_subifd_with_rational_list() {
    // IFD0 含 GPS 指针(0x8825)→ GPS IFD 含 GPSLatitude(0x0002) RATIONAL cnt=3。
    //  @0 II,42,IFD0@8 | @8 count=1 | @10 0x8825 LONG cnt1 val=26 | @22 next=0
    //  @26 GPS IFD count=1 | @28 0x0002 RATIONAL cnt3 val=44 | @40 next=0
    //  @44 三个 rational: (12/1),(34/1),(56/1) = 24 字节
    let mut t: Vec<u8> = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x8825u16.to_le_bytes());
    t.extend_from_slice(&4u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&26u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x0002u16.to_le_bytes());
    t.extend_from_slice(&5u16.to_le_bytes());
    t.extend_from_slice(&3u32.to_le_bytes());
    t.extend_from_slice(&44u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    debug_assert_eq!(t.len(), 44);
    for n in [12u32, 34, 56] {
        t.extend_from_slice(&n.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
    }
    let (out, warns) = decode_one(&t);
    assert!(warns.is_empty(), "warns: {:?}", warns);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].ifd, IfdKind::Gps);
    assert_eq!(
        out[0].value,
        Value::List(Vec::from([
            Value::Rational(12, 1),
            Value::Rational(34, 1),
            Value::Rational(56, 1),
        ]))
    );
}

#[test]
fn follows_next_ifd_as_thumbnail() {
    // IFD0(Orientation=1)→ next 指向 IFD1(Orientation=6)。
    //  @0 II,42,IFD0@8 | @8 count=1 | @10 0x0112 SHORT cnt1 val=1 | @22 next=26
    //  @26 IFD1 count=1 | @28 0x0112 SHORT cnt1 val=6 | @40 next=0
    let mut t: Vec<u8> = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x0112u16.to_le_bytes());
    t.extend_from_slice(&3u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes()); // 内联值=1
    t.extend_from_slice(&26u32.to_le_bytes()); // next=26 → IFD1
    debug_assert_eq!(t.len(), 26);
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x0112u16.to_le_bytes());
    t.extend_from_slice(&3u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&6u32.to_le_bytes()); // 内联值=6
    t.extend_from_slice(&0u32.to_le_bytes()); // next=0
    let (out, warns) = decode_one(&t);
    assert!(warns.is_empty(), "warns: {:?}", warns);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].ifd, IfdKind::Primary);
    assert_eq!(out[0].value, Value::U16(1));
    assert_eq!(out[1].ifd, IfdKind::Thumbnail);
    assert_eq!(out[1].value, Value::U16(6));
}

#[test]
fn cyclic_subifd_pointer_terminates() {
    // IFD0 的 Exif 指针指回 IFD0(@8)→ visited 防护,终止不挂起不 panic。
    //  @0 II,42,IFD0@8 | @8 count=1 | @10 0x8769 LONG cnt1 val=8 | @22 next=0
    let mut t: Vec<u8> = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x8769u16.to_le_bytes());
    t.extend_from_slice(&4u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes()); // 指回 IFD0
    t.extend_from_slice(&0u32.to_le_bytes());
    let (out, _warns) = decode_one(&t);
    // IFD0 只有指针标签(不发出)→ 0 条;关键是终止。
    assert_eq!(out.len(), 0);
}

#[test]
fn max_ifds_caps_subifd_traversal() {
    // max_ifds=1 → 只解 IFD0,Exif sub-IFD 的标签缺席。
    let t = tiff_with_exif_subifd();
    let mut out = Vec::new();
    let mut warns = Vec::new();
    let limits = Limits { max_ifds: 1, ..Limits::default() };
    decode(&t, &mut out, &mut warns, &limits);
    assert!(out.is_empty()); // IFD0 仅含指针标签 → 无数据
}

#[test]
fn out_of_bounds_subifd_pointer_warns_without_panic() {
    // IFD0 的 Exif 指针指向越界偏移 9999。
    //  @8 count=1 | @10 0x8769 LONG cnt1 val=9999 | @22 next=0
    let mut t: Vec<u8> = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x8769u16.to_le_bytes());
    t.extend_from_slice(&4u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&9999u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    let (out, warns) = decode_one(&t);
    assert!(out.is_empty());
    // 越界 IFD 偏移 → parse_ifd seek 失败 → BadExifHeader
    assert_eq!(warns.len(), 1);
    assert_eq!(warns[0].kind, WarnKind::BadExifHeader);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p omni-meta-core follows_exif_subifd_and_tags_ifd`
Expected: FAIL —— 当前不跟随指针;`0x8769` 被当作普通 LONG 标签发出,`out` 含 IFD0 的指针标签而非 Exif IFD 的 FNumber。

- [ ] **Step 3: 重写 `decode` 为工作队列**

把 `exif.rs` 的 `decode` 函数体中 `parse_ifd(tiff, endian, ifd0, out, warnings, limits);` 那一行(及其前面的 header 解析保持不变)替换为工作队列循环。即把:

```rust
    parse_ifd(tiff, endian, ifd0, out, warnings, limits);
}
```

替换为:

```rust
    let mut queue: Vec<(usize, IfdKind)> = Vec::from([(ifd0, IfdKind::Primary)]);
    let mut visited: Vec<usize> = Vec::new();
    let mut ifd_count = 0usize;
    while let Some((off, kind)) = queue.pop() {
        if ifd_count >= limits.max_ifds {
            break;
        }
        if visited.contains(&off) {
            continue;
        }
        visited.push(off);
        ifd_count += 1;
        parse_ifd(tiff, endian, off, kind, out, warnings, limits, &mut queue);
    }
}
```

- [ ] **Step 4: 改造 `parse_ifd` 签名与内部**

把 `parse_ifd` 的签名改为带 `kind` 与 `queue`:

```rust
fn parse_ifd(
    tiff: &[u8],
    e: Endian,
    off: usize,
    kind: IfdKind,
    out: &mut Vec<ExifTag>,
    warnings: &mut Vec<Warning>,
    limits: &Limits,
    queue: &mut Vec<(usize, IfdKind)>,
) {
    let mut cur = ByteCursor::new(tiff);
    if cur.seek(off).is_none() {
        warnings.push(Warning { offset: off as u64, kind: WarnKind::BadExifHeader });
        return;
    }
    let count = match cur.u16(e) {
        Some(c) => c,
        None => {
            warnings.push(Warning { offset: off as u64, kind: WarnKind::Truncated });
            return;
        }
    };
    for _ in 0..count {
        if out.len() >= limits.max_tags {
            break;
        }
        let tag = match cur.u16(e) {
            Some(v) => v,
            None => break,
        };
        let typ = match cur.u16(e) {
            Some(v) => v,
            None => break,
        };
        let cnt = match cur.u32(e) {
            Some(v) => v,
            None => break,
        };
        let valoff = match cur.take(4) {
            Some(s) => s,
            None => break,
        };
        // sub-IFD 指针:读 LONG 偏移入队,指针标签本身不作为数据发出。
        if let Some(child) = subifd_target(kind, tag) {
            queue.push((read_u32_at(e, valoff) as usize, child));
            continue;
        }
        if let Some(val) = read_value(tiff, e, typ, cnt, valoff, limits.max_payload_bytes) {
            out.push(ExifTag { ifd: kind, tag, value: val });
        }
    }
    // next-IFD 仅由 Primary 跟随 → IFD1(Thumbnail)。
    // 用显式偏移定位 next 字段,不依赖提前 break 后的游标位置。
    if kind == IfdKind::Primary
        && let Some(np) = next_ifd_pos(off, count)
    {
        let mut c2 = ByteCursor::new(tiff);
        if c2.seek(np).is_some()
            && let Some(next) = c2.u32(e)
            && next != 0
        {
            queue.push((next as usize, IfdKind::Thumbnail));
        }
    }
}

/// IFD 内 next-IFD 偏移字段的位置:off + 2(count) + count*12(条目)。
fn next_ifd_pos(off: usize, count: u16) -> Option<usize> {
    off.checked_add(2)?
        .checked_add((count as usize).checked_mul(12)?)
}

/// 给定父 IFD 与标签,返回该标签所指向的子 IFD 种类(若为受承认的指针)。
fn subifd_target(parent: IfdKind, tag: u16) -> Option<IfdKind> {
    const TAG_EXIF_IFD: u16 = 0x8769;
    const TAG_GPS_IFD: u16 = 0x8825;
    const TAG_INTEROP_IFD: u16 = 0xA005;
    match (parent, tag) {
        (IfdKind::Primary, TAG_EXIF_IFD) => Some(IfdKind::Exif),
        (IfdKind::Primary, TAG_GPS_IFD) => Some(IfdKind::Gps),
        (IfdKind::Exif, TAG_INTEROP_IFD) => Some(IfdKind::Interop),
        _ => None,
    }
}
```

注:`read_u32_at` 已在 Task 2 定义,此处复用。

- [ ] **Step 5: 跑测试确认通过(含既有测试不回归)**

Run: `cargo test -p omni-meta-core`
Expected: PASS —— Task 4 新增 6 个测试全过;既有 IFD0 测试(`decodes_make_and_orientation` 等,next=0、无指针)行为不变。

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: 无警告(let-chains 写法避免 `collapsible_if`)

- [ ] **Step 6: Commit**

```bash
git add omni-meta-core/src/codecs/exif.rs
git commit -m "feat: EXIF 扁平工作队列遍历 sub-IFD/IFD1 (visited 防环 + max_ifds 封顶)"
```

---

## Task 5: `normalize` 的 `Primary` 防护

IFD1 出现后,Orientation 等标签可能同时存在于 IFD0 与缩略图 IFD。限制 `normalize` 的 EXIF 循环只取 `Primary`,防止新 IFD 泄漏进既有 `Unified` 字段。

**Files:**
- Modify: `omni-meta-core/src/normalize.rs`

- [ ] **Step 1: 写失败测试**

在 `normalize.rs` 测试模块加入:

```rust
#[test]
fn thumbnail_ifd_does_not_pollute_unified() {
    // IFD0 Orientation=Normal(1),IFD1(Thumbnail) Orientation=Rotate90(6)。
    // Unified.orientation 必须只反映 IFD0。
    let raw = RawTags {
        exif: Vec::from([
            ExifTag { ifd: IfdKind::Primary, tag: 0x0112, value: Value::U16(1) },
            ExifTag { ifd: IfdKind::Thumbnail, tag: 0x0112, value: Value::U16(6) },
        ]),
        xmp: Vec::new(),
    };
    let mut warnings = Vec::new();
    let u = normalize(&raw, &mut warnings);
    assert_eq!(u.orientation, Some(Orientation::Normal));
    assert!(warnings.is_empty(), "warnings: {:?}", warnings);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p omni-meta-core thumbnail_ifd_does_not_pollute_unified`
Expected: FAIL —— 当前 `normalize` 按 tag 号匹配,IFD1 的 Orientation=6 覆盖了 IFD0 的 1,`u.orientation` 得到 `Rotate90`。

- [ ] **Step 3: 加 `Primary` 防护**

在 `normalize.rs` 顶部导入加上 `IfdKind`:把 `use crate::model::{Orientation, RawTags, Unified, Value, WarnKind, Warning};` 改为:

```rust
use crate::model::{IfdKind, Orientation, RawTags, Unified, Value, WarnKind, Warning};
```

在 EXIF 循环体首行加守卫。把:

```rust
    for t in &raw.exif {
        match (t.tag, &t.value) {
```

改为:

```rust
    for t in &raw.exif {
        if t.ifd != IfdKind::Primary {
            continue;
        }
        match (t.tag, &t.value) {
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p omni-meta-core`
Expected: PASS —— 新测试过,既有 normalize 测试(全部 `Primary`)不受影响。

- [ ] **Step 5: Commit**

```bash
git add omni-meta-core/src/normalize.rs
git commit -m "fix: normalize 仅投影 Primary IFD (防 IFD1 标签污染 Unified)"
```

---

## Task 6: 差分测试 —— sub-IFD 跨适配器一致

确认带 sub-IFD 的 EXIF 在 slice/blocking/seek/push 四适配器下逐字段一致。

**Files:**
- Modify: `omni-meta/tests/differential.rs`

- [ ] **Step 1: 加带 sub-IFD 的 TIFF 构造器与差分测试**

在 `differential.rs` 末尾加入(`make_tiff_subifd` 复刻 `exif.rs` 测试中 `tiff_with_exif_subifd` 的布局:IFD0→Exif sub-IFD,FNumber RATIONAL=4/1):

```rust
fn make_tiff_subifd() -> Vec<u8> {
    let mut t: Vec<u8> = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    // IFD0 @8: 仅一个 Exif 指针
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x8769u16.to_le_bytes());
    t.extend_from_slice(&4u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&26u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    // Exif IFD @26: FNumber RATIONAL cnt1 @44
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x829Du16.to_le_bytes());
    t.extend_from_slice(&5u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&44u32.to_le_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    // @44 数据
    t.extend_from_slice(&4u32.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t
}

fn fixture_exif_subifd() -> Vec<u8> {
    let tiff = make_tiff_subifd();
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(b"Exif\0\0");
    body.extend_from_slice(&tiff);
    let len = (body.len() + 2) as u16;
    let mut j: Vec<u8> = Vec::new();
    j.extend_from_slice(&[0xFF, 0xD8]); // SOI
    j.extend_from_slice(&[0xFF, 0xE1]); // APP1
    j.extend_from_slice(&len.to_be_bytes());
    j.extend_from_slice(&body);
    j.extend_from_slice(&[0xFF, 0xD9]); // EOI
    j
}

#[test]
fn differential_exif_subifd() {
    assert_all_equal(&fixture_exif_subifd());
}
```

- [ ] **Step 2: 跑差分测试确认通过**

Run: `cargo test -p omni-meta --test differential differential_exif_subifd`
Expected: PASS —— 四适配器对含 sub-IFD 的输入产出一致的 `Metadata`。

- [ ] **Step 3: 跑全工作区测试与 clippy 做最终回归**

Run: `cargo test --workspace`
Expected: PASS(全部)

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: 无警告

- [ ] **Step 4: Commit**

```bash
git add omni-meta/tests/differential.rs
git commit -m "test: sub-IFD EXIF 四适配器差分一致性"
```

---

## 完成标准

- [ ] EXIF 解码跟随 Exif(0x8769)/GPS(0x8825)/Interop(0xA005) sub-IFD 与 IFD0→IFD1 next 指针。
- [ ] 每条标签带正确 `IfdKind` 来源;指针标签本身不作为数据发出。
- [ ] `Value` 可表达 LONG/RATIONAL/SRATIONAL/UNDEFINED/数组(`List`)。
- [ ] 遍历对环、越界指针、海量 IFD 安全(`visited` + `max_ifds` + `max_tags`),全程不 panic。
- [ ] `normalize` 仅投影 `Primary`,不新增 `Unified` 字段,"≥2 来源"规则不变。
- [ ] 四适配器差分一致;`cargo clippy --workspace --all-targets -- -D warnings` 干净。
