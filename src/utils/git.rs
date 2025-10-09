use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::{LazyLock, RwLock};

// Global cache for Git remote URLs (thread-safe)
// Key: canonical path, Value: remote URL
static GIT_URL_CACHE: LazyLock<RwLock<HashMap<String, String>>> =
    LazyLock::new(|| RwLock::new(HashMap::with_capacity(20)));

/// Get git remote origin URL from a directory (with caching)
///
/// This function caches results to avoid repeated file I/O operations.
/// The cache is keyed by canonical path to handle symbolic links correctly.
pub fn get_git_remote_url<P: AsRef<Path>>(cwd: P) -> String {
    // Try to get canonical path for better cache hits
    let canonical_path = std::fs::canonicalize(cwd.as_ref())
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| cwd.as_ref().to_string_lossy().to_string());

    // Check cache first
    if let Ok(cache) = GIT_URL_CACHE.read() {
        if let Some(url) = cache.get(&canonical_path) {
            return url.clone();
        }
    }

    // Cache miss - perform actual lookup
    let url = get_git_remote_url_impl(cwd.as_ref());

    // Cache the result
    if let Ok(mut cache) = GIT_URL_CACHE.write() {
        cache.insert(canonical_path, url.clone());
    }

    url
}

/// Internal implementation of Git remote URL lookup
fn get_git_remote_url_impl(cwd: &Path) -> String {
    let git_config = cwd.join(".git").join("config");

    let file = match File::open(&git_config) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };

    let reader = BufReader::new(file);
    let mut in_origin_section = false;

    for line in reader.lines().map_while(Result::ok) {
        let trimmed = line.trim();

        // Check for section headers
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_origin_section = trimmed.starts_with("[remote \"origin\"");
            continue;
        }

        // Look for url in origin section
        if in_origin_section && trimmed.starts_with("url = ") {
            let url = trimmed.trim_start_matches("url = ").trim();
            // Remove .git suffix if present
            let url = url.strip_suffix(".git").unwrap_or(url);
            return url.to_string();
        }
    }

    String::new()
}
