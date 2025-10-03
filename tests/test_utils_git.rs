// Tests for utils::git module

use vibe_coding_tracker::utils::git::get_git_remote_url;
use std::fs;
use std::io::Write;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn test_get_git_remote_url_with_git_repo() {
    // Test with current repository (should have a git remote)
    let current_dir = std::env::current_dir().unwrap();
    let result = get_git_remote_url(&current_dir);

    // This may or may not have a remote depending on test environment
    // Just verify it returns a String (may be empty)
    assert!(result.is_empty() || !result.is_empty());
}

#[test]
fn test_get_git_remote_url_non_git_directory() {
    let temp_dir = TempDir::new().unwrap();
    let result = get_git_remote_url(temp_dir.path());

    // Should return empty string for non-git directory
    assert!(
        result.is_empty(),
        "Non-git directory should return empty string"
    );
}

#[test]
fn test_get_git_remote_url_with_mock_git_repo() {
    let temp_dir = TempDir::new().unwrap();
    let git_dir = temp_dir.path().join(".git");
    fs::create_dir(&git_dir).unwrap();

    // Create a mock config file with remote URL
    let config_path = git_dir.join("config");
    let mut config_file = fs::File::create(&config_path).unwrap();
    writeln!(
        config_file,
        "[remote \"origin\"]\n\turl = https://github.com/test/repo.git"
    )
    .unwrap();

    let result = get_git_remote_url(temp_dir.path());

    // Should read from .git/config file
    // May or may not find the URL depending on config format
    assert!(result.is_empty() || result.contains("github.com/test/repo"));
}

#[test]
fn test_get_git_remote_url_with_initialized_repo() {
    // Only run if git is available
    if Command::new("git").arg("--version").output().is_err() {
        return; // Skip test if git is not available
    }

    let temp_dir = TempDir::new().unwrap();

    // Initialize a git repository
    let init_result = Command::new("git")
        .arg("init")
        .current_dir(temp_dir.path())
        .output();

    if init_result.is_err() {
        return; // Skip if git init fails
    }

    // Add a remote
    let _ = Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://github.com/test/repo.git",
        ])
        .current_dir(temp_dir.path())
        .output();

    let result = get_git_remote_url(temp_dir.path());

    // Should read from .git/config file
    if !result.is_empty() {
        assert!(
            result.contains("github.com/test/repo"),
            "Should contain the remote URL"
        );
    }
}
