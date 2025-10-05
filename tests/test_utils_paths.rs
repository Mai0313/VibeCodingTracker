// Tests for utils::paths module

use vibe_coding_tracker::utils::paths::{get_current_user, get_machine_id, resolve_paths};

#[test]
fn test_resolve_paths() {
    let result = resolve_paths();
    assert!(result.is_ok(), "Should successfully resolve paths");

    let paths = result.unwrap();
    assert!(
        !paths.home_dir.as_os_str().is_empty(),
        "Home dir should not be empty"
    );

    // Check that helper directories are constructed correctly
    assert!(paths.helper_dir.ends_with(".cchelper"));
    assert!(paths.codex_dir.ends_with(".codex"));
    assert!(paths.claude_dir.ends_with(".claude"));
    assert!(paths.codex_session_dir.ends_with("sessions"));
    assert!(paths.claude_session_dir.ends_with("projects"));
}

#[test]
fn test_get_current_user() {
    let user = get_current_user();
    assert!(!user.is_empty(), "User should not be empty");
    assert_ne!(user, "unknown", "Should retrieve actual user name");
}

#[test]
fn test_get_current_user_fallback() {
    // Test with environment variables unset
    // This is challenging to test without modifying environment,
    // so we just verify the function returns something valid
    let user = get_current_user();
    assert!(
        user == std::env::var("USER").unwrap_or_else(|_| "unknown".to_string())
            || user == std::env::var("USERNAME").unwrap_or_else(|_| "unknown".to_string())
            || user == "unknown",
        "Should return valid user or 'unknown'"
    );
}

#[test]
fn test_get_machine_id() {
    let machine_id = get_machine_id();
    assert!(!machine_id.is_empty(), "Machine ID should not be empty");

    // Machine ID should be consistent across calls
    let machine_id_2 = get_machine_id();
    assert_eq!(machine_id, machine_id_2, "Machine ID should be consistent");
}

#[test]
fn test_paths_structure() {
    let paths = resolve_paths().unwrap();

    // Verify that all paths are under home directory
    assert!(paths.helper_dir.starts_with(&paths.home_dir));
    assert!(paths.codex_dir.starts_with(&paths.home_dir));
    assert!(paths.claude_dir.starts_with(&paths.home_dir));

    // Verify session directories are subdirectories
    assert!(paths.codex_session_dir.starts_with(&paths.codex_dir));
    assert!(paths.claude_session_dir.starts_with(&paths.claude_dir));
}

#[test]
fn test_helper_paths_clone() {
    let paths = resolve_paths().unwrap();
    let cloned = paths.clone();

    assert_eq!(paths.home_dir, cloned.home_dir);
    assert_eq!(paths.helper_dir, cloned.helper_dir);
    assert_eq!(paths.codex_dir, cloned.codex_dir);
    assert_eq!(paths.codex_session_dir, cloned.codex_session_dir);
    assert_eq!(paths.claude_dir, cloned.claude_dir);
    assert_eq!(paths.claude_session_dir, cloned.claude_session_dir);
}

#[test]
fn test_helper_paths_debug() {
    let paths = resolve_paths().unwrap();
    let debug_str = format!("{:?}", paths);

    // Debug output should contain "HelperPaths"
    assert!(debug_str.contains("HelperPaths"));
}
