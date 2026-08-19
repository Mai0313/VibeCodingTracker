//! RFC 3339 timestamp helpers: parsing the ISO-8601 strings that provider
//! session logs and quota responses carry, and formatting the UTC stamps vct
//! writes back into credential and cache files.

use chrono::{DateTime, SecondsFormat, Utc};

/// Current UTC time as RFC3339 with nanoseconds and a `Z` suffix
/// (e.g. `2026-07-07T05:34:50.563606999Z`).
///
/// Matches the format Codex writes for `auth.json`'s `last_refresh`, so a
/// refreshed token can be written back in that CLI's own shape; the caches and
/// log lines vct writes itself reuse the same stamp.
pub fn now_rfc3339_utc_nanos() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true)
}

/// Formats `unix_secs` in the same shape [`now_rfc3339_utc_nanos`] produces.
///
/// Used to stamp a refreshed token's expiry in the exact format the Grok CLI
/// writes into `auth.json`. Returns `None` for a timestamp outside the
/// representable range rather than silently substituting another instant.
pub fn rfc3339_utc_nanos(unix_secs: i64) -> Option<String> {
    DateTime::from_timestamp(unix_secs, 0).map(|dt| dt.to_rfc3339_opts(SecondsFormat::Nanos, true))
}

/// Parses an RFC 3339 timestamp into Unix milliseconds.
///
/// Any offset the standard allows is accepted (`Z` or a numeric offset), at
/// any sub-second precision. An input carrying no offset is rejected, and
/// surrounding whitespace is *not* tolerated.
///
/// Returns `0` for an empty string or any input that is not RFC 3339;
/// callers treat `0` as "unknown time" rather than the Unix epoch.
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
    if ts.is_empty() {
        return 0;
    }

    match DateTime::parse_from_rfc3339(ts) {
        Ok(dt) => dt.timestamp_millis(),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_utc_nanos_round_trips_and_matches_the_now_shape() {
        let stamp = rfc3339_utc_nanos(1_785_058_368).expect("in range");
        assert_eq!(stamp, "2026-07-26T09:32:48.000000000Z");
        // The same shape `now_rfc3339_utc_nanos` writes, and re-readable.
        assert_eq!(parse_iso_timestamp(&stamp), 1_785_058_368_000);
        assert_eq!(
            stamp.len(),
            now_rfc3339_utc_nanos().len(),
            "both stamps must be the fixed-width nanosecond form"
        );
        // Out of range yields None rather than a substituted instant.
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
        // Every sub-second width RFC 3339 allows lands on the same instant,
        // truncated to milliseconds.
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
        // A naive timestamp has no offset to resolve, so it is rejected
        // rather than assumed to be UTC.
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
        // Should be ~999ms apart
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
        // Test that whitespace is not tolerated
        let result = parse_iso_timestamp(" 2024-01-15T10:30:45Z ");
        assert_eq!(result, 0);
    }

    #[test]
    fn test_parse_iso_timestamp_partial() {
        // Test partial timestamps (invalid)
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

        // All should be non-zero
        assert!(results.iter().all(|&r| r > 0));

        // Should be in ascending order
        for i in 1..results.len() {
            assert!(results[i] > results[i - 1]);
        }
    }
}
