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
    assert!(paths.codex_dir.ends_with(".codex"));
    assert!(paths.claude_dir.ends_with(".claude"));
    assert!(paths.gemini_dir.ends_with(".gemini"));
    assert!(paths.codex_session_dir.ends_with("sessions"));
    assert!(paths.claude_session_dir.ends_with("projects"));
    assert!(paths.gemini_session_dir.ends_with("tmp"));
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
    assert!(paths.codex_dir.starts_with(&paths.home_dir));
    assert!(paths.claude_dir.starts_with(&paths.home_dir));
    assert!(paths.gemini_dir.starts_with(&paths.home_dir));

    // Verify session directories are subdirectories
    assert!(paths.codex_session_dir.starts_with(&paths.codex_dir));
    assert!(paths.claude_session_dir.starts_with(&paths.claude_dir));
    assert!(paths.gemini_session_dir.starts_with(&paths.gemini_dir));
}

#[test]
fn test_helper_paths_clone() {
    let paths = resolve_paths().unwrap();
    let cloned = paths.clone();

    assert_eq!(paths.home_dir, cloned.home_dir);
    assert_eq!(paths.codex_dir, cloned.codex_dir);
    assert_eq!(paths.codex_session_dir, cloned.codex_session_dir);
    assert_eq!(paths.claude_dir, cloned.claude_dir);
    assert_eq!(paths.claude_session_dir, cloned.claude_session_dir);
    assert_eq!(paths.gemini_dir, cloned.gemini_dir);
    assert_eq!(paths.gemini_session_dir, cloned.gemini_session_dir);
}

#[test]
fn test_helper_paths_debug() {
    let paths = resolve_paths().unwrap();
    let debug_str = format!("{:?}", paths);

    // Debug output should contain "HelperPaths"
    assert!(debug_str.contains("HelperPaths"));
}

#[test]
fn test_get_machine_id_on_linux() {
    let machine_id = get_machine_id();
    assert!(
        !machine_id.is_empty(),
        "Machine ID should not be empty on Linux"
    );

    // On Linux, machine ID should either be from /etc/machine-id or hostname
    // Just verify it's not the fallback value
    assert_ne!(machine_id, "", "Machine ID should have a value");
}

#[test]
fn test_paths_are_absolute() {
    let paths = resolve_paths().unwrap();

    assert!(paths.home_dir.is_absolute(), "Home dir should be absolute");
    assert!(
        paths.codex_dir.is_absolute(),
        "Codex dir should be absolute"
    );
    assert!(
        paths.codex_session_dir.is_absolute(),
        "Codex session dir should be absolute"
    );
    assert!(
        paths.claude_dir.is_absolute(),
        "Claude dir should be absolute"
    );
    assert!(
        paths.claude_session_dir.is_absolute(),
        "Claude session dir should be absolute"
    );
    assert!(
        paths.gemini_dir.is_absolute(),
        "Gemini dir should be absolute"
    );
    assert!(
        paths.gemini_session_dir.is_absolute(),
        "Gemini session dir should be absolute"
    );
}

#[test]
fn test_codex_dir_name() {
    let paths = resolve_paths().unwrap();
    let codex_name = paths.codex_dir.file_name().unwrap().to_str().unwrap();
    assert_eq!(codex_name, ".codex", "Codex dir should be named .codex");
}

#[test]
fn test_claude_dir_name() {
    let paths = resolve_paths().unwrap();
    let claude_name = paths.claude_dir.file_name().unwrap().to_str().unwrap();
    assert_eq!(claude_name, ".claude", "Claude dir should be named .claude");
}

#[test]
fn test_session_subdirs() {
    let paths = resolve_paths().unwrap();

    let codex_session_name = paths
        .codex_session_dir
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(
        codex_session_name, "sessions",
        "Codex session dir should be named 'sessions'"
    );

    let claude_session_name = paths
        .claude_session_dir
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(
        claude_session_name, "projects",
        "Claude session dir should be named 'projects'"
    );
}

#[test]
fn test_get_current_user_with_env() {
    // This test verifies that get_current_user() returns a user
    let user = get_current_user();

    // Should either match USER or USERNAME env var, or be "unknown"
    let env_user = std::env::var("USER").or_else(|_| std::env::var("USERNAME"));

    if let Ok(env_val) = env_user {
        assert_eq!(user, env_val, "Should match environment variable");
    } else {
        assert_eq!(user, "unknown", "Should be 'unknown' if env vars not set");
    }
}
