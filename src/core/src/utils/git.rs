use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};

// Keyed by both the caller's original absolute path and the canonical path,
// so a symlinked alias resolves to the same entry. Never invalidated.
static GIT_URL_CACHE: LazyLock<RwLock<HashMap<PathBuf, String>>> =
    LazyLock::new(|| RwLock::new(HashMap::with_capacity(20)));

/// Returns the `origin` remote URL for the git repository at `cwd`.
///
/// The returned URL has any trailing `.git` stripped. Returns an empty
/// `String` when `cwd` is empty, is not a git working tree, has no `origin`
/// remote, or the config cannot be read; callers treat the empty string as
/// "no remote".
///
/// Results, including the empty ones, are memoized in a process-global cache
/// that is never invalidated, so a directory that gains or changes its
/// `origin` after the first lookup keeps reporting the first answer for the
/// life of the process.
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
/// [`get_git_remote_url`]. Only the `url = <value>` spelling git itself
/// writes is recognized.
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

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_origin_section = trimmed.starts_with("[remote \"origin\"");
            continue;
        }

        if in_origin_section && trimmed.starts_with("url = ") {
            let url = trimmed.trim_start_matches("url = ").trim();
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
        // `origin` wins even when another remote's section comes first.
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
        // Deleting the repo proves the second lookup served the cached entry
        // for the symlink itself, without touching the filesystem again.
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
