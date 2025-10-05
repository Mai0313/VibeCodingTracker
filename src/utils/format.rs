use chrono::Local;

/// Format a number with thousand separators (e.g., 1234567 -> "1,234,567")
pub fn format_number<T: ToString>(n: T) -> String {
    let s = n.to_string();

    if s == "0" {
        return "0".to_string();
    }

    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();

    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push(*c);
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
    fn test_get_current_date() {
        let date = get_current_date();
        assert_eq!(date.len(), 10); // YYYY-MM-DD format
        assert!(date.contains('-'));
    }
}
