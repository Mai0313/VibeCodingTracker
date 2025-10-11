// Unit tests for utils/time.rs
//
// Tests timestamp parsing utilities

use vibe_coding_tracker::utils::time::parse_iso_timestamp;

#[test]
fn test_parse_iso_timestamp_rfc3339() {
    // Test parsing RFC3339 format (most common)
    let ts = "2024-01-15T10:30:45.123Z";
    let result = parse_iso_timestamp(ts);
    assert!(result > 0);
    
    // Should parse to a valid timestamp (2024)
    assert!(result > 1_700_000_000_000); // After 2023
    assert!(result < 1_800_000_000_000); // Before 2027
}

#[test]
fn test_parse_iso_timestamp_with_timezone() {
    // Test parsing with timezone offset
    let ts = "2024-01-15T10:30:45.123+08:00";
    let result = parse_iso_timestamp(ts);
    assert!(result > 0);
}

#[test]
fn test_parse_iso_timestamp_no_millis() {
    // Test parsing without milliseconds
    let ts = "2024-01-15T10:30:45Z";
    let result = parse_iso_timestamp(ts);
    assert!(result > 0);
}

#[test]
fn test_parse_iso_timestamp_fallback_formats() {
    // Test fallback format with milliseconds
    let ts1 = "2024-01-15T10:30:45.123Z";
    let result1 = parse_iso_timestamp(ts1);
    assert!(result1 > 0);
    
    // Test fallback format with fractional seconds
    let ts2 = "2024-01-15T10:30:45.123456Z";
    let result2 = parse_iso_timestamp(ts2);
    assert!(result2 > 0);
    
    // Test fallback format without fractional seconds
    let ts3 = "2024-01-15T10:30:45Z";
    let result3 = parse_iso_timestamp(ts3);
    assert!(result3 > 0);
}

#[test]
fn test_parse_iso_timestamp_empty() {
    // Test parsing empty string
    let result = parse_iso_timestamp("");
    assert_eq!(result, 0);
}

#[test]
fn test_parse_iso_timestamp_invalid() {
    // Test parsing invalid format
    let result = parse_iso_timestamp("not a timestamp");
    assert_eq!(result, 0);
    
    let result = parse_iso_timestamp("2024-13-45");
    assert_eq!(result, 0);
    
    let result = parse_iso_timestamp("invalid-date-time");
    assert_eq!(result, 0);
}

#[test]
fn test_parse_iso_timestamp_different_years() {
    // Test different years to ensure parsing is consistent
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
    // Test that milliseconds are preserved
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
    // Test parsing the same timestamp twice
    let ts = "2024-01-15T10:30:45.123Z";
    let result1 = parse_iso_timestamp(ts);
    let result2 = parse_iso_timestamp(ts);
    
    assert_eq!(result1, result2);
}

#[test]
fn test_parse_iso_timestamp_edge_cases() {
    // Test edge cases
    
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
    // Test parsing with negative timezone offset
    let ts = "2024-01-15T10:30:45.123-05:00";
    let result = parse_iso_timestamp(ts);
    assert!(result > 0);
}

#[test]
fn test_parse_iso_timestamp_midnight() {
    // Test midnight timestamps
    let ts = "2024-01-15T00:00:00.000Z";
    let result = parse_iso_timestamp(ts);
    assert!(result > 0);
}

#[test]
fn test_parse_iso_timestamp_noon() {
    // Test noon timestamps
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
    // Test that timestamps maintain proper ordering
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

