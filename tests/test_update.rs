use vibe_coding_tracker::update::{
    extract_semver_version, get_asset_pattern, GitHubAsset, GitHubRelease,
};

#[test]
fn test_github_release_deserialization() {
    let json = r#"{
        "tag_name": "v0.1.6",
        "name": "Release v0.1.6",
        "body": "Bug fixes and improvements",
        "assets": [
            {
                "name": "vibe_coding_tracker-v0.1.6-linux-x64-gnu.tar.gz",
                "browser_download_url": "https://github.com/Mai0313/VibeCodingTracker/releases/download/v0.1.6/vibe_coding_tracker-v0.1.6-linux-x64-gnu.tar.gz",
                "size": 5242880
            },
            {
                "name": "vibe_coding_tracker-v0.1.6-windows-x64.zip",
                "browser_download_url": "https://github.com/Mai0313/VibeCodingTracker/releases/download/v0.1.6/vibe_coding_tracker-v0.1.6-windows-x64.zip",
                "size": 4194304
            }
        ]
    }"#;

    let release: GitHubRelease = serde_json::from_str(json).unwrap();

    assert_eq!(release.tag_name, "v0.1.6");
    assert_eq!(release.name, "Release v0.1.6");
    assert_eq!(release.body, Some("Bug fixes and improvements".to_string()));
    assert_eq!(release.assets.len(), 2);
    assert_eq!(
        release.assets[0].name,
        "vibe_coding_tracker-v0.1.6-linux-x64-gnu.tar.gz"
    );
    assert_eq!(release.assets[0].size, 5242880);
}

#[test]
fn test_github_release_deserialization_no_body() {
    let json = r#"{
        "tag_name": "v0.1.0",
        "name": "Initial Release",
        "body": null,
        "assets": []
    }"#;

    let release: GitHubRelease = serde_json::from_str(json).unwrap();

    assert_eq!(release.tag_name, "v0.1.0");
    assert_eq!(release.name, "Initial Release");
    assert_eq!(release.body, None);
    assert_eq!(release.assets.len(), 0);
}

#[test]
fn test_github_asset_deserialization() {
    let json = r#"{
        "name": "vibe_coding_tracker-v0.1.6-macos-arm64.tar.gz",
        "browser_download_url": "https://github.com/Mai0313/VibeCodingTracker/releases/download/v0.1.6/vibe_coding_tracker-v0.1.6-macos-arm64.tar.gz",
        "size": 3145728
    }"#;

    let asset: GitHubAsset = serde_json::from_str(json).unwrap();

    assert_eq!(asset.name, "vibe_coding_tracker-v0.1.6-macos-arm64.tar.gz");
    assert_eq!(
        asset.browser_download_url,
        "https://github.com/Mai0313/VibeCodingTracker/releases/download/v0.1.6/vibe_coding_tracker-v0.1.6-macos-arm64.tar.gz"
    );
    assert_eq!(asset.size, 3145728);
}

#[test]
#[cfg(target_os = "linux")]
fn test_get_asset_pattern_linux_x64() {
    #[cfg(target_arch = "x86_64")]
    {
        let pattern = get_asset_pattern("0.1.6").unwrap();
        assert_eq!(pattern, "vibe_coding_tracker-v0.1.6-linux-x64-gnu.tar.gz");
    }
}

#[test]
#[cfg(target_os = "linux")]
fn test_get_asset_pattern_linux_arm64() {
    #[cfg(target_arch = "aarch64")]
    {
        let pattern = get_asset_pattern("0.1.6").unwrap();
        assert_eq!(pattern, "vibe_coding_tracker-v0.1.6-linux-arm64-gnu.tar.gz");
    }
}

#[test]
#[cfg(target_os = "macos")]
fn test_get_asset_pattern_macos_x64() {
    #[cfg(target_arch = "x86_64")]
    {
        let pattern = get_asset_pattern("0.1.6").unwrap();
        assert_eq!(pattern, "vibe_coding_tracker-v0.1.6-macos-x64.tar.gz");
    }
}

#[test]
#[cfg(target_os = "macos")]
fn test_get_asset_pattern_macos_arm64() {
    #[cfg(target_arch = "aarch64")]
    {
        let pattern = get_asset_pattern("0.1.6").unwrap();
        assert_eq!(pattern, "vibe_coding_tracker-v0.1.6-macos-arm64.tar.gz");
    }
}

#[test]
#[cfg(target_os = "windows")]
fn test_get_asset_pattern_windows_x64() {
    #[cfg(target_arch = "x86_64")]
    {
        let pattern = get_asset_pattern("0.1.6").unwrap();
        assert_eq!(pattern, "vibe_coding_tracker-v0.1.6-windows-x64.zip");
    }
}

#[test]
#[cfg(target_os = "windows")]
fn test_get_asset_pattern_windows_arm64() {
    #[cfg(target_arch = "aarch64")]
    {
        let pattern = get_asset_pattern("0.1.6").unwrap();
        assert_eq!(pattern, "vibe_coding_tracker-v0.1.6-windows-arm64.zip");
    }
}

#[test]
fn test_get_asset_pattern_with_different_versions() {
    let pattern1 = get_asset_pattern("1.0.0").unwrap();
    let pattern2 = get_asset_pattern("2.5.3").unwrap();

    // Both should follow the same naming convention, just with different versions
    assert!(pattern1.starts_with("vibe_coding_tracker-v1.0.0-"));
    assert!(pattern2.starts_with("vibe_coding_tracker-v2.5.3-"));
}

#[test]
fn test_semver_version_comparison() {
    use semver::Version;

    // Test that version comparison works correctly (simulating check_update logic)
    let current = Version::parse("0.1.5").unwrap();
    let latest_older = Version::parse("0.1.4").unwrap();
    let latest_same = Version::parse("0.1.5").unwrap();
    let latest_newer = Version::parse("0.1.6").unwrap();

    assert!(latest_older <= current); // No update needed
    assert!(latest_same <= current); // No update needed
    assert!(latest_newer > current); // Update available
}

#[test]
fn test_version_tag_parsing() {
    use semver::Version;

    // Test parsing version tags with 'v' prefix (simulating actual usage)
    let tag_with_v = "v0.1.6";
    let version = Version::parse(tag_with_v.trim_start_matches('v')).unwrap();

    assert_eq!(version.major, 0);
    assert_eq!(version.minor, 1);
    assert_eq!(version.patch, 6);
}

#[test]
fn test_version_tag_parsing_without_v() {
    use semver::Version;

    // Test parsing version tags without 'v' prefix
    let tag_without_v = "1.2.3";
    let version = Version::parse(tag_without_v.trim_start_matches('v')).unwrap();

    assert_eq!(version.major, 1);
    assert_eq!(version.minor, 2);
    assert_eq!(version.patch, 3);
}

#[test]
fn test_github_release_serialization() {
    let asset = GitHubAsset {
        name: "test-binary.tar.gz".to_string(),
        browser_download_url: "https://example.com/test-binary.tar.gz".to_string(),
        size: 1024,
    };

    let release = GitHubRelease {
        tag_name: "v1.0.0".to_string(),
        name: "Test Release".to_string(),
        body: Some("Test body".to_string()),
        assets: vec![asset],
    };

    let json = serde_json::to_string(&release).unwrap();
    let deserialized: GitHubRelease = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.tag_name, "v1.0.0");
    assert_eq!(deserialized.assets.len(), 1);
    assert_eq!(deserialized.assets[0].name, "test-binary.tar.gz");
}

#[test]
fn test_asset_pattern_format_consistency() {
    let pattern = get_asset_pattern("0.1.6").unwrap();

    // All patterns should start with the binary name
    assert!(pattern.starts_with("vibe_coding_tracker-v"));

    // Should contain version
    assert!(pattern.contains("0.1.6"));

    // Should end with proper extension
    #[cfg(target_os = "windows")]
    assert!(pattern.ends_with(".zip"));

    #[cfg(not(target_os = "windows"))]
    assert!(pattern.ends_with(".tar.gz"));
}

#[test]
fn test_extract_semver_version_clean() {
    // Clean version (on a tag, no changes)
    assert_eq!(extract_semver_version("0.1.6"), "0.1.6");
}

#[test]
fn test_extract_semver_version_with_commits() {
    // Version with commits after tag
    assert_eq!(extract_semver_version("0.1.6-5-g1234567"), "0.1.6");
}

#[test]
fn test_extract_semver_version_with_dirty() {
    // Version with uncommitted changes
    assert_eq!(extract_semver_version("0.1.6-5-g1234567-dirty"), "0.1.6");
}

#[test]
fn test_extract_semver_version_only_dirty() {
    // Edge case: just dirty marker (shouldn't happen in practice)
    assert_eq!(extract_semver_version("0.1.6-dirty"), "0.1.6");
}

#[test]
fn test_extract_semver_version_complex() {
    // Complex version string
    assert_eq!(extract_semver_version("1.2.3-15-gabcdef0-dirty"), "1.2.3");
}

#[test]
fn test_extract_semver_version_with_prerelease() {
    // Prerelease version (though not used in our build.rs currently)
    assert_eq!(extract_semver_version("0.1.6-alpha.1"), "0.1.6");
}
