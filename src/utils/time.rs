use chrono::DateTime;

/// Parse ISO timestamp to Unix milliseconds
pub fn parse_iso_timestamp(ts: &str) -> i64 {
    if ts.is_empty() {
        return 0;
    }

    // Try parsing with different formats
    let formats = [
        "%Y-%m-%dT%H:%M:%S%.3fZ",
        "%Y-%m-%dT%H:%M:%S%.fZ",
        "%Y-%m-%dT%H:%M:%SZ",
        "%+", // RFC3339
    ];

    for format in &formats {
        if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
            return dt.timestamp_millis();
        }
        if let Ok(dt) = DateTime::parse_from_str(ts, format) {
            return dt.timestamp_millis();
        }
    }

    0
}
