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

fn read_value(
    tiff: &[u8],
    e: Endian,
    typ: u16,
    cnt: u32,
    valoff: &[u8],
    max_value_bytes: usize,
) -> Option<Value> {
    debug_assert_eq!(valoff.len(), 4);
    match typ {
        // SHORT：本计划只取 cnt==1。
        3 => {
            if cnt != 1 {
                return None;
            }
            let v = match e {
                Endian::Little => u16::from_le_bytes([valoff[0], valoff[1]]),
                Endian::Big => u16::from_be_bytes([valoff[0], valoff[1]]),
            };
            Some(Value::U16(v))
        }
        // ASCII：<=4 字节内联，否则按偏移取。
        2 => {
            let total = cnt as usize;
            // cnt==0 畸形（ASCII 至少含 NUL）；total 超上界则丢弃，
            // 防止 max_tags 条目各自分配大字符串造成聚合放大。
            // 注：完整的 max_total_alloc 预算化留待后续硬化/模糊测试计划。
            if total == 0 || total > max_value_bytes {
                return None;
            }
            let bytes: &[u8] = if total <= 4 {
                &valoff[..total]
            } else {
                let off = match e {
                    Endian::Little => {
                        u32::from_le_bytes([valoff[0], valoff[1], valoff[2], valoff[3]])
                    }
                    Endian::Big => {
                        u32::from_be_bytes([valoff[0], valoff[1], valoff[2], valoff[3]])
                    }
                } as usize;
                let end = off.checked_add(total)?;
                tiff.get(off..end)?
            };
            let nul = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            let s = core::str::from_utf8(&bytes[..nul]).ok()?;
            Some(Value::Text(String::from(s)))
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
}
