//! UTC timestamps for the crash log and for names of moved-aside data,
//! without a date-time dependency.

use std::time::{SystemTime, UNIX_EPOCH};

/// A moment in UTC, broken into its calendar fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Utc {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    millisecond: u32,
}

impl Utc {
    fn from_system_time(time: SystemTime) -> Self {
        let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
        let seconds = i64::try_from(since.as_secs()).unwrap_or(i64::MAX);
        let (days, of_day) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
        let (year, month, day) = civil_from_days(days);
        // `of_day` is below 86 400, so these fit.
        let of_day = u32::try_from(of_day).unwrap_or(0);
        Self {
            year,
            month,
            day,
            hour: of_day / 3600,
            minute: of_day % 3600 / 60,
            second: of_day % 60,
            millisecond: since.subsec_millis(),
        }
    }
}

/// `2026-11-05T10:15:00.123Z`, for log lines.
pub fn rfc3339_millis(time: SystemTime) -> String {
    let t = Utc::from_system_time(time);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year, t.month, t.day, t.hour, t.minute, t.second, t.millisecond
    )
}

/// `20261105T101500Z`, for file and directory names (no `:`, which Windows
/// refuses).
pub fn compact(time: SystemTime) -> String {
    let t = Utc::from_system_time(time);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day), after
/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn formats_known_moments() {
        assert_eq!(rfc3339_millis(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        // 2026-11-05T10:15:30.250Z
        let moment = UNIX_EPOCH + Duration::from_millis(1_793_873_730_250);
        assert_eq!(rfc3339_millis(moment), "2026-11-05T10:15:30.250Z");
        assert_eq!(compact(moment), "20261105T101530Z");
        // A leap day, and the last second of a year.
        let leap = UNIX_EPOCH + Duration::from_secs(1_709_164_800);
        assert_eq!(compact(leap), "20240229T000000Z");
        let last = UNIX_EPOCH + Duration::from_secs(1_767_225_599);
        assert_eq!(compact(last), "20251231T235959Z");
    }
}
