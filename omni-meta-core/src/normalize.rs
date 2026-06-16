//! 把原始标签投影成统一规范字段。映射规则集中在此，便于测试。

use alloc::vec::Vec;

use crate::model::{DateTimeParts, IfdKind, Orientation, RawTags, Unified, Value, WarnKind, Warning};

const TAG_MAKE: u16 = 0x010F;
const TAG_MODEL: u16 = 0x0110;
const TAG_ORIENTATION: u16 = 0x0112;
const TAG_DATETIME: u16 = 0x0132; // IFD0
const TAG_DATETIME_ORIGINAL: u16 = 0x9003; // Exif IFD
const TAG_OFFSET_TIME: u16 = 0x9010; // 对应 0x0132
const TAG_OFFSET_TIME_ORIGINAL: u16 = 0x9011; // 对应 0x9003

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

/// 把原始标签投影到统一模型。
///
/// 遇到“存在但取值超出规范范围”的标签（如 orientation 不在 1..=8）时，
/// 丢弃该值并向 `warnings` 追加一条 `WarnKind::UnrecognizedValue`，使调用者能
/// 区分“缺失”与“存在但无法识别”。normalize 作用于已解码标签、无字节偏移，
/// 故此类警告的 `offset` 固定为 0。
pub fn normalize(raw: &RawTags, warnings: &mut Vec<Warning>) -> Unified {
    let mut u = Unified::default();
    for t in &raw.exif {
        if t.ifd != IfdKind::Primary {
            continue;
        }
        match (t.tag, &t.value) {
            (TAG_MAKE, Value::Text(s)) => u.camera_make = Some(s.clone()),
            (TAG_MODEL, Value::Text(s)) => u.camera_model = Some(s.clone()),
            (TAG_ORIENTATION, Value::U16(v)) => match Orientation::from_u16(*v) {
                Some(o) => u.orientation = Some(o),
                None => warnings.push(Warning {
                    offset: 0,
                    kind: WarnKind::UnrecognizedValue,
                }),
            },
            _ => {}
        }
    }
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
                if let Ok(v) = p.value.parse::<u16>()
                    && let Some(o) = Orientation::from_u16(v)
                {
                    u.orientation = Some(o);
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
    // created：DateTimeOriginal(Exif IFD 0x9003) 优先，回退 DateTime(IFD0 0x0132)。
    // 时区：默认 None；对应 OffsetTime* 标签存在则解析 "±HH:MM"。
    let find = |ifd: IfdKind, tag: u16| -> Option<&str> {
        raw.exif.iter().find_map(|t| {
            if t.ifd == ifd && t.tag == tag
                && let Value::Text(s) = &t.value
            {
                return Some(s.as_str());
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
    if let Some(s) = dt_str
        && let Some(mut dt) = parse_exif_datetime(s)
    {
        dt.tz_offset_min = off_str.and_then(parse_exif_offset);
        u.created = Some(dt);
    }
    u
}

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
    if year == 0 || !(1..=12).contains(&month) || !(1..=31).contains(&day)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ExifTag, IfdKind, XmpProperty};
    use alloc::string::String;
    use alloc::vec::Vec;

    #[test]
    fn projects_exif_tags_to_unified() {
        let raw = RawTags {
            exif: Vec::from([
                ExifTag { ifd: IfdKind::Primary, tag: 0x010F, value: Value::Text(String::from("Acme")) },
                ExifTag { ifd: IfdKind::Primary, tag: 0x0110, value: Value::Text(String::from("X100")) },
                ExifTag { ifd: IfdKind::Primary, tag: 0x0112, value: Value::U16(6) },
            ]),
            xmp: Vec::new(),
        };
        let mut warnings = Vec::new();
        let u = normalize(&raw, &mut warnings);
        assert_eq!(u.camera_make.as_deref(), Some("Acme"));
        assert_eq!(u.camera_model.as_deref(), Some("X100"));
        assert_eq!(u.orientation, Some(Orientation::Rotate90));
        assert!(warnings.is_empty(), "warnings: {:?}", warnings);
    }

    #[test]
    fn unknown_orientation_value_is_dropped_with_warning() {
        let raw = RawTags {
            exif: Vec::from([ExifTag { ifd: IfdKind::Primary, tag: 0x0112, value: Value::U16(99) }]),
            xmp: Vec::new(),
        };
        let mut warnings = Vec::new();
        let u = normalize(&raw, &mut warnings);
        assert_eq!(u.orientation, None);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, WarnKind::UnrecognizedValue);
    }

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
                xmp("tiff", "Model", "XmpModel"),
                xmp("tiff", "Orientation", "6"),
                xmp("tiff", "ImageWidth", "1280"),
                xmp("tiff", "ImageLength", "720"),
            ]),
        };
        let mut warnings = Vec::new();
        let u = normalize(&raw, &mut warnings);
        assert_eq!(u.camera_make.as_deref(), Some("XmpMake"));
        assert_eq!(u.camera_model.as_deref(), Some("XmpModel"));
        assert_eq!(u.orientation, Some(Orientation::Rotate90));
        assert_eq!(u.width, Some(1280));
        assert_eq!(u.height, Some(720));
    }

    #[test]
    fn exif_wins_over_xmp() {
        let raw = RawTags {
            exif: Vec::from([ExifTag {
                ifd: IfdKind::Primary,
                tag: 0x010F,
                value: Value::Text(String::from("ExifMake")),
            }]),
            xmp: Vec::from([xmp("tiff", "Make", "XmpMake")]),
        };
        let mut warnings = Vec::new();
        let u = normalize(&raw, &mut warnings);
        assert_eq!(u.camera_make.as_deref(), Some("ExifMake"));
    }

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
        assert_eq!(c.tz_offset_min, None);
    }

    #[test]
    fn created_from_datetime_original_with_offset() {
        let raw = RawTags {
            exif: Vec::from([
                exif_tag(IfdKind::Exif, 0x9003, "2003:01:24 09:20:00"),
                exif_tag(IfdKind::Exif, 0x9011, "+09:00"),
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
                exif_tag(IfdKind::Exif, 0x9010, "-05:00"),
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
        assert_eq!(u.created.unwrap().year, 2003);
    }

    #[test]
    fn created_malformed_is_none() {
        for bad in ["not-a-date", "2003-01-24 09:20:00", "2003:13:40 25:99:99", "", "0000:01:01 00:00:00"] {
            let raw = RawTags {
                exif: Vec::from([exif_tag(IfdKind::Exif, 0x9003, bad)]),
                xmp: Vec::new(),
            };
            let mut w = Vec::new();
            let u = normalize(&raw, &mut w);
            assert_eq!(u.created, None, "input {bad:?} 应判为无效");
        }
    }

    #[test]
    fn deg_to_e7_rounds_and_signs() {
        assert_eq!(super::deg_to_e7(27.5916), Some(275_916_000));
        assert_eq!(super::deg_to_e7(-86.5640), Some(-865_640_000));
        assert_eq!(super::deg_to_e7(0.0), Some(0));
        assert_eq!(super::deg_to_e7(1e30), None);
        assert_eq!(super::deg_to_e7(f64::NAN), None);
    }

    #[test]
    fn meters_to_mm_rounds() {
        assert_eq!(super::meters_to_mm(8850.0), Some(8_850_000));
        assert_eq!(super::meters_to_mm(-10.5), Some(-10_500));
        assert_eq!(super::meters_to_mm(1e30), None);
    }
}
