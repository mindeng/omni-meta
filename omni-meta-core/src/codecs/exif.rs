//! EXIF 解码：TIFF 头 (II/MM + 42 + IFD0 偏移) → IFD0 标签。
//! 本计划只解 ASCII (type 2) 与 SHORT (type 3) 标签，足够 Make/Model/Orientation。

use alloc::string::String;
use alloc::vec::Vec;

use crate::cursor::{ByteCursor, Endian};
use crate::limits::Limits;
use crate::model::{ExifTag, IfdKind, Value, WarnKind, Warning};

/// 解码一段 TIFF 字节（即 APP1 "Exif\0\0" 之后的内容）。
pub fn decode(
    tiff: &[u8],
    out: &mut Vec<ExifTag>,
    warnings: &mut Vec<Warning>,
    limits: &Limits,
) {
    let mut cur = ByteCursor::new(tiff);
    let endian = match cur.take(2) {
        Some(s) if s == b"II" => Endian::Little,
        Some(s) if s == b"MM" => Endian::Big,
        _ => {
            warnings.push(Warning { offset: 0, kind: WarnKind::BadExifHeader });
            return;
        }
    };
    if cur.u16(endian) != Some(42) {
        warnings.push(Warning { offset: 2, kind: WarnKind::BadExifHeader });
        return;
    }
    let ifd0 = match cur.u32(endian) {
        Some(v) => v as usize,
        None => {
            warnings.push(Warning { offset: 4, kind: WarnKind::BadExifHeader });
            return;
        }
    };
    parse_ifd(tiff, endian, ifd0, out, warnings, limits);
}

fn parse_ifd(
    tiff: &[u8],
    e: Endian,
    off: usize,
    out: &mut Vec<ExifTag>,
    warnings: &mut Vec<Warning>,
    limits: &Limits,
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
        if let Some(val) = read_value(tiff, e, typ, cnt, valoff, limits.max_payload_bytes) {
            out.push(ExifTag { ifd: IfdKind::Primary, tag, value: val });
        }
    }
}

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

fn read_u32_at(e: Endian, b: &[u8; 4]) -> u32 {
    match e {
        Endian::Little => u32::from_le_bytes(*b),
        Endian::Big => u32::from_be_bytes(*b),
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
        let off = read_u32_at(e, valoff.try_into().ok()?) as usize;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个小端 TIFF：IFD0 含 Make="Acme"(0x010F) 与 Orientation=6(0x0112)。
    /// 布局：II,42,IFD0@8 | count=2 | Make 条目(偏移指向 38) | Orientation 条目(内联 6) | next=0 | "Acme\0"@38
    fn tiff_fixture() -> Vec<u8> {
        let mut t: Vec<u8> = Vec::new();
        t.extend_from_slice(b"II"); // 0..2 小端
        t.extend_from_slice(&42u16.to_le_bytes()); // 2..4
        t.extend_from_slice(&8u32.to_le_bytes()); // 4..8 IFD0 偏移
        // IFD0 @ 8
        t.extend_from_slice(&2u16.to_le_bytes()); // entry count
        // 条目 1: Make, ASCII, count=5, 偏移=38
        t.extend_from_slice(&0x010Fu16.to_le_bytes());
        t.extend_from_slice(&2u16.to_le_bytes());
        t.extend_from_slice(&5u32.to_le_bytes());
        t.extend_from_slice(&38u32.to_le_bytes());
        // 条目 2: Orientation, SHORT, count=1, 内联值=6
        t.extend_from_slice(&0x0112u16.to_le_bytes());
        t.extend_from_slice(&3u16.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&6u32.to_le_bytes());
        // next IFD = 0
        t.extend_from_slice(&0u32.to_le_bytes());
        // "Acme\0" @ 38
        debug_assert_eq!(t.len(), 38);
        t.extend_from_slice(b"Acme\0");
        t
    }

    #[test]
    fn decodes_make_and_orientation() {
        let tiff = tiff_fixture();
        let mut out = Vec::new();
        let mut warns = Vec::new();
        decode(&tiff, &mut out, &mut warns, &Limits::default());
        assert!(warns.is_empty(), "unexpected warnings: {:?}", warns);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], ExifTag { ifd: IfdKind::Primary, tag: 0x010F, value: Value::Text(String::from("Acme")) });
        assert_eq!(out[1], ExifTag { ifd: IfdKind::Primary, tag: 0x0112, value: Value::U16(6) });
    }

    #[test]
    fn bad_header_yields_warning_not_panic() {
        let mut out = Vec::new();
        let mut warns = Vec::new();
        decode(b"XX", &mut out, &mut warns, &Limits::default());
        assert!(out.is_empty());
        assert_eq!(warns.len(), 1);
        assert_eq!(warns[0].kind, WarnKind::BadExifHeader);
    }

    /// 大端 (MM) 版本：同样 Make="Acme" + Orientation=6。
    fn tiff_fixture_be() -> Vec<u8> {
        let mut t: Vec<u8> = Vec::new();
        t.extend_from_slice(b"MM");
        t.extend_from_slice(&42u16.to_be_bytes());
        t.extend_from_slice(&8u32.to_be_bytes());
        t.extend_from_slice(&2u16.to_be_bytes());
        // Make, ASCII, count=5, offset=38
        t.extend_from_slice(&0x010Fu16.to_be_bytes());
        t.extend_from_slice(&2u16.to_be_bytes());
        t.extend_from_slice(&5u32.to_be_bytes());
        t.extend_from_slice(&38u32.to_be_bytes());
        // Orientation, SHORT, count=1, 大端内联值=6（左对齐：00 06 00 00）
        t.extend_from_slice(&0x0112u16.to_be_bytes());
        t.extend_from_slice(&3u16.to_be_bytes());
        t.extend_from_slice(&1u32.to_be_bytes());
        t.extend_from_slice(&6u16.to_be_bytes()); // 高 2 字节 = 值
        t.extend_from_slice(&0u16.to_be_bytes()); // 低 2 字节填充
        // next IFD = 0
        t.extend_from_slice(&0u32.to_be_bytes());
        debug_assert_eq!(t.len(), 38);
        t.extend_from_slice(b"Acme\0");
        t
    }

    #[test]
    fn decodes_big_endian() {
        let tiff = tiff_fixture_be();
        let mut out = Vec::new();
        let mut warns = Vec::new();
        decode(&tiff, &mut out, &mut warns, &Limits::default());
        assert!(warns.is_empty(), "unexpected warnings: {:?}", warns);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], ExifTag { ifd: IfdKind::Primary, tag: 0x010F, value: Value::Text(String::from("Acme")) });
        assert_eq!(out[1], ExifTag { ifd: IfdKind::Primary, tag: 0x0112, value: Value::U16(6) });
    }

    #[test]
    fn max_tags_limit_is_enforced() {
        let tiff = tiff_fixture();
        let mut out = Vec::new();
        let mut warns = Vec::new();
        let limits = Limits { max_tags: 1, ..Limits::default() };
        decode(&tiff, &mut out, &mut warns, &limits);
        assert_eq!(out.len(), 1); // 第二个标签被上界截断
    }

    #[test]
    fn out_of_bounds_ascii_offset_drops_tag_without_panic() {
        // II,42,IFD0@8 | count=1 | ASCII cnt=100 offset=9999(越界) | next=0
        let mut t: Vec<u8> = Vec::new();
        t.extend_from_slice(b"II");
        t.extend_from_slice(&42u16.to_le_bytes());
        t.extend_from_slice(&8u32.to_le_bytes());
        t.extend_from_slice(&1u16.to_le_bytes());
        t.extend_from_slice(&0x010Fu16.to_le_bytes());
        t.extend_from_slice(&2u16.to_le_bytes());
        t.extend_from_slice(&100u32.to_le_bytes());
        t.extend_from_slice(&9999u32.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        let mut out = Vec::new();
        let mut warns = Vec::new();
        decode(&t, &mut out, &mut warns, &Limits::default());
        assert!(out.is_empty()); // 越界偏移 → 丢弃该标签，不 panic
    }

    #[test]
    fn ifd0_offset_past_end_warns_without_panic() {
        // II,42,IFD0@9999(越界)
        let mut t: Vec<u8> = Vec::new();
        t.extend_from_slice(b"II");
        t.extend_from_slice(&42u16.to_le_bytes());
        t.extend_from_slice(&9999u32.to_le_bytes());
        let mut out = Vec::new();
        let mut warns = Vec::new();
        decode(&t, &mut out, &mut warns, &Limits::default());
        assert!(out.is_empty());
        assert_eq!(warns.len(), 1);
        assert_eq!(warns[0].kind, WarnKind::BadExifHeader);
    }

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
        let t = tiff_one(0x0111, 4, 1, 1234u32.to_le_bytes(), &[]);
        let (out, _) = decode_one(&t);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].value, Value::U32(1234));
    }

    #[test]
    fn reads_srational_external() {
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
        let t = tiff_one(0x9000, 7, 4, *b"0230", &[]);
        let (out, _) = decode_one(&t);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].value, Value::Bytes(Vec::from(b"0230".as_slice())));
    }

    #[test]
    fn reads_short_array_as_list() {
        let t = tiff_one(0x0212, 3, 2, [2, 0, 3, 0], &[]);
        let (out, _) = decode_one(&t);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].value, Value::List(Vec::from([Value::U16(2), Value::U16(3)])));
    }

    #[test]
    fn unknown_type_drops_tag() {
        let t = tiff_one(0x0100, 99, 1, [0, 0, 0, 0], &[]);
        let (out, warns) = decode_one(&t);
        assert!(out.is_empty());
        assert!(warns.is_empty());
    }
}
