//! ISO-BMFF 顶层解析骨架。本里程碑（A1）只校验首个 box 是 `ftyp` 即 `Done`；
//! `meta`/`moov` 下钻在 A2/A3 引入。沿用既有 sans-io MetaParser 契约。

use alloc::vec::Vec;

use crate::containers::isobmff::{full_box_vf, iter_child_boxes, read_box_header, read_uint_be};
use crate::cursor::{ByteCursor, Endian};
use crate::demand::{Demand, Event, MetaParser, PayloadKind, PullResult};
use crate::model::{Field, WarnKind, Warning};

/// 我们关心的一个 item（EXIF 或 XMP）及其 ID。
struct Wanted {
    id: u32,
    kind: PayloadKind,
}

/// 取 null 终止字符串（到首个 0 字节为止，无 0 则取全部）。
fn take_cstr(b: &[u8]) -> &[u8] {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    &b[..end]
}

/// 解析一个 `infe`（ItemInfoEntry）载荷；仅识别 version 2/3（带 item_type）。
/// 返回我们关心的 item（Exif，或 content_type 为 application/rdf+xml 的 mime），否则 None。
fn parse_infe(payload: &[u8]) -> Option<Wanted> {
    let (version, _flags) = full_box_vf(payload)?;
    let mut cur = ByteCursor::new(payload);
    cur.seek(4)?; // 跳过 version/flags
    let id = match version {
        2 => u32::from(cur.u16(Endian::Big)?),
        3 => cur.u32(Endian::Big)?,
        _ => return None,
    };
    let _protection = cur.u16(Endian::Big)?;
    let item_type = cur.take(4)?;
    if item_type == b"Exif" {
        return Some(Wanted { id, kind: PayloadKind::Exif });
    }
    if item_type == b"mime" {
        // ItemInfoEntry v2/3：item_name(null 终止) 在 item_type 之后、content_type 之前。
        let rest = &payload[cur.position()..];
        let after_name = match rest.iter().position(|&c| c == 0) {
            Some(i) => i + 1,
            None => return None,
        };
        if take_cstr(&rest[after_name..]) == b"application/rdf+xml" {
            return Some(Wanted { id, kind: PayloadKind::Xmp });
        }
    }
    None
}

/// 解析 `iinf`（ItemInfoBox）载荷 → 我们关心的 item 列表。
fn parse_iinf(payload: &[u8]) -> Vec<Wanted> {
    let mut out = Vec::new();
    let (version, _flags) = match full_box_vf(payload) {
        Some(v) => v,
        None => return out,
    };
    let mut cur = ByteCursor::new(payload);
    if cur.seek(4).is_none() {
        return out;
    }
    let count = if version == 0 {
        match cur.u16(Endian::Big) {
            Some(c) => u32::from(c),
            None => return out,
        }
    } else {
        match cur.u32(Endian::Big) {
            Some(c) => c,
            None => return out,
        }
    };
    let entries = &payload[cur.position()..];
    let mut seen = 0u32;
    for (hdr, infe_payload) in iter_child_boxes(entries) {
        if seen >= count {
            break;
        }
        seen += 1;
        if &hdr.kind != b"infe" {
            continue;
        }
        if let Some(w) = parse_infe(infe_payload) {
            out.push(w);
        }
    }
    out
}

/// 一条 item 定位（仅保留首个 extent；多 extent 在装配时按警告跳过）。
struct Loc {
    id: u32,
    method: u8,
    extent_count: u16,
    /// 首个 extent：(偏移, 长度)。method 0 为绝对文件偏移；method 1 为 idat 内相对偏移。
    first_extent: Option<(u64, u64)>,
}

/// 解析 `iloc`（ItemLocationBox）载荷 → 各 item 定位。
fn parse_iloc(payload: &[u8]) -> Vec<Loc> {
    let mut out = Vec::new();
    let (version, _flags) = match full_box_vf(payload) {
        Some(v) => v,
        None => return out,
    };
    let mut cur = ByteCursor::new(payload);
    if cur.seek(4).is_none() {
        return out;
    }
    let sizes = match cur.u8() {
        Some(b) => b,
        None => return out,
    };
    let offset_size = sizes >> 4;
    let length_size = sizes & 0x0F;
    let sizes2 = match cur.u8() {
        Some(b) => b,
        None => return out,
    };
    let base_offset_size = sizes2 >> 4;
    let index_size = sizes2 & 0x0F; // 仅 version 1/2 使用
    let item_count = if version < 2 {
        match cur.u16(Endian::Big) {
            Some(c) => u32::from(c),
            None => return out,
        }
    } else {
        match cur.u32(Endian::Big) {
            Some(c) => c,
            None => return out,
        }
    };
    for _ in 0..item_count {
        let id = if version < 2 {
            match cur.u16(Endian::Big) {
                Some(v) => u32::from(v),
                None => break,
            }
        } else {
            match cur.u32(Endian::Big) {
                Some(v) => v,
                None => break,
            }
        };
        let method = if version == 1 || version == 2 {
            match cur.u16(Endian::Big) {
                Some(v) => (v & 0x0F) as u8,
                None => break,
            }
        } else {
            0
        };
        if cur.u16(Endian::Big).is_none() {
            break; // data_reference_index
        }
        let base_offset = match read_uint_be(&mut cur, base_offset_size) {
            Some(v) => v,
            None => break,
        };
        let extent_count = match cur.u16(Endian::Big) {
            Some(v) => v,
            None => break,
        };
        let mut first_extent = None;
        let mut ok = true;
        for i in 0..extent_count {
            if (version == 1 || version == 2) && index_size > 0 && read_uint_be(&mut cur, index_size).is_none() {
                ok = false;
                break;
            }
            let eo = match read_uint_be(&mut cur, offset_size) {
                Some(v) => v,
                None => {
                    ok = false;
                    break;
                }
            };
            let el = match read_uint_be(&mut cur, length_size) {
                Some(v) => v,
                None => {
                    ok = false;
                    break;
                }
            };
            if i == 0 {
                first_extent = Some((base_offset.saturating_add(eo), el));
            }
        }
        if !ok {
            break;
        }
        out.push(Loc { id, method, extent_count, first_extent });
    }
    out
}

#[derive(Debug, Default)]
pub struct BmffParser {
    done: bool,
}

impl BmffParser {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MetaParser for BmffParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        let events: Vec<Event<'a>> = Vec::new();
        if self.done {
            return PullResult { demand: Demand::Done, consumed: 0, events };
        }
        // 读首个 box 头需 ≥8 字节（largesize 也只需头部，不读 ftyp 载荷）。
        let hdr = match read_box_header(input) {
            Some(h) => h,
            None => {
                // size==1 标记 largesize：头部需 16 字节，否则基本头 8 字节。
                let need = if input.len() >= 4
                    && u32::from_be_bytes([input[0], input[1], input[2], input[3]]) == 1
                {
                    16
                } else {
                    8
                };
                return PullResult { demand: Demand::NeedBytes(need), consumed: 0, events };
            }
        };
        // probe 已确保首盒为 ftyp（hdr 仅用于确认头部可完整读出）。
        // A1 不抽取元数据，读到首盒头即完成；box 链续走留给 A2/A3。
        let _ = hdr.kind;
        self.done = true;
        PullResult { demand: Demand::Done, consumed: 0, events }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ftyp_box() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&20u32.to_be_bytes());
        b.extend_from_slice(b"ftyp");
        b.extend_from_slice(b"heic");
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(b"mif1");
        b
    }

    #[test]
    fn ftyp_then_done_no_events() {
        let buf = ftyp_box();
        let mut p = BmffParser::new();
        let res = p.pull(&buf);
        assert_eq!(res.demand, Demand::Done);
        assert!(res.events.is_empty());
    }

    #[test]
    fn short_input_needs_bytes() {
        let mut p = BmffParser::new();
        let res = p.pull(&[0, 0, 0]); // <8 字节
        assert_eq!(res.demand, Demand::NeedBytes(8));
        assert_eq!(res.consumed, 0);
    }

    #[test]
    fn largesize_partial_header_needs_16() {
        // size32==1 标记 largesize：头部需 16 字节。
        // 输入仅 12 字节（8 基本头 + 4/8 largesize），应索要 16 而非 8。
        let mut b = Vec::new();
        b.extend_from_slice(&1u32.to_be_bytes()); // size32==1
        b.extend_from_slice(b"mdat");
        b.extend_from_slice(&[0u8; 4]); // largesize 仅 4 字节
        let mut p = BmffParser::new();
        let res = p.pull(&b);
        assert_eq!(res.demand, Demand::NeedBytes(16));
        assert_eq!(res.consumed, 0);
    }

    #[test]
    fn second_pull_after_done_stays_done() {
        let buf = ftyp_box();
        let mut p = BmffParser::new();
        let _ = p.pull(&buf);
        let res = p.pull(&buf);
        assert_eq!(res.demand, Demand::Done);
    }

    fn box_bytes(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
        b.extend_from_slice(kind);
        b.extend_from_slice(payload);
        b
    }

    fn infe(id: u16, typ: &[u8; 4], content_type: Option<&[u8]>) -> Vec<u8> {
        let mut p = alloc::vec![2u8, 0, 0, 0]; // version 2, flags 0
        p.extend_from_slice(&id.to_be_bytes());
        p.extend_from_slice(&0u16.to_be_bytes()); // protection index
        p.extend_from_slice(typ);
        p.push(0); // item_name = "" (spec 要求 v2/3 存在)
        if let Some(ct) = content_type {
            p.extend_from_slice(ct);
            p.push(0);
        }
        box_bytes(b"infe", &p)
    }

    #[test]
    fn parse_iinf_picks_exif_and_xmp() {
        let mut p = alloc::vec![0u8, 0, 0, 0]; // version 0, flags 0
        p.extend_from_slice(&3u16.to_be_bytes()); // count
        p.extend_from_slice(&infe(1, b"Exif", None));
        p.extend_from_slice(&infe(2, b"mime", Some(b"application/rdf+xml")));
        p.extend_from_slice(&infe(3, b"hvc1", None)); // 图像数据，忽略
        let wanted = parse_iinf(&p);
        assert_eq!(wanted.len(), 2);
        assert_eq!(wanted[0].id, 1);
        assert_eq!(wanted[0].kind, PayloadKind::Exif);
        assert_eq!(wanted[1].id, 2);
        assert_eq!(wanted[1].kind, PayloadKind::Xmp);
    }

    #[test]
    fn parse_iinf_ignores_non_rdf_mime() {
        let mut p = alloc::vec![0u8, 0, 0, 0];
        p.extend_from_slice(&1u16.to_be_bytes());
        p.extend_from_slice(&infe(1, b"mime", Some(b"text/plain")));
        assert!(parse_iinf(&p).is_empty());
    }

    #[test]
    fn parse_iloc_v0_method0_single_extent() {
        // version 0：offset_size=4,length_size=4,base_offset_size=0,index_size=0
        let mut p = alloc::vec![0u8, 0, 0, 0]; // vf
        p.push(0x44); // offset4 | length4
        p.push(0x00); // base0 | index0
        p.extend_from_slice(&1u16.to_be_bytes()); // item_count
        p.extend_from_slice(&1u16.to_be_bytes()); // item_id=1
        p.extend_from_slice(&0u16.to_be_bytes()); // data_reference_index
        p.extend_from_slice(&1u16.to_be_bytes()); // extent_count
        p.extend_from_slice(&1000u32.to_be_bytes()); // extent_offset
        p.extend_from_slice(&42u32.to_be_bytes()); // extent_length
        let locs = parse_iloc(&p);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].id, 1);
        assert_eq!(locs[0].method, 0);
        assert_eq!(locs[0].extent_count, 1);
        assert_eq!(locs[0].first_extent, Some((1000, 42)));
    }

    #[test]
    fn parse_iloc_v1_method1_idat() {
        // version 1：带 construction_method 字段；method=1（idat）
        let mut p = alloc::vec![1u8, 0, 0, 0]; // vf, version 1
        p.push(0x44); // offset4 | length4
        p.push(0x00); // base0 | index0
        p.extend_from_slice(&1u16.to_be_bytes()); // item_count
        p.extend_from_slice(&5u16.to_be_bytes()); // item_id=5
        p.extend_from_slice(&1u16.to_be_bytes()); // construction_method=1
        p.extend_from_slice(&0u16.to_be_bytes()); // data_reference_index
        p.extend_from_slice(&1u16.to_be_bytes()); // extent_count
        p.extend_from_slice(&0u32.to_be_bytes()); // extent_offset (idat 内)
        p.extend_from_slice(&8u32.to_be_bytes()); // extent_length
        let locs = parse_iloc(&p);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].id, 5);
        assert_eq!(locs[0].method, 1);
        assert_eq!(locs[0].first_extent, Some((0, 8)));
    }
}
