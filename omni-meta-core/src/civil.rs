//! 民用历法：自 1970-01-01 起的天数 → (year, month, day)。
//! Howard Hinnant `civil_from_days` 算法，纯整数、no_std 安全、无浮点。
//! BMFF（1904 纪元）与 EBML（2001 纪元）共用此换算。

/// 自 1970-01-01 起的天数 → (year, month, day)。负天数表示 1970 之前。
pub(crate) fn civil_from_days(days: i64) -> (u16, u8, u8) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8; // [1, 12]
    let year = (y + if m <= 2 { 1 } else { 0 }) as u16;
    (year, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_known_vectors() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(31), (1970, 2, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(59), (1970, 3, 1)); // 1970 非闰
    }
}
