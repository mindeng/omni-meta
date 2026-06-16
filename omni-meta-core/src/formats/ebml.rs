//! EBML（Matroska/WebM）顶层解析器。前向走盒：跳过 EBML 头与不关心元素、
//! 下钻 Segment（不缓冲）、整元素缓冲解析 Info/Tracks、遇未知大小媒体即干净停止。

use alloc::vec::Vec;

use crate::containers::ebml::{iter_child_elements, needed_header_bytes, read_element_header, read_float, read_int, read_uint, ElemHeader};
use crate::demand::{Demand, Event, MetaParser, PullResult};
use crate::model::{DateTimeParts, Field, WarnKind, Warning};

/// Matroska DateUTC 纪元（2001-01-01）相对 Unix 纪元（1970-01-01）的天数差。
const MATROSKA_EPOCH_DAYS_AFTER_UNIX: i64 = 11_323;

/// Matroska `DateUTC`（自 2001-01-01 00:00:00 UTC 的纳秒，有符号）→ DateTimeParts（UTC）。
fn datetime_from_matroska_epoch(date_ns: i64) -> DateTimeParts {
    let secs = date_ns.div_euclid(1_000_000_000);
    let days = secs.div_euclid(86_400) + MATROSKA_EPOCH_DAYS_AFTER_UNIX;
    let tod = secs.rem_euclid(86_400) as u32;
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

// EBML / Matroska 元素 ID（保留标记位的规范值）。
const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_A966;
const TIMESTAMP_SCALE: u32 = 0x2AD7_B1; // 旧名 TimecodeScale，同 ID
const DURATION: u32 = 0x4489;
const DATE_UTC: u32 = 0x4461;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const VIDEO: u32 = 0xE0;
const PIXEL_WIDTH: u32 = 0xB0;
const PIXEL_HEIGHT: u32 = 0xBA;

/// `Info` 解析产物。`invalid` 标记 Duration 存在但不可用（非有限/负/scale=0/溢出）。
struct InfoData {
    duration_ms: Option<u64>,
    created: Option<DateTimeParts>,
    invalid: bool,
}

/// 解析 `Info` 载荷 → 时长 + 创建时间。
fn parse_info(payload: &[u8]) -> InfoData {
    let mut scale: Option<u64> = None;
    let mut duration_raw: Option<f64> = None;
    let mut date_ns: Option<i64> = None;
    for (hdr, p) in iter_child_elements(payload) {
        match hdr.id {
            TIMESTAMP_SCALE => scale = Some(read_uint(p)),
            DURATION => duration_raw = read_float(p),
            DATE_UTC => date_ns = Some(read_int(p)),
            _ => {}
        }
    }
    let mut out = InfoData { duration_ms: None, created: None, invalid: false };
    let scale = scale.unwrap_or(1_000_000);
    if let Some(d) = duration_raw {
        if scale == 0 || !d.is_finite() || d < 0.0 {
            out.invalid = true;
        } else {
            let ms = d * scale as f64 / 1_000_000.0;
            if ms < 0.0 || ms > u64::MAX as f64 {
                out.invalid = true;
            } else {
                out.duration_ms = Some(ms as u64);
            }
        }
    }
    if let Some(ns) = date_ns {
        out.created = Some(datetime_from_matroska_epoch(ns));
    }
    out
}

/// 解析 `Tracks` 载荷 → 首个含非零 PixelWidth/Height 的视频轨维度。
fn parse_tracks(payload: &[u8]) -> Option<(u32, u32)> {
    for (hdr, p) in iter_child_elements(payload) {
        if hdr.id != TRACK_ENTRY {
            continue;
        }
        if let Some(dims) = track_entry_dims(p) {
            return Some(dims);
        }
    }
    None
}

/// 在一个 `TrackEntry` 内找 `Video` → (PixelWidth, PixelHeight)，任一为 0 / 缺失 → None。
fn track_entry_dims(payload: &[u8]) -> Option<(u32, u32)> {
    for (hdr, p) in iter_child_elements(payload) {
        if hdr.id != VIDEO {
            continue;
        }
        let mut w: Option<u32> = None;
        let mut h: Option<u32> = None;
        for (vh, vp) in iter_child_elements(p) {
            match vh.id {
                PIXEL_WIDTH => w = Some(read_uint(vp) as u32),
                PIXEL_HEIGHT => h = Some(read_uint(vp) as u32),
                _ => {}
            }
        }
        if let (Some(w), Some(h)) = (w, h)
            && w != 0
            && h != 0
        {
            return Some((w, h));
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    TopLevel,
    InSegment,
}

#[derive(Debug)]
pub struct EbmlParser {
    done: bool,
    phase: Phase,
    got_info: bool,
    got_tracks: bool,
    /// 当前待读元素的绝对偏移，仅用于警告偏移保真。
    pos: u64,
}

impl Default for EbmlParser {
    fn default() -> Self {
        Self { done: false, phase: Phase::TopLevel, got_info: false, got_tracks: false, pos: 0 }
    }
}

impl EbmlParser {
    pub fn new() -> Self {
        Self::default()
    }
}

fn done_result<'a>() -> PullResult<'a> {
    PullResult { demand: Demand::Done, consumed: 0, events: Vec::new() }
}

fn need_result<'a>(n: usize) -> PullResult<'a> {
    PullResult { demand: Demand::NeedBytes(n), consumed: 0, events: Vec::new() }
}

impl MetaParser for EbmlParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        if self.done {
            return done_result();
        }
        if input.is_empty() {
            self.done = true; // 空窗口（驱动保证仅 EOF 出现）= 干净结束
            return done_result();
        }
        let hdr = match read_element_header(input) {
            Some(h) => h,
            None => {
                let need = needed_header_bytes(input);
                if input.len() >= need {
                    // 字节已够却仍读不出头 → 畸形，干净结束（防卡死）。
                    self.done = true;
                    return done_result();
                }
                return need_result(need);
            }
        };
        let header_len = hdr.header_len as usize;
        match self.phase {
            Phase::TopLevel => self.step_top(&hdr, header_len),
            Phase::InSegment => self.step_segment(input, &hdr, header_len),
        }
    }
}

impl EbmlParser {
    /// 顶层：下钻 Segment（仅消费其头部，不缓冲）；其它元素跳过整体。
    fn step_top<'a>(&mut self, hdr: &ElemHeader, header_len: usize) -> PullResult<'a> {
        if hdr.id == SEGMENT {
            self.phase = Phase::InSegment;
            self.pos = self.pos.saturating_add(header_len as u64);
            // 仅消费 Segment 头，索要首个子元素头（最小 2 字节）。
            return PullResult { demand: Demand::NeedBytes(2), consumed: header_len, events: Vec::new() };
        }
        match hdr.size {
            Some(sz) => {
                self.pos = self.pos.saturating_add(header_len as u64).saturating_add(sz);
                PullResult { demand: Demand::Skip(sz), consumed: header_len, events: Vec::new() }
            }
            None => {
                // 未知大小且非 Segment → 不可能再有 Segment，干净结束。
                self.done = true;
                done_result()
            }
        }
    }

    /// Segment 内：缓冲并解析 Info/Tracks；跳过定长不关心元素；遇未知大小媒体即停止。
    fn step_segment<'a>(&mut self, input: &'a [u8], hdr: &ElemHeader, header_len: usize) -> PullResult<'a> {
        let sz = match hdr.size {
            Some(s) => s,
            None => {
                // 未知大小媒体（如直播 Cluster）。
                self.done = true;
                if self.got_info && self.got_tracks {
                    return done_result();
                }
                let events = alloc::vec![Event::Warning(Warning {
                    offset: self.pos,
                    kind: WarnKind::UnreachableSection,
                })];
                return PullResult { demand: Demand::Done, consumed: 0, events };
            }
        };
        let wanted = hdr.id == INFO || hdr.id == TRACKS;
        if !wanted {
            self.pos = self.pos.saturating_add(header_len as u64).saturating_add(sz);
            return PullResult { demand: Demand::Skip(sz), consumed: header_len, events: Vec::new() };
        }
        // 关心的元素：须整元素入窗。
        let total = match usize::try_from(sz).ok().and_then(|s| header_len.checked_add(s)) {
            Some(t) => t,
            None => {
                self.done = true;
                return done_result();
            }
        };
        if input.len() < total {
            return need_result(total); // 不足 → 索要整元素（slice 下即截断；stream 下补字节）
        }
        let payload = &input[header_len..total];
        let mut events: Vec<Event<'a>> = Vec::new();
        if hdr.id == INFO {
            let info = parse_info(payload);
            if let Some(ms) = info.duration_ms {
                events.push(Event::Field(Field::Duration(ms)));
            }
            if let Some(dt) = info.created {
                events.push(Event::Field(Field::Created(dt)));
            }
            if info.invalid {
                events.push(Event::Warning(Warning { offset: self.pos, kind: WarnKind::UnrecognizedValue }));
            }
            self.got_info = true;
        } else {
            if let Some((w, h)) = parse_tracks(payload) {
                events.push(Event::Field(Field::Width(w)));
                events.push(Event::Field(Field::Height(h)));
            }
            self.got_tracks = true;
        }
        self.pos = self.pos.saturating_add(total as u64);
        if self.got_info && self.got_tracks {
            self.done = true;
            return PullResult { demand: Demand::Done, consumed: total, events };
        }
        PullResult { demand: Demand::NeedBytes(2), consumed: total, events }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doctype_header(doctype: &[u8]) -> Vec<u8> {
        let dt = elem(&[0x42, 0x82], doctype);
        elem(&[0x1A, 0x45, 0xDF, 0xA3], &dt)
    }

    fn segment(children: &[u8]) -> Vec<u8> {
        elem(&[0x18, 0x53, 0x80, 0x67], children)
    }

    /// 构造完整 MKV/WebM：EBML头 + Segment{ Info, Tracks, Cluster }。
    fn full_ebml(doctype: &[u8], w: u32, h: u32, dur: f64, date_ns: i64) -> Vec<u8> {
        let info = elem(&[0x15, 0x49, 0xA9, 0x66], &info_payload(Some(1_000_000), Some(dur), Some(date_ns)));
        let tracks = elem(&[0x16, 0x54, 0xAE, 0x6B], &video_track(w, h));
        let cluster = elem(&[0x1F, 0x43, 0xB6, 0x75], &[0u8; 8]);
        let mut seg_children = Vec::new();
        seg_children.extend_from_slice(&info);
        seg_children.extend_from_slice(&tracks);
        seg_children.extend_from_slice(&cluster);
        let mut f = doctype_header(doctype);
        f.extend_from_slice(&segment(&seg_children));
        f
    }

    #[test]
    fn end_to_end_webm_slice() {
        let buf = full_ebml(b"webm", 1280, 720, 5000.0, 0);
        let col = crate::driver::drive_slice(&buf, &mut EbmlParser::new(), crate::limits::Limits::default());
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Webm);
        assert!(meta.warnings.is_empty(), "warnings: {:?}", meta.warnings);
        assert_eq!(meta.unified.width, Some(1280));
        assert_eq!(meta.unified.height, Some(720));
        assert_eq!(meta.unified.duration_ms, Some(5000));
        assert_eq!(meta.unified.created.map(|d| d.year), Some(2001));
        assert_eq!(meta.unified.created.and_then(|d| d.tz_offset_min), Some(0));
    }

    #[test]
    fn walk_skips_ebml_header_and_descends_segment() {
        let buf = full_ebml(b"matroska", 640, 480, 1000.0, 0);
        let mut p = EbmlParser::new();
        let res = p.pull(&buf);
        match res.demand {
            Demand::Skip(_) => {}
            other => panic!("expected Skip over EBML header, got {other:?}"),
        }
    }

    #[test]
    fn unknown_size_media_before_info_warns_and_stops() {
        // Segment 内首个子元素是未知大小 Cluster（在集齐 Info+Tracks 前）→ 警告 + Done。
        let mut cluster = Vec::new();
        cluster.extend_from_slice(&[0x1F, 0x43, 0xB6, 0x75]); // Cluster id
        cluster.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]); // 未知大小
        cluster.extend_from_slice(&[0u8; 4]);
        let mut f = doctype_header(b"webm");
        f.extend_from_slice(&segment(&cluster));
        let col = crate::driver::drive_slice(&f, &mut EbmlParser::new(), crate::limits::Limits::default());
        assert!(col.warnings.iter().any(|w| w.kind == crate::model::WarnKind::UnreachableSection));
    }

    #[test]
    fn truncated_info_warns_truncated() {
        // Info 声明 size 远大于实际 → driver 到 EOF 记 Truncated，不 panic。
        let mut info = Vec::new();
        info.extend_from_slice(&[0x15, 0x49, 0xA9, 0x66]); // Info id
        info.extend_from_slice(&[0x01]);
        info.extend_from_slice(&300u64.to_be_bytes()[1..]); // 声明 300
        info.extend_from_slice(&[0u8; 8]); // 实际仅 8
        let mut f = doctype_header(b"webm");
        f.extend_from_slice(&segment(&info));
        let col = crate::driver::drive_slice(&f, &mut EbmlParser::new(), crate::limits::Limits::default());
        assert!(col.warnings.iter().any(|w| w.kind == crate::model::WarnKind::Truncated));
    }

    #[test]
    fn matroska_epoch_anchor_and_offsets() {
        // date_ns = 0 → 2001-01-01T00:00:00 UTC
        let dt = datetime_from_matroska_epoch(0);
        assert_eq!((dt.year, dt.month, dt.day), (2001, 1, 1));
        assert_eq!((dt.hour, dt.minute, dt.second), (0, 0, 0));
        assert_eq!(dt.tz_offset_min, Some(0));
        // +1 天
        let nd = datetime_from_matroska_epoch(86_400 * 1_000_000_000);
        assert_eq!((nd.year, nd.month, nd.day), (2001, 1, 2));
        // +01:01:01
        let tod = datetime_from_matroska_epoch(3_661 * 1_000_000_000);
        assert_eq!((tod.hour, tod.minute, tod.second), (1, 1, 1));
        // 负值（2000-12-31）
        let neg = datetime_from_matroska_epoch(-1 * 1_000_000_000);
        assert_eq!((neg.year, neg.month, neg.day), (2000, 12, 31));
    }

    fn elem(id: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut e = Vec::new();
        e.extend_from_slice(id);
        e.push(0x01);
        e.extend_from_slice(&(payload.len() as u64).to_be_bytes()[1..]);
        e.extend_from_slice(payload);
        e
    }

    fn info_payload(scale: Option<u64>, duration: Option<f64>, date_ns: Option<i64>) -> Vec<u8> {
        let mut p = Vec::new();
        if let Some(s) = scale {
            p.extend_from_slice(&elem(&[0x2A, 0xD7, 0xB1], &s.to_be_bytes()));
        }
        if let Some(d) = duration {
            p.extend_from_slice(&elem(&[0x44, 0x89], &d.to_be_bytes()));
        }
        if let Some(n) = date_ns {
            p.extend_from_slice(&elem(&[0x44, 0x61], &n.to_be_bytes()));
        }
        p
    }

    #[test]
    fn parse_info_duration_default_scale() {
        // 默认 scale = 1_000_000 ns；duration 5000.0 → 5000 ms
        let info = parse_info(&info_payload(None, Some(5000.0), None));
        assert_eq!(info.duration_ms, Some(5000));
        assert!(!info.invalid);
    }

    #[test]
    fn parse_info_explicit_scale_and_f32_path() {
        // scale 1_000_000；duration 1500.0 → 1500 ms
        let info = parse_info(&info_payload(Some(1_000_000), Some(1500.0), Some(0)));
        assert_eq!(info.duration_ms, Some(1500));
        assert_eq!(info.created.map(|d| d.year), Some(2001));
    }

    #[test]
    fn parse_info_invalid_duration_warns() {
        // 负 duration → 无 duration、invalid
        let neg = parse_info(&info_payload(Some(1_000_000), Some(-1.0), None));
        assert_eq!(neg.duration_ms, None);
        assert!(neg.invalid);
        // NaN → invalid
        let nan = parse_info(&info_payload(Some(1_000_000), Some(f64::NAN), None));
        assert!(nan.invalid);
        // scale == 0 → invalid
        let zero = parse_info(&info_payload(Some(0), Some(5000.0), None));
        assert_eq!(zero.duration_ms, None);
        assert!(zero.invalid);
    }

    fn video_track(w: u32, h: u32) -> Vec<u8> {
        let mut vid = Vec::new();
        vid.extend_from_slice(&elem(&[0xB0], &w.to_be_bytes())); // PixelWidth
        vid.extend_from_slice(&elem(&[0xBA], &h.to_be_bytes())); // PixelHeight
        let video = elem(&[0xE0], &vid);
        elem(&[0xAE], &video) // TrackEntry { Video }
    }

    fn audio_track() -> Vec<u8> {
        // TrackEntry 无 Video 子元素（仅一个占位子元素 0x83 TrackType=2）
        let inner = elem(&[0x83], &[2]);
        elem(&[0xAE], &inner)
    }

    #[test]
    fn parse_tracks_picks_first_video() {
        let mut tracks = Vec::new();
        tracks.extend_from_slice(&audio_track());          // 音频轨在前
        tracks.extend_from_slice(&video_track(1280, 720)); // 视频轨
        assert_eq!(parse_tracks(&tracks), Some((1280, 720)));
    }

    #[test]
    fn parse_tracks_audio_only_is_none() {
        assert_eq!(parse_tracks(&audio_track()), None);
    }

    #[test]
    fn parse_tracks_zero_dims_is_none() {
        assert_eq!(parse_tracks(&video_track(0, 0)), None);
    }
}
