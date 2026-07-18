use chrono::{Local, NaiveDate};
use std::sync::RwLock;

/// Formats an integer with comma thousands-separators.
///
/// Accepts any [`itoa::Integer`], so both signed and unsigned widths work.
/// Uses `itoa` to render the digits into a stack buffer, then inserts commas
/// every three digits from the right — no intermediate `String` allocation
/// for the conversion itself.
///
/// # Examples
///
/// ```
/// use vct_core::utils::format_number;
///
/// assert_eq!(format_number(0), "0");
/// assert_eq!(format_number(1234), "1,234");
/// assert_eq!(format_number(1_234_567), "1,234,567");
/// ```
pub fn format_number<T>(n: T) -> String
where
    T: itoa::Integer,
{
    // Use itoa for fast conversion (no allocations, direct to buffer)
    let mut buf = itoa::Buffer::new();
    let s = buf.format(n);

    if s.len() <= 3 {
        return s.to_string();
    }

    let mut result = String::with_capacity(s.len() + (s.len() - 1) / 3);
    let remainder = s.len() % 3;

    // Handle first group (which might be 1, 2, or 3 digits)
    if remainder > 0 {
        result.push_str(&s[..remainder]);
    }

    // Handle remaining groups of 3 (direct byte operations for speed)
    for (i, chunk) in s.as_bytes()[remainder..].chunks_exact(3).enumerate() {
        // Add comma before each group (including first if remainder > 0)
        if remainder > 0 || i > 0 {
            result.push(',');
        }
        // SAFETY: chunks_exact(3) guarantees valid UTF-8 ASCII digits
        unsafe {
            result.push_str(std::str::from_utf8_unchecked(chunk));
        }
    }

    result
}

/// Formats an integer into a compact human string with a K/M/B/T suffix.
///
/// Targets a dense dashboard where column width is scarce: values under 1000
/// render verbatim, otherwise the largest fitting unit is used with 3
/// significant figures (2 / 1 / 0 decimals as the scaled value crosses 10 and
/// 100), so the result stays within ~5 characters. Rounding that would reach
/// 1000 within a unit is promoted to the next unit, so `999_950` renders as
/// `"1.00M"` rather than `"1000K"`. Negative values keep a leading `-`.
///
/// Only the interactive TUI uses this; the static table, text, and JSON paths
/// keep [`format_number`] so their numbers stay exact.
///
/// # Examples
///
/// ```
/// use vct_core::utils::format_compact;
///
/// assert_eq!(format_compact(999), "999");
/// assert_eq!(format_compact(1_000), "1.00K");
/// assert_eq!(format_compact(1_234_567), "1.23M");
/// assert_eq!(format_compact(999_950), "1.00M");
/// ```
pub fn format_compact(n: i64) -> String {
    // Descending so the first match is the largest fitting unit.
    const UNITS: [(f64, char); 4] = [(1e12, 'T'), (1e9, 'B'), (1e6, 'M'), (1e3, 'K')];

    let abs = n.unsigned_abs() as f64;
    if abs < 1000.0 {
        // No suffix needed; render the integer verbatim (keeps the sign).
        return n.to_string();
    }

    // Decimals by magnitude, with a tiny epsilon so e.g. 9.999 classifies as
    // the 1-decimal bucket and renders "10.0K" instead of "10.00K".
    let decimals = |v: f64| -> usize {
        if v < 9.995 {
            2
        } else if v < 99.95 {
            1
        } else {
            0
        }
    };

    let mut idx = UNITS
        .iter()
        .position(|&(threshold, _)| abs >= threshold)
        .unwrap_or(UNITS.len() - 1);
    let mut scaled = abs / UNITS[idx].0;

    // Rounding can push the scaled value to 1000 (e.g. 999_950 -> "1000K");
    // promote one unit up so it reads "1.00M".
    let dec = decimals(scaled);
    let factor = 10f64.powi(dec as i32);
    if (scaled * factor).round() / factor >= 1000.0 && idx > 0 {
        idx -= 1;
        scaled = abs / UNITS[idx].0;
    }

    let dec = decimals(scaled);
    let sign = if n < 0 { "-" } else { "" };
    format!("{sign}{scaled:.dec$}{suffix}", suffix = UNITS[idx].1)
}

/// Formats a USD amount as `"$1,234.56"` (comma-grouped dollars, two decimals).
///
/// Unlike [`format_compact`], money is never abbreviated to K/M/B — that would
/// drop the cents — so long totals are kept readable with thousands separators
/// instead. Negative amounts render as `"-$1.23"`.
///
/// # Examples
///
/// ```
/// use vct_core::utils::format_cost;
///
/// assert_eq!(format_cost(0.0), "$0.00");
/// assert_eq!(format_cost(1234.5), "$1,234.50");
/// ```
pub fn format_cost(cost: f64) -> String {
    let cents = (cost.abs() * 100.0).round() as i64;
    let sign = if cost < 0.0 && cents != 0 { "-" } else { "" };
    format!("{sign}${}.{:02}", format_number(cents / 100), cents % 100)
}

/// Formats a USD amount for the dense interactive dashboard, abbreviating large
/// values with a K/M/B/T suffix.
///
/// Amounts under `$1000` keep full cents via [`format_cost`] (`$74.18`); larger
/// amounts drop to [`format_compact`]'s 3-significant-figure form prefixed with
/// `$` (`$8.63K`, `$14.2K`). This mirrors how the TUI already abbreviates token
/// counts — only the interactive view uses it, while the static table, text, and
/// JSON paths keep [`format_cost`] so their amounts stay exact to the cent.
///
/// # Examples
///
/// ```
/// use vct_core::utils::format_cost_compact;
///
/// assert_eq!(format_cost_compact(74.18), "$74.18");
/// assert_eq!(format_cost_compact(4114.28), "$4.11K");
/// assert_eq!(format_cost_compact(10021.35), "$10.0K");
/// assert_eq!(format_cost_compact(-1500.0), "-$1.50K");
/// ```
pub fn format_cost_compact(cost: f64) -> String {
    if cost.abs() < 1000.0 {
        return format_cost(cost);
    }
    let sign = if cost < 0.0 { "-" } else { "" };
    format!("{sign}${}", format_compact(cost.abs().round() as i64))
}

// Cache for current date (updated once per day)
static DATE_CACHE: RwLock<Option<(NaiveDate, String)>> = RwLock::new(None);

/// Returns today's local date as a `YYYY-MM-DD` string.
///
/// The formatted string is cached behind an `RwLock` and only recomputed
/// when the local calendar day changes, so repeated calls within the same
/// day (e.g. tagging every aggregated row) avoid re-running `strftime`. A
/// poisoned lock degrades gracefully: the value is recomputed without
/// touching the cache rather than panicking.
pub fn get_current_date() -> String {
    let today = Local::now().date_naive();

    // Fast path: read lock to check if cache is valid
    {
        if let Ok(cache) = DATE_CACHE.read()
            && let Some((cached_date, cached_string)) = cache.as_ref()
            && *cached_date == today
        {
            return cached_string.clone();
        }
    }

    // Slow path: write lock to update cache
    let date_string = today.format("%Y-%m-%d").to_string();
    if let Ok(mut cache) = DATE_CACHE.write() {
        *cache = Some((today, date_string.clone()));
    }

    date_string
}

/// Formats the time remaining until `reset_unix` as a compact human string.
///
/// Returns "now" when the reset is in the past or under a minute away
/// (clamped at 0), otherwise the two most significant units: "13m", "2h13m",
/// or "4d2h".
///
/// # Examples
///
/// ```
/// use vct_core::utils::format_duration_until;
///
/// assert_eq!(format_duration_until(100, 100), "now");
/// assert_eq!(format_duration_until(100 + 13 * 60, 100), "13m");
/// assert_eq!(format_duration_until(100 + 2 * 3600 + 13 * 60, 100), "2h13m");
/// assert_eq!(format_duration_until(100 + 4 * 86400 + 2 * 3600, 100), "4d2h");
/// ```
pub fn format_duration_until(reset_unix: i64, now_unix: i64) -> String {
    let secs = (reset_unix - now_unix).max(0);
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        "now".to_string()
    }
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
    fn test_format_number_with_large_integers() {
        assert_eq!(format_number(12345_i64), "12,345");
        assert_eq!(format_number(999_i32), "999");
    }

    #[test]
    fn test_format_compact() {
        // Below 1000: verbatim, no suffix.
        assert_eq!(format_compact(0), "0");
        assert_eq!(format_compact(42), "42");
        assert_eq!(format_compact(999), "999");

        // K / M / B / T with 3 significant figures.
        assert_eq!(format_compact(1_000), "1.00K");
        assert_eq!(format_compact(1_234), "1.23K");
        assert_eq!(format_compact(12_345), "12.3K");
        assert_eq!(format_compact(123_456), "123K");
        assert_eq!(format_compact(1_234_567), "1.23M");
        assert_eq!(format_compact(1_230_000_000), "1.23B");
        assert_eq!(format_compact(2_000_000_000_000), "2.00T");
    }

    #[test]
    fn test_format_compact_rounding_promotion() {
        // Rounding that reaches 1000 within a unit promotes to the next unit.
        // 999_499 rounds to 999K; 999_500 rounds up and promotes to 1.00M.
        assert_eq!(format_compact(999_499), "999K");
        assert_eq!(format_compact(999_500), "1.00M");
        assert_eq!(format_compact(999_999), "1.00M");
        assert_eq!(format_compact(1_000_000), "1.00M");
        // 9.999K classifies into the 1-decimal bucket, not "10.00K".
        assert_eq!(format_compact(9_999), "10.0K");
    }

    #[test]
    fn test_format_compact_negative() {
        assert_eq!(format_compact(-42), "-42");
        assert_eq!(format_compact(-1_234), "-1.23K");
        assert_eq!(format_compact(-1_234_567), "-1.23M");
    }

    #[test]
    fn test_format_cost() {
        assert_eq!(format_cost(0.0), "$0.00");
        assert_eq!(format_cost(1.5), "$1.50");
        assert_eq!(format_cost(1234.56), "$1,234.56");
        assert_eq!(format_cost(1_234_567.891), "$1,234,567.89");
        assert_eq!(format_cost(-5.5), "-$5.50");
    }

    #[test]
    fn test_format_cost_compact() {
        // Under $1000: full cents via format_cost.
        assert_eq!(format_cost_compact(0.0), "$0.00");
        assert_eq!(format_cost_compact(2.42), "$2.42");
        assert_eq!(format_cost_compact(74.18), "$74.18");
        assert_eq!(format_cost_compact(999.99), "$999.99");

        // $1000 and up: K/M/B/T with 3 significant figures.
        assert_eq!(format_cost_compact(1000.0), "$1.00K");
        assert_eq!(format_cost_compact(4114.28), "$4.11K");
        assert_eq!(format_cost_compact(10021.35), "$10.0K");
        assert_eq!(format_cost_compact(14212.22), "$14.2K");
        assert_eq!(format_cost_compact(8_631_270.0), "$8.63M");

        // Negative (credit) amounts keep a leading minus.
        assert_eq!(format_cost_compact(-50.0), "-$50.00");
        assert_eq!(format_cost_compact(-1500.0), "-$1.50K");
    }

    #[test]
    fn test_get_current_date() {
        let date = get_current_date();
        assert_eq!(date.len(), 10); // YYYY-MM-DD format
        assert!(date.contains('-'));
    }
}
