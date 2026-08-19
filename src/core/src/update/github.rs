//! Minimal GitHub Releases client used by the self-updater.
//!
//! Wraps the "latest release" REST endpoint and a streaming file download
//! using a blocking `reqwest` client. Only the fields the updater needs are
//! deserialized from the API response.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// GitHub REST endpoint for the repository's latest release.
const GITHUB_API_RELEASES_URL: &str =
    "https://api.github.com/repos/Mai0313/VibeCodingTracker/releases/latest";
/// `User-Agent` header value (`vibe_coding_tracker/<version>`), required by the
/// GitHub API. Pinned to the product name rather than this crate's name.
const USER_AGENT: &str = concat!("vibe_coding_tracker/", env!("CARGO_PKG_VERSION"));
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const RELEASE_TIMEOUT: Duration = Duration::from_secs(5);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// A GitHub release, deserialized from the Releases API.
#[derive(Debug, Deserialize, Serialize)]
pub struct GitHubRelease {
    /// Git tag the release points at (e.g. `"v0.1.6"`).
    pub tag_name: String,
    /// Human-readable release title.
    pub name: String,
    /// Release notes body, absent when the release has none.
    pub body: Option<String>,
    /// Downloadable assets attached to the release.
    pub assets: Vec<GitHubAsset>,
}

/// A single downloadable file attached to a [`GitHubRelease`].
#[derive(Debug, Deserialize, Serialize)]
pub struct GitHubAsset {
    /// Asset file name, matched against the platform pattern.
    pub name: String,
    /// Direct download URL for the asset.
    pub browser_download_url: String,
    /// Asset size in bytes.
    pub size: u64,
}

/// Fetches the repository's latest release from the GitHub API.
///
/// # Errors
///
/// Returns an error if the HTTP client cannot be built, if the request fails,
/// if GitHub responds with a non-success status, or if the response body is
/// not the expected release JSON.
pub fn fetch_latest_release() -> Result<GitHubRelease> {
    fetch_latest_release_from(GITHUB_API_RELEASES_URL)
}

/// Fetches the latest release from an explicit endpoint URL.
///
/// The injectable counterpart of [`fetch_latest_release`]: production passes the
/// real GitHub endpoint, tests point `url` at a local mock server so no real API
/// is reached.
///
/// # Errors
///
/// Returns an error if the HTTP client cannot be built, if the request fails,
/// if the server responds with a non-success status, or if the response body is
/// not the expected release JSON.
pub fn fetch_latest_release_from(url: &str) -> Result<GitHubRelease> {
    fetch_latest_release_from_with_timeout(url, RELEASE_TIMEOUT)
}

fn fetch_latest_release_from_with_timeout(url: &str, timeout: Duration) -> Result<GitHubRelease> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT.min(timeout))
        .timeout(timeout)
        .build()
        .context("Failed to create HTTP client")?;

    let response = client
        .get(url)
        .send()
        .context("Failed to fetch release information from GitHub")?;

    if !response.status().is_success() {
        anyhow::bail!("GitHub API returned error status: {}", response.status());
    }

    let release: GitHubRelease = response
        .json()
        .context("Failed to parse GitHub release JSON")?;

    Ok(release)
}

/// Downloads the file at `url` and writes it to `dest`.
///
/// Streams the response body straight to the destination file rather than
/// buffering it in memory, and returns the number of bytes written.
///
/// # Errors
///
/// Returns an error if the HTTP client cannot be built, if the request fails,
/// if the server responds with a non-success status, if `dest` cannot be
/// created, or if writing the body to disk fails.
pub fn download_file(url: &str, dest: &std::path::Path) -> Result<u64> {
    download_file_with_timeout(url, dest, DOWNLOAD_TIMEOUT)
}

fn download_file_with_timeout(url: &str, dest: &std::path::Path, timeout: Duration) -> Result<u64> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT.min(timeout))
        .timeout(timeout)
        .build()
        .context("Failed to create HTTP client")?;

    let mut response = client.get(url).send().context("Failed to download file")?;

    if !response.status().is_success() {
        anyhow::bail!("Download failed with status: {}", response.status());
    }

    let mut file = std::fs::File::create(dest)
        .context(format!("Failed to create file: {}", dest.display()))?;

    let bytes = response
        .copy_to(&mut file)
        .context("Failed to write downloaded content to file")?;

    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;
    use serde_json::json;

    #[test]
    fn fetch_latest_release_parses_mock_response() {
        let server = MockServer::start();
        let endpoint = server.mock(|when, then| {
            when.method(GET).path("/releases/latest");
            then.status(200).json_body(json!({
                "tag_name": "v1.2.3",
                "name": "Release 1.2.3",
                "body": "notes",
                "assets": [
                    {
                        "name": "vct-linux-x64.tar.gz",
                        "browser_download_url": "https://example.test/vct-linux-x64.tar.gz",
                        "size": 42
                    }
                ]
            }));
        });

        let release = fetch_latest_release_from(&server.url("/releases/latest"))
            .expect("should parse the release JSON");
        endpoint.assert();
        assert_eq!(release.tag_name, "v1.2.3");
        assert_eq!(release.assets.len(), 1);
        assert_eq!(release.assets[0].name, "vct-linux-x64.tar.gz");
        assert_eq!(release.assets[0].size, 42);
    }

    #[test]
    fn fetch_latest_release_errors_on_non_success() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/releases/latest");
            then.status(404);
        });
        assert!(fetch_latest_release_from(&server.url("/releases/latest")).is_err());
    }

    #[test]
    fn download_file_streams_body_to_disk() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/asset.bin");
            then.status(200).body("binary-contents");
        });
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("asset.bin");

        let bytes =
            download_file(&server.url("/asset.bin"), &dest).expect("download should succeed");
        assert_eq!(bytes, 15);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "binary-contents");
    }

    #[test]
    fn download_file_errors_on_non_success() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/missing.bin");
            then.status(500);
        });
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("missing.bin");
        assert!(download_file(&server.url("/missing.bin"), &dest).is_err());
    }

    #[test]
    fn release_fetch_has_a_bounded_timeout() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/slow");
            then.status(200)
                .delay(Duration::from_millis(200))
                .json_body(json!({
                    "tag_name": "v1.2.3",
                    "name": "Release 1.2.3",
                    "body": null,
                    "assets": []
                }));
        });

        let error =
            fetch_latest_release_from_with_timeout(&server.url("/slow"), Duration::from_millis(20))
                .expect_err("slow response must time out");
        assert!(error.to_string().contains("Failed to fetch release"));
    }

    #[test]
    fn asset_download_has_a_bounded_timeout() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/slow-asset");
            then.status(200)
                .delay(Duration::from_millis(200))
                .body("binary-contents");
        });
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("asset.bin");

        let error = download_file_with_timeout(
            &server.url("/slow-asset"),
            &dest,
            Duration::from_millis(20),
        )
        .expect_err("slow download must time out");
        assert!(error.to_string().contains("Failed to download file"));
    }
}
