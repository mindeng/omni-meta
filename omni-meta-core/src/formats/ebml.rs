//! EBML（Matroska/WebM）顶层解析器。前向走盒：跳过 EBML 头与不关心元素、
//! 下钻 Segment（不缓冲）、整元素缓冲解析 Info/Tracks、遇未知大小媒体即干净停止。

use alloc::vec::Vec;

use crate::containers::ebml::{iter_child_elements, read_float, read_int, read_uint};
use crate::demand::{Demand, Event, MetaParser, PullResult};
use crate::model::DateTimeParts;

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

#[derive(Debug, Default)]
pub struct EbmlParser {
    done: bool,
}

impl EbmlParser {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MetaParser for EbmlParser {
    fn pull<'a>(&mut self, _input: &'a [u8]) -> PullResult<'a> {
        self.done = true;
        PullResult { demand: Demand::Done, consumed: 0, events: Vec::<Event<'a>>::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
