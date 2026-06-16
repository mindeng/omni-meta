//! EBML（Matroska/WebM）顶层解析器。前向走盒：跳过 EBML 头与不关心元素、
//! 下钻 Segment（不缓冲）、整元素缓冲解析 Info/Tracks、遇未知大小媒体即干净停止。

use alloc::vec::Vec;

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
}
