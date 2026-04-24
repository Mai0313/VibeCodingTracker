use chrono::DateTime;

/// Parse ISO timestamp to Unix milliseconds
pub fn parse_iso_timestamp(ts: &str) -> i64 {
    if ts.is_empty() {
        return 0;
    }

    // Try RFC3339 first (most common format)
    if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
        return dt.timestamp_millis();
    }

    // Try other formats
    let formats = [
        "%Y-%m-%dT%H:%M:%S%.3fZ",
        "%Y-%m-%dT%H:%M:%S%.fZ",
        "%Y-%m-%dT%H:%M:%SZ",
    ];

    for format in &formats {
        if let Ok(dt) = DateTime::parse_from_str(ts, format) {
            return dt.timestamp_millis();
        }
    }

    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_iso_timestamp_rfc3339() {
        let ts = "2024-01-15T10:30:45.123Z";
        let result = parse_iso_timestamp(ts);
        assert!(result > 0);

        assert!(result > 1_700_000_000_000);
        assert!(result < 1_800_000_000_000);
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
    fn test_parse_iso_timestamp_fallback_formats() {
        let ts1 = "2024-01-15T10:30:45.123Z";
        let result1 = parse_iso_timestamp(ts1);
        assert!(result1 > 0);

        let ts2 = "2024-01-15T10:30:45.123456Z";
        let result2 = parse_iso_timestamp(ts2);
        assert!(result2 > 0);

        let ts3 = "2024-01-15T10:30:45Z";
        let result3 = parse_iso_timestamp(ts3);
        assert!(result3 > 0);
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
        let ts1 = "2024-01-01T00:00:00Z";
        let result1 = parse_iso_timestamp(ts1);
        assert!(result1 > 0);

        let ts2 = "2024-12-31T23:59:59Z";
        let result2 = parse_iso_timestamp(ts2);
        assert!(result2 > 0);
        assert!(result2 > result1);

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
}
