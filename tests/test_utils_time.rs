// Tests for utils::time module

use codex_usage::utils::time::parse_iso_timestamp;

#[test]
fn test_parse_iso_timestamp_standard_format() {
    let timestamp = "2025-08-28T12:57:08.611Z";
    let result = parse_iso_timestamp(timestamp);
    assert!(result > 0, "Timestamp should be positive");

    // Expected value: ~1724851028611
    // Just verify it's in a reasonable range (year 2025)
    assert!(result > 1700000000000, "Timestamp should be in the future");
}

#[test]
fn test_parse_iso_timestamp_without_milliseconds() {
    let timestamp = "2025-08-28T12:57:08Z";
    let result = parse_iso_timestamp(timestamp);
    assert!(result > 0, "Timestamp should be positive");
}

#[test]
fn test_parse_iso_timestamp_with_timezone() {
    let timestamp = "2025-08-28T12:57:08+08:00";
    let result = parse_iso_timestamp(timestamp);
    assert!(result > 0, "Timestamp should be positive");
}

#[test]
fn test_parse_iso_timestamp_empty_string() {
    let result = parse_iso_timestamp("");
    assert_eq!(result, 0, "Empty string should return 0");
}

#[test]
fn test_parse_iso_timestamp_invalid_format() {
    let result = parse_iso_timestamp("invalid-timestamp");
    assert_eq!(result, 0, "Invalid format should return 0");
}

#[test]
fn test_parse_iso_timestamp_comparison() {
    let ts1 = parse_iso_timestamp("2025-08-28T12:00:00Z");
    let ts2 = parse_iso_timestamp("2025-08-28T13:00:00Z");

    assert!(ts2 > ts1, "Later timestamp should be greater");
    assert_eq!(
        ts2 - ts1,
        3600000,
        "Difference should be 1 hour in milliseconds"
    );
}
