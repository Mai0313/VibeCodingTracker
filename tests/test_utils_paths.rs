// Tests for utils::paths module

use codex_usage::utils::paths::{get_current_user, get_machine_id, resolve_paths};

#[test]
fn test_resolve_paths() {
    let result = resolve_paths();
    assert!(result.is_ok(), "Should successfully resolve paths");
    
    let paths = result.unwrap();
    assert!(!paths.home_dir.as_os_str().is_empty(), "Home dir should not be empty");
    
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
