//! `vct statusline` / `vct statusline ingest` — capture Claude Code rate
//! limits from the statusLine hook's stdin JSON.
//!
//! Claude Code only exposes its rate limits through the statusLine payload, so
//! `ingest` is meant to be wired into an existing statusLine script as a
//! backgrounded, output-discarding line; it parses `rate_limits` and writes
//! them atomically to the cache, never printing and never failing. `run_default`
//! additionally prints a compact one-line status for users who have no other
//! statusLine command of their own.

use crate::models::{ClaudeRateLimitsCache, ClaudeStatuslineInput, ClaudeWindowIn, QuotaWindow};
use std::io::Read;

/// Reads all of stdin into a `String` (empty on any read error).
fn read_stdin_to_string() -> String {
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    buf
}

/// Converts a Claude statusLine window into the normalized [`QuotaWindow`].
fn to_window(w: &ClaudeWindowIn) -> QuotaWindow {
    QuotaWindow {
        used_percent: w.used_percentage,
        resets_at_unix: (w.resets_at > 0).then_some(w.resets_at),
    }
}

/// Parses a Claude statusLine stdin payload into a cache record.
///
/// Returns `None` when the input is not valid JSON or carries no
/// `rate_limits`, so callers can stay silent on malformed input.
pub fn parse_statusline_input(raw: &str) -> Option<(ClaudeRateLimitsCache, ClaudeStatuslineInput)> {
    let input: ClaudeStatuslineInput = serde_json::from_str(raw).ok()?;
    let rl = input.rate_limits.as_ref()?;
    let cache = ClaudeRateLimitsCache {
        fetched_at: chrono::Local::now().timestamp(),
        five_hour: rl.five_hour.as_ref().map(to_window),
        seven_day: rl.seven_day.as_ref().map(to_window),
    };
    Some((cache, input))
}

/// Writes the cache atomically to
/// `~/.vibe_coding_tracker/claude_rate_limits.json`.
fn write_cache(cache: &ClaudeRateLimitsCache) -> anyhow::Result<()> {
    let path = crate::utils::get_claude_rate_limits_path()?;
    crate::utils::write_json_atomic(&path, cache)
}

/// `vct statusline ingest` — read stdin, cache rate limits, print nothing.
///
/// Deliberately infallible from the caller's view: all errors are swallowed so
/// a misbehaving cache write can never disturb the Claude statusLine, and it
/// always exits successfully.
pub fn run_ingest() {
    let raw = read_stdin_to_string();
    if let Some((cache, _)) = parse_statusline_input(&raw) {
        let _ = write_cache(&cache);
    }
}

/// `vct statusline` — cache rate limits and print one status line.
///
/// For users who have no other statusLine command: writes the same cache as
/// `ingest`, then prints a compact one-liner. A missing or malformed payload
/// still prints a minimal line so the statusLine is never blank.
pub fn run_default() -> anyhow::Result<()> {
    let raw = read_stdin_to_string();
    match parse_statusline_input(&raw) {
        Some((cache, input)) => {
            let _ = write_cache(&cache);
            println!("{}", render_status_line(&cache, &input));
        }
        None => println!("Claude"),
    }
    Ok(())
}

/// Builds a compact one-line status string from the cached rate limits.
fn render_status_line(cache: &ClaudeRateLimitsCache, input: &ClaudeStatuslineInput) -> String {
    let model = input
        .model
        .as_ref()
        .and_then(|m| m.get("display_name").or_else(|| m.get("id")))
        .and_then(|v| v.as_str())
        .unwrap_or("Claude");

    let mut parts = vec![model.to_string()];
    if let Some(w) = &cache.five_hour {
        parts.push(format!("5h {:.0}%", w.used_percent));
    }
    if let Some(w) = &cache.seven_day {
        parts.push(format!("7d {:.0}%", w.used_percent));
    }
    parts.join("  ·  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_payload_with_both_windows() {
        let raw = r#"{"rate_limits":{"five_hour":{"used_percentage":16,"resets_at":1782765000},"seven_day":{"used_percentage":28.0,"resets_at":1782990000}},"model":{"display_name":"Opus"}}"#;
        let (cache, _input) = parse_statusline_input(raw).expect("should parse");
        assert_eq!(cache.five_hour.as_ref().unwrap().used_percent, 16.0);
        assert_eq!(
            cache.five_hour.as_ref().unwrap().resets_at_unix,
            Some(1782765000)
        );
        assert_eq!(cache.seven_day.as_ref().unwrap().used_percent, 28.0);
        assert!(cache.fetched_at > 0);
    }

    #[test]
    fn returns_none_without_rate_limits() {
        assert!(parse_statusline_input(r#"{"model":{"id":"x"}}"#).is_none());
    }

    #[test]
    fn returns_none_on_garbage() {
        assert!(parse_statusline_input("not json at all").is_none());
        assert!(parse_statusline_input("").is_none());
    }

    #[test]
    fn renders_one_line_with_percentages() {
        let raw = r#"{"rate_limits":{"five_hour":{"used_percentage":16,"resets_at":1782765000},"seven_day":{"used_percentage":28.0,"resets_at":1782990000}},"model":{"display_name":"Opus"}}"#;
        let (cache, input) = parse_statusline_input(raw).unwrap();
        let line = render_status_line(&cache, &input);
        assert!(line.contains("Opus"));
        assert!(line.contains("5h 16%"));
        assert!(line.contains("7d 28%"));
    }
}
