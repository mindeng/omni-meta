//! 把原始标签投影成统一规范字段。映射规则集中在此，便于测试。

use alloc::vec::Vec;

use crate::model::{ContainerSource, DateTimeParts, Gps, IfdKind, Orientation, RawTags, Unified, Value, WarnKind, Warning};

const TAG_MAKE: u16 = 0x010F;
const TAG_MODEL: u16 = 0x0110;
const TAG_ORIENTATION: u16 = 0x0112;
const TAG_DATETIME: u16 = 0x0132; // IFD0
const TAG_DATETIME_ORIGINAL: u16 = 0x9003; // Exif IFD
const TAG_OFFSET_TIME: u16 = 0x9010; // 对应 0x0132
const TAG_OFFSET_TIME_ORIGINAL: u16 = 0x9011; // 对应 0x9003
const TAG_SOFTWARE: u16 = 0x0131;
const TAG_ARTIST: u16 = 0x013B;

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

const GPS_LAT_REF: u16 = 0x0001;
const GPS_LAT: u16 = 0x0002;
const GPS_LON_REF: u16 = 0x0003;
const GPS_LON: u16 = 0x0004;
const GPS_ALT_REF: u16 = 0x0005;
const GPS_ALT: u16 = 0x0006;

/// 把 Value（List 多有理数，或单个 Rational）取前 3 个有理数合成度（d + m/60 + s/3600）。
fn dms_value_to_deg(v: &Value) -> Option<f64> {
    let mut deg = 0.0f64;
    let mut scale = 1.0f64;
    let mut any = false;
    let mut acc = |n: u32, d: u32| -> Option<()> {
        if d == 0 { return None; }
        deg += (n as f64 / d as f64) / scale;
        scale *= 60.0;
        any = true;
        Some(())
    };
    match v {
        Value::List(items) => {
            for x in items.iter().take(3) {
                if let Value::Rational(n, d) = x {
                    acc(*n, *d)?;
                }
            }
        }
        Value::Rational(n, d) => {
            acc(*n, *d)?;
        }
        _ => return None,
    }
    if any { Some(deg) } else { None }
}

/// 解析无符号十进制 "D" 或 "D.DDDD" → 值 × 10^scale_pow10（截断多余小数位）。i64 防溢出。
/// 允许可选前导 +/-；格式不符/溢出 → None。（no_std：不用 f64::FromStr。）
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

/// 解析 XMP exif:GPSLatitude/Longitude 坐标字符串 → E7。
/// 支持三种形式（末尾方位字母 N/S/E/W 可选）：
/// * `"DDD.ddd[NSEW]"` — 裸十进制度数（无逗号）
/// * `"DDD,MM.mmm[NSEW]"` — 度分十进制形式
/// * `"DDD,MM,SS[NSEW]"` — 度分秒形式
fn parse_xmp_coord(s: &str) -> Option<i32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let last = s.as_bytes()[s.len() - 1];
    let neg = matches!(last, b'S' | b'W' | b's' | b'w');
    let core = if last.is_ascii_alphabetic() { &s[..s.len() - 1] } else { s };
    let has_comma = core.as_bytes().contains(&b',');
    let mut parts = core.split(',');
    let first = parts.next()?;
    let mut e7: i64 = if has_comma {
        parse_scaled_decimal(first, 0)?.checked_mul(10_000_000)?
    } else {
        // 裸十进制度数 "DDD.ddd"：整体按 E7 解析（避免丢弃小数 → 臆造错误坐标）。
        parse_scaled_decimal(first, 7)?
    };
    if let Some(min_str) = parts.next() {
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

/// 从 EXIF GPS IFD 投影坐标。lat+lon 都成功才返回 Some；altitude 可选。
fn gps_from_exif(raw: &RawTags) -> Option<Gps> {
    let find = |tag: u16| raw.exif.iter().find(|t| t.ifd == IfdKind::Gps && t.tag == tag);
    let lat_v = find(GPS_LAT)?;
    let lon_v = find(GPS_LON)?;
    let mut lat = dms_value_to_deg(&lat_v.value)?;
    let mut lon = dms_value_to_deg(&lon_v.value)?;
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

/// 取指定来源/键的容器文本标签值。
fn container_text<'a>(raw: &'a RawTags, source: ContainerSource, key: &str) -> Option<&'a str> {
    raw.container.iter().find_map(|t| {
        if t.source == source && t.key == key
            && let Value::Text(s) = &t.value
        {
            return Some(s.as_str());
        }
        None
    })
}

/// 取 Primary IFD 指定 tag 的文本值。
fn exif_primary_text(raw: &RawTags, tag: u16) -> Option<alloc::string::String> {
    raw.exif.iter().find_map(|t| {
        if t.ifd == IfdKind::Primary && t.tag == tag
            && let Value::Text(s) = &t.value
        {
            return Some(s.clone());
        }
        None
    })
}

/// 取指定 prefix/name 的 XMP 属性值。
fn xmp_text(raw: &RawTags, prefix: &str, name: &str) -> Option<alloc::string::String> {
    raw.xmp.iter().find_map(|p| {
        if p.prefix == prefix && p.name == name {
            Some(p.value.clone())
        } else {
            None
        }
    })
}

/// 把原始标签投影到统一模型。
///
/// 遇到”存在但取值超出规范范围”的标签（如 orientation 不在 1..=8）时，
/// 丢弃该值并向 `warnings` 追加一条 `WarnKind::UnrecognizedValue`，使调用者能
/// 区分”缺失”与”存在但无法识别”。normalize 作用于已解码标签、无字节偏移，
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
    // GPS：EXIF GPS IFD 优先。lat/lon 任一存在但整体无法合成 → UnrecognizedValue。
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
    // software：容器 > EXIF(0x0131) > XMP(xmp:CreatorTool)
    u.software = container_text(raw, ContainerSource::QuickTimeMdta, "com.apple.quicktime.software")
        .or_else(|| container_text(raw, ContainerSource::Udta, "©swr"))
        .map(alloc::string::String::from)
        .or_else(|| exif_primary_text(raw, TAG_SOFTWARE))
        .or_else(|| xmp_text(raw, "xmp", "CreatorTool"));
    // creator：容器 > EXIF(0x013B Artist) > XMP(dc:creator)
    u.creator = container_text(raw, ContainerSource::QuickTimeMdta, "com.apple.quicktime.author")
        .or_else(|| container_text(raw, ContainerSource::Udta, "©aut"))
        .map(alloc::string::String::from)
        .or_else(|| exif_primary_text(raw, TAG_ARTIST))
        .or_else(|| xmp_text(raw, "dc", "creator"));
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

/// 解析 ISO 8601 "YYYY-MM-DDThh:mm:ss[Z|±hh:mm]" → DateTimeParts。
/// 严格定长定分隔；Z→Some(0)，±hh:mm→分钟，无后缀→None；越界→None（不臆造）。
pub(crate) fn parse_iso8601(s: &str) -> Option<DateTimeParts> {
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
    let two = |i: usize| -> Option<i16> {
        let (h, l) = (b[i], b[i + 1]);
        if !h.is_ascii_digit() || !l.is_ascii_digit() {
            return None;
        }
        Some(i16::from((h - b'0') * 10 + (l - b'0')))
    };
    let tz = match b.get(19) {
        None => None,
        Some(b'Z') if b.len() == 20 => Some(0i16),
        Some(c @ (b'+' | b'-')) if b.len() == 25 && b[22] == b':' => {
            let hh = two(20)?;
            let mm = two(23)?;
            if hh > 23 || mm > 59 {
                return None;
            }
            let mag = hh * 60 + mm;
            Some(if *c == b'-' { -mag } else { mag })
        }
        Some(c @ (b'+' | b'-')) if b.len() == 24 => {
            let hh = two(20)?;
            let mm = two(22)?;
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
    use crate::model::{ExifTag, IfdKind, Value, WarnKind, XmpProperty};
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
            container: Vec::new(),
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
            container: Vec::new(),
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
            container: Vec::new(),
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
            container: Vec::new(),
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
            container: Vec::new(),
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
            container: Vec::new(),
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
            container: Vec::new(),
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
            container: Vec::new(),
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
            container: Vec::new(),
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
                container: Vec::new(),
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
            container: Vec::new(),
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
            container: Vec::new(),
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
            container: Vec::new(),
        };
        let mut w = Vec::new();
        let u = normalize(&raw, &mut w);
        assert_eq!(u.gps, None);
        assert_eq!(w.iter().filter(|x| x.kind == WarnKind::UnrecognizedValue).count(), 1);
    }

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
            container: Vec::new(),
        };
        let mut w = Vec::new();
        let g = normalize(&raw, &mut w).gps.expect("gps");
        assert!((g.lat_e7 - 399_515_000).abs() <= 2, "lat_e7={}", g.lat_e7);
        assert!((g.lon_e7 - 1_163_900_000).abs() <= 2, "lon_e7={}", g.lon_e7);
    }

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
        for bad in ["", "2020-13-02T03:04:05Z", "2020-01-02 03:04:05", "not-a-date", "2020-01-02T25:00:00Z", "2020-01-02T03:04:05Z ", "2020-01-02T03:04:05+10"] {
            assert_eq!(super::parse_iso8601(bad), None, "input {bad:?}");
        }
    }

    #[test]
    fn iso8601_offset_without_colon() {
        // Apple iPhone .MOV creationdate form: 无冒号偏移
        let a = super::parse_iso8601("2017-07-22T16:06:06+1000").unwrap();
        assert_eq!((a.year, a.month, a.day, a.hour, a.minute, a.second), (2017, 7, 22, 16, 6, 6));
        assert_eq!(a.tz_offset_min, Some(600));
        let neg = super::parse_iso8601("2020-01-02T03:04:05-0530").unwrap();
        assert_eq!(neg.tz_offset_min, Some(-330));
        // 仍拒绝畸形：长度不符
        assert_eq!(super::parse_iso8601("2020-01-02T03:04:05+10:0"), None);
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
            container: Vec::new(),
        };
        let mut w = Vec::new();
        let g = normalize(&raw, &mut w).gps.expect("gps");
        assert_eq!(g.lat_e7, 100_000_000); // EXIF 的 10°，非 XMP 的 39°
    }

    #[test]
    fn gps_from_xmp_decimal_degrees_form() {
        // 裸十进制度数（无逗号）"39.9515N" / "116.3900E"
        let raw = RawTags {
            exif: Vec::new(),
            xmp: Vec::from([
                xmp_p("exif", "GPSLatitude", "39.9515N"),
                xmp_p("exif", "GPSLongitude", "116.3900E"),
            ]),
            container: Vec::new(),
        };
        let mut w = Vec::new();
        let g = normalize(&raw, &mut w).gps.expect("gps");
        assert_eq!(g.lat_e7, 399_515_000);
        assert_eq!(g.lon_e7, 1_163_900_000);
    }

    #[test]
    fn gps_from_xmp_comma_form_still_works() {
        // 回归：逗号形式不变
        let raw = RawTags {
            exif: Vec::new(),
            xmp: Vec::from([
                xmp_p("exif", "GPSLatitude", "39,57.0900N"),
                xmp_p("exif", "GPSLongitude", "116,23.4000E"),
            ]),
            container: Vec::new(),
        };
        let mut w = Vec::new();
        let g = normalize(&raw, &mut w).gps.expect("gps");
        assert!((g.lat_e7 - 399_515_000).abs() <= 2);
        assert!((g.lon_e7 - 1_163_900_000).abs() <= 2);
    }

    #[test]
    fn software_precedence_container_over_exif_over_xmp() {
        use crate::model::{ContainerSource, ContainerTag, ExifTag, IfdKind, Value, XmpProperty};
        let mut warnings = Vec::new();
        let raw = RawTags {
            exif: alloc::vec![ExifTag { ifd: IfdKind::Primary, tag: 0x0131, value: Value::Text(alloc::string::String::from("ExifSW")) }],
            xmp: alloc::vec![XmpProperty { prefix: alloc::string::String::from("xmp"), name: alloc::string::String::from("CreatorTool"), value: alloc::string::String::from("XmpSW") }],
            container: alloc::vec![ContainerTag { source: ContainerSource::QuickTimeMdta, key: alloc::string::String::from("com.apple.quicktime.software"), value: Value::Text(alloc::string::String::from("ContainerSW")) }],
        };
        let u = normalize(&raw, &mut warnings);
        assert_eq!(u.software.as_deref(), Some("ContainerSW"));
    }

    #[test]
    fn software_falls_back_exif_then_xmp() {
        use crate::model::{ExifTag, IfdKind, Value, XmpProperty};
        let mut warnings = Vec::new();
        let raw_exif = RawTags {
            exif: alloc::vec![ExifTag { ifd: IfdKind::Primary, tag: 0x0131, value: Value::Text(alloc::string::String::from("ExifSW")) }],
            xmp: Vec::new(), container: Vec::new(),
        };
        assert_eq!(normalize(&raw_exif, &mut warnings).software.as_deref(), Some("ExifSW"));
        let raw_xmp = RawTags {
            exif: Vec::new(),
            xmp: alloc::vec![XmpProperty { prefix: alloc::string::String::from("xmp"), name: alloc::string::String::from("CreatorTool"), value: alloc::string::String::from("XmpSW") }],
            container: Vec::new(),
        };
        assert_eq!(normalize(&raw_xmp, &mut warnings).software.as_deref(), Some("XmpSW"));
    }

    #[test]
    fn creator_from_container_udta_and_exif_artist() {
        use crate::model::{ContainerSource, ContainerTag, ExifTag, IfdKind, Value};
        let mut warnings = Vec::new();
        let raw_udta = RawTags {
            exif: Vec::new(), xmp: Vec::new(),
            container: alloc::vec![ContainerTag { source: ContainerSource::Udta, key: alloc::string::String::from("©aut"), value: Value::Text(alloc::string::String::from("Auteur")) }],
        };
        assert_eq!(normalize(&raw_udta, &mut warnings).creator.as_deref(), Some("Auteur"));
        let raw_artist = RawTags {
            exif: alloc::vec![ExifTag { ifd: IfdKind::Primary, tag: 0x013B, value: Value::Text(alloc::string::String::from("Shooter")) }],
            xmp: Vec::new(), container: Vec::new(),
        };
        assert_eq!(normalize(&raw_artist, &mut warnings).creator.as_deref(), Some("Shooter"));
    }
}
