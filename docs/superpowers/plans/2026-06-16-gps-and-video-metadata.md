# GPS 字段投影 + 视频元数据来源 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把地理坐标 `gps` 纳入 Unified（EXIF GPS IFD + XMP 投影 + 视频 udta ©xyz/loci + QuickTime mdta），并借 mdta 解析器补齐视频 `camera_make`/`camera_model`/`created` 来源。

**Architecture:** 沿用既有双路径：图像 GPS 经 codec→`RawTags`→`normalize()` 投影；视频字段经 `formats/bmff.rs` 发 `Event::Field`→`Collector`→`finalize()` 覆盖。坐标以 E7（度×10⁷ i32）+ 毫米存储，保 `Eq`。全程 no_std 安全（不用 `f64::FromStr`/`f64::round`）。

**Tech Stack:** Rust，`#![no_std]` + `alloc`，`#![forbid(unsafe_code)]`，sans-io `MetaParser` 契约。

**基准 spec：** `docs/superpowers/specs/2026-06-16-gps-and-video-metadata-design.md`

---

## 文件结构

| 文件 | 责任 | 改动 |
|---|---|---|
| `omni-meta-core/src/model.rs` | 数据模型 | `Gps` 结构；`Unified.gps`；`Field` 去 `Copy` + 三新变体 |
| `omni-meta-core/src/normalize.rs` | raw→Unified 投影 | E7/十进制 helper；EXIF GPS + XMP GPS 投影；ISO 8601 helper |
| `omni-meta-core/src/driver.rs` | 引擎收尾 | `Collector` 三新字段 + `handle` 分支 + `finalize` 覆盖 |
| `omni-meta-core/src/formats/bmff.rs` | BMFF 解析 | ISO 6709 parser；udta ©xyz/loci；QuickTime mdta keys/ilst；`MoovInfo` 扩展 + 优先级发射 |
| `omni-meta/tests/differential.rs` | 适配器一致性 | 三个 GPS 样本的四适配器差分 |
| `docs/ROADMAP.md` | 活文档 | 标记里程碑完成 |

**约定（贯穿全程）：** 每个任务 TDD（先写失败测试→跑红→最小实现→跑绿→提交）。所有偏移 `checked_*`/`get`，越界→`None`/`Warning`，绝不 panic。提交信息用既有中文风格（如 `feat(model): ...`）。

**构建/测试命令速查：**
- 单测（单个）：`cargo test -p omni-meta-core <test_name> -- --exact --nocapture`
- 模块测试：`cargo test -p omni-meta-core <module>::`
- 全量：`cargo test`
- no_std 构建：`cargo build -p omni-meta-core --no-default-features`

---

## Task 1: `Gps` 结构 + `Unified.gps`

**Files:**
- Modify: `omni-meta-core/src/model.rs`

- [ ] **Step 1: 写失败测试**

在 `model.rs` 的 `#[cfg(test)] mod tests` 内追加：

```rust
#[test]
fn gps_constructs_and_eq() {
    let a = Gps { lat_e7: 275_916_000, lon_e7: 865_640_000, alt_mm: Some(8_850_000) };
    let b = Gps { lat_e7: 275_916_000, lon_e7: 865_640_000, alt_mm: None };
    assert_eq!(a.lat_e7, 275_916_000);
    assert_ne!(a, b);
}

#[test]
fn unified_has_gps_defaulting_none() {
    let u = Unified::default();
    assert_eq!(u.gps, None);
}
```

- [ ] **Step 2: 跑红**

Run: `cargo test -p omni-meta-core gps_constructs_and_eq`
Expected: FAIL —「cannot find type `Gps`」。

- [ ] **Step 3: 最小实现**

在 `model.rs` 的 `DateTimeParts` 定义之后插入：

```rust
/// 地理坐标。E7 = 度 × 10^7（±180e7 < i32 上限；Android/Google Location 行业标准定点）。
/// `alt_mm` 高程毫米（正 = 海平面以上）。全整数 → 保留 Eq，无浮点相等脆弱性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gps {
    pub lat_e7: i32,
    pub lon_e7: i32,
    pub alt_mm: Option<i32>,
}
```

在 `Unified` 结构体末尾（`created` 字段后）加：

```rust
    pub gps: Option<Gps>,
```

- [ ] **Step 4: 跑绿**

Run: `cargo test -p omni-meta-core gps_constructs_and_eq unified_has_gps_defaulting_none`
Expected: PASS（2 passed）。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/model.rs
git commit -m "feat(model): Gps 结构（E7/mm 整数）+ Unified.gps 字段"
```

---

## Task 2: `Field` 去 `Copy` + 新增 `Gps`/`CameraMake`/`CameraModel` 变体

**Files:**
- Modify: `omni-meta-core/src/model.rs`

- [ ] **Step 1: 写失败测试**

在 `model.rs` tests 内追加：

```rust
#[test]
fn field_has_gps_make_model_variants() {
    let g = Field::Gps(Gps { lat_e7: 1, lon_e7: 2, alt_mm: None });
    assert_eq!(g, Field::Gps(Gps { lat_e7: 1, lon_e7: 2, alt_mm: None }));
    assert_eq!(
        Field::CameraMake(String::from("Apple")),
        Field::CameraMake(String::from("Apple"))
    );
    assert_ne!(
        Field::CameraModel(String::from("iPhone 15")),
        Field::CameraModel(String::from("iPhone 14"))
    );
}
```

> 注：`model.rs` tests 顶部已 `use super::*;`；`String` 需确保在测试作用域可见——文件已 `use alloc::string::String;`，测试内可直接用 `String`。若编译报缺失，在测试函数内 `use alloc::string::String;`。

- [ ] **Step 2: 跑红**

Run: `cargo test -p omni-meta-core field_has_gps_make_model_variants`
Expected: FAIL —「no variant named `Gps`」。

- [ ] **Step 3: 最小实现**

把 `Field` 的 derive 行从

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
```

改为（**去掉 `Copy`**）：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
```

并在 `Created(DateTimeParts),` 之后加三个变体：

```rust
    /// 地理坐标（容器原生，如 mp4/mov udta/mdta）。
    Gps(Gps),
    /// 相机厂商（容器原生，如 QuickTime mdta）。
    CameraMake(String),
    /// 相机型号（容器原生，如 QuickTime mdta）。
    CameraModel(String),
```

- [ ] **Step 4: 跑绿（含回归）**

Run: `cargo test -p omni-meta-core`
Expected: PASS。若出现「`Field` 不再是 `Copy`」相关错误（如某处 `let f = *field;`），改为 `.clone()` 或按引用匹配。预期改动点极少（`Event<'a>` 本就只 `Clone`）。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/model.rs
git commit -m "feat(model): Field 去 Copy，新增 Gps/CameraMake/CameraModel 变体"
```

---

## Task 3: E7/米 换算 helper（隔离 f64）

**Files:**
- Modify: `omni-meta-core/src/normalize.rs`

- [ ] **Step 1: 写失败测试**

在 `normalize.rs` 的 `#[cfg(test)] mod tests` 内追加：

```rust
#[test]
fn deg_to_e7_rounds_and_signs() {
    assert_eq!(super::deg_to_e7(27.5916), Some(275_916_000));
    assert_eq!(super::deg_to_e7(-86.5640), Some(-865_640_000));
    assert_eq!(super::deg_to_e7(0.0), Some(0));
    // 越界（>180 度也仍在 i32 E7 范围内，这里测真正越界）
    assert_eq!(super::deg_to_e7(1e30), None);
    assert_eq!(super::deg_to_e7(f64::NAN), None);
}

#[test]
fn meters_to_mm_rounds() {
    assert_eq!(super::meters_to_mm(8850.0), Some(8_850_000));
    assert_eq!(super::meters_to_mm(-10.5), Some(-10_500));
    assert_eq!(super::meters_to_mm(1e30), None);
}
```

- [ ] **Step 2: 跑红**

Run: `cargo test -p omni-meta-core deg_to_e7_rounds_and_signs`
Expected: FAIL —「cannot find function `deg_to_e7`」。

- [ ] **Step 3: 最小实现**

在 `normalize.rs` 顶部 `use` 之后、`normalize` 函数之前插入（**no_std：仅用 f64 算术、`as` 转换、`is_finite`，不用 `round()`/`FromStr`**）：

```rust
/// 度（f64）→ E7（i32）。隔离的 f64 换算：手动 ±0.5 偏置后 `as i32` 取整（no_std 无 round()）。
/// 非有限 / 越 i32 界 → None（不臆造）。
fn deg_to_e7(deg: f64) -> Option<i32> {
    let bias = if deg < 0.0 { -0.5 } else { 0.5 };
    let scaled = deg * 1e7 + bias;
    if scaled.is_finite() && scaled >= i32::MIN as f64 && scaled < i32::MAX as f64 + 1.0 {
        Some(scaled as i32)
    } else {
        None
    }
}

/// 米（f64）→ 毫米（i32），规则同 deg_to_e7。
fn meters_to_mm(m: f64) -> Option<i32> {
    let bias = if m < 0.0 { -0.5 } else { 0.5 };
    let scaled = m * 1000.0 + bias;
    if scaled.is_finite() && scaled >= i32::MIN as f64 && scaled < i32::MAX as f64 + 1.0 {
        Some(scaled as i32)
    } else {
        None
    }
}
```

- [ ] **Step 4: 跑绿**

Run: `cargo test -p omni-meta-core deg_to_e7_rounds_and_signs meters_to_mm_rounds`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/normalize.rs
git commit -m "feat(normalize): 隔离 f64 的 deg→E7 / 米→mm 换算 helper"
```

---

## Task 4: EXIF GPS IFD → `unified.gps`

**Files:**
- Modify: `omni-meta-core/src/normalize.rs`

- [ ] **Step 1: 写失败测试**

在 `normalize.rs` tests 内追加。GPS 标签按 EXIF 模型：Latitude=`Value::List([Rational;3])`、Ref=`Value::Text`、Altitude=`Value::Rational`、AltitudeRef=`Value::Bytes`。

```rust
fn rat(n: u32, d: u32) -> Value { Value::Rational(n, d) }

#[test]
fn gps_from_exif_dms_four_quadrants() {
    // 纬 27°35'29.76"N、经 86°33'50.4"W → 约 27.5916, -86.5640
    let raw = RawTags {
        exif: Vec::from([
            ExifTag { ifd: IfdKind::Gps, tag: 0x0001, value: Value::Text(String::from("N")) },
            ExifTag { ifd: IfdKind::Gps, tag: 0x0002,
                value: Value::List(Vec::from([rat(27, 1), rat(35, 1), rat(2976, 100)])) },
            ExifTag { ifd: IfdKind::Gps, tag: 0x0003, value: Value::Text(String::from("W")) },
            ExifTag { ifd: IfdKind::Gps, tag: 0x0004,
                value: Value::List(Vec::from([rat(86, 1), rat(33, 1), rat(504, 10)])) },
        ]),
        xmp: Vec::new(),
    };
    let mut w = Vec::new();
    let g = normalize(&raw, &mut w).gps.expect("gps");
    assert!((g.lat_e7 - 275_916_000).abs() <= 2, "lat_e7={}", g.lat_e7);
    assert!((g.lon_e7 + 865_640_000).abs() <= 2, "lon_e7={}", g.lon_e7);
    assert_eq!(g.alt_mm, None);
}

#[test]
fn gps_altitude_below_sea_level_is_negative() {
    let raw = RawTags {
        exif: Vec::from([
            ExifTag { ifd: IfdKind::Gps, tag: 0x0001, value: Value::Text(String::from("N")) },
            ExifTag { ifd: IfdKind::Gps, tag: 0x0002,
                value: Value::List(Vec::from([rat(10, 1), rat(0, 1), rat(0, 1)])) },
            ExifTag { ifd: IfdKind::Gps, tag: 0x0003, value: Value::Text(String::from("E")) },
            ExifTag { ifd: IfdKind::Gps, tag: 0x0004,
                value: Value::List(Vec::from([rat(20, 1), rat(0, 1), rat(0, 1)])) },
            ExifTag { ifd: IfdKind::Gps, tag: 0x0005, value: Value::Bytes(Vec::from([1u8])) },
            ExifTag { ifd: IfdKind::Gps, tag: 0x0006, value: rat(105, 10) }, // 10.5 m
        ]),
        xmp: Vec::new(),
    };
    let mut w = Vec::new();
    let g = normalize(&raw, &mut w).gps.expect("gps");
    assert_eq!(g.lat_e7, 100_000_000);
    assert_eq!(g.lon_e7, 200_000_000);
    assert_eq!(g.alt_mm, Some(-10_500));
}

#[test]
fn gps_only_latitude_yields_none_with_warning() {
    let raw = RawTags {
        exif: Vec::from([
            ExifTag { ifd: IfdKind::Gps, tag: 0x0001, value: Value::Text(String::from("N")) },
            ExifTag { ifd: IfdKind::Gps, tag: 0x0002,
                value: Value::List(Vec::from([rat(10, 1), rat(0, 1), rat(0, 1)])) },
        ]),
        xmp: Vec::new(),
    };
    let mut w = Vec::new();
    let u = normalize(&raw, &mut w);
    assert_eq!(u.gps, None);
    assert_eq!(w.iter().filter(|x| x.kind == WarnKind::UnrecognizedValue).count(), 1);
}
```

- [ ] **Step 2: 跑红**

Run: `cargo test -p omni-meta-core gps_from_exif_dms_four_quadrants`
Expected: FAIL（`gps` 为 `None`，断言失败）。

- [ ] **Step 3: 实现**

在 `normalize.rs` 的 helper 区（Task 3 函数附近）加 GPS 投影函数：

```rust
const GPS_LAT_REF: u16 = 0x0001;
const GPS_LAT: u16 = 0x0002;
const GPS_LON_REF: u16 = 0x0003;
const GPS_LON: u16 = 0x0004;
const GPS_ALT_REF: u16 = 0x0005;
const GPS_ALT: u16 = 0x0006;

/// 把 Value（List 多有理数，或单个 Rational）取前 3 个有理数合成度（d + m/60 + s/3600）。
fn dms_value_to_deg(v: &Value) -> Option<f64> {
    let rats: alloc::vec::Vec<(u32, u32)> = match v {
        Value::List(items) => items
            .iter()
            .take(3)
            .filter_map(|x| if let Value::Rational(n, d) = x { Some((*n, *d)) } else { None })
            .collect(),
        Value::Rational(n, d) => alloc::vec::Vec::from([(*n, *d)]),
        _ => return None,
    };
    if rats.is_empty() {
        return None;
    }
    let mut deg = 0.0f64;
    let mut scale = 1.0f64;
    for (n, d) in rats {
        if d == 0 {
            return None;
        }
        deg += (n as f64 / d as f64) / scale;
        scale *= 60.0;
    }
    Some(deg)
}

/// 从 EXIF GPS IFD 投影坐标。lat+lon 都成功才返回 Some；altitude 可选。
fn gps_from_exif(raw: &RawTags) -> Option<Gps> {
    let find = |tag: u16| raw.exif.iter().find(|t| t.ifd == IfdKind::Gps && t.tag == tag);
    let lat_v = find(GPS_LAT)?;
    let lon_v = find(GPS_LON)?;
    let mut lat = dms_value_to_deg(&lat_v.value)?;
    let mut lon = dms_value_to_deg(&lon_v.value)?;
    // Ref 定号：S/W 为负。
    if let Some(t) = find(GPS_LAT_REF)
        && let Value::Text(s) = &t.value
        && s.eq_ignore_ascii_case("S")
    {
        lat = -lat;
    }
    if let Some(t) = find(GPS_LON_REF)
        && let Value::Text(s) = &t.value
        && s.eq_ignore_ascii_case("W")
    {
        lon = -lon;
    }
    let lat_e7 = deg_to_e7(lat)?;
    let lon_e7 = deg_to_e7(lon)?;
    // 高程：Altitude(Rational 米) + 可选 AltitudeRef(1=海平面下)。
    let alt_mm = find(GPS_ALT).and_then(|t| {
        if let Value::Rational(n, d) = &t.value {
            if *d == 0 {
                return None;
            }
            let mut m = *n as f64 / *d as f64;
            if let Some(r) = find(GPS_ALT_REF)
                && let Value::Bytes(b) = &r.value
                && b.first() == Some(&1)
            {
                m = -m;
            }
            meters_to_mm(m)
        } else {
            None
        }
    });
    Some(Gps { lat_e7, lon_e7, alt_mm })
}
```

在 `normalize()` 函数体内、`u` 构造之后、`return u`（或函数末 `u`）之前，插入投影与「坏值」告警逻辑。先确认存在 GPS 标签再判定坏值：

```rust
    // GPS：EXIF GPS IFD 优先。lat+lon 任一存在但整体无法合成 → UnrecognizedValue。
    let has_gps_exif = raw.exif.iter().any(|t| {
        t.ifd == IfdKind::Gps && (t.tag == GPS_LAT || t.tag == GPS_LON)
    });
    if let Some(g) = gps_from_exif(raw) {
        u.gps = Some(g);
    } else if has_gps_exif {
        warnings.push(Warning { offset: 0, kind: WarnKind::UnrecognizedValue });
    }
```

> 注意把这段放在 `normalize` 现有逻辑（camera/created）之后、返回之前。`alloc::vec::Vec` 已在文件顶部 `use alloc::vec::Vec;` —— helper 内用了全路径 `alloc::vec::Vec` 可简化为 `Vec`，按文件现有导入决定（文件已 `use alloc::vec::Vec;`，可直接用 `Vec`）。`Gps` 需加入顶部 `use crate::model::{...}`。

更新 `normalize.rs` 顶部导入，把 `Gps` 加进来：

```rust
use crate::model::{DateTimeParts, Gps, IfdKind, Orientation, RawTags, Unified, Value, WarnKind, Warning};
```

- [ ] **Step 4: 跑绿**

Run: `cargo test -p omni-meta-core gps_from_exif_dms_four_quadrants gps_altitude_below_sea_level_is_negative gps_only_latitude_yields_none_with_warning`
Expected: PASS（3 passed）。

- [ ] **Step 5: 回归 + 提交**

Run: `cargo test -p omni-meta-core normalize::`
Expected: PASS（既有 normalize 测试不破）。

```bash
git add omni-meta-core/src/normalize.rs
git commit -m "feat(normalize): EXIF GPS IFD（d/m/s + Ref + Altitude）投影到 unified.gps"
```

---

## Task 5: XMP GPS 回退投影

**Files:**
- Modify: `omni-meta-core/src/normalize.rs`

- [ ] **Step 1: 写失败测试**

```rust
fn xmp_p(prefix: &str, name: &str, value: &str) -> XmpProperty {
    XmpProperty { prefix: String::from(prefix), name: String::from(name), value: String::from(value) }
}

#[test]
fn gps_from_xmp_decimal_minutes_form() {
    // exif:GPSLatitude "39,57.0900N"、exif:GPSLongitude "116,23.4000E"
    let raw = RawTags {
        exif: Vec::new(),
        xmp: Vec::from([
            xmp_p("exif", "GPSLatitude", "39,57.0900N"),
            xmp_p("exif", "GPSLongitude", "116,23.4000E"),
        ]),
    };
    let mut w = Vec::new();
    let g = normalize(&raw, &mut w).gps.expect("gps");
    assert!((g.lat_e7 - 399_515_000).abs() <= 2, "lat_e7={}", g.lat_e7);
    assert!((g.lon_e7 - 1_163_900_000).abs() <= 2, "lon_e7={}", g.lon_e7);
}

#[test]
fn gps_exif_wins_over_xmp() {
    let raw = RawTags {
        exif: Vec::from([
            ExifTag { ifd: IfdKind::Gps, tag: 0x0001, value: Value::Text(String::from("N")) },
            ExifTag { ifd: IfdKind::Gps, tag: 0x0002,
                value: Value::List(Vec::from([rat(10, 1), rat(0, 1), rat(0, 1)])) },
            ExifTag { ifd: IfdKind::Gps, tag: 0x0003, value: Value::Text(String::from("E")) },
            ExifTag { ifd: IfdKind::Gps, tag: 0x0004,
                value: Value::List(Vec::from([rat(20, 1), rat(0, 1), rat(0, 1)])) },
        ]),
        xmp: Vec::from([
            xmp_p("exif", "GPSLatitude", "39,57.0900N"),
            xmp_p("exif", "GPSLongitude", "116,23.4000E"),
        ]),
    };
    let mut w = Vec::new();
    let g = normalize(&raw, &mut w).gps.expect("gps");
    assert_eq!(g.lat_e7, 100_000_000); // EXIF 的 10°，非 XMP 的 39°
}
```

- [ ] **Step 2: 跑红**

Run: `cargo test -p omni-meta-core gps_from_xmp_decimal_minutes_form`
Expected: FAIL（`gps` 为 None）。

- [ ] **Step 3: 实现**

在 `normalize.rs` helper 区加无符号整数缩放十进制解析与 XMP GPS 解析（**no_std：手写，不用 f64::FromStr**）：

```rust
/// 解析无符号十进制 "D" 或 "D.DDDD" → 值 × 10^scale_pow10（截断多余小数位）。i64 防溢出。
/// 允许可选前导 +/-；格式不符/溢出 → None。
fn parse_scaled_decimal(s: &str, scale_pow10: u32) -> Option<i64> {
    let b = s.as_bytes();
    if b.is_empty() {
        return None;
    }
    let (neg, rest): (bool, &[u8]) = match b[0] {
        b'+' => (false, &b[1..]),
        b'-' => (true, &b[1..]),
        _ => (false, b),
    };
    if rest.is_empty() {
        return None;
    }
    let mut acc: i64 = 0;
    let mut frac: u32 = 0;
    let mut seen_dot = false;
    let mut any = false;
    for &c in rest {
        if c == b'.' {
            if seen_dot {
                return None;
            }
            seen_dot = true;
            continue;
        }
        if !c.is_ascii_digit() {
            return None;
        }
        any = true;
        if seen_dot {
            if frac < scale_pow10 {
                acc = acc.checked_mul(10)?.checked_add((c - b'0') as i64)?;
                frac += 1;
            }
            // 超精度的小数位截断丢弃
        } else {
            acc = acc.checked_mul(10)?.checked_add((c - b'0') as i64)?;
        }
    }
    if !any {
        return None;
    }
    let pad = scale_pow10.checked_sub(frac)?;
    for _ in 0..pad {
        acc = acc.checked_mul(10)?;
    }
    Some(if neg { -acc } else { acc })
}

/// 解析 XMP exif:GPSLatitude/Longitude "DDD,MM.mmm[NSEW]" 或 "DDD,MM,SS[NSEW]" → E7。
fn parse_xmp_coord(s: &str) -> Option<i32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let last = s.as_bytes()[s.len() - 1];
    let neg = matches!(last, b'S' | b'W' | b's' | b'w');
    let core = if last.is_ascii_alphabetic() { &s[..s.len() - 1] } else { s };
    let mut parts = core.split(',');
    let deg: i64 = {
        let d = parts.next()?;
        parse_scaled_decimal(d, 0)?
    };
    let mut e7: i64 = deg.checked_mul(10_000_000)?;
    if let Some(min_str) = parts.next() {
        // 十进制分：min × 1e7 / 60
        let min_e7 = parse_scaled_decimal(min_str, 7)?;
        e7 = e7.checked_add(min_e7 / 60)?;
    }
    if let Some(sec_str) = parts.next() {
        let sec_e7 = parse_scaled_decimal(sec_str, 7)?;
        e7 = e7.checked_add(sec_e7 / 3600)?;
    }
    let e7 = if neg { -e7 } else { e7 };
    i32::try_from(e7).ok()
}

/// XMP 回退坐标：lat+lon 都成功才 Some。altitude 暂不从 XMP 取（来源足够，YAGNI）。
fn gps_from_xmp(raw: &RawTags) -> Option<Gps> {
    let get = |name: &str| {
        raw.xmp
            .iter()
            .find(|p| p.prefix == "exif" && p.name == name)
            .map(|p| p.value.as_str())
    };
    let lat = parse_xmp_coord(get("GPSLatitude")?)?;
    let lon = parse_xmp_coord(get("GPSLongitude")?)?;
    Some(Gps { lat_e7: lat, lon_e7: lon, alt_mm: None })
}
```

把 Task 4 的 GPS 投影块改为「EXIF 优先、XMP 回退」：

```rust
    // GPS：EXIF GPS IFD 优先，XMP exif:GPS* 回退。
    let has_gps_exif = raw.exif.iter().any(|t| {
        t.ifd == IfdKind::Gps && (t.tag == GPS_LAT || t.tag == GPS_LON)
    });
    if let Some(g) = gps_from_exif(raw) {
        u.gps = Some(g);
    } else {
        if has_gps_exif {
            warnings.push(Warning { offset: 0, kind: WarnKind::UnrecognizedValue });
        }
        if let Some(g) = gps_from_xmp(raw) {
            u.gps = Some(g);
        }
    }
```

- [ ] **Step 4: 跑绿**

Run: `cargo test -p omni-meta-core gps_from_xmp_decimal_minutes_form gps_exif_wins_over_xmp`
Expected: PASS。

- [ ] **Step 5: 回归 + 提交**

Run: `cargo test -p omni-meta-core normalize::`
Expected: PASS。

```bash
git add omni-meta-core/src/normalize.rs
git commit -m "feat(normalize): XMP exif:GPS* 回退投影（手写十进制解析，no_std 安全）"
```

---

## Task 6: ISO 8601 解析 helper（mdta creationdate 用）

**Files:**
- Modify: `omni-meta-core/src/normalize.rs`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn iso8601_with_offset_and_z_and_naive() {
    let a = super::parse_iso8601("2017-07-22T16:06:06+10:00").unwrap();
    assert_eq!((a.year, a.month, a.day, a.hour, a.minute, a.second), (2017, 7, 22, 16, 6, 6));
    assert_eq!(a.tz_offset_min, Some(600));
    let z = super::parse_iso8601("2020-01-02T03:04:05Z").unwrap();
    assert_eq!(z.tz_offset_min, Some(0));
    let naive = super::parse_iso8601("2020-01-02T03:04:05").unwrap();
    assert_eq!(naive.tz_offset_min, None);
}

#[test]
fn iso8601_malformed_is_none() {
    for bad in ["", "2020-13-02T03:04:05Z", "2020-01-02 03:04:05", "not-a-date", "2020-01-02T25:00:00Z"] {
        assert_eq!(super::parse_iso8601(bad), None, "input {bad:?}");
    }
}
```

- [ ] **Step 2: 跑红**

Run: `cargo test -p omni-meta-core iso8601_with_offset_and_z_and_naive`
Expected: FAIL —「cannot find function `parse_iso8601`」。

- [ ] **Step 3: 实现**

在 `normalize.rs` helper 区加（复用既有 `parse_exif_offset` 风格；时间部分严格定长）：

```rust
/// 解析 ISO 8601 "YYYY-MM-DDThh:mm:ss[Z|±hh:mm]" → DateTimeParts。
/// 严格定长定分隔；Z→Some(0)，±hh:mm→分钟，无后缀→None；越界→None（不臆造）。
fn parse_iso8601(s: &str) -> Option<DateTimeParts> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T'
        || b[13] != b':' || b[16] != b':'
    {
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
    let hour = num(11..13)?;
    let minute = num(14..16)?;
    let second = num(17..19)?;
    if year == 0 || !(1..=12).contains(&month) || !(1..=31).contains(&day)
        || hour > 23 || minute > 59 || second > 60
    {
        return None;
    }
    // 时区后缀：剩余从索引 19 起。
    let tz = match b.get(19) {
        None => None,
        Some(b'Z') if b.len() == 20 => Some(0i16),
        Some(c @ (b'+' | b'-')) if b.len() == 25 && b[22] == b':' => {
            let two = |i: usize| -> Option<i16> {
                let (h, l) = (b[i], b[i + 1]);
                if !h.is_ascii_digit() || !l.is_ascii_digit() {
                    return None;
                }
                Some(i16::from((h - b'0') * 10 + (l - b'0')))
            };
            let hh = two(20)?;
            let mm = two(23)?;
            if hh > 23 || mm > 59 {
                return None;
            }
            let mag = hh * 60 + mm;
            Some(if *c == b'-' { -mag } else { mag })
        }
        _ => return None,
    };
    Some(DateTimeParts {
        year: year as u16, month: month as u8, day: day as u8,
        hour: hour as u8, minute: minute as u8, second: second as u8,
        tz_offset_min: tz,
    })
}
```

> 该函数当前仅供 bmff（经 `pub(crate)`）。把签名改为 `pub(crate) fn parse_iso8601` 以便 `formats/bmff.rs` 调用，并在 Task 11 通过 `crate::normalize::parse_iso8601` 引用。

- [ ] **Step 4: 跑绿**

Run: `cargo test -p omni-meta-core iso8601_with_offset_and_z_and_naive iso8601_malformed_is_none`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/normalize.rs
git commit -m "feat(normalize): pub(crate) ISO 8601 解析 helper（mdta creationdate 用）"
```

---

## Task 7: `Collector` 增 gps/make/model + finalize 覆盖

**Files:**
- Modify: `omni-meta-core/src/driver.rs`

- [ ] **Step 1: 写失败测试**

在 `driver.rs` tests 内追加（仿既有 `FieldXmpEmitter` 风格的发射器测试）：

```rust
#[test]
fn collector_applies_gps_make_model_fields() {
    use crate::model::Gps;
    struct Emitter;
    impl MetaParser for Emitter {
        fn pull<'a>(&mut self, _input: &'a [u8]) -> PullResult<'a> {
            PullResult {
                demand: Demand::Done,
                consumed: 0,
                events: alloc::vec![
                    Event::Field(Field::Gps(Gps { lat_e7: 1, lon_e7: 2, alt_mm: Some(3) })),
                    Event::Field(Field::CameraMake(alloc::string::String::from("Apple"))),
                    Event::Field(Field::CameraModel(alloc::string::String::from("iPhone 15"))),
                ],
            }
        }
    }
    let buf = [0u8; 4];
    let mut p = Emitter;
    let col = drive_slice(&buf, &mut p, Limits::default());
    let meta = finalize(col, FileFormat::Mov);
    assert_eq!(meta.unified.gps, Some(Gps { lat_e7: 1, lon_e7: 2, alt_mm: Some(3) }));
    assert_eq!(meta.unified.camera_make.as_deref(), Some("Apple"));
    assert_eq!(meta.unified.camera_model.as_deref(), Some("iPhone 15"));
}
```

> 驱动方式照搬同模块既有 `collector_records_fields_and_xmp` 测试：`drive_slice(&buf, &mut p, Limits::default())` → `finalize(col, FileFormat::Mov)`。`drive_slice`/`finalize`/`Limits`/`FileFormat`/`MetaParser`/`Demand`/`Event`/`PullResult` 均在该 tests 模块作用域（必要时 `use crate::demand::PullResult;`）。`Field`/`Gps` 经 `use crate::model::{...}`。

- [ ] **Step 2: 跑红**

Run: `cargo test -p omni-meta-core collector_applies_gps_make_model_fields`
Expected: FAIL —「no field `gps`」/ 模式不匹配。

- [ ] **Step 3: 实现**

`driver.rs` 顶部导入加 `Gps`：

```rust
use crate::model::{DateTimeParts, ExifTag, Field, FileFormat, Gps, Metadata, RawTags, WarnKind, Warning, XmpProperty};
```

> 若现有导入未含 `DateTimeParts`，保持原样仅追加 `Gps`。以现有 `use` 行为准、只增不改其余。

`Collector` 结构体增三字段：

```rust
    gps: Option<Gps>,
    camera_make: Option<alloc::string::String>,
    camera_model: Option<alloc::string::String>,
```

找到 `Collector` 的构造处（`new`/字面量初始化，与 `width: None` 同列）补：

```rust
            gps: None,
            camera_make: None,
            camera_model: None,
```

`handle` 的 `match ev` 内增三分支（首个非空胜出）：

```rust
            Event::Field(Field::Gps(g)) => {
                if self.gps.is_none() {
                    self.gps = Some(g);
                }
            }
            Event::Field(Field::CameraMake(s)) => {
                if self.camera_make.is_none() {
                    self.camera_make = Some(s);
                }
            }
            Event::Field(Field::CameraModel(s)) => {
                if self.camera_model.is_none() {
                    self.camera_model = Some(s);
                }
            }
```

`finalize` 在 normalize 之后、`Metadata { ... }` 之前增覆盖（容器优先，沿用 `created` 约定）：

```rust
    if let Some(g) = col.gps {
        unified.gps = Some(g);
    }
    if let Some(m) = col.camera_make {
        unified.camera_make = Some(m);
    }
    if let Some(m) = col.camera_model {
        unified.camera_model = Some(m);
    }
```

> `finalize` 现有把 `col.width` 等读到局部再用——`col.gps`/`camera_make`/`camera_model` 直接在上面这段消费即可（注意 `col` 的部分移动：把这段放在 `let raw = RawTags { exif: col.exif, xmp: col.xmp };` **之前**，或先 `let gps = col.gps; let make = col.camera_make.take();` 取出。最稳妥：在 `let raw = ...` 之前先 `let (gps, make, model) = (col.gps, col.camera_make, col.camera_model);` 再在 normalize 后用这三个局部变量。）

为避免借用/移动冲突，推荐改法：在 `finalize` 开头与现有 `let (width, height) = (col.width, col.height);` 同处加：

```rust
    let (gps, camera_make, camera_model) = (col.gps, col.camera_make, col.camera_model);
```

然后覆盖段用局部变量：

```rust
    if let Some(g) = gps { unified.gps = Some(g); }
    if let Some(m) = camera_make { unified.camera_make = Some(m); }
    if let Some(m) = camera_model { unified.camera_model = Some(m); }
```

- [ ] **Step 4: 跑绿**

Run: `cargo test -p omni-meta-core collector_applies_gps_make_model_fields`
Expected: PASS。

- [ ] **Step 5: 回归 + 提交**

Run: `cargo test -p omni-meta-core driver::`
Expected: PASS。

```bash
git add omni-meta-core/src/driver.rs
git commit -m "feat(driver): Collector 收集 gps/make/model，finalize 容器优先覆盖"
```

---

## Task 8: ISO 6709 解析（©xyz / mdta location 共用）

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`

- [ ] **Step 1: 写失败测试**

在 `bmff.rs` tests 模块内追加：

```rust
#[test]
fn iso6709_parses_lat_lon_alt() {
    let g = parse_iso6709("+27.5916+086.5640+8850/").expect("gps");
    assert!((g.lat_e7 - 275_916_000).abs() <= 2);
    assert!((g.lon_e7 - 865_640_000).abs() <= 2);
    assert_eq!(g.alt_mm, Some(8_850_000));
}

#[test]
fn iso6709_without_altitude() {
    let g = parse_iso6709("+40.7128-074.0060/").expect("gps");
    assert!((g.lat_e7 - 407_128_000).abs() <= 2);
    assert!((g.lon_e7 + 740_060_000).abs() <= 2);
    assert_eq!(g.alt_mm, None);
}

#[test]
fn iso6709_malformed_is_none() {
    assert_eq!(parse_iso6709("garbage"), None);
    assert_eq!(parse_iso6709("+27.5916"), None); // 缺经度
    assert_eq!(parse_iso6709(""), None);
}
```

- [ ] **Step 2: 跑红**

Run: `cargo test -p omni-meta-core iso6709_parses_lat_lon_alt`
Expected: FAIL —「cannot find function `parse_iso6709`」。

- [ ] **Step 3: 实现**

在 `bmff.rs` 顶部导入加 `Gps`：

```rust
use crate::model::{DateTimeParts, Field, Gps, WarnKind, Warning};
```

加解析函数（**复用 normalize 的整数十进制思路，bmff 内自带一份以免跨模块耦合**；no_std 安全）：

```rust
/// 解析有符号十进制 "±D.DDDD" → 值 × 10^scale（截断超精度位）。i64 防溢出；格式不符→None。
fn scaled_decimal_i64(s: &str, scale_pow10: u32) -> Option<i64> {
    let b = s.as_bytes();
    if b.is_empty() {
        return None;
    }
    let (neg, rest): (bool, &[u8]) = match b[0] {
        b'+' => (false, &b[1..]),
        b'-' => (true, &b[1..]),
        _ => (false, b),
    };
    if rest.is_empty() {
        return None;
    }
    let mut acc: i64 = 0;
    let mut frac: u32 = 0;
    let mut seen_dot = false;
    let mut any = false;
    for &c in rest {
        if c == b'.' {
            if seen_dot {
                return None;
            }
            seen_dot = true;
            continue;
        }
        if !c.is_ascii_digit() {
            return None;
        }
        any = true;
        if seen_dot {
            if frac < scale_pow10 {
                acc = acc.checked_mul(10)?.checked_add((c - b'0') as i64)?;
                frac += 1;
            }
        } else {
            acc = acc.checked_mul(10)?.checked_add((c - b'0') as i64)?;
        }
    }
    if !any {
        return None;
    }
    let pad = scale_pow10.checked_sub(frac)?;
    for _ in 0..pad {
        acc = acc.checked_mul(10)?;
    }
    Some(if neg { -acc } else { acc })
}

/// 解析 ISO 6709 串（©xyz / mdta location.ISO6709）→ Gps。
/// 形如 "+27.5916+086.5640+8850/"：按 +/- 切有符号十进制段 → ①纬 ②经 ③可选高(米)。
fn parse_iso6709(s: &str) -> Option<Gps> {
    let s = s.trim().trim_end_matches('/');
    let bytes = s.as_bytes();
    // 按 +/- 起始切段（保留符号在段首）。
    let mut fields: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
    let mut start: Option<usize> = None;
    for (i, &c) in bytes.iter().enumerate() {
        if c == b'+' || c == b'-' {
            if let Some(st) = start {
                fields.push(&s[st..i]);
            }
            start = Some(i);
        }
    }
    if let Some(st) = start {
        fields.push(&s[st..]);
    }
    if fields.len() < 2 {
        return None;
    }
    let lat = i32::try_from(scaled_decimal_i64(fields[0], 7)?).ok()?;
    let lon = i32::try_from(scaled_decimal_i64(fields[1], 7)?).ok()?;
    let alt_mm = fields
        .get(2)
        .and_then(|f| scaled_decimal_i64(f, 3))
        .and_then(|v| i32::try_from(v).ok());
    Some(Gps { lat_e7: lat, lon_e7: lon, alt_mm })
}
```

- [ ] **Step 4: 跑绿**

Run: `cargo test -p omni-meta-core iso6709_parses_lat_lon_alt iso6709_without_altitude iso6709_malformed_is_none`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat(bmff): ISO 6709 坐标串解析（©xyz/mdta 共用，no_std 整数解析）"
```

---

## Task 9: `udta/©xyz` 解析

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn parse_xyz_atom_yields_gps() {
    // ©xyz payload: u16 size + u16 lang + ISO6709 文本
    let text = b"+27.5916+086.5640/";
    let mut payload = alloc::vec::Vec::new();
    payload.extend_from_slice(&(text.len() as u16).to_be_bytes());
    payload.extend_from_slice(&0x15c7u16.to_be_bytes()); // 任意 lang
    payload.extend_from_slice(text);
    let g = parse_xyz(&payload).expect("gps");
    assert!((g.lat_e7 - 275_916_000).abs() <= 2);
    assert!((g.lon_e7 - 865_640_000).abs() <= 2);
}

#[test]
fn parse_xyz_truncated_is_none() {
    assert_eq!(parse_xyz(&[0u8, 5]), None); // 不足 size+lang
    assert_eq!(parse_xyz(&[]), None);
}
```

- [ ] **Step 2: 跑红**

Run: `cargo test -p omni-meta-core parse_xyz_atom_yields_gps`
Expected: FAIL —「cannot find function `parse_xyz`」。

- [ ] **Step 3: 实现**

```rust
/// 解析 `©xyz` 载荷：u16 size + u16 lang + ISO6709 文本。越界/非 UTF-8 → None。
fn parse_xyz(payload: &[u8]) -> Option<Gps> {
    if payload.len() < 4 {
        return None;
    }
    let size = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let text_bytes = payload.get(4..4 + size).or_else(|| payload.get(4..))?;
    let text = core::str::from_utf8(text_bytes).ok()?;
    parse_iso6709(text)
}
```

> 说明：部分写入方 `size` 字段不可靠，故 `get(4..4+size)` 失败时回退到「4 之后全部」。

- [ ] **Step 4: 跑绿**

Run: `cargo test -p omni-meta-core parse_xyz_atom_yields_gps parse_xyz_truncated_is_none`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat(bmff): moov/udta/©xyz 解析 → Gps"
```

---

## Task 10: `udta/loci` 解析

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn parse_loci_lon_first_16_16() {
    // loci FullBox: ver(1)+flags(3) + lang(2) + name("\0") + role(1)
    //   + lon(16.16) + lat(16.16) + alt(16.16)。注意经在前。
    let mut p = alloc::vec::Vec::new();
    p.extend_from_slice(&[0u8, 0, 0, 0]); // version/flags
    p.extend_from_slice(&0x15c7u16.to_be_bytes()); // language
    p.push(0); // name 空串（null 终止）
    p.push(0); // role
    // 经 86.5640 → 86.5640*65536 ≈ 5_672_300 ; 纬 27.5916 → 27.5916*65536 ≈ 1_808_400
    let lon_fixed = (86.5640f64 * 65536.0) as i32;
    let lat_fixed = (27.5916f64 * 65536.0) as i32;
    let alt_fixed = (8850.0f64 * 65536.0) as i32;
    p.extend_from_slice(&lon_fixed.to_be_bytes());
    p.extend_from_slice(&lat_fixed.to_be_bytes());
    p.extend_from_slice(&alt_fixed.to_be_bytes());
    let g = parse_loci(&p).expect("gps");
    assert!((g.lat_e7 - 275_916_000).abs() <= 20_000, "lat_e7={}", g.lat_e7);
    assert!((g.lon_e7 - 865_640_000).abs() <= 20_000, "lon_e7={}", g.lon_e7);
    assert!((g.alt_mm.unwrap() - 8_850_000).abs() <= 20_000);
}

#[test]
fn parse_loci_truncated_is_none() {
    assert_eq!(parse_loci(&[0u8, 0, 0, 0, 0, 0]), None);
}
```

> 容差放大到 2e4 E7（≈2m）因为 16.16 定点本身精度有限。

- [ ] **Step 2: 跑红**

Run: `cargo test -p omni-meta-core parse_loci_lon_first_16_16`
Expected: FAIL —「cannot find function `parse_loci`」。

- [ ] **Step 3: 实现**

```rust
/// 16.16 有符号定点（i32）→ E7。隔离 f64：raw/65536 度 → E7（±0.5 偏置取整）。
fn fixed16_16_to_e7(raw: i32) -> Option<i32> {
    let deg = raw as f64 / 65536.0;
    let bias = if deg < 0.0 { -0.5 } else { 0.5 };
    let scaled = deg * 1e7 + bias;
    if scaled.is_finite() && scaled >= i32::MIN as f64 && scaled < i32::MAX as f64 + 1.0 {
        Some(scaled as i32)
    } else {
        None
    }
}

/// 16.16 有符号定点（i32，米）→ 毫米（i32）。
fn fixed16_16_to_mm(raw: i32) -> Option<i32> {
    let m = raw as f64 / 65536.0;
    let bias = if m < 0.0 { -0.5 } else { 0.5 };
    let scaled = m * 1000.0 + bias;
    if scaled.is_finite() && scaled >= i32::MIN as f64 && scaled < i32::MAX as f64 + 1.0 {
        Some(scaled as i32)
    } else {
        None
    }
}

/// 解析 `loci`（3GPP FullBox）：ver/flags + lang(2) + name(变长 null 终止) + role(1)
///   + lon(16.16) + lat(16.16) + alt(16.16)。**经在前**。越界 → None。
fn parse_loci(payload: &[u8]) -> Option<Gps> {
    let mut cur = ByteCursor::new(payload);
    cur.seek(4)?; // version+flags
    cur.seek(2)?; // language（packed，跳过）
    // name：从当前位置起的 null 终止串（UTF-8 或 UTF-16-by-BOM）。边界安全跳到 0 之后。
    let pos = cur.position();
    let rest = payload.get(pos..)?;
    let name_len = skip_loci_name(rest)?;
    cur.seek(name_len)?; // 跳过 name（含终止符）
    cur.seek(1)?; // role
    let lon_raw = cur.u32(Endian::Big)? as i32;
    let lat_raw = cur.u32(Endian::Big)? as i32;
    let alt_raw = cur.u32(Endian::Big)? as i32;
    Some(Gps {
        lat_e7: fixed16_16_to_e7(lat_raw)?,
        lon_e7: fixed16_16_to_e7(lon_raw)?,
        alt_mm: fixed16_16_to_mm(alt_raw),
    })
}

/// 计算 loci name 串占用的字节数（含终止符）。UTF-16（BOM 0xFEFF/0xFFFE）按 u16 对齐找 0x0000；
/// 否则按 UTF-8 找单字节 0。找不到终止符 → None（畸形）。
fn skip_loci_name(b: &[u8]) -> Option<usize> {
    if b.len() >= 2 && ((b[0] == 0xFE && b[1] == 0xFF) || (b[0] == 0xFF && b[1] == 0xFE)) {
        // UTF-16：从 BOM 后每 2 字节找 0x0000
        let mut i = 2;
        while i + 1 < b.len() {
            if b[i] == 0 && b[i + 1] == 0 {
                return Some(i + 2);
            }
            i += 2;
        }
        None
    } else {
        b.iter().position(|&c| c == 0).map(|i| i + 1)
    }
}
```

- [ ] **Step 4: 跑绿**

Run: `cargo test -p omni-meta-core parse_loci_lon_first_16_16 parse_loci_truncated_is_none`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat(bmff): moov/udta/loci 解析（经在前·16.16·name 跳过）→ Gps"
```

---

## Task 11: QuickTime `moov/meta` keys/ilst（mdta）解析

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`

- [ ] **Step 1: 写失败测试**

```rust
/// 构造 QuickTime mdta meta：hdlr(mdta) + keys(键表) + ilst(索引→data)。
/// data atom: type(4)+locale(4)+payload。
fn qt_meta_with_keys(keys_and_vals: &[(&str, &[u8])]) -> alloc::vec::Vec<u8> {
    // hdlr：仅需 handler_type 字段在偏移 8..12 为 "mdta"
    let mut hdlr = alloc::vec::Vec::new();
    hdlr.extend_from_slice(&[0u8; 8]); // version/flags + pre_defined
    hdlr.extend_from_slice(b"mdta"); // handler_type
    hdlr.extend_from_slice(&[0u8; 12]); // reserved(3*4)
    hdlr.push(0); // name 空

    // keys
    let mut keys = alloc::vec::Vec::new();
    keys.extend_from_slice(&[0u8; 4]); // version/flags
    keys.extend_from_slice(&(keys_and_vals.len() as u32).to_be_bytes()); // entry_count
    for (k, _) in keys_and_vals {
        let entry_size = 8 + k.len();
        keys.extend_from_slice(&(entry_size as u32).to_be_bytes());
        keys.extend_from_slice(b"mdta"); // namespace
        keys.extend_from_slice(k.as_bytes());
    }

    // ilst：每项 box 类型 = 1-based 索引；内含 data atom
    let mut ilst = alloc::vec::Vec::new();
    for (i, (_, v)) in keys_and_vals.iter().enumerate() {
        let idx = (i as u32) + 1;
        let mut data = alloc::vec::Vec::new();
        data.extend_from_slice(&[0u8; 4]); // type
        data.extend_from_slice(&[0u8; 4]); // locale
        data.extend_from_slice(v);
        let data_box = box_bytes(b"data", &data);
        let mut item = alloc::vec::Vec::new();
        item.extend_from_slice(&data_box);
        // item box，type = 索引的大端字节
        let mut item_box = alloc::vec::Vec::new();
        item_box.extend_from_slice(&((8 + item.len()) as u32).to_be_bytes());
        item_box.extend_from_slice(&idx.to_be_bytes());
        item_box.extend_from_slice(&item);
        ilst.extend_from_slice(&item_box);
    }

    let mut meta = alloc::vec::Vec::new();
    meta.extend_from_slice(&box_bytes(b"hdlr", &hdlr));
    meta.extend_from_slice(&box_bytes(b"keys", &keys));
    meta.extend_from_slice(&box_bytes(b"ilst", &ilst));
    meta
}

#[test]
fn parse_qt_meta_harvests_four_keys() {
    let meta = qt_meta_with_keys(&[
        ("com.apple.quicktime.location.ISO6709", b"+27.5916+086.5640+8850/"),
        ("com.apple.quicktime.make", b"Apple"),
        ("com.apple.quicktime.model", b"iPhone 15"),
        ("com.apple.quicktime.creationdate", b"2017-07-22T16:06:06+10:00"),
    ]);
    let out = parse_qt_mdta(&meta);
    let g = out.gps.expect("gps");
    assert!((g.lat_e7 - 275_916_000).abs() <= 2);
    assert_eq!(out.make.as_deref(), Some("Apple"));
    assert_eq!(out.model.as_deref(), Some("iPhone 15"));
    assert_eq!(out.created.map(|d| d.year), Some(2017));
    assert_eq!(out.created.and_then(|d| d.tz_offset_min), Some(600));
}

#[test]
fn parse_qt_meta_non_mdta_handler_is_empty() {
    // handler 非 mdta → 全空
    let mut hdlr = alloc::vec::Vec::new();
    hdlr.extend_from_slice(&[0u8; 8]);
    hdlr.extend_from_slice(b"vide");
    hdlr.extend_from_slice(&[0u8; 12]);
    let meta = box_bytes(b"hdlr", &hdlr);
    let out = parse_qt_mdta(&meta);
    assert!(out.gps.is_none() && out.make.is_none() && out.created.is_none());
}
```

- [ ] **Step 2: 跑红**

Run: `cargo test -p omni-meta-core parse_qt_meta_harvests_four_keys`
Expected: FAIL —「cannot find function `parse_qt_mdta`」/`QtMdta`。

- [ ] **Step 3: 实现**

```rust
/// QuickTime mdta 抽取产物。
struct QtMdta {
    gps: Option<Gps>,
    make: Option<alloc::string::String>,
    model: Option<alloc::string::String>,
    created: Option<DateTimeParts>,
}

/// 解析 QuickTime `moov/meta`（**非 FullBox** 容器）：hdlr(校验 mdta) + keys + ilst。
/// 返回四键抽取结果；任一缺失/畸形 → 对应字段 None，绝不 panic。
fn parse_qt_mdta(meta_payload: &[u8]) -> QtMdta {
    let mut out = QtMdta { gps: None, make: None, model: None, created: None };
    let mut keys: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
    let mut is_mdta = false;
    let mut ilst_payload: Option<&[u8]> = None;

    for (hdr, p) in iter_child_boxes(meta_payload) {
        match &hdr.kind {
            b"hdlr" => {
                // handler_type 在载荷偏移 8..12（version/flags(4)+pre_defined(4) 之后）。
                if p.get(8..12) == Some(b"mdta") {
                    is_mdta = true;
                }
            }
            b"keys" => keys = parse_qt_keys(p),
            b"ilst" => ilst_payload = Some(p),
            _ => {}
        }
    }
    if !is_mdta {
        return out;
    }
    let Some(ilst) = ilst_payload else { return out };

    for (hdr, item_payload) in iter_child_boxes(ilst) {
        // box 类型 = 1-based key 索引（大端 u32）。
        let idx = u32::from_be_bytes(hdr.kind);
        if idx == 0 || (idx as usize) > keys.len() {
            continue;
        }
        let key = &keys[idx as usize - 1];
        // 取内层 data atom 的 payload（type(4)+locale(4) 之后）。
        let Some(value) = qt_data_value(item_payload) else { continue };
        match key.as_str() {
            "com.apple.quicktime.location.ISO6709" => {
                if out.gps.is_none()
                    && let Ok(s) = core::str::from_utf8(value)
                {
                    out.gps = parse_iso6709(s);
                }
            }
            "com.apple.quicktime.make" => {
                if out.make.is_none()
                    && let Ok(s) = core::str::from_utf8(value)
                {
                    out.make = Some(alloc::string::String::from(s));
                }
            }
            "com.apple.quicktime.model" => {
                if out.model.is_none()
                    && let Ok(s) = core::str::from_utf8(value)
                {
                    out.model = Some(alloc::string::String::from(s));
                }
            }
            "com.apple.quicktime.creationdate" => {
                if out.created.is_none()
                    && let Ok(s) = core::str::from_utf8(value)
                {
                    out.created = crate::normalize::parse_iso8601(s);
                }
            }
            _ => {}
        }
    }
    out
}

/// 解析 `keys` 载荷（FullBox + entry_count + 逐项 size(4)+namespace(4)+key_string）。
fn parse_qt_keys(payload: &[u8]) -> alloc::vec::Vec<alloc::string::String> {
    let mut out = alloc::vec::Vec::new();
    let mut cur = ByteCursor::new(payload);
    if cur.seek(4).is_none() {
        return out;
    }
    let count = match cur.u32(Endian::Big) {
        Some(c) => c,
        None => return out,
    };
    for _ in 0..count {
        let entry_size = match cur.u32(Endian::Big) {
            Some(s) => s as usize,
            None => break,
        };
        if entry_size < 8 {
            break;
        }
        if cur.take(4).is_none() {
            break; // namespace
        }
        let key_len = entry_size - 8;
        let key_bytes = match cur.take(key_len) {
            Some(b) => b,
            None => break,
        };
        match core::str::from_utf8(key_bytes) {
            Ok(s) => out.push(alloc::string::String::from(s)),
            Err(_) => out.push(alloc::string::String::new()), // 占位以保索引对齐
        }
    }
    out
}

/// 从 ilst item 载荷取内层 `data` atom 的值（type(4)+locale(4) 之后的字节）。
fn qt_data_value(item_payload: &[u8]) -> Option<&[u8]> {
    for (hdr, p) in iter_child_boxes(item_payload) {
        if &hdr.kind == b"data" {
            return p.get(8..); // 跳过 type(4)+locale(4)
        }
    }
    None
}
```

> `parse_iso8601` 来自 Task 6 的 `pub(crate) fn`。`box_bytes`/`Endian`/`ByteCursor`/`iter_child_boxes` 均已在 bmff 作用域。

- [ ] **Step 4: 跑绿**

Run: `cargo test -p omni-meta-core parse_qt_meta_harvests_four_keys parse_qt_meta_non_mdta_handler_is_empty`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat(bmff): QuickTime moov/meta keys/ilst（mdta）四键抽取"
```

---

## Task 12: `parse_moov` 下钻 udta/meta + 优先级聚合

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn parse_moov_xyz_beats_loci_for_gps() {
    // udta 同时含 ©xyz 与 loci（不同坐标）；©xyz 应胜出。
    let xyz_text = b"+10.0000+020.0000/";
    let mut xyz_payload = alloc::vec::Vec::new();
    xyz_payload.extend_from_slice(&(xyz_text.len() as u16).to_be_bytes());
    xyz_payload.extend_from_slice(&0u16.to_be_bytes());
    xyz_payload.extend_from_slice(xyz_text);

    let mut loci = alloc::vec::Vec::new();
    loci.extend_from_slice(&[0u8, 0, 0, 0, 0, 0]); // ver/flags+lang
    loci.push(0); // name
    loci.push(0); // role
    loci.extend_from_slice(&((50.0f64 * 65536.0) as i32).to_be_bytes()); // lon
    loci.extend_from_slice(&((60.0f64 * 65536.0) as i32).to_be_bytes()); // lat
    loci.extend_from_slice(&((0.0f64 * 65536.0) as i32).to_be_bytes()); // alt

    let mut udta = alloc::vec::Vec::new();
    udta.extend_from_slice(&box_bytes(b"\xA9xyz", &xyz_payload));
    udta.extend_from_slice(&box_bytes(b"loci", &loci));

    let mut moov_p = alloc::vec::Vec::new();
    moov_p.extend_from_slice(&box_bytes(b"udta", &udta));
    let info = parse_moov(&moov_p, 0);
    let g = info.gps.expect("gps");
    assert_eq!(g.lat_e7, 100_000_000); // ©xyz 的 10°，非 loci 的 60°
}

#[test]
fn parse_moov_mdta_creationdate_beats_mvhd() {
    // mvhd created 1970；mdta creationdate 2017 → created 取 2017。
    let meta = qt_meta_with_keys(&[
        ("com.apple.quicktime.creationdate", b"2017-07-22T16:06:06Z"),
        ("com.apple.quicktime.make", b"Apple"),
    ]);
    let mut moov_p = alloc::vec::Vec::new();
    moov_p.extend_from_slice(&box_bytes(b"mvhd", &mvhd_v0(2_082_844_800, 600, 600)));
    moov_p.extend_from_slice(&box_bytes(b"meta", &meta));
    let info = parse_moov(&moov_p, 0);
    assert_eq!(info.created.map(|d| d.year), Some(2017));
    assert_eq!(info.camera_make.as_deref(), Some("Apple"));
}
```

- [ ] **Step 2: 跑红**

Run: `cargo test -p omni-meta-core parse_moov_xyz_beats_loci_for_gps`
Expected: FAIL —「no field `gps` on MoovInfo」。

- [ ] **Step 3: 实现**

扩展 `MoovInfo` 结构：

```rust
struct MoovInfo {
    dims: Option<(u32, u32)>,
    duration_ms: Option<u64>,
    created: Option<DateTimeParts>,
    gps: Option<Gps>,
    camera_make: Option<alloc::string::String>,
    camera_model: Option<alloc::string::String>,
    warnings: Vec<Warning>,
}
```

`parse_moov` 起始构造同步加新字段为 `None`：

```rust
    let mut info = MoovInfo {
        dims: None, duration_ms: None, created: None,
        gps: None, camera_make: None, camera_model: None,
        warnings: Vec::new(),
    };
```

在 `parse_moov` 的 `match &hdr.kind { ... }` 内，`b"trak"` 分支之后、`_ => {}` 之前，加 `udta` 与 `meta` 处理。先把 mvhd 的 created 存入 `info.created`（既有逻辑不变），再用局部变量收集 udta/mdta 候选，最后定优先级：

```rust
            b"udta" => {
                for (uhdr, up) in iter_child_boxes(p) {
                    match &uhdr.kind {
                        b"\xA9xyz" if xyz_gps.is_none() => xyz_gps = parse_xyz(up),
                        b"loci" if loci_gps.is_none() => loci_gps = parse_loci(up),
                        _ => {}
                    }
                }
            }
            b"meta" => {
                // QuickTime moov/meta 为非-FullBox 容器：直接按子盒解析 mdta。
                let m = parse_qt_mdta(p);
                if mdta.gps.is_none() { mdta.gps = m.gps; }
                if mdta.make.is_none() { mdta.make = m.make; }
                if mdta.model.is_none() { mdta.model = m.model; }
                if mdta.created.is_none() { mdta.created = m.created; }
            }
```

> 为此需在 `parse_moov` 顶部（`info` 之后）声明聚合用局部变量：

```rust
    let mut xyz_gps: Option<Gps> = None;
    let mut loci_gps: Option<Gps> = None;
    let mut mdta = QtMdta { gps: None, make: None, model: None, created: None };
```

在 `parse_moov` 返回 `info` **之前**，定优先级写回 `info`：

```rust
    // GPS 优先级：©xyz > mdta > loci。
    info.gps = xyz_gps.or(mdta.gps).or(loci_gps);
    // created：mdta creationdate 优先于 mvhd（mdta 带真实时区）。
    if let Some(c) = mdta.created {
        info.created = Some(c);
    }
    // make/model：mdta 唯一视频来源。
    info.camera_make = mdta.make;
    info.camera_model = mdta.model;
```

> 注意：`meta` 的 box 名在 QuickTime 顶层 HEIF 抽取路径（`pull_walk`）里是独立处理的；这里改的是 **`parse_moov` 内部**（moov 子盒），与 A2 的文件顶层 `meta`（HEIF）互不影响——HEIF 的 `meta` 在 moov 之外、走 `BmffParser` 的 Walk/Extract，不经 `parse_moov`。

- [ ] **Step 4: 跑绿**

Run: `cargo test -p omni-meta-core parse_moov_xyz_beats_loci_for_gps parse_moov_mdta_creationdate_beats_mvhd`
Expected: PASS。

- [ ] **Step 5: 回归 + 提交**

Run: `cargo test -p omni-meta-core formats::bmff::`
Expected: PASS（既有 moov 维度/时长/created 测试不破）。

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat(bmff): parse_moov 下钻 udta/meta，GPS ©xyz>mdta>loci、created mdta>mvhd、make/model"
```

---

## Task 13: `pull_walk` 发射新 `Field` 事件

**Files:**
- Modify: `omni-meta-core/src/formats/bmff.rs`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn end_to_end_mov_mdta_gps_make() {
    // 文件：ftyp(qt) + moov{ mvhd, udta{©xyz}, meta{mdta make/model} }
    let xyz_text = b"+35.0000+139.0000/";
    let mut xyz_payload = alloc::vec::Vec::new();
    xyz_payload.extend_from_slice(&(xyz_text.len() as u16).to_be_bytes());
    xyz_payload.extend_from_slice(&0u16.to_be_bytes());
    xyz_payload.extend_from_slice(xyz_text);
    let mut udta = alloc::vec::Vec::new();
    udta.extend_from_slice(&box_bytes(b"\xA9xyz", &xyz_payload));

    let meta = qt_meta_with_keys(&[
        ("com.apple.quicktime.make", b"Apple"),
        ("com.apple.quicktime.model", b"iPhone 15"),
    ]);

    let mut moov_p = alloc::vec::Vec::new();
    moov_p.extend_from_slice(&box_bytes(b"mvhd", &mvhd_v0(2_082_844_800, 600, 600)));
    moov_p.extend_from_slice(&box_bytes(b"udta", &udta));
    moov_p.extend_from_slice(&box_bytes(b"meta", &meta));

    let mut f = ftyp_mp4(); // 既有 helper；parse_moov 不依赖 brand，format 由 finalize 显式传入
    f.extend_from_slice(&box_bytes(b"moov", &moov_p));

    // 驱动入口照搬既有 end_to_end_mp4_moov：
    let col = crate::driver::drive_slice(&f, &mut BmffParser::new(), crate::limits::Limits::default());
    let meta_out = crate::driver::finalize(col, crate::model::FileFormat::Mov);
    assert_eq!(meta_out.unified.gps.map(|g| g.lat_e7), Some(350_000_000));
    assert_eq!(meta_out.unified.camera_make.as_deref(), Some("Apple"));
    assert_eq!(meta_out.unified.camera_model.as_deref(), Some("iPhone 15"));
}
```

- [ ] **Step 2: 跑红**

Run: `cargo test -p omni-meta-core end_to_end_mov_mdta_gps_make`
Expected: FAIL（`gps`/`camera_make` 为 None —— 事件未发射）。

- [ ] **Step 3: 实现**

在 `pull_walk` 的 moov 分支（`bmff.rs:645` 附近，`parse_moov` 之后组装 `events` 处），于既有 `Created`/`Duration` 发射之后追加三个新 `Field`：

```rust
            if let Some(g) = info.gps {
                events.push(Event::Field(Field::Gps(g)));
            }
            if let Some(m) = info.camera_make {
                events.push(Event::Field(Field::CameraMake(m)));
            }
            if let Some(m) = info.camera_model {
                events.push(Event::Field(Field::CameraModel(m)));
            }
```

> 这些在 `for warn in info.warnings { ... }` 之前或之后均可；保持与 `Width`/`Created` 同段。注意 `info` 字段为 `String`（非 Copy），`if let Some(m) = info.camera_make` 会移动，须在 `info.warnings` 被 `for warn in info.warnings`（消费 `info.warnings`）之前完成所有 `info.*` 读取——既有代码先发 Field 再 `for warn in info.warnings`，顺序天然满足。

- [ ] **Step 4: 跑绿**

Run: `cargo test -p omni-meta-core end_to_end_mov_mdta_gps_make`
Expected: PASS。

- [ ] **Step 5: 回归 + 提交**

Run: `cargo test -p omni-meta-core`
Expected: PASS（全 core 单测）。

```bash
git add omni-meta-core/src/formats/bmff.rs
git commit -m "feat(bmff): pull_walk 发射 Gps/CameraMake/CameraModel Field 事件"
```

---

## Task 14: 四适配器差分一致性

**Files:**
- Modify: `omni-meta/tests/differential.rs`

- [ ] **Step 1: 读现有差分测试结构**

打开 `omni-meta/tests/differential.rs`。既有一致性 helper 是 **`fn assert_all_equal(bytes: &[u8])`**（内部跑 `read_slice`/`read_blocking`/`read_seek` + 多 chunk `push_drive` 并逐字段相等断言）。文件内已有 BMFF 字节构造 helper：`box_bytes(kind, payload)`、`mvhd_v0(creation, timescale, duration)`、`ftyp`/`tkhd` 等（约 421–724 行），以及 `make_tiff()`（第 6 行）。复用它们。

- [ ] **Step 2: 写失败测试**

新增（用真实 helper `assert_all_equal` + `read_slice` 取值断言）：

```rust
#[test]
fn gps_mov_mdta_consistent_across_adapters() {
    let bytes = build_mov_with_gps_and_mdta(); // 本文件内新增构造函数（见 Step 4）
    // 取值断言（任一适配器即可，slice 为基准）
    let m = read_slice(&bytes, Options::default()).unwrap();
    assert_eq!(m.unified.gps.map(|g| g.lat_e7), Some(350_000_000));
    assert_eq!(m.unified.camera_make.as_deref(), Some("Apple"));
    // 四适配器一致
    assert_all_equal(&bytes);
}

#[test]
fn gps_jpeg_exif_consistent_across_adapters() {
    let bytes = build_jpeg_with_gps_ifd(); // APP1/Exif，GPS IFD 含 lat/lon
    let m = read_slice(&bytes, Options::default()).unwrap();
    assert!(m.unified.gps.is_some(), "gps 应被投影");
    assert_all_equal(&bytes);
}
```

- [ ] **Step 3: 跑红**

Run: `cargo test -p omni-meta gps_mov_mdta_consistent_across_adapters`
Expected: FAIL（构造函数未定义）。

- [ ] **Step 4: 补齐构造函数使测试通过**

在 `differential.rs` 内实现两个构造函数：

- `build_mov_with_gps_and_mdta()`：照搬 Task 13 的字节拼装（`ftyp` + `moov{ mvhd, udta{©xyz}, meta{mdta make/model} }`），复用本文件既有 `box_bytes`/`mvhd_v0`；mdta meta 拼装照搬 Task 11 的 `qt_meta_with_keys`（把该 helper 复制进 differential.rs）。`©xyz` 文本用 `"+35.0000+139.0000/"`。
- `build_jpeg_with_gps_ifd()`：构造内嵌 GPS IFD 的 TIFF（IFD0 含 `0x8825` 指向 GPS IFD；GPS IFD 含 `0x0001`=`"N"`/`0x0002`=lat 3×RATIONAL/`0x0003`=`"E"`/`0x0004`=lon 3×RATIONAL），参照 `omni-meta-core/src/codecs/exif.rs::follows_gps_subifd_with_rational_list`；外层用本文件既有 JPEG APP1 构造 helper（搜索 `make_tiff`/APP1 包裹的既有 fixture，复用其 TIFF→JPEG 包裹方式）。

- [ ] **Step 5: 跑绿**

Run: `cargo test -p omni-meta gps_mov_mdta_consistent_across_adapters gps_jpeg_exif_consistent_across_adapters`
Expected: PASS。

- [ ] **Step 6: 提交**

```bash
git add omni-meta/tests/differential.rs
git commit -m "test(differential): GPS（.MOV mdta/©xyz + JPEG GPS IFD）四适配器一致性"
```

---

## Task 15: no_std 构建验证 + ROADMAP 更新

**Files:**
- Modify: `docs/ROADMAP.md`

- [ ] **Step 1: no_std 构建**

Run: `cargo build -p omni-meta-core --no-default-features`
Expected: 构建成功，无 `f64::round`/`FromStr` 相关错误。若失败，定位到使用了 std-only 浮点方法处并改为整数/算术实现。

- [ ] **Step 2: 全量测试**

Run: `cargo test`
Expected: 全绿。

- [ ] **Step 3: clippy（与既有里程碑一致的清洁度）**

Run: `cargo clippy -p omni-meta-core --all-targets -- -D warnings`
Expected: 无告警（如有 `neg_multiply`/分组等小问题，按既有风格修正）。

- [ ] **Step 4: 更新 ROADMAP**

在 `docs/ROADMAP.md` 的「§4 横切待办」里把 `gps` 项标记完成，并在「当前 Unified 字段」段补一行说明 gps 达成与视频 make/model/created 新来源。示例编辑：

- §4 第一条改为：`gps`（EXIF GPS IFD + XMP + 视频 ©xyz/loci/mdta，**已达 ≥3 来源并投影**）✅
- 「当前 Unified 字段」追加 `gps`，并注明 `camera_make`/`camera_model` 增 QuickTime mdta（第 3 来源、首覆盖视频）、`created` 增 mdta（第 4 来源）。

- [ ] **Step 5: 提交**

```bash
git add docs/ROADMAP.md
git commit -m "docs(roadmap): GPS 字段投影 + 视频 mdta 来源里程碑完成"
```

---

## Self-Review 记录（写计划时自检）

**Spec 覆盖：**
- §3 模型（Gps/Unified.gps/Field 变体）→ Task 1、2 ✅
- §5 图像投影（EXIF GPS + XMP 回退 + 坏值告警）→ Task 3、4、5 ✅
- §6 视频三布局（©xyz/loci/mdta）+ 优先级 → Task 8、9、10、11、12 ✅
- §7 ISO 8601 → Task 6 ✅
- §8 引擎 Collector/finalize → Task 7、13 ✅
- §9 测试（normalize/bmff/差分/no_std）→ 各 Task 内置 + Task 14、15 ✅
- §11 不变量（forbid unsafe / checked / 缺失即 None / 差分）→ 贯穿 + Task 14、15 ✅

**类型一致性：** `Gps{lat_e7,lon_e7,alt_mm}`、`Field::{Gps,CameraMake,CameraModel}`、`MoovInfo{gps,camera_make,camera_model}`、`QtMdta{gps,make,model,created}`、`parse_iso6709`/`parse_xyz`/`parse_loci`/`parse_qt_mdta`/`parse_qt_keys`/`qt_data_value`/`parse_iso8601`（`pub(crate)`）—— 全计划统一。

**Placeholder：** 无 TODO/TBD；每个代码步给出完整可编译代码。差分测试（Task 14）依赖既有 helper 名，已用「核对既有名并替换」的明确指示替代硬编码假名——这是对既有测试基座的必要适配，非占位。
