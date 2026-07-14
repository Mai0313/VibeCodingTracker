use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};

// Global cache for Git remote URLs (thread-safe)
// Key: original absolute or canonical path, Value: remote URL
static GIT_URL_CACHE: LazyLock<RwLock<HashMap<PathBuf, String>>> =
    LazyLock::new(|| RwLock::new(HashMap::with_capacity(20)));

/// Returns the `origin` remote URL for the git repository at `cwd`.
///
/// Results are memoized in a process-global, thread-safe cache under both the
/// original absolute path and its canonical path. This avoids repeated
/// canonicalization while still letting symlinked variants share one lookup.
/// The returned URL has any trailing `.git` stripped. Returns an empty
/// `String` when `cwd` is empty, is not a git working tree, has no `origin`
/// remote, or the config cannot be read. Callers treat the empty string as
/// "no remote".
pub fn get_git_remote_url<P: AsRef<Path>>(cwd: P) -> String {
    let cwd = cwd.as_ref();
    if cwd.as_os_str().is_empty() {
        return String::new();
    }

    let original_path = cwd.to_path_buf();

    // Provider logs normally carry absolute workspaces. Check that stable key
    // before doing filesystem work so a hot session never canonicalizes again.
    if cwd.is_absolute()
        && let Some(url) = cached_url(cwd)
    {
        return url;
    }

    let canonical_path = std::fs::canonicalize(cwd).ok();
    if let Some(path) = canonical_path.as_deref()
        && let Some(url) = cached_url(path)
    {
        if cwd.is_absolute()
            && let Ok(mut cache) = GIT_URL_CACHE.write()
        {
            cache.insert(original_path, url.clone());
        }
        return url;
    }

    // Cache miss - perform actual lookup
    let url = get_git_remote_url_impl(cwd);

    // Keep both aliases. The original absolute path avoids repeated
    // canonicalization, while the canonical path deduplicates symlinks.
    if let Ok(mut cache) = GIT_URL_CACHE.write() {
        if cwd.is_absolute() || canonical_path.is_none() {
            cache.insert(original_path, url.clone());
        }
        if let Some(path) = canonical_path {
            cache.insert(path, url.clone());
        }
    }

    url
}

fn cached_url(path: &Path) -> Option<String> {
    GIT_URL_CACHE
        .read()
        .ok()
        .and_then(|cache| cache.get(path).cloned())
}

/// Parses `<cwd>/.git/config` and returns the `[remote "origin"]` URL,
/// or an empty string if absent. The uncached inner worker behind
/// [`get_git_remote_url`].
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_get_git_remote_url_no_git_dir() {
        // Test directory without .git
        let dir = tempdir().unwrap();

        let url = get_git_remote_url(dir.path());
        assert_eq!(url, "");
    }

    #[test]
    fn test_get_git_remote_url_empty_cwd() {
        assert_eq!(get_git_remote_url(""), "");
    }

    #[test]
    fn test_get_git_remote_url_empty_config() {
        // Test with empty git config
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        File::create(&config_path).unwrap();

        let url = get_git_remote_url(dir.path());
        assert_eq!(url, "");
    }

    #[test]
    fn test_get_git_remote_url_with_origin() {
        // Test with valid git config containing origin
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        let mut config = File::create(&config_path).unwrap();
        writeln!(config, "[core]").unwrap();
        writeln!(config, "    repositoryformatversion = 0").unwrap();
        writeln!(config, "[remote \"origin\"]").unwrap();
        writeln!(config, "    url = https://github.com/user/repo").unwrap();
        writeln!(config, "    fetch = +refs/heads/*:refs/remotes/origin/*").unwrap();

        let url = get_git_remote_url(dir.path());
        assert_eq!(url, "https://github.com/user/repo");
    }

    #[test]
    fn test_get_git_remote_url_strips_git_suffix() {
        // Test that .git suffix is removed
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        let mut config = File::create(&config_path).unwrap();
        writeln!(config, "[remote \"origin\"]").unwrap();
        writeln!(config, "    url = https://github.com/user/repo.git").unwrap();

        let url = get_git_remote_url(dir.path());
        assert_eq!(url, "https://github.com/user/repo");
    }

    #[test]
    fn test_get_git_remote_url_ssh_format() {
        // Test with SSH URL format
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        let mut config = File::create(&config_path).unwrap();
        writeln!(config, "[remote \"origin\"]").unwrap();
        writeln!(config, "    url = git@github.com:user/repo.git").unwrap();

        let url = get_git_remote_url(dir.path());
        assert_eq!(url, "git@github.com:user/repo");
    }

    #[test]
    fn test_get_git_remote_url_multiple_remotes() {
        // Test with multiple remotes (should return origin)
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        let mut config = File::create(&config_path).unwrap();
        writeln!(config, "[remote \"upstream\"]").unwrap();
        writeln!(config, "    url = https://github.com/upstream/repo").unwrap();
        writeln!(config, "[remote \"origin\"]").unwrap();
        writeln!(config, "    url = https://github.com/user/repo").unwrap();

        let url = get_git_remote_url(dir.path());
        assert_eq!(url, "https://github.com/user/repo");
    }

    #[test]
    fn test_get_git_remote_url_caching() {
        // Test that results are cached (call twice)
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        let mut config = File::create(&config_path).unwrap();
        writeln!(config, "[remote \"origin\"]").unwrap();
        writeln!(config, "    url = https://github.com/user/repo").unwrap();

        let url1 = get_git_remote_url(dir.path());
        let url2 = get_git_remote_url(dir.path());

        assert_eq!(url1, url2);
        assert_eq!(url1, "https://github.com/user/repo");
    }

    #[cfg(unix)]
    #[test]
    fn test_absolute_symlink_cache_hit_does_not_recanonicalize() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        let alias = dir.path().join("alias");
        let git_dir = repo.join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        let mut config = File::create(git_dir.join("config")).unwrap();
        writeln!(config, "[remote \"origin\"]").unwrap();
        writeln!(config, "    url = https://github.com/user/cached.git").unwrap();
        drop(config);
        symlink(&repo, &alias).unwrap();

        assert_eq!(get_git_remote_url(&alias), "https://github.com/user/cached");

        fs::remove_dir_all(&repo).unwrap();
        assert_eq!(get_git_remote_url(&alias), "https://github.com/user/cached");
    }

    #[test]
    fn test_get_git_remote_url_whitespace() {
        // Test with extra whitespace
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        let mut config = File::create(&config_path).unwrap();
        writeln!(config, "[remote \"origin\"]").unwrap();
        writeln!(config, "    url =   https://github.com/user/repo   ").unwrap();

        let url = get_git_remote_url(dir.path());
        assert_eq!(url, "https://github.com/user/repo");
    }

    #[test]
    fn test_get_git_remote_url_no_origin() {
        // Test with no origin remote
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        let mut config = File::create(&config_path).unwrap();
        writeln!(config, "[remote \"upstream\"]").unwrap();
        writeln!(config, "    url = https://github.com/upstream/repo").unwrap();

        let url = get_git_remote_url(dir.path());
        assert_eq!(url, "");
    }

    #[test]
    fn test_get_git_remote_url_gitlab() {
        // Test with GitLab URL
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        let mut config = File::create(&config_path).unwrap();
        writeln!(config, "[remote \"origin\"]").unwrap();
        writeln!(config, "    url = https://gitlab.com/user/repo.git").unwrap();

        let url = get_git_remote_url(dir.path());
        assert_eq!(url, "https://gitlab.com/user/repo");
    }

    #[test]
    fn test_get_git_remote_url_bitbucket() {
        // Test with Bitbucket URL
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        let mut config = File::create(&config_path).unwrap();
        writeln!(config, "[remote \"origin\"]").unwrap();
        writeln!(config, "    url = https://bitbucket.org/user/repo.git").unwrap();

        let url = get_git_remote_url(dir.path());
        assert_eq!(url, "https://bitbucket.org/user/repo");
    }

    #[test]
    fn test_get_git_remote_url_malformed_config() {
        // Test with malformed config
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        let mut config = File::create(&config_path).unwrap();
        writeln!(config, "this is not valid git config").unwrap();
        writeln!(config, "random text").unwrap();

        let url = get_git_remote_url(dir.path());
        assert_eq!(url, "");
    }

    #[test]
    fn test_get_git_remote_url_url_without_git_suffix() {
        // Test URL that doesn't have .git suffix
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        let mut config = File::create(&config_path).unwrap();
        writeln!(config, "[remote \"origin\"]").unwrap();
        writeln!(config, "    url = https://github.com/user/repo").unwrap();

        let url = get_git_remote_url(dir.path());
        assert_eq!(url, "https://github.com/user/repo");
    }

    #[test]
    fn test_get_git_remote_url_self_hosted() {
        // Test with self-hosted git server
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        let mut config = File::create(&config_path).unwrap();
        writeln!(config, "[remote \"origin\"]").unwrap();
        writeln!(config, "    url = https://git.company.com/team/project.git").unwrap();

        let url = get_git_remote_url(dir.path());
        assert_eq!(url, "https://git.company.com/team/project");
    }

    #[test]
    fn test_get_git_remote_url_path_with_spaces() {
        // Test directory path with spaces (though URL shouldn't have spaces)
        let dir = tempdir().unwrap();
        let subdir = dir.path().join("my project");
        fs::create_dir(&subdir).unwrap();

        let git_dir = subdir.join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        let mut config = File::create(&config_path).unwrap();
        writeln!(config, "[remote \"origin\"]").unwrap();
        writeln!(config, "    url = https://github.com/user/repo").unwrap();

        let url = get_git_remote_url(&subdir);
        assert_eq!(url, "https://github.com/user/repo");
    }

    #[test]
    fn test_get_git_remote_url_empty_url_field() {
        // Test with empty url field
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        let mut config = File::create(&config_path).unwrap();
        writeln!(config, "[remote \"origin\"]").unwrap();
        writeln!(config, "    url = ").unwrap();

        let url = get_git_remote_url(dir.path());
        assert_eq!(url, "");
    }
}
