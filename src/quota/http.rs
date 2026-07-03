//! Shared HTTP helpers for the quota fetchers.
//!
//! One blocking client is built without a default `User-Agent` and shared
//! across the provider workers; each request sets its own UA. Also holds the
//! ISO-timestamp conversion used by the fetchers.

use anyhow::{Context, Result};

/// Per-request User-Agent for the Codex wham client.
pub const CODEX_UA: &str = "codex-cli";

/// Builds the shared blocking HTTP client (8s timeout, no default UA).
///
/// The UA is intentionally left unset so each request can supply its own via a
/// header; setting it on both the client and the request would send a duplicate
/// `User-Agent`.
///
/// # Errors
///
/// Returns an error if the client cannot be constructed.
pub fn build_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .context("Failed to build HTTP client")
}

/// Parses an ISO-8601 timestamp into Unix **seconds**, or `None` on failure.
///
/// [`crate::utils::parse_iso_timestamp`] returns Unix *milliseconds* and `0` on
/// failure; this divides by 1000 and maps `0` to `None` so a bad timestamp
/// renders as "no reset" rather than the epoch.
pub fn iso_to_unix_secs(s: &str) -> Option<i64> {
    let ms = crate::utils::parse_iso_timestamp(s);
    (ms > 0).then_some(ms / 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_to_unix_secs_handles_bad_input() {
        assert_eq!(iso_to_unix_secs("not-a-date"), None);
        assert!(iso_to_unix_secs("2026-07-03T17:09:59.651608+00:00").unwrap() > 0);
    }
}
