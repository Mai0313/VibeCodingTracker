//! RFC 3339 timestamp parsing and UTC nanosecond formatting.

use chrono::{DateTime, SecondsFormat, Utc};

/// Returns the current UTC time as an RFC 3339 string with nanosecond precision and a `Z` suffix (for example, `2026-07-07T05:34:50.563606999Z`).
pub fn now_rfc3339_utc_nanos() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true)
}

/// Formats `unix_secs` as an RFC 3339 UTC timestamp with nanosecond precision and a `Z` suffix.
///
/// Returns `None` if `unix_secs` is outside the representable `DateTime` range.
pub fn rfc3339_utc_nanos(unix_secs: i64) -> Option<String> {
    DateTime::from_timestamp(unix_secs, 0).map(|dt| dt.to_rfc3339_opts(SecondsFormat::Nanos, true))
}

/// Parses an RFC 3339 timestamp into Unix milliseconds, returning `0` on invalid or empty input.
///
/// Accepts `Z` or a numeric UTC offset at any sub-second precision (truncated to milliseconds). Naive timestamps without an offset and inputs with surrounding whitespace return `0`.
///
/// # Examples
///
/// ```
/// use vct_core::utils::parse_iso_timestamp;
///
/// assert_eq!(parse_iso_timestamp("1970-01-01T00:00:01Z"), 1_000);
/// assert_eq!(parse_iso_timestamp(""), 0);
/// assert_eq!(parse_iso_timestamp("not a timestamp"), 0);
/// ```
pub fn parse_iso_timestamp(ts: &str) -> i64 {
    if ts.len() < 20 {
        return 0;
    }

    if let Some(ms) = parse_rfc3339_ascii(ts.as_bytes()) {
        return ms;
    }

    match DateTime::parse_from_rfc3339(ts) {
        Ok(dt) => dt.timestamp_millis(),
        Err(_) => 0,
    }
}

/// Parses standard ASCII RFC 3339 bytes without allocating; requires `bytes.len() >= 20` and returns `None` to defer uncommon forms (such as leap seconds) to `chrono`.
#[inline]
fn parse_rfc3339_ascii(bytes: &[u8]) -> Option<i64> {
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }

    let year = i32::try_from(parse_4_digits(bytes, 0)?).ok()?;
    let month = parse_2_digits(bytes, 5)?;
    let day = parse_2_digits(bytes, 8)?;
    let hour = parse_2_digits(bytes, 11)?;
    let minute = parse_2_digits(bytes, 14)?;
    let second = parse_2_digits(bytes, 17)?;

    if second == 60 {
        return None;
    }
    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Some(0);
    }

    let mut idx = 19;
    let mut millis = 0_i64;
    if bytes.get(idx) == Some(&b'.') {
        idx += 1;
        let frac_start = idx;
        let mut weight = 100_i64;
        while let Some(&b) = bytes.get(idx) {
            if !b.is_ascii_digit() {
                break;
            }
            if weight > 0 {
                millis += i64::from(b - b'0') * weight;
                weight /= 10;
            }
            idx += 1;
        }
        if idx == frac_start {
            return Some(0);
        }
    }

    let offset_secs = match &bytes[idx..] {
        [b'Z' | b'z'] => 0_i64,
        [sign @ (b'+' | b'-'), h0, h1, b':', m0, m1] => {
            let Some(oh) = parse_2_raw(*h0, *h1) else {
                return Some(0);
            };
            let Some(om) = parse_2_raw(*m0, *m1) else {
                return Some(0);
            };
            if oh > 23 || om > 59 {
                return Some(0);
            }
            let total = i64::from(oh) * 3_600 + i64::from(om) * 60;
            if *sign == b'-' { -total } else { total }
        }
        _ => return None,
    };

    let days = days_from_civil(year, month, day);
    let local_secs =
        days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second);
    let utc_secs = local_secs - offset_secs;
    Some(utc_secs * 1_000 + millis)
}

#[inline]
fn parse_2_raw(d0: u8, d1: u8) -> Option<u32> {
    if !d0.is_ascii_digit() || !d1.is_ascii_digit() {
        return None;
    }
    Some(u32::from(d0 - b'0') * 10 + u32::from(d1 - b'0'))
}

#[inline]
fn parse_2_digits(bytes: &[u8], offset: usize) -> Option<u32> {
    parse_2_raw(bytes[offset], bytes[offset + 1])
}

#[inline]
fn parse_4_digits(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(parse_2_digits(bytes, offset)? * 100 + parse_2_digits(bytes, offset + 2)?)
}

#[inline]
fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// Howard Hinnant's civil-to-Unix-day conversion (`1970-01-01` -> `0`).
#[inline]
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + i64::from(doe) - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_utc_nanos_round_trips_and_matches_the_now_shape() {
        let stamp = rfc3339_utc_nanos(1_785_058_368).expect("in range");
        assert_eq!(stamp, "2026-07-26T09:32:48.000000000Z");
        assert_eq!(parse_iso_timestamp(&stamp), 1_785_058_368_000);
        assert_eq!(
            stamp.len(),
            now_rfc3339_utc_nanos().len(),
            "both stamps must be the fixed-width nanosecond form"
        );
        assert!(rfc3339_utc_nanos(i64::MAX).is_none());
    }

    #[test]
    fn test_parse_iso_timestamp_rfc3339() {
        let ts = "2024-01-15T10:30:45.123Z";
        let result = parse_iso_timestamp(ts);
        assert!(result > 0);
        assert!(result > 1_700_000_000_000); // After 2023
        assert!(result < 1_800_000_000_000); // Before 2027
    }

    #[test]
    fn test_parse_iso_timestamp_with_timezone() {
        let ts = "2024-01-15T10:30:45.123+08:00";
        let result = parse_iso_timestamp(ts);
        assert!(result > 0);
    }

    #[test]
    fn test_parse_iso_timestamp_no_millis() {
        let ts = "2024-01-15T10:30:45Z";
        let result = parse_iso_timestamp(ts);
        assert!(result > 0);
    }

    #[test]
    fn test_parse_iso_timestamp_sub_second_precision() {
        // Every sub-second width RFC 3339 allows lands on the same instant, truncated to milliseconds.
        assert_eq!(
            parse_iso_timestamp("2024-01-15T10:30:45.123Z"),
            1_705_314_645_123
        );
        assert_eq!(
            parse_iso_timestamp("2024-01-15T10:30:45.123456Z"),
            1_705_314_645_123
        );
        assert_eq!(
            parse_iso_timestamp("2024-01-15T10:30:45Z"),
            1_705_314_645_000
        );
    }

    #[test]
    fn test_parse_iso_timestamp_requires_an_offset() {
        assert_eq!(parse_iso_timestamp("2024-01-15T10:30:45"), 0);
        assert_eq!(parse_iso_timestamp("2024-01-15T10:30:45.123"), 0);
    }

    #[test]
    fn test_parse_iso_timestamp_empty() {
        let result = parse_iso_timestamp("");
        assert_eq!(result, 0);
    }

    #[test]
    fn test_parse_iso_timestamp_invalid() {
        let result = parse_iso_timestamp("not a timestamp");
        assert_eq!(result, 0);

        let result = parse_iso_timestamp("2024-13-45");
        assert_eq!(result, 0);

        let result = parse_iso_timestamp("invalid-date-time");
        assert_eq!(result, 0);
    }

    #[test]
    fn test_parse_iso_timestamp_different_years() {
        let ts_2020 = "2020-06-15T12:00:00Z";
        let ts_2024 = "2024-06-15T12:00:00Z";

        let result_2020 = parse_iso_timestamp(ts_2020);
        let result_2024 = parse_iso_timestamp(ts_2024);

        assert!(result_2020 > 0);
        assert!(result_2024 > 0);
        assert!(result_2024 > result_2020);
    }

    #[test]
    fn test_parse_iso_timestamp_milliseconds_precision() {
        let ts1 = "2024-01-15T10:30:45.000Z";
        let ts2 = "2024-01-15T10:30:45.999Z";

        let result1 = parse_iso_timestamp(ts1);
        let result2 = parse_iso_timestamp(ts2);

        assert!(result1 > 0);
        assert!(result2 > 0);
        assert!(result2 > result1);
        assert!(result2 - result1 < 1000);
    }

    #[test]
    fn test_parse_iso_timestamp_same_time() {
        let ts = "2024-01-15T10:30:45.123Z";
        let result1 = parse_iso_timestamp(ts);
        let result2 = parse_iso_timestamp(ts);

        assert_eq!(result1, result2);
    }

    #[test]
    fn test_parse_iso_timestamp_edge_cases() {
        // Beginning of year
        let ts1 = "2024-01-01T00:00:00Z";
        let result1 = parse_iso_timestamp(ts1);
        assert!(result1 > 0);

        // End of year
        let ts2 = "2024-12-31T23:59:59Z";
        let result2 = parse_iso_timestamp(ts2);
        assert!(result2 > 0);
        assert!(result2 > result1);

        // Leap year day
        let ts3 = "2024-02-29T12:00:00Z";
        let result3 = parse_iso_timestamp(ts3);
        assert!(result3 > 0);
    }

    #[test]
    fn test_parse_iso_timestamp_negative_timezone() {
        let ts = "2024-01-15T10:30:45.123-05:00";
        let result = parse_iso_timestamp(ts);
        assert!(result > 0);
    }

    #[test]
    fn test_parse_iso_timestamp_midnight() {
        let ts = "2024-01-15T00:00:00.000Z";
        let result = parse_iso_timestamp(ts);
        assert!(result > 0);
    }

    #[test]
    fn test_parse_iso_timestamp_noon() {
        let ts = "2024-01-15T12:00:00.000Z";
        let result = parse_iso_timestamp(ts);
        assert!(result > 0);
    }

    #[test]
    fn test_parse_iso_timestamp_whitespace() {
        let result = parse_iso_timestamp(" 2024-01-15T10:30:45Z ");
        assert_eq!(result, 0);
    }

    #[test]
    fn test_parse_iso_timestamp_partial() {
        let result = parse_iso_timestamp("2024-01-15");
        assert_eq!(result, 0);

        let result = parse_iso_timestamp("2024-01-15T10:30");
        assert_eq!(result, 0);
    }

    #[test]
    fn test_parse_iso_timestamp_ordering() {
        let timestamps = [
            "2024-01-15T10:00:00Z",
            "2024-01-15T11:00:00Z",
            "2024-01-15T12:00:00Z",
            "2024-01-15T13:00:00Z",
        ];

        let results: Vec<i64> = timestamps
            .iter()
            .map(|ts| parse_iso_timestamp(ts))
            .collect();

        assert!(results.iter().all(|&r| r > 0));

        for i in 1..results.len() {
            assert!(results[i] > results[i - 1]);
        }
    }

    #[test]
    fn fast_rfc3339_matches_chrono_across_calendars_offsets_and_invalid_inputs() {
        let samples = [
            "1969-12-31T23:59:59.123Z",
            "1970-01-01T00:00:00Z",
            "2000-02-29T23:59:59.999Z",
            "1900-02-29T12:00:00Z",
            "2023-02-29T12:00:00Z",
            "2024-02-29T00:00:00+08:00",
            "2026-11-18T22:30:00.5-05:30",
            "2026-11-18t22:30:00.123456789z",
            "2026-11-18T22:30:00-00:00",
            "2024-12-31T23:59:60Z",
            "2024-01-15T10:30:45.Z",
            "2024-01-15T10:30:45Zextra",
            "2024-01-15T24:00:00Z",
            "2024-01-15T10:30:45+24:00",
            "2024-01-15T10:30:45+00:60",
        ];

        for sample in samples {
            let expected = DateTime::parse_from_rfc3339(sample)
                .map(|dt| dt.timestamp_millis())
                .unwrap_or(0);
            assert_eq!(
                parse_iso_timestamp(sample),
                expected,
                "mismatch on {sample}"
            );
        }
    }
}
