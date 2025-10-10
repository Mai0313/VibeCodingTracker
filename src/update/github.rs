use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const GITHUB_API_RELEASES_URL: &str =
    "https://api.github.com/repos/Mai0313/VibeCodingTracker/releases/latest";
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Deserialize, Serialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub name: String,
    pub body: Option<String>,
    pub assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GitHubAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

/// Fetches the latest release information from GitHub API
pub fn fetch_latest_release() -> Result<GitHubRelease> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("Failed to create HTTP client")?;

    let response = client
        .get(GITHUB_API_RELEASES_URL)
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

/// Downloads a file from URL to the specified destination path
pub fn download_file(url: &str, dest: &std::path::Path) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("Failed to create HTTP client")?;

    let mut response = client.get(url).send().context("Failed to download file")?;

    if !response.status().is_success() {
        anyhow::bail!("Download failed with status: {}", response.status());
    }

    let mut file = std::fs::File::create(dest)
        .context(format!("Failed to create file: {}", dest.display()))?;

    response
        .copy_to(&mut file)
        .context("Failed to write downloaded content to file")?;

    Ok(())
}
