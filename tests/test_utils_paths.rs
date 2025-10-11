// Unit tests for utils/paths.rs
//
// Tests path resolution and machine identification utilities

use vibe_coding_tracker::utils::paths::{
    get_current_user, get_machine_id, resolve_paths,
};

#[test]
fn test_resolve_paths() {
    // Test that resolve_paths returns valid paths
    let result = resolve_paths();
    
    // Should succeed (home directory should exist)
    assert!(result.is_ok());
    
    if let Ok(paths) = result {
        // Home directory should exist
        assert!(paths.home_dir.exists());
        
        // All paths should be absolute
        assert!(paths.home_dir.is_absolute());
        assert!(paths.codex_dir.is_absolute());
        assert!(paths.claude_dir.is_absolute());
        assert!(paths.copilot_dir.is_absolute());
        assert!(paths.gemini_dir.is_absolute());
        assert!(paths.cache_dir.is_absolute());
        
        // Verify directory names
        assert!(paths.codex_dir.ends_with(".codex"));
        assert!(paths.claude_dir.ends_with(".claude"));
        assert!(paths.copilot_dir.ends_with(".copilot"));
        assert!(paths.gemini_dir.ends_with(".gemini"));
        assert!(paths.cache_dir.ends_with(".vibe_coding_tracker"));
        
        // Verify session directories
        assert!(paths.codex_session_dir.ends_with("sessions"));
        assert!(paths.claude_session_dir.ends_with("projects"));
        assert!(paths.copilot_session_dir.ends_with("history-session-state"));
        assert!(paths.gemini_session_dir.ends_with("tmp"));
    }
}

#[test]
fn test_get_current_user() {
    // Test getting current user
    let user = get_current_user();
    
    // Should not be empty
    assert!(!user.is_empty());
    
    // Should not contain invalid characters
    assert!(!user.contains('\0'));
    
    // Should be reasonable length
    assert!(user.len() < 256);
}

#[test]
fn test_get_machine_id() {
    // Test getting machine ID
    let machine_id = get_machine_id();
    
    // Should not be empty
    assert!(!machine_id.is_empty());
    
    // Should not contain null characters
    assert!(!machine_id.contains('\0'));
    
    // Should be reasonable length
    assert!(machine_id.len() < 1024);
}

#[test]
fn test_get_machine_id_cached() {
    // Test that machine ID is cached (same value on multiple calls)
    let id1 = get_machine_id();
    let id2 = get_machine_id();
    let id3 = get_machine_id();
    
    assert_eq!(id1, id2);
    assert_eq!(id2, id3);
}

#[test]
fn test_paths_structure() {
    // Test that paths structure is properly constructed
    let paths = resolve_paths().unwrap();
    
    // Codex paths
    assert_eq!(
        paths.codex_session_dir,
        paths.codex_dir.join("sessions")
    );
    
    // Claude paths
    assert_eq!(
        paths.claude_session_dir,
        paths.claude_dir.join("projects")
    );
    
    // Copilot paths
    assert_eq!(
        paths.copilot_session_dir,
        paths.copilot_dir.join("history-session-state")
    );
    
    // Gemini paths
    assert_eq!(
        paths.gemini_session_dir,
        paths.gemini_dir.join("tmp")
    );
}

#[test]
fn test_paths_all_under_home() {
    // Test that all paths are under home directory
    let paths = resolve_paths().unwrap();
    
    assert!(paths.codex_dir.starts_with(&paths.home_dir));
    assert!(paths.claude_dir.starts_with(&paths.home_dir));
    assert!(paths.copilot_dir.starts_with(&paths.home_dir));
    assert!(paths.gemini_dir.starts_with(&paths.home_dir));
    assert!(paths.cache_dir.starts_with(&paths.home_dir));
}

#[test]
fn test_cache_dir_name() {
    // Test that cache directory has correct name
    let paths = resolve_paths().unwrap();
    let cache_name = paths.cache_dir.file_name().unwrap();
    
    assert_eq!(cache_name, ".vibe_coding_tracker");
}

#[test]
fn test_session_dirs_are_subdirs() {
    // Test that session directories are subdirectories of their parent
    let paths = resolve_paths().unwrap();
    
    assert!(paths.codex_session_dir.starts_with(&paths.codex_dir));
    assert!(paths.claude_session_dir.starts_with(&paths.claude_dir));
    assert!(paths.copilot_session_dir.starts_with(&paths.copilot_dir));
    assert!(paths.gemini_session_dir.starts_with(&paths.gemini_dir));
}

#[test]
fn test_get_current_user_not_empty() {
    // Test that current user is never empty (should at least return "unknown")
    let user = get_current_user();
    assert!(!user.is_empty());
}

#[test]
fn test_get_machine_id_not_empty() {
    // Test that machine ID is never empty
    let machine_id = get_machine_id();
    assert!(!machine_id.is_empty());
}

#[test]
fn test_paths_debug_format() {
    // Test that HelperPaths can be debug formatted
    let paths = resolve_paths().unwrap();
    let debug_str = format!("{:?}", paths);
    
    // Should contain key fields
    assert!(debug_str.contains("home_dir"));
    assert!(debug_str.contains("cache_dir"));
}

#[test]
fn test_paths_clone() {
    // Test that HelperPaths can be cloned
    let paths1 = resolve_paths().unwrap();
    let paths2 = paths1.clone();
    
    assert_eq!(paths1.home_dir, paths2.home_dir);
    assert_eq!(paths1.cache_dir, paths2.cache_dir);
    assert_eq!(paths1.codex_dir, paths2.codex_dir);
}

#[test]
fn test_resolve_paths_deterministic() {
    // Test that resolve_paths returns the same paths on multiple calls
    let paths1 = resolve_paths().unwrap();
    let paths2 = resolve_paths().unwrap();
    
    assert_eq!(paths1.home_dir, paths2.home_dir);
    assert_eq!(paths1.codex_dir, paths2.codex_dir);
    assert_eq!(paths1.claude_dir, paths2.claude_dir);
    assert_eq!(paths1.copilot_dir, paths2.copilot_dir);
    assert_eq!(paths1.gemini_dir, paths2.gemini_dir);
    assert_eq!(paths1.cache_dir, paths2.cache_dir);
}

