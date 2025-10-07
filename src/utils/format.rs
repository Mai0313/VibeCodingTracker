use chrono::Local;

/// Format a number with thousand separators (e.g., 1234567 -> "1,234,567")
pub fn format_number<T: ToString>(n: T) -> String {
    let s = n.to_string();

    if s.len() <= 3 {
        return s;
    }

    let mut result = String::with_capacity(s.len() + (s.len() - 1) / 3);
    let remainder = s.len() % 3;

    // Handle first group (which might be 1, 2, or 3 digits)
    if remainder > 0 {
        result.push_str(&s[..remainder]);
        if s.len() > remainder {
            result.push(',');
        }
    }

    // Handle remaining groups of 3
    for (i, chunk) in s.as_bytes()[remainder..].chunks(3).enumerate() {
        if i > 0 {
            result.push(',');
        }
        result.push_str(std::str::from_utf8(chunk).unwrap());
    }

    result
}

/// Get current date in YYYY-MM-DD format
pub fn get_current_date() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(123), "123");
        assert_eq!(format_number(1234), "1,234");
        assert_eq!(format_number(1234567), "1,234,567");
        assert_eq!(format_number(1234567890), "1,234,567,890");
    }

    #[test]
    fn test_format_number_edge_cases() {
        // Single digit
        assert_eq!(format_number(1), "1");
        assert_eq!(format_number(9), "9");

        // Two digits
        assert_eq!(format_number(10), "10");
        assert_eq!(format_number(99), "99");

        // Three digits (boundary - no comma needed)
        assert_eq!(format_number(100), "100");
        assert_eq!(format_number(999), "999");

        // Four digits (boundary - first comma appears)
        assert_eq!(format_number(1000), "1,000");
        assert_eq!(format_number(9999), "9,999");

        // Exact multiples of 1000
        assert_eq!(format_number(10000), "10,000");
        assert_eq!(format_number(100000), "100,000");
        assert_eq!(format_number(1000000), "1,000,000");

        // Large numbers
        assert_eq!(format_number(12345678901_i64), "12,345,678,901");
        assert_eq!(format_number(999999999999_i64), "999,999,999,999");
    }

    #[test]
    fn test_format_number_with_string_input() {
        assert_eq!(format_number("12345"), "12,345");
        assert_eq!(format_number("999"), "999");
    }

    #[test]
    fn test_get_current_date() {
        let date = get_current_date();
        assert_eq!(date.len(), 10); // YYYY-MM-DD format
        assert!(date.contains('-'));
    }
}
