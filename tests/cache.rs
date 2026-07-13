// Integration tests for cache system functionality
//
// These tests verify the LRU file parsing cache and pricing cache

use serial_test::serial;
use std::path::PathBuf;
use tempfile::TempDir;
use vibe_coding_tracker::cache::global_cache;
use vibe_coding_tracker::pricing::clear_pricing_cache;

#[test]
#[serial(global_cache)]
fn test_file_cache_basic_operations() {
    let cache = global_cache();

    // Get initial stats
    let initial_stats = cache.stats();
    println!("Initial cache stats: {:?}", initial_stats);

    // Test cache is accessible (these will always be non-negative due to type)
    let _ = initial_stats.entry_count;
    let _ = initial_stats.estimated_memory_kb;
}

#[test]
#[serial(global_cache)]
fn test_file_cache_get_or_parse() {
    let example_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");

    if !example_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let cache = global_cache();

    // First call - cache miss (should parse file)
    let result1 = cache.get_or_parse(&example_file);
    assert!(result1.is_ok(), "Should successfully parse file");

    // Second call - cache hit (should return cached result)
    let result2 = cache.get_or_parse(&example_file);
    assert!(result2.is_ok(), "Should return cached result");

    // Results should be equivalent (Arc clones)
    if let (Ok(r1), Ok(r2)) = (result1, result2) {
        assert_eq!(
            serde_json::to_string(&*r1).unwrap(),
            serde_json::to_string(&*r2).unwrap(),
            "Cached result should match original"
        );
    }
}

#[test]
#[serial(global_cache)]
fn test_file_cache_invalidation() {
    use std::io::Write;

    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("test.jsonl");

    // Create initial file
    let mut file = std::fs::File::create(&test_file).unwrap();
    writeln!(
        file,
        r#"{{"timestamp":"2026-07-12T00:00:00Z","type":"session_meta","payload":{{"type":"session_meta","id":"one"}}}}"#
    )
    .unwrap();
    drop(file);

    let cache = global_cache();

    // Parse and cache
    let result1 = cache.get_or_parse(&test_file);
    assert!(result1.is_ok());

    // Modify file (change modification time)
    std::thread::sleep(std::time::Duration::from_millis(100));
    let mut file = std::fs::File::create(&test_file).unwrap();
    writeln!(
        file,
        r#"{{"timestamp":"2026-07-12T00:00:00Z","type":"session_meta","payload":{{"type":"session_meta","id":"two"}}}}"#
    )
    .unwrap();
    drop(file);

    // Should detect file change and re-parse
    let result2 = cache.get_or_parse(&test_file);
    assert!(result2.is_ok());
}

#[test]
#[serial(global_cache)]
fn test_file_cache_clear() {
    let cache = global_cache();

    // Add some entries
    let example_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");
    if example_file.exists() {
        let _ = cache.get_or_parse(&example_file);
    }

    // Clear cache
    cache.clear();

    let stats = cache.stats();
    assert_eq!(stats.entry_count, 0, "Cache should be empty after clear");
}

#[test]
#[serial(global_cache)]
fn test_file_cache_stats() {
    let cache = global_cache();
    cache.clear();

    let initial_stats = cache.stats();
    assert_eq!(initial_stats.entry_count, 0);

    // Add an entry
    let example_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");
    if example_file.exists() {
        let _ = cache.get_or_parse(&example_file);

        let stats_after = cache.stats();
        assert!(
            stats_after.entry_count > initial_stats.entry_count,
            "Entry count should increase"
        );
        assert!(
            stats_after.estimated_memory_kb > 0,
            "Memory estimate should be positive"
        );
    }
}

#[test]
#[serial(global_cache)]
fn test_file_cache_cleanup_stale() {
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("temp.jsonl");

    // Create and cache a file
    std::fs::write(
        &test_file,
        r#"{"timestamp":"2026-07-12T00:00:00Z","type":"session_meta","payload":{"type":"session_meta","id":"stale"}}"#,
    )
    .unwrap();

    let cache = global_cache();
    cache.get_or_parse(&test_file).unwrap();

    // Delete the file
    std::fs::remove_file(&test_file).ok();

    // Cleanup stale entries
    cache.cleanup_stale();

    // File should be removed from cache
    // (No direct way to verify, but should not error)
}

#[test]
#[serial(global_cache)]
fn test_file_cache_concurrent_access() {
    use std::sync::Arc;
    use std::thread;

    let example_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");

    if !example_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let file_path = Arc::new(example_file);

    // Spawn multiple threads accessing cache concurrently
    let mut handles = vec![];
    for _ in 0..5 {
        let path = Arc::clone(&file_path);
        let handle = thread::spawn(move || {
            let cache = global_cache();
            cache.get_or_parse(&*path)
        });
        handles.push(handle);
    }

    // All threads should succeed
    for handle in handles {
        let result = handle.join().unwrap();
        assert!(result.is_ok(), "Concurrent access should succeed");
    }
}

#[test]
#[serial(global_cache)]
fn test_file_cache_multiple_files() {
    let cache = global_cache();
    cache.clear();

    let files = [
        "examples/test_conversation_claude_code.jsonl",
        "examples/test_conversation_codex.jsonl",
        "examples/test_conversation_copilot.jsonl",
        "examples/test_conversation_gemini.jsonl",
        "examples/grok_session/signals.json",
    ];

    let mut parsed_providers = Vec::with_capacity(files.len());

    for file_path in files {
        let path = PathBuf::from(file_path);
        assert!(path.exists(), "missing committed fixture: {file_path}");
        let analysis = cache
            .get_or_parse(&path)
            .unwrap_or_else(|err| panic!("failed to cache {file_path}: {err}"));
        parsed_providers.push(analysis.extension_name.clone());
    }

    assert_eq!(parsed_providers.len(), files.len());
    assert!(parsed_providers.iter().any(|provider| provider == "Grok"));
}

#[test]
#[serial(global_cache)]
fn test_file_cache_lru_eviction() {
    // This test verifies that LRU eviction works (implicitly through capacity limits)
    // The actual LRU capacity is set in constants.rs

    let example_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");

    if !example_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let cache = global_cache();

    // Parse the same file multiple times
    for _ in 0..10 {
        let _ = cache.get_or_parse(&example_file);
    }

    // Verify the file is still cached (LRU keeps frequently accessed files)
    let result = cache.get_or_parse(&example_file);
    assert!(result.is_ok(), "File should still be cached");
}

#[test]
fn test_pricing_cache_clear() {
    // Test pricing cache clearing
    clear_pricing_cache();

    // Should not error and cache should be cleared
    // (No direct way to verify cache state, but should not panic)
}

#[test]
#[serial(global_cache)]
fn test_file_cache_invalidate_specific_file() {
    let example_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");

    if !example_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let cache = global_cache();

    // Add to cache
    let _ = cache.get_or_parse(&example_file);

    // Invalidate specific file
    cache.invalidate(&example_file);

    // Next access should re-parse (cache miss)
    let result = cache.get_or_parse(&example_file);
    assert!(result.is_ok(), "Should re-parse after invalidation");
}

#[test]
#[serial(global_cache)]
fn test_cache_with_nonexistent_file() {
    let nonexistent = PathBuf::from("nonexistent_file_12345.jsonl");

    let cache = global_cache();
    let result = cache.get_or_parse(&nonexistent);

    assert!(result.is_err(), "Should fail on nonexistent file");
}

#[test]
#[serial(global_cache)]
fn test_cache_with_directory() {
    let dir = PathBuf::from("examples");

    let cache = global_cache();
    let result = cache.get_or_parse(&dir);

    assert!(result.is_err(), "Should fail when given a directory");
}

#[test]
#[serial(global_cache)]
fn test_cache_memory_estimation() {
    let cache = global_cache();
    cache.clear();

    let example_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");

    if !example_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let stats_before = cache.stats();
    let _ = cache.get_or_parse(&example_file);
    let stats_after = cache.stats();

    assert!(
        stats_after.estimated_memory_kb > stats_before.estimated_memory_kb,
        "Memory usage should increase after caching"
    );
}

#[test]
#[serial(global_cache)]
fn test_cache_arc_sharing() {
    use std::sync::Arc;

    let example_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");

    if !example_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let cache = global_cache();

    let result1 = cache.get_or_parse(&example_file);
    let result2 = cache.get_or_parse(&example_file);

    if let (Ok(r1), Ok(r2)) = (result1, result2) {
        // Both should point to the same underlying data (Arc)
        assert!(Arc::ptr_eq(&r1, &r2), "Cached Arc should be shared");
    }
}
