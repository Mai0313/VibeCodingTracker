// Unit tests for utils/git.rs
//
// Tests Git remote URL detection utilities

use std::fs::{self, File};
use std::io::Write;
use tempfile::tempdir;
use vibe_coding_tracker::utils::git::get_git_remote_url;

#[test]
fn test_get_git_remote_url_no_git_dir() {
    // Test directory without .git
    let dir = tempdir().unwrap();

    let url = get_git_remote_url(dir.path());
    assert_eq!(url, "");
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
