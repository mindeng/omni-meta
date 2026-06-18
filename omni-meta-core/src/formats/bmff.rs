//! ISO-BMFF 顶层解析骨架。本里程碑（A1）只校验首个 box 是 `ftyp` 即 `Done`；
//! `meta`/`moov` 下钻在 A2/A3 引入。沿用既有 sans-io MetaParser 契约。

use alloc::vec::Vec;

use crate::containers::isobmff::{full_box_vf, iter_child_boxes, read_box_header, read_uint_be};
use crate::cursor::{ByteCursor, Endian};
use crate::demand::{Demand, Event, MetaParser, PayloadKind, PullResult};
use crate::model::{
    ContainerSource, ContainerTag, DateTimeParts, Field, Gps, Value, WarnKind, Warning,
};

/// MP4/MOV 纪元起点（1904-01-01）相对 Unix 纪元（1970-01-01）的天数差。
const MP4_EPOCH_DAYS_BEFORE_UNIX: i64 = 24107;

/// MP4/MOV creation_time（自 1904-01-01 00:00:00 UTC 的秒）→ DateTimeParts（UTC）。
fn datetime_from_mp4_epoch(secs: u64) -> DateTimeParts {
    let days = (secs / 86_400) as i64 - MP4_EPOCH_DAYS_BEFORE_UNIX;
    let tod = (secs % 86_400) as u32;
    let (year, month, day) = crate::civil::civil_from_days(days);
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
        let creation = match read_uint_be(&mut cur, 8) {
            Some(v) => v,
            None => return out,
        };
        if read_uint_be(&mut cur, 8).is_none() {
            return out;
        } // modification_time
        let timescale = match cur.u32(Endian::Big) {
            Some(v) => v,
            None => return out,
        };
        let duration = match read_uint_be(&mut cur, 8) {
            Some(v) => v,
            None => return out,
        };
        (creation, timescale, duration)
    } else {
        let creation = match cur.u32(Endian::Big) {
            Some(v) => u64::from(v),
            None => return out,
        };
        if cur.u32(Endian::Big).is_none() {
            return out;
        } // modification_time
        let timescale = match cur.u32(Endian::Big) {
            Some(v) => v,
            None => return out,
        };
        let duration = match cur.u32(Endian::Big) {
            Some(v) => u64::from(v),
            None => return out,
        };
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

/// 解析 `tkhd`（TrackHeaderBox）载荷 → (width, height) 像素整数。
/// width/height 为载荷末 8 字节的 16.16 定点；按 version 计算偏移以避免误读
/// 可能的尾随字节。任一为 0（音频/数据/提示轨）或截断 → None。
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

/// moov 解析产物：维度、时长、创建时间、GPS、警告。
struct MoovInfo {
    dims: Option<(u32, u32)>,
    duration_ms: Option<u64>,
    created: Option<DateTimeParts>,
    gps: Option<Gps>,
    warnings: Vec<Warning>,
    container_tags: Vec<ContainerTag>,
}

/// 解析 `moov` 载荷：mvhd → 时长/创建时间；逐 trak → tkhd 取首个非零维度。
/// 亦下钻 udta（©xyz/loci）与 meta（QuickTime mdta）取 GPS/make/model/creationdate。
/// `moov_abs_base` 仅用于警告偏移保真。深度 2 显式迭代，非递归。
/// `max_tags`：各来源 container_tags 单独封顶；峰值 ≤ 2×max_tags，Collector 再裁到 max_tags。
fn parse_moov(moov_payload: &[u8], moov_abs_base: u64, max_tags: usize) -> MoovInfo {
    let mut info = MoovInfo {
        dims: None,
        duration_ms: None,
        created: None,
        gps: None,
        warnings: Vec::new(),
        container_tags: Vec::new(),
    };
    let mut xyz_gps: Option<Gps> = None;
    let mut loci_gps: Option<Gps> = None;
    let mut mdta = QtMdta {
        gps: None,
        tags: alloc::vec::Vec::new(),
    };
    let mut udta_tags: Vec<ContainerTag> = Vec::new();
    for (hdr, p) in iter_child_boxes(moov_payload) {
        match &hdr.kind {
            b"mvhd" => {
                let m = parse_mvhd(p);
                info.duration_ms = m.duration_ms;
                info.created = m.created;
                if m.timescale_invalid {
                    info.warnings.push(Warning {
                        offset: moov_abs_base,
                        kind: WarnKind::UnrecognizedValue,
                    });
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
            b"udta" => {
                for (uhdr, up) in iter_child_boxes(p) {
                    match &uhdr.kind {
                        b"\xA9xyz" if xyz_gps.is_none() => xyz_gps = parse_xyz(up),
                        b"loci" if loci_gps.is_none() => loci_gps = parse_loci(up),
                        // ©xyz は GPS 専用；generic arm から除外（二重目も container に漏らさない）。
                        k if k[0] == 0xA9 && k != b"\xA9xyz" => {
                            if udta_tags.len() < max_tags
                                && let (Some(key), Some(text)) =
                                    (udta_key_string(k), parse_udta_text(up))
                            {
                                udta_tags.push(ContainerTag {
                                    source: ContainerSource::Udta,
                                    key,
                                    value: Value::Text(alloc::string::String::from(text)),
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            b"meta" => {
                let remaining = max_tags.saturating_sub(mdta.tags.len());
                let m = parse_qt_mdta(p, remaining);
                if mdta.gps.is_none() {
                    mdta.gps = m.gps;
                }
                mdta.tags.extend(m.tags);
            }
            _ => {}
        }
    }
    // GPS 优先级：©xyz > mdta > loci。
    info.gps = xyz_gps.or(mdta.gps).or(loci_gps);
    // created：仅保留 mvhd 结构值；mdta creationdate 由 normalize 从 container_tags 读取并排首位。
    // 各来源已独立封顶：mdta 用递减预算（峰值 ≤ max_tags），udta 逐条守卫（≤ max_tags）；
    // 峰值 ≤ 2×max_tags（与 meta 盒数无关）；合并后再裁到 max_tags，使合并总量精确有界。
    info.container_tags = mdta.tags;
    info.container_tags.append(&mut udta_tags);
    info.container_tags.truncate(max_tags);
    info
}

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
        return Some(Wanted {
            id,
            kind: PayloadKind::Exif,
        });
    }
    if item_type == b"mime" {
        // ItemInfoEntry v2/3：item_name(null 终止) 在 item_type 之后、content_type 之前。
        let rest = &payload[cur.position()..];
        let after_name = match rest.iter().position(|&c| c == 0) {
            Some(i) => i + 1,
            None => return None,
        };
        if take_cstr(&rest[after_name..]) == b"application/rdf+xml" {
            return Some(Wanted {
                id,
                kind: PayloadKind::Xmp,
            });
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
    for (seen, (hdr, infe_payload)) in iter_child_boxes(entries).enumerate() {
        if seen as u32 >= count {
            break;
        }
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
            if (version == 1 || version == 2)
                && index_size > 0
                && read_uint_be(&mut cur, index_size).is_none()
            {
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
        out.push(Loc {
            id,
            method,
            extent_count,
            first_extent,
        });
    }
    out
}

/// 解析 `pitm`（PrimaryItemBox）→ 主 item ID。
fn parse_pitm(payload: &[u8]) -> Option<u32> {
    let (version, _flags) = full_box_vf(payload)?;
    let mut cur = ByteCursor::new(payload);
    cur.seek(4)?;
    if version == 0 {
        cur.u16(Endian::Big).map(u32::from)
    } else {
        cur.u32(Endian::Big)
    }
}

/// 解析 `ispe`（ImageSpatialExtentsProperty）→ (width, height)。
fn parse_ispe(payload: &[u8]) -> Option<(u32, u32)> {
    let _vf = full_box_vf(payload)?;
    let mut cur = ByteCursor::new(payload);
    cur.seek(4)?;
    let w = cur.u32(Endian::Big)?;
    let h = cur.u32(Endian::Big)?;
    Some((w, h))
}

/// 从 `ipma` 关联中找主 item 的 ispe 维度。`props` 为 ipco 子盒按序的 ispe 维度（非 ispe 为 None）。
fn dims_via_ipma(payload: &[u8], primary: u32, props: &[Option<(u32, u32)>]) -> Option<(u32, u32)> {
    let (version, flags) = full_box_vf(payload)?;
    let mut cur = ByteCursor::new(payload);
    cur.seek(4)?;
    let entry_count = cur.u32(Endian::Big)?;
    let wide_index = (flags & 1) == 1;
    for _ in 0..entry_count {
        let item_id = if version < 1 {
            u32::from(cur.u16(Endian::Big)?)
        } else {
            cur.u32(Endian::Big)?
        };
        let assoc_count = cur.u8()?;
        for _ in 0..assoc_count {
            let idx = if wide_index {
                (cur.u16(Endian::Big)? & 0x7FFF) as usize
            } else {
                (cur.u8()? & 0x7F) as usize
            };
            if item_id == primary
                && idx >= 1
                && let Some(Some(dims)) = props.get(idx - 1)
            {
                return Some(*dims);
            }
        }
    }
    None
}

/// 解析 `iprp`（ItemPropertiesBox）→ 主 item 维度。
/// 优先 pitm+ipma 关联；兜底：ipco 内恰好一个 ispe 时直接用。
fn dims_from_iprp(iprp_payload: &[u8], primary: Option<u32>) -> Option<(u32, u32)> {
    let mut ipco_payload: Option<&[u8]> = None;
    let mut ipma_payload: Option<&[u8]> = None;
    for (hdr, p) in iter_child_boxes(iprp_payload) {
        match &hdr.kind {
            b"ipco" => ipco_payload = Some(p),
            b"ipma" => ipma_payload = Some(p),
            _ => {}
        }
    }
    let ipco = ipco_payload?;
    let mut props: Vec<Option<(u32, u32)>> = Vec::new();
    for (hdr, p) in iter_child_boxes(ipco) {
        props.push(if &hdr.kind == b"ispe" {
            parse_ispe(p)
        } else {
            None
        });
    }
    if let (Some(ipma), Some(pid)) = (ipma_payload, primary)
        && let Some(dims) = dims_via_ipma(ipma, pid, &props)
    {
        return Some(dims);
    }
    // 兜底：恰好一个 ispe
    let mut found = None;
    let mut n = 0u32;
    for d in props.iter().flatten() {
        found = Some(*d);
        n += 1;
    }
    if n == 1 { found } else { None }
}

/// 一个 method-0 抽取目标（数据在文件别处，需 SeekTo）。
#[derive(Debug, Clone, Copy)]
struct Target {
    offset: u64,
    length: u64,
    kind: PayloadKind,
    strip_exif: bool,
}

/// meta 解析产物。
struct MetaPlan<'a> {
    dims: Option<(u32, u32)>,
    /// method-1（idat 内联）载荷：已切片、EXIF 已剥前缀。
    inline: Vec<(PayloadKind, &'a [u8])>,
    /// method-0 目标，按 offset 升序。
    targets: Vec<Target>,
    warnings: Vec<Warning>,
}

/// Exif item 数据 = 4 字节 BE tiff_header_offset N，TIFF 自 4+N 起；容错 "Exif\0\0"。
fn strip_exif_prefix(d: &[u8]) -> &[u8] {
    if d.len() < 4 {
        return d;
    }
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) as usize;
    let start = 4usize.saturating_add(n);
    let rest = d.get(start..).unwrap_or(&[]);
    rest.strip_prefix(b"Exif\0\0").unwrap_or(rest)
}

/// 解析 meta box 载荷（meta 自身是 FullBox）。`meta_abs_base` 为 meta box 在文件中的绝对起点
/// （仅用于警告偏移）。
fn parse_meta(meta_payload: &[u8], meta_abs_base: u64) -> MetaPlan<'_> {
    let mut plan = MetaPlan {
        dims: None,
        inline: Vec::new(),
        targets: Vec::new(),
        warnings: Vec::new(),
    };
    if full_box_vf(meta_payload).is_none() {
        return plan;
    }
    let children = &meta_payload[4..];
    let mut items: Vec<Wanted> = Vec::new();
    let mut locs: Vec<Loc> = Vec::new();
    let mut idat: Option<&[u8]> = None;
    let mut primary: Option<u32> = None;
    let mut iprp: Option<&[u8]> = None;
    for (hdr, p) in iter_child_boxes(children) {
        match &hdr.kind {
            b"iinf" => items = parse_iinf(p),
            b"iloc" => locs = parse_iloc(p),
            b"idat" => idat = Some(p),
            b"pitm" => primary = parse_pitm(p),
            b"iprp" => iprp = Some(p),
            _ => {}
        }
    }
    if let Some(iprp) = iprp {
        plan.dims = dims_from_iprp(iprp, primary);
    }
    for w in &items {
        let loc = match locs.iter().find(|l| l.id == w.id) {
            Some(l) => l,
            None => continue,
        };
        if loc.extent_count != 1 {
            // 多 extent（需拼接）暂不支持
            plan.warnings.push(Warning {
                offset: meta_abs_base,
                kind: WarnKind::UnreachableSection,
            });
            continue;
        }
        let (off, len) = match loc.first_extent {
            Some(e) => e,
            None => continue,
        };
        match loc.method {
            0 => plan.targets.push(Target {
                offset: off,
                length: len,
                kind: w.kind,
                strip_exif: w.kind == PayloadKind::Exif,
            }),
            1 => {
                let data = idat.and_then(|d| {
                    let start = usize::try_from(off).ok()?;
                    let end = start.checked_add(usize::try_from(len).ok()?)?;
                    d.get(start..end)
                });
                match data {
                    Some(d) => {
                        let payload = if w.kind == PayloadKind::Exif {
                            strip_exif_prefix(d)
                        } else {
                            d
                        };
                        plan.inline.push((w.kind, payload));
                    }
                    None => plan.warnings.push(Warning {
                        offset: meta_abs_base,
                        kind: WarnKind::UnreachableSection,
                    }),
                }
            }
            _ => plan.warnings.push(Warning {
                offset: meta_abs_base,
                kind: WarnKind::UnreachableSection,
            }),
        }
    }
    plan.targets.sort_by_key(|t| t.offset);
    plan
}

#[derive(Debug, Default)]
pub struct BmffParser {
    done: bool,
    /// Walk 阶段已走过的绝对偏移（当前待读 box 的起点），仅用于警告偏移保真。
    pos: u64,
    /// 是否已解析完 meta、进入 Extract 阶段。
    extracting: bool,
    /// Extract 阶段当前目标下标。
    idx: usize,
    /// method-0 目标，按 offset 升序。
    targets: Vec<Target>,
    /// container_tags 源头上限（来自 Limits::max_tags）。
    max_tags: usize,
}

impl BmffParser {
    #[allow(dead_code)] // 公共 API：测试及外部调用者使用，no_std 构建中不可见
    pub fn new() -> Self {
        Self::with_limits(crate::limits::Limits::default())
    }
    pub fn with_limits(limits: crate::limits::Limits) -> Self {
        Self {
            max_tags: limits.max_tags,
            ..Self::default()
        }
    }
}

/// 读首个 box 头所需字节：size==1（largesize）需 16，否则 8。
fn needed_header_bytes(input: &[u8]) -> usize {
    if input.len() >= 4 && u32::from_be_bytes([input[0], input[1], input[2], input[3]]) == 1 {
        16
    } else {
        8
    }
}

impl MetaParser for BmffParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        if self.done {
            return PullResult {
                demand: Demand::Done,
                consumed: 0,
                events: Vec::new(),
            };
        }
        if self.extracting {
            return self.pull_extract(input);
        }
        self.pull_walk(input)
    }
}

impl BmffParser {
    /// 顶层走盒：跳过非 meta box，命中 meta 后整盒入窗并解析。
    /// 空窗口（由驱动保证仅在 EOF 出现）= 干净结束。
    fn pull_walk<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        if input.is_empty() {
            self.done = true;
            return PullResult {
                demand: Demand::Done,
                consumed: 0,
                events: Vec::new(),
            };
        }
        let hdr = match read_box_header(input) {
            Some(h) => h,
            None => {
                return PullResult {
                    demand: Demand::NeedBytes(needed_header_bytes(input)),
                    consumed: 0,
                    events: Vec::new(),
                };
            }
        };
        if &hdr.kind == b"meta" {
            let total = match hdr.total_size {
                Some(t) => t,
                None => {
                    // size0 meta（延伸至 EOF）：本里程碑不处理，干净结束。
                    self.done = true;
                    return PullResult {
                        demand: Demand::Done,
                        consumed: 0,
                        events: Vec::new(),
                    };
                }
            };
            let need = match usize::try_from(total) {
                Ok(n) => n,
                Err(_) => {
                    self.done = true;
                    return PullResult {
                        demand: Demand::Done,
                        consumed: 0,
                        events: Vec::new(),
                    };
                }
            };
            let header_len = hdr.header_len as usize;
            if need < header_len {
                // 畸形 meta：声明大小小于其自身头部 → 干净结束，绝不 panic。
                self.done = true;
                return PullResult {
                    demand: Demand::Done,
                    consumed: 0,
                    events: Vec::new(),
                };
            }
            if input.len() < need {
                return PullResult {
                    demand: Demand::NeedBytes(need),
                    consumed: 0,
                    events: Vec::new(),
                };
            }
            let plan = parse_meta(&input[header_len..need], self.pos);
            let mut events: Vec<Event<'a>> = Vec::new();
            if let Some((w, h)) = plan.dims {
                events.push(Event::Field(Field::Width(w)));
                events.push(Event::Field(Field::Height(h)));
            }
            for (kind, data) in plan.inline {
                events.push(Event::Payload { kind, data });
            }
            for warn in plan.warnings {
                events.push(Event::Warning(warn));
            }
            self.targets = plan.targets;
            if self.targets.is_empty() {
                self.done = true;
                return PullResult {
                    demand: Demand::Done,
                    consumed: need,
                    events,
                };
            }
            self.extracting = true;
            self.idx = 0;
            let first = self.targets[0].offset;
            return PullResult {
                demand: Demand::SeekTo(first),
                consumed: need,
                events,
            };
        }
        if &hdr.kind == b"moov" {
            let total = match hdr.total_size {
                Some(t) => t,
                None => {
                    // size0 moov（延伸至 EOF）：本里程碑不处理，干净结束。
                    self.done = true;
                    return PullResult {
                        demand: Demand::Done,
                        consumed: 0,
                        events: Vec::new(),
                    };
                }
            };
            let need = match usize::try_from(total) {
                Ok(n) => n,
                Err(_) => {
                    self.done = true;
                    return PullResult {
                        demand: Demand::Done,
                        consumed: 0,
                        events: Vec::new(),
                    };
                }
            };
            let header_len = hdr.header_len as usize;
            if need < header_len {
                // 畸形 moov：声明大小小于其自身头部 → 干净结束，绝不 panic。
                self.done = true;
                return PullResult {
                    demand: Demand::Done,
                    consumed: 0,
                    events: Vec::new(),
                };
            }
            if input.len() < need {
                return PullResult {
                    demand: Demand::NeedBytes(need),
                    consumed: 0,
                    events: Vec::new(),
                };
            }
            let info = parse_moov(&input[header_len..need], self.pos, self.max_tags);
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
            if let Some(g) = info.gps {
                events.push(Event::Field(Field::Gps(g)));
            }
            for t in info.container_tags {
                events.push(Event::ContainerTag(t));
            }
            for warn in info.warnings {
                events.push(Event::Warning(warn));
            }
            self.done = true;
            return PullResult {
                demand: Demand::Done,
                consumed: need,
                events,
            };
        }
        // 非 meta/moov：跳过整盒。size0 / 畸形（payload_len None）→ 不可能再有 meta，干净结束。
        match hdr.payload_len() {
            Some(pl) => {
                self.pos = self.pos.saturating_add(hdr.header_len).saturating_add(pl);
                PullResult {
                    demand: Demand::Skip(pl),
                    consumed: hdr.header_len as usize,
                    events: Vec::new(),
                }
            }
            None => {
                self.done = true;
                PullResult {
                    demand: Demand::Done,
                    consumed: 0,
                    events: Vec::new(),
                }
            }
        }
    }

    /// Extract 阶段：窗口起点 = 当前目标的绝对偏移（驱动已 SeekTo）。
    fn pull_extract<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        let t = self.targets[self.idx];
        let len = match usize::try_from(t.length) {
            Ok(l) => l,
            Err(_) => {
                self.done = true;
                return PullResult {
                    demand: Demand::Done,
                    consumed: 0,
                    events: Vec::new(),
                };
            }
        };
        if input.len() < len {
            return PullResult {
                demand: Demand::NeedBytes(len),
                consumed: 0,
                events: Vec::new(),
            };
        }
        let raw = &input[..len];
        let data = if t.strip_exif {
            strip_exif_prefix(raw)
        } else {
            raw
        };
        let events: Vec<Event<'a>> = alloc::vec![Event::Payload { kind: t.kind, data }];
        self.idx += 1;
        if self.idx >= self.targets.len() {
            self.done = true;
            return PullResult {
                demand: Demand::Done,
                consumed: len,
                events,
            };
        }
        let next = self.targets[self.idx].offset;
        PullResult {
            demand: Demand::SeekTo(next),
            consumed: len,
            events,
        }
    }
}

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

/// 16.16 有符号定点（i32）→ E7。隔离 f64：raw/65536 度 → E7（±0.5 偏置取整，no_std 无 round()）。
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

/// 计算 loci name 串占用的字节数（含终止符）。UTF-16（BOM 0xFEFF/0xFFFE）按 u16 对齐找 0x0000；
/// 否则按 UTF-8 找单字节 0。找不到终止符 → None（畸形）。
fn skip_loci_name(b: &[u8]) -> Option<usize> {
    if b.len() >= 2 && ((b[0] == 0xFE && b[1] == 0xFF) || (b[0] == 0xFF && b[1] == 0xFE)) {
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

/// 解析 `loci`（3GPP FullBox）：ver/flags + lang(2) + name(变长 null 终止) + role(1)
///   + lon(16.16) + lat(16.16) + alt(16.16)。**经在前**。越界 → None。
fn parse_loci(payload: &[u8]) -> Option<Gps> {
    let mut cur = ByteCursor::new(payload);
    cur.seek(4)?; // version+flags（绝对跳到位置 4）
    cur.u16(Endian::Big)?; // language（2 字节，读取并自动前进）
    let pos = cur.position();
    let rest = payload.get(pos..)?;
    let name_len = skip_loci_name(rest)?;
    cur.skip(name_len)?; // 跳过 name（含终止符）
    cur.skip(1)?; // role
    let lon_raw = cur.u32(Endian::Big)? as i32; // 3GPP loci: 经度(lon)先于纬度读出
    let lat_raw = cur.u32(Endian::Big)? as i32;
    let alt_raw = cur.u32(Endian::Big)? as i32;
    Some(Gps {
        lat_e7: fixed16_16_to_e7(lat_raw)?,
        lon_e7: fixed16_16_to_e7(lon_raw)?,
        alt_mm: fixed16_16_to_mm(alt_raw),
    })
}

/// udta ©-atom 的 FourCC → key 串：首字节 0xA9 映射为 '©'(U+00A9)，后 3 字节须 ASCII。
fn udta_key_string(kind: &[u8; 4]) -> Option<alloc::string::String> {
    if kind[0] != 0xA9 {
        return None;
    }
    let mut s = alloc::string::String::from("©");
    for &c in &kind[1..] {
        if !c.is_ascii() {
            return None;
        }
        s.push(c as char);
    }
    Some(s)
}

/// 解析 udta ©-atom 文本载荷：u16 size + u16 lang + text。越界/非 UTF-8 → None。
fn parse_udta_text(payload: &[u8]) -> Option<&str> {
    let size = u16::from_be_bytes(payload.get(0..2)?.try_into().ok()?) as usize;
    let text = payload.get(4..4 + size)?;
    core::str::from_utf8(text).ok()
}

/// 解析 `©xyz` 载荷：u16 size + u16 lang + ISO6709 文本。越界/非 UTF-8 → None。
/// 部分写入方 size 字段不可靠，取 size 失败时回退到「偏移 4 之后全部」。
fn parse_xyz(payload: &[u8]) -> Option<Gps> {
    if payload.len() < 4 {
        return None;
    }
    let size = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let text_bytes = payload.get(4..4 + size).or_else(|| payload.get(4..))?;
    let text = core::str::from_utf8(text_bytes).ok()?;
    parse_iso6709(text)
}

/// 解析 ISO 6709 串（©xyz / mdta location.ISO6709）→ Gps。
/// 形如 "+27.5916+086.5640+8850/"：按 +/- 切有符号十进制段 → ①纬 ②经 ③可选高(米)。
fn parse_iso6709(s: &str) -> Option<Gps> {
    let s = s.trim().trim_end_matches('/');
    // ISO 6709 串必须以符号字符起始；拒绝前缀垃圾（如 "foo+1.0+2.0"）。
    if !matches!(s.as_bytes().first(), Some(b'+' | b'-')) {
        return None;
    }
    let bytes = s.as_bytes();
    let mut fields: Vec<&str> = Vec::new();
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
    Some(Gps {
        lat_e7: lat,
        lon_e7: lon,
        alt_mm,
    })
}

/// QuickTime mdta 抽取产物。
struct QtMdta {
    gps: Option<Gps>,
    tags: alloc::vec::Vec<ContainerTag>,
}

/// 解析 QuickTime `moov/meta`（**非 FullBox** 容器）：hdlr(校验 mdta) + keys + ilst。
/// 任一缺失/畸形 → 对应字段 None，绝不 panic。
/// 注意：个别写入方（如 FCP7）在 moov/meta 前置 4 字节规范外 version/flags；
/// 本实现不兼容该情形，遇此则 mdta 字段静默为空（无警告）。
/// `max_tags`：out.tags 源头上限，防 DoS 放大。
fn parse_qt_mdta(meta_payload: &[u8], max_tags: usize) -> QtMdta {
    let mut out = QtMdta {
        gps: None,
        tags: alloc::vec::Vec::new(),
    };
    let mut keys: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
    let mut is_mdta = false;
    let mut ilst_payload: Option<&[u8]> = None;

    for (hdr, p) in iter_child_boxes(meta_payload) {
        match &hdr.kind {
            b"hdlr" => {
                // hdlr 载荷：version/flags(4) + pre_defined(4) + handler_type(4)
                if p.get(8..12).map(|s| s == b"mdta").unwrap_or(false) {
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
        let idx = u32::from_be_bytes(hdr.kind);
        if idx == 0 || (idx as usize) > keys.len() {
            continue;
        }
        let key = &keys[idx as usize - 1];
        let Some((type_code, value)) = qt_data_typed(item_payload) else {
            continue;
        };
        match key.as_str() {
            "com.apple.quicktime.location.ISO6709" => {
                if out.gps.is_none()
                    && let Ok(s) = core::str::from_utf8(value)
                {
                    out.gps = parse_iso6709(s);
                }
            }
            _ => {}
        }
        // raw 层：UTF-8 文本键（type==1）原样入 container；focal length 整数（type 21/22）→ U32。
        // 源头按 max_tags 封顶，防 DoS 放大。
        const DATA_UTF8: u32 = 1;
        const DATA_INT_SIGNED: u32 = 21;
        const DATA_INT_UNSIGNED: u32 = 22;
        if type_code == DATA_UTF8 {
            if out.tags.len() < max_tags
                && let Ok(s) = core::str::from_utf8(value)
            {
                out.tags.push(ContainerTag {
                    source: ContainerSource::QuickTimeMdta,
                    key: alloc::string::String::from(key.as_str()),
                    value: Value::Text(alloc::string::String::from(s)),
                });
            }
        } else if (type_code == DATA_INT_SIGNED || type_code == DATA_INT_UNSIGNED)
            && key.ends_with("focal_length.35mm_equivalent")
            && let Some(n) = be_uint_u32(value)
            && out.tags.len() < max_tags
        {
            out.tags.push(ContainerTag {
                source: ContainerSource::QuickTimeMdta,
                key: alloc::string::String::from(key.as_str()),
                value: Value::U32(n),
            });
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
            // 非法 UTF-8：压入空串占位以保 1-based 索引对齐；
            // 空串永不匹配任何目标键，在 ilst 阶段被安全跳过。
            Err(_) => out.push(alloc::string::String::new()),
        }
    }
    out
}

/// 大端无符整数（1/2/4 字节）→ u32；其它长度 → None。
fn be_uint_u32(b: &[u8]) -> Option<u32> {
    match b.len() {
        1 => Some(u32::from(b[0])),
        2 => Some(u32::from(u16::from_be_bytes(b.try_into().ok()?))),
        4 => Some(u32::from_be_bytes(b.try_into().ok()?)),
        _ => None,
    }
}

/// 从 ilst item 载荷取内层 `data` atom 的 (类型码, 值)。
/// data 载荷布局：type(4) + locale(4) + value。越界 → None。
fn qt_data_typed(item_payload: &[u8]) -> Option<(u32, &[u8])> {
    for (hdr, p) in iter_child_boxes(item_payload) {
        if &hdr.kind == b"data" {
            let type_code = u32::from_be_bytes(p.get(0..4)?.try_into().ok()?);
            let value = p.get(8..)?;
            return Some((type_code, value));
        }
    }
    None
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
    fn walk_skips_non_meta_box() {
        // 单次 pull：首盒 ftyp 非 meta → Skip(载荷=12)，消费头部 8。
        let buf = ftyp_box();
        let mut p = BmffParser::new();
        let res = p.pull(&buf);
        assert_eq!(res.demand, Demand::Skip(12));
        assert_eq!(res.consumed, 8);
        assert!(res.events.is_empty());
    }

    #[test]
    fn walk_empty_window_is_clean_done() {
        // 空窗口（驱动保证仅 EOF 出现）→ 干净 Done、无事件。
        let mut p = BmffParser::new();
        let res = p.pull(&[]);
        assert_eq!(res.demand, Demand::Done);
        assert!(res.events.is_empty());
    }

    #[test]
    fn drive_slice_lone_ftyp_is_clean() {
        // 仅 ftyp（无 meta）经 drive_slice 应干净收尾、无警告、无产物。
        let buf = ftyp_box();
        let col = crate::driver::drive_slice(
            &buf,
            &mut BmffParser::new(),
            crate::limits::Limits::default(),
        );
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
        assert!(col.exif.is_empty());
        assert!(col.xmp.is_empty());
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

    fn ispe(w: u32, h: u32) -> Vec<u8> {
        let mut p = alloc::vec![0u8, 0, 0, 0];
        p.extend_from_slice(&w.to_be_bytes());
        p.extend_from_slice(&h.to_be_bytes());
        box_bytes(b"ispe", &p)
    }

    #[test]
    fn parse_pitm_and_ispe() {
        let mut pitm_p = alloc::vec![0u8, 0, 0, 0];
        pitm_p.extend_from_slice(&7u16.to_be_bytes());
        assert_eq!(parse_pitm(&box_bytes(b"pitm", &pitm_p)[8..]), Some(7));
        assert_eq!(parse_ispe(&ispe(4032, 3024)[8..]), Some((4032, 3024)));
    }

    #[test]
    fn dims_from_iprp_via_ipma() {
        // ipco: [ispe 4032x3024]；ipma: item 1 → property #1
        let ipco = box_bytes(b"ipco", &ispe(4032, 3024));
        let mut ipma_p = alloc::vec![0u8, 0, 0, 0];
        ipma_p.extend_from_slice(&1u32.to_be_bytes()); // entry_count
        ipma_p.extend_from_slice(&1u16.to_be_bytes()); // item_id=1
        ipma_p.push(1); // assoc_count
        ipma_p.push(1); // 属性序号 1（essential bit 0）
        let ipma = box_bytes(b"ipma", &ipma_p);
        let mut iprp_p = Vec::new();
        iprp_p.extend_from_slice(&ipco);
        iprp_p.extend_from_slice(&ipma);
        assert_eq!(
            dims_from_iprp(&box_bytes(b"iprp", &iprp_p)[8..], Some(1)),
            Some((4032, 3024))
        );
    }

    #[test]
    fn dims_from_iprp_single_ispe_fallback() {
        // 无 ipma 关联，但 ipco 仅一个 ispe → 兜底直接用
        let ipco = box_bytes(b"ipco", &ispe(640, 480));
        assert_eq!(
            dims_from_iprp(&box_bytes(b"iprp", &ipco)[8..], None),
            Some((640, 480))
        );
    }

    #[test]
    fn strip_exif_prefix_zero_offset() {
        let mut d = alloc::vec![0u8, 0, 0, 0]; // tiff_header_offset = 0
        d.extend_from_slice(b"II*\0rest");
        assert_eq!(strip_exif_prefix(&d), b"II*\0rest");
    }

    #[test]
    fn strip_exif_prefix_tolerates_exif_marker() {
        let mut d = alloc::vec![0u8, 0, 0, 0];
        d.extend_from_slice(b"Exif\0\0MM\0*");
        assert_eq!(strip_exif_prefix(&d), b"MM\0*");
    }

    #[test]
    fn walk_meta_smaller_than_header_is_clean_done() {
        // 畸形 meta：size32=6 < 头部 8。绝不 panic，干净结束。
        let mut buf = Vec::new();
        buf.extend_from_slice(&6u32.to_be_bytes());
        buf.extend_from_slice(b"meta");
        buf.extend_from_slice(&[0u8; 8]); // 填充使 input >= 8
        let mut p = BmffParser::new();
        let res = p.pull(&buf);
        assert_eq!(res.demand, Demand::Done);
        assert!(res.events.is_empty());
    }

    #[test]
    fn drive_truncated_meta_warns_truncated() {
        // meta 声明 size=200，但实际只有 20 字节 → 解析器索要 200 字节，driver 到 EOF 记 Truncated。
        let mut buf = Vec::new();
        buf.extend_from_slice(&200u32.to_be_bytes()); // size=200
        buf.extend_from_slice(b"meta");
        buf.extend_from_slice(&[0u8; 12]); // 仅 12 字节载荷（合计 20 < 200）
        let col = crate::driver::drive_slice(
            &buf,
            &mut BmffParser::new(),
            crate::limits::Limits::default(),
        );
        assert_eq!(col.warnings.len(), 1);
        assert_eq!(col.warnings[0].kind, WarnKind::Truncated);
    }

    #[test]
    fn parse_meta_method2_warns_and_skips() {
        // iinf: Exif item id=1；iloc version1 method=2（item 间接引用，不支持）
        let mut iinf_p = alloc::vec![0u8, 0, 0, 0];
        iinf_p.extend_from_slice(&1u16.to_be_bytes());
        iinf_p.extend_from_slice(&infe(1, b"Exif", None));
        let iinf = box_bytes(b"iinf", &iinf_p);
        let mut iloc_p = alloc::vec![1u8, 0, 0, 0]; // version 1
        iloc_p.push(0x44);
        iloc_p.push(0x00);
        iloc_p.extend_from_slice(&1u16.to_be_bytes()); // item_count
        iloc_p.extend_from_slice(&1u16.to_be_bytes()); // id=1
        iloc_p.extend_from_slice(&2u16.to_be_bytes()); // construction_method=2
        iloc_p.extend_from_slice(&0u16.to_be_bytes()); // dri
        iloc_p.extend_from_slice(&1u16.to_be_bytes()); // extent_count
        iloc_p.extend_from_slice(&0u32.to_be_bytes()); // offset
        iloc_p.extend_from_slice(&4u32.to_be_bytes()); // length
        let iloc = box_bytes(b"iloc", &iloc_p);
        let mut meta_p = alloc::vec![0u8, 0, 0, 0]; // meta vf
        meta_p.extend_from_slice(&iinf);
        meta_p.extend_from_slice(&iloc);
        let plan = parse_meta(&meta_p, 0);
        assert!(plan.targets.is_empty());
        assert!(plan.inline.is_empty());
        assert_eq!(plan.warnings.len(), 1);
        assert_eq!(plan.warnings[0].kind, WarnKind::UnreachableSection);
    }

    /// 最小 TIFF：II + 42 + IFD0(Make=Acme)。与 driver/webp 测试同款。
    fn make_tiff() -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(b"II");
        t.extend_from_slice(&42u16.to_le_bytes());
        t.extend_from_slice(&8u32.to_le_bytes());
        t.extend_from_slice(&1u16.to_le_bytes()); // 1 entry
        t.extend_from_slice(&0x010Fu16.to_le_bytes()); // Make
        t.extend_from_slice(&2u16.to_le_bytes()); // ASCII
        t.extend_from_slice(&5u32.to_le_bytes()); // count
        t.extend_from_slice(&26u32.to_le_bytes()); // 值偏移
        t.extend_from_slice(&0u32.to_le_bytes()); // next IFD
        t.extend_from_slice(b"Acme\0");
        t
    }

    fn ftyp_heic() -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(b"heic");
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(b"mif1");
        box_bytes(b"ftyp", &p)
    }

    /// 构造 meta box（Exif=item1, xmp=item2, ispe 关联 item1）。method 0 时偏移为绝对值。
    fn build_meta_method0(exif_off: u64, exif_len: u64, xmp_off: u64, xmp_len: u64) -> Vec<u8> {
        let mut pitm_p = alloc::vec![0u8, 0, 0, 0];
        pitm_p.extend_from_slice(&1u16.to_be_bytes());
        let pitm = box_bytes(b"pitm", &pitm_p);

        let mut iinf_p = alloc::vec![0u8, 0, 0, 0];
        iinf_p.extend_from_slice(&2u16.to_be_bytes());
        iinf_p.extend_from_slice(&infe(1, b"Exif", None));
        iinf_p.extend_from_slice(&infe(2, b"mime", Some(b"application/rdf+xml")));
        let iinf = box_bytes(b"iinf", &iinf_p);

        let ipco = box_bytes(b"ipco", &ispe(4032, 3024));
        let mut ipma_p = alloc::vec![0u8, 0, 0, 0];
        ipma_p.extend_from_slice(&1u32.to_be_bytes());
        ipma_p.extend_from_slice(&1u16.to_be_bytes());
        ipma_p.push(1);
        ipma_p.push(1);
        let ipma = box_bytes(b"ipma", &ipma_p);
        let mut iprp_p = Vec::new();
        iprp_p.extend_from_slice(&ipco);
        iprp_p.extend_from_slice(&ipma);
        let iprp = box_bytes(b"iprp", &iprp_p);

        let mut iloc_p = alloc::vec![0u8, 0, 0, 0]; // version 0 → method 0
        iloc_p.push(0x44);
        iloc_p.push(0x00);
        iloc_p.extend_from_slice(&2u16.to_be_bytes());
        for (id, off, len) in [(1u16, exif_off, exif_len), (2u16, xmp_off, xmp_len)] {
            iloc_p.extend_from_slice(&id.to_be_bytes());
            iloc_p.extend_from_slice(&0u16.to_be_bytes()); // dri
            iloc_p.extend_from_slice(&1u16.to_be_bytes()); // extent_count
            iloc_p.extend_from_slice(&(off as u32).to_be_bytes());
            iloc_p.extend_from_slice(&(len as u32).to_be_bytes());
        }
        let iloc = box_bytes(b"iloc", &iloc_p);

        let mut meta_p = alloc::vec![0u8, 0, 0, 0];
        meta_p.extend_from_slice(&pitm);
        meta_p.extend_from_slice(&iinf);
        meta_p.extend_from_slice(&iprp);
        meta_p.extend_from_slice(&iloc);
        box_bytes(b"meta", &meta_p)
    }

    fn exif_item_block() -> Vec<u8> {
        let mut b = alloc::vec![0u8, 0, 0, 0]; // tiff_header_offset = 0
        b.extend_from_slice(&make_tiff());
        b
    }

    /// 完整 HEIC：ftyp + meta + mdat(exif, xmp)。两遍：先测 meta 长度，再算绝对偏移。
    fn heic_method0() -> Vec<u8> {
        let exif = exif_item_block();
        let xmp = br#"<rdf:Description tiff:Make="Acme"/>"#.to_vec();
        let ftyp = ftyp_heic();
        let meta_probe = build_meta_method0(0, exif.len() as u64, 0, xmp.len() as u64);
        let base = ftyp.len() as u64 + meta_probe.len() as u64 + 8; // mdat 头 8 字节
        let exif_off = base;
        let xmp_off = base + exif.len() as u64;
        let meta = build_meta_method0(exif_off, exif.len() as u64, xmp_off, xmp.len() as u64);
        assert_eq!(meta.len(), meta_probe.len(), "两遍 meta 长度必须一致");
        let mut mdat_payload = Vec::new();
        mdat_payload.extend_from_slice(&exif);
        mdat_payload.extend_from_slice(&xmp);
        let mdat = box_bytes(b"mdat", &mdat_payload);
        let mut f = Vec::new();
        f.extend_from_slice(&ftyp);
        f.extend_from_slice(&meta);
        f.extend_from_slice(&mdat);
        f
    }

    #[test]
    fn end_to_end_heic_method0() {
        let buf = heic_method0();
        let col = crate::driver::drive_slice(
            &buf,
            &mut BmffParser::new(),
            crate::limits::Limits::default(),
        );
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Heif);
        assert!(meta.warnings.is_empty(), "warnings: {:?}", meta.warnings);
        assert_eq!(meta.unified.width, Some(4032));
        assert_eq!(meta.unified.height, Some(3024));
        assert!(
            meta.raw.exif.iter().any(|t| t.tag == 0x010F),
            "应抽到 Make 标签"
        );
        assert!(
            meta.raw
                .xmp
                .iter()
                .any(|x| x.name == "Make" && x.value == "Acme")
        );
        assert_eq!(
            meta.unified.camera_make.as_deref(),
            Some("Acme"),
            "unified.camera_make 须经 normalize 从 EXIF IFD0 Make 投影"
        );
    }

    /// 构造 mvhd 载荷（box 头之后的字节），version 0。
    fn mvhd_v0(creation: u32, timescale: u32, duration: u32) -> Vec<u8> {
        let mut p = alloc::vec![0u8, 0, 0, 0]; // version 0, flags 0
        p.extend_from_slice(&creation.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes()); // modification_time
        p.extend_from_slice(&timescale.to_be_bytes());
        p.extend_from_slice(&duration.to_be_bytes());
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
        assert!(!m.timescale_invalid);
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
        assert!(m.timescale_invalid);
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

    /// trak{ tkhd }。
    fn trak(tkhd_payload: &[u8]) -> Vec<u8> {
        box_bytes(b"trak", &box_bytes(b"tkhd", tkhd_payload))
    }

    #[test]
    fn parse_moov_picks_video_track_and_time() {
        // moov{ mvhd, trak(audio 0×0), trak(video 1920×1080) }
        let mut moov_p = Vec::new();
        moov_p.extend_from_slice(&box_bytes(b"mvhd", &mvhd_v0(2_082_844_800, 600, 900_900)));
        moov_p.extend_from_slice(&trak(&tkhd_v0(0, 0))); // 音频轨先出现
        moov_p.extend_from_slice(&trak(&tkhd_v0(1920, 1080))); // 视频轨
        let info = parse_moov(&moov_p, 0, usize::MAX);
        assert_eq!(info.dims, Some((1920, 1080))); // 跳过 0×0，选视频
        assert_eq!(info.duration_ms, Some(1_501_500));
        assert_eq!(info.created.map(|d| d.year), Some(1970));
        assert!(info.warnings.is_empty());
    }

    #[test]
    fn parse_moov_timescale_zero_warns() {
        let mut moov_p = Vec::new();
        moov_p.extend_from_slice(&box_bytes(b"mvhd", &mvhd_v0(0, 0, 1000)));
        let info = parse_moov(&moov_p, 0, usize::MAX);
        assert_eq!(info.duration_ms, None);
        assert_eq!(info.warnings.len(), 1);
        assert_eq!(info.warnings[0].kind, WarnKind::UnrecognizedValue);
    }

    #[test]
    fn parse_moov_no_mvhd_no_trak_is_empty() {
        let info = parse_moov(&box_bytes(b"free", &[0u8; 4]), 0, usize::MAX);
        assert_eq!(info.dims, None);
        assert_eq!(info.duration_ms, None);
        assert_eq!(info.created, None);
        assert!(info.warnings.is_empty());
    }

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
        let col = crate::driver::drive_slice(
            &buf,
            &mut BmffParser::new(),
            crate::limits::Limits::default(),
        );
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
        let col = crate::driver::drive_slice(
            &f,
            &mut BmffParser::new(),
            crate::limits::Limits::default(),
        );
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Mp4);
        assert_eq!(meta.unified.width, Some(640));
        assert_eq!(meta.unified.duration_ms, Some(2000));
        assert_eq!(meta.unified.created, None); // creation_time==0
    }

    #[test]
    fn end_to_end_heic_idat_method1() {
        // meta 内嵌 idat：Exif item 数据放 idat，construction_method=1。
        let exif = exif_item_block();
        let pitm = {
            let mut p = alloc::vec![0u8, 0, 0, 0];
            p.extend_from_slice(&1u16.to_be_bytes());
            box_bytes(b"pitm", &p)
        };
        let mut iinf_p = alloc::vec![0u8, 0, 0, 0];
        iinf_p.extend_from_slice(&1u16.to_be_bytes());
        iinf_p.extend_from_slice(&infe(1, b"Exif", None));
        let iinf = box_bytes(b"iinf", &iinf_p);
        let idat = box_bytes(b"idat", &exif);
        let mut iloc_p = alloc::vec![1u8, 0, 0, 0]; // version 1（带 method）
        iloc_p.push(0x44);
        iloc_p.push(0x00);
        iloc_p.extend_from_slice(&1u16.to_be_bytes()); // item_count
        iloc_p.extend_from_slice(&1u16.to_be_bytes()); // id=1
        iloc_p.extend_from_slice(&1u16.to_be_bytes()); // method=1
        iloc_p.extend_from_slice(&0u16.to_be_bytes()); // dri
        iloc_p.extend_from_slice(&1u16.to_be_bytes()); // extent_count
        iloc_p.extend_from_slice(&0u32.to_be_bytes()); // idat 内偏移 0
        iloc_p.extend_from_slice(&(exif.len() as u32).to_be_bytes()); // 长度(含 4 字节 tiff_header_offset 前缀, parse_meta 内剥离)
        let iloc = box_bytes(b"iloc", &iloc_p);
        let mut meta_p = alloc::vec![0u8, 0, 0, 0];
        meta_p.extend_from_slice(&pitm);
        meta_p.extend_from_slice(&iinf);
        meta_p.extend_from_slice(&idat);
        meta_p.extend_from_slice(&iloc);
        let meta = box_bytes(b"meta", &meta_p);
        let mut f = ftyp_heic();
        f.extend_from_slice(&meta);
        let col = crate::driver::drive_slice(
            &f,
            &mut BmffParser::new(),
            crate::limits::Limits::default(),
        );
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Heif);
        assert!(meta.warnings.is_empty(), "warnings: {:?}", meta.warnings);
        assert!(
            meta.raw.exif.iter().any(|t| t.tag == 0x010F),
            "idat 内联 Exif 应被抽到"
        );
        assert_eq!(
            meta.unified.camera_make.as_deref(),
            Some("Acme"),
            "idat 路径 EXIF 同样须经 normalize 投影至 unified"
        );
    }

    #[test]
    fn drive_truncated_moov_warns_truncated() {
        // moov 声明 size=300 但实际仅 20 字节 → driver 到 EOF 记 Truncated，不 panic。
        let mut buf = ftyp_mp4();
        let mut moov = Vec::new();
        moov.extend_from_slice(&300u32.to_be_bytes());
        moov.extend_from_slice(b"moov");
        moov.extend_from_slice(&[0u8; 12]);
        buf.extend_from_slice(&moov);
        let col = crate::driver::drive_slice(
            &buf,
            &mut BmffParser::new(),
            crate::limits::Limits::default(),
        );
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
        let info = parse_moov(&moov_p, 0, usize::MAX);
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
        let col = crate::driver::drive_slice(
            &buf,
            &mut BmffParser::new(),
            crate::limits::Limits::default(),
        );
        assert!(col.warnings.iter().any(|w| w.kind == WarnKind::Truncated));
    }

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
        assert_eq!(parse_iso6709("foo+1.0+2.0"), None); // 前缀垃圾必须拒绝
    }

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

    #[test]
    fn parse_loci_lon_first_16_16() {
        // loci FullBox: ver(1)+flags(3) + lang(2) + name("\0") + role(1)
        //   + lon(16.16) + lat(16.16) + alt(16.16)。注意经在前。
        let mut p = alloc::vec::Vec::new();
        p.extend_from_slice(&[0u8, 0, 0, 0]); // version/flags
        p.extend_from_slice(&0x15c7u16.to_be_bytes()); // language
        p.push(0); // name 空串（null 终止）
        p.push(0); // role
        let lon_fixed = (86.5640f64 * 65536.0) as i32;
        let lat_fixed = (27.5916f64 * 65536.0) as i32;
        let alt_fixed = (8850.0f64 * 65536.0) as i32;
        p.extend_from_slice(&lon_fixed.to_be_bytes());
        p.extend_from_slice(&lat_fixed.to_be_bytes());
        p.extend_from_slice(&alt_fixed.to_be_bytes());
        let g = parse_loci(&p).expect("gps");
        assert!(
            (g.lat_e7 - 275_916_000).abs() <= 20_000,
            "lat_e7={}",
            g.lat_e7
        );
        assert!(
            (g.lon_e7 - 865_640_000).abs() <= 20_000,
            "lon_e7={}",
            g.lon_e7
        );
        assert!((g.alt_mm.unwrap() - 8_850_000).abs() <= 20_000);
    }

    #[test]
    fn parse_loci_truncated_is_none() {
        assert_eq!(parse_loci(&[0u8, 0, 0, 0, 0, 0]), None);
    }

    /// 构造 QuickTime mdta meta：hdlr(mdta) + keys(键表) + ilst(索引→data)。
    /// data atom: type(4)+locale(4)+payload。
    fn qt_meta_with_keys(keys_and_vals: &[(&str, &[u8])]) -> alloc::vec::Vec<u8> {
        let mut hdlr = alloc::vec::Vec::new();
        hdlr.extend_from_slice(&[0u8; 8]); // version/flags + pre_defined
        hdlr.extend_from_slice(b"mdta"); // handler_type
        hdlr.extend_from_slice(&[0u8; 12]); // reserved(3*4)
        hdlr.push(0); // name 空

        let mut keys = alloc::vec::Vec::new();
        keys.extend_from_slice(&[0u8; 4]); // version/flags
        keys.extend_from_slice(&(keys_and_vals.len() as u32).to_be_bytes()); // entry_count
        for (k, _) in keys_and_vals {
            let entry_size = 8 + k.len();
            keys.extend_from_slice(&(entry_size as u32).to_be_bytes());
            keys.extend_from_slice(b"mdta"); // namespace
            keys.extend_from_slice(k.as_bytes());
        }

        let mut ilst = alloc::vec::Vec::new();
        for (i, (_, v)) in keys_and_vals.iter().enumerate() {
            let idx = (i as u32) + 1;
            let mut data = alloc::vec::Vec::new();
            data.extend_from_slice(&1u32.to_be_bytes()); // type=1 (DATA_UTF8)
            data.extend_from_slice(&[0u8; 4]); // locale
            data.extend_from_slice(v);
            let data_box = box_bytes(b"data", &data);
            let mut item = alloc::vec::Vec::new();
            item.extend_from_slice(&data_box);
            let mut item_box = alloc::vec::Vec::new();
            item_box.extend_from_slice(&((8 + item.len()) as u32).to_be_bytes());
            item_box.extend_from_slice(&idx.to_be_bytes()); // box "kind" = 索引
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
            (
                "com.apple.quicktime.location.ISO6709",
                b"+27.5916+086.5640+8850/",
            ),
            ("com.apple.quicktime.make", b"Apple"),
            ("com.apple.quicktime.model", b"iPhone 15"),
            (
                "com.apple.quicktime.creationdate",
                b"2017-07-22T16:06:06+10:00",
            ),
        ]);
        let out = parse_qt_mdta(&meta, usize::MAX);
        let g = out.gps.expect("gps");
        assert!((g.lat_e7 - 275_916_000).abs() <= 2);
        // make/model 不再是 QtMdta 的独立字段，而是经 tags → normalize 投影
        assert!(
            out.tags.iter().any(|t| t.key == "com.apple.quicktime.make"
                && matches!(&t.value, crate::model::Value::Text(s) if s == "Apple")),
            "make 须在 tags 中"
        );
        assert!(
            out.tags.iter().any(|t| t.key == "com.apple.quicktime.model"
                && matches!(&t.value, crate::model::Value::Text(s) if s == "iPhone 15")),
            "model 须在 tags 中"
        );
        // creationdate 不再由 QtMdta 语义解析；仍经 tags(type=1 UTF-8)进入 container → normalize 读取。
        assert!(
            out.tags.iter().any(|t| t.key == "com.apple.quicktime.creationdate"
                && matches!(&t.value, crate::model::Value::Text(s) if s == "2017-07-22T16:06:06+10:00")),
            "creationdate 须在 tags 中供 normalize 读取"
        );
    }

    #[test]
    fn parse_qt_meta_non_mdta_handler_is_empty() {
        let mut hdlr = alloc::vec::Vec::new();
        hdlr.extend_from_slice(&[0u8; 8]);
        hdlr.extend_from_slice(b"vide");
        hdlr.extend_from_slice(&[0u8; 12]);
        let meta = box_bytes(b"hdlr", &hdlr);
        let out = parse_qt_mdta(&meta, usize::MAX);
        assert!(out.gps.is_none() && out.tags.is_empty());
    }

    #[test]
    fn parse_moov_xyz_beats_loci_for_gps() {
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
        let info = parse_moov(&moov_p, 0, usize::MAX);
        let g = info.gps.expect("gps");
        assert_eq!(g.lat_e7, 100_000_000); // ©xyz 的 10°，非 loci 的 60°
    }

    #[test]
    fn parse_moov_mdta_creationdate_captured_in_container_tags() {
        // 新契约：parser 不再语义解析 mdta creationdate（info.created 仅来自 mvhd）；
        // creationdate 作为 UTF-8 文本留在 container_tags，由 normalize 读取并排首位。
        let meta = qt_meta_with_keys(&[
            ("com.apple.quicktime.creationdate", b"2017-07-22T16:06:06Z"),
            ("com.apple.quicktime.make", b"Apple"),
        ]);
        let mut moov_p = alloc::vec::Vec::new();
        moov_p.extend_from_slice(&box_bytes(b"mvhd", &mvhd_v0(2_082_844_800, 600, 600)));
        moov_p.extend_from_slice(&box_bytes(b"meta", &meta));
        let info = parse_moov(&moov_p, 0, usize::MAX);
        // info.created 现在是 mvhd 值（1970），mdta 不再直接影响 parser 层 created。
        assert_eq!(info.created.map(|d| d.year), Some(1970));
        // mdta creationdate 须存在于 container_tags，供 normalize 读取后排在 mvhd 之上。
        assert!(
            info.container_tags
                .iter()
                .any(|t| t.key == "com.apple.quicktime.creationdate"
                    && matches!(&t.value, crate::model::Value::Text(s) if s == "2017-07-22T16:06:06Z")),
            "creationdate 须保留在 container_tags 供 normalize 读取"
        );
        // make/model 不再是 MoovInfo 的直接字段，而是经 container_tags → normalize 投影
        assert!(
            info.container_tags
                .iter()
                .any(|t| t.key == "com.apple.quicktime.make"
                    && matches!(&t.value, crate::model::Value::Text(s) if s == "Apple")),
            "make 须保留在 container_tags 供 normalize 读取，且值须为 Apple"
        );
    }

    #[test]
    fn end_to_end_mov_mdta_gps_make() {
        // 文件：ftyp + moov{ mvhd, udta{©xyz}, meta{mdta make/model} }
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

        let mut f = ftyp_mp4();
        f.extend_from_slice(&box_bytes(b"moov", &moov_p));

        let col = crate::driver::drive_slice(
            &f,
            &mut BmffParser::new(),
            crate::limits::Limits::default(),
        );
        let meta_out = crate::driver::finalize(col, crate::model::FileFormat::Mov);
        assert_eq!(meta_out.unified.gps.map(|g| g.lat_e7), Some(350_000_000));
        assert_eq!(meta_out.unified.camera_make.as_deref(), Some("Apple"));
        assert_eq!(meta_out.unified.camera_model.as_deref(), Some("iPhone 15"));
    }

    #[test]
    fn qt_data_typed_returns_type_and_value() {
        // data atom: type(4)=1(UTF-8) + locale(4) + "hi"
        let mut data = alloc::vec::Vec::new();
        data.extend_from_slice(&1u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(b"hi");
        let item = box_bytes(b"data", &data);
        let (ty, val) = qt_data_typed(&item).expect("data");
        assert_eq!(ty, 1);
        assert_eq!(val, b"hi");
    }

    /// 同 qt_meta_with_keys，但每键带 data 类型码。
    fn qt_meta_with_typed_keys(items: &[(&str, u32, &[u8])]) -> alloc::vec::Vec<u8> {
        let mut hdlr = alloc::vec::Vec::new();
        hdlr.extend_from_slice(&[0u8; 8]);
        hdlr.extend_from_slice(b"mdta");
        hdlr.extend_from_slice(&[0u8; 12]);
        hdlr.push(0);

        let mut keys = alloc::vec::Vec::new();
        keys.extend_from_slice(&[0u8; 4]);
        keys.extend_from_slice(&(items.len() as u32).to_be_bytes());
        for (k, _, _) in items {
            let entry_size = 8 + k.len();
            keys.extend_from_slice(&(entry_size as u32).to_be_bytes());
            keys.extend_from_slice(b"mdta");
            keys.extend_from_slice(k.as_bytes());
        }

        let mut ilst = alloc::vec::Vec::new();
        for (i, (_, ty, v)) in items.iter().enumerate() {
            let idx = (i as u32) + 1;
            let mut data = alloc::vec::Vec::new();
            data.extend_from_slice(&ty.to_be_bytes());
            data.extend_from_slice(&0u32.to_be_bytes()); // locale
            data.extend_from_slice(v);
            let data_box = box_bytes(b"data", &data);
            let mut item_box = alloc::vec::Vec::new();
            item_box.extend_from_slice(&((8 + data_box.len()) as u32).to_be_bytes());
            item_box.extend_from_slice(&idx.to_be_bytes());
            item_box.extend_from_slice(&data_box);
            ilst.extend_from_slice(&item_box);
        }

        let mut meta = alloc::vec::Vec::new();
        meta.extend_from_slice(&box_bytes(b"hdlr", &hdlr));
        meta.extend_from_slice(&box_bytes(b"keys", &keys));
        meta.extend_from_slice(&box_bytes(b"ilst", &ilst));
        meta
    }

    #[test]
    fn parse_qt_mdta_captures_text_and_focal_length_tags() {
        use crate::model::{ContainerSource, Value};
        let meta = qt_meta_with_typed_keys(&[
            ("com.apple.quicktime.software", 1, b"13.5.1"),
            ("com.apple.quicktime.author", 1, b"Jane"),
            (
                "com.apple.quicktime.camera.focal_length.35mm_equivalent",
                22,
                &28u32.to_be_bytes(),
            ),
            ("com.apple.quicktime.junkbinary", 13, &[0xFF, 0xD8, 0xFF]), // JPEG 类型 → 跳过
        ]);
        let out = parse_qt_mdta(&meta, usize::MAX);
        let find = |k: &str| out.tags.iter().find(|t| t.key == k);
        assert!(
            matches!(find("com.apple.quicktime.software").map(|t| &t.value),
            Some(Value::Text(s)) if s == "13.5.1")
        );
        assert!(
            matches!(find("com.apple.quicktime.author").map(|t| &t.value),
            Some(Value::Text(s)) if s == "Jane")
        );
        assert!(matches!(
            find("com.apple.quicktime.camera.focal_length.35mm_equivalent").map(|t| &t.value),
            Some(Value::U32(28))
        ));
        assert!(
            find("com.apple.quicktime.junkbinary").is_none(),
            "二进制类型不收"
        );
        assert!(
            out.tags
                .iter()
                .all(|t| t.source == ContainerSource::QuickTimeMdta)
        );
    }

    #[test]
    fn parse_moov_collects_udta_and_mdta_container_tags() {
        use crate::model::{ContainerSource, Value};
        // udta { ©swr="MyCam 1.0" }
        let swr_text = b"MyCam 1.0";
        let mut swr_payload = alloc::vec::Vec::new();
        swr_payload.extend_from_slice(&(swr_text.len() as u16).to_be_bytes());
        swr_payload.extend_from_slice(&0u16.to_be_bytes()); // lang
        swr_payload.extend_from_slice(swr_text);
        let udta = box_bytes(b"\xA9swr", &swr_payload);

        // meta { mdta software }
        let meta = qt_meta_with_typed_keys(&[("com.apple.quicktime.software", 1, b"13.5.1")]);

        let mut moov_p = alloc::vec::Vec::new();
        moov_p.extend_from_slice(&box_bytes(b"udta", &udta));
        moov_p.extend_from_slice(&box_bytes(b"meta", &meta));
        let info = parse_moov(&moov_p, 0, usize::MAX);

        let find = |src: ContainerSource, k: &str| {
            info.container_tags
                .iter()
                .find(|t| t.source == src && t.key == k)
        };
        assert!(
            matches!(find(ContainerSource::Udta, "©swr").map(|t| &t.value),
            Some(Value::Text(s)) if s == "MyCam 1.0")
        );
        assert!(
            matches!(find(ContainerSource::QuickTimeMdta, "com.apple.quicktime.software").map(|t| &t.value),
            Some(Value::Text(s)) if s == "13.5.1")
        );
    }

    #[test]
    fn end_to_end_mov_container_tags_reach_raw() {
        let meta = qt_meta_with_typed_keys(&[("com.apple.quicktime.software", 1, b"13.5.1")]);
        let mut moov_p = alloc::vec::Vec::new();
        moov_p.extend_from_slice(&box_bytes(b"meta", &meta));
        let mut f = ftyp_mp4();
        f.extend_from_slice(&box_bytes(b"moov", &moov_p));

        let col = crate::driver::drive_slice(
            &f,
            &mut BmffParser::new(),
            crate::limits::Limits::default(),
        );
        let meta_out = crate::driver::finalize(col, crate::model::FileFormat::Mov);
        assert!(
            meta_out
                .raw
                .container
                .iter()
                .any(|t| t.key == "com.apple.quicktime.software"
                    && matches!(&t.value, crate::model::Value::Text(s) if s == "13.5.1"))
        );
    }

    // ── Finding #2: ©xyz 不得泄漏进 container_tags ──────────────────────────
    #[test]
    fn parse_moov_second_xyz_not_captured_into_container() {
        use crate::model::ContainerSource;
        // 两个 ©xyz：第一个喂 GPS，第二个不得泄漏进 container。
        let mk_xyz = |t: &[u8]| {
            let mut p = alloc::vec::Vec::new();
            p.extend_from_slice(&(t.len() as u16).to_be_bytes());
            p.extend_from_slice(&0u16.to_be_bytes());
            p.extend_from_slice(t);
            p
        };
        let mut udta = alloc::vec::Vec::new();
        udta.extend_from_slice(&box_bytes(b"\xA9xyz", &mk_xyz(b"+10.0000+020.0000/")));
        udta.extend_from_slice(&box_bytes(b"\xA9xyz", &mk_xyz(b"+30.0000+040.0000/")));
        let mut moov_p = alloc::vec::Vec::new();
        moov_p.extend_from_slice(&box_bytes(b"udta", &udta));
        let info = parse_moov(&moov_p, 0, usize::MAX);
        assert!(
            info.container_tags
                .iter()
                .all(|t| !(t.source == ContainerSource::Udta && t.key == "©xyz")),
            "©xyz 不得进入 container_tags"
        );
    }

    // ── Finding #1: source-cap tests ─────────────────────────────────────────
    #[test]
    fn parse_qt_mdta_caps_tags_at_budget() {
        // 10 个文本键，预算 3 → out.tags 不超过 3。
        let items: alloc::vec::Vec<(alloc::string::String, u32, &[u8])> = (0..10u32)
            .map(|i| {
                (
                    alloc::format!("com.apple.quicktime.k{i}"),
                    1u32,
                    b"v" as &[u8],
                )
            })
            .collect();
        let refs: alloc::vec::Vec<(&str, u32, &[u8])> =
            items.iter().map(|(k, t, v)| (k.as_str(), *t, *v)).collect();
        let meta = qt_meta_with_typed_keys(&refs);
        let out = parse_qt_mdta(&meta, 3);
        assert!(
            out.tags.len() <= 3,
            "源头封顶：out.tags={} 应 ≤ 3",
            out.tags.len()
        );
    }

    #[test]
    fn parse_moov_caps_container_tags_at_budget() {
        // udta 多个 ©-atom + mdta 多个键，预算很小 → 峰值受限。
        let mk = |t: &[u8]| {
            let mut p = alloc::vec::Vec::new();
            p.extend_from_slice(&(t.len() as u16).to_be_bytes());
            p.extend_from_slice(&0u16.to_be_bytes());
            p.extend_from_slice(t);
            p
        };
        let mut udta = alloc::vec::Vec::new();
        for fourcc in [b"\xA9nam", b"\xA9cmt", b"\xA9day", b"\xA9too"] {
            udta.extend_from_slice(&box_bytes(fourcc, &mk(b"x")));
        }
        let mut moov_p = alloc::vec::Vec::new();
        moov_p.extend_from_slice(&box_bytes(b"udta", &udta));
        let info = parse_moov(&moov_p, 0, 2);
        assert!(
            info.container_tags.len() <= 2,
            "udta 源头封顶 ≤ 2，实际 {}",
            info.container_tags.len()
        );
    }

    #[test]
    fn parse_moov_multi_meta_bounded_by_budget() {
        // 多个 meta 兄弟盒，每个含多个文本键；预算 3 → 跨盒累积仍 ≤ 3（峰值不随 meta 盒数放大）。
        let mut moov_p = alloc::vec::Vec::new();
        for _ in 0..4 {
            let meta = qt_meta_with_typed_keys(&[
                ("com.apple.quicktime.a", 1, b"x"),
                ("com.apple.quicktime.b", 1, b"y"),
            ]);
            moov_p.extend_from_slice(&box_bytes(b"meta", &meta));
        }
        let info = parse_moov(&moov_p, 0, 3);
        assert!(
            info.container_tags.len() <= 3,
            "跨多 meta 盒仍受预算限，实际 {}",
            info.container_tags.len()
        );
    }
}
