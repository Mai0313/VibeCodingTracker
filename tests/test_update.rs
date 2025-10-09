use vibe_coding_tracker::update::{
    GitHubAsset, GitHubRelease, extract_semver_version, get_asset_pattern,
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

#[cfg(test)]
mod archive_tests {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::fs::{self, File};
    use std::io::Write;
    use std::path::PathBuf;
    use tar::Builder;
    use tempfile::TempDir;

    fn create_test_targz(content: &str, binary_name: &str) -> (TempDir, PathBuf) {
        let temp_dir = TempDir::new().unwrap();
        let archive_path = temp_dir.path().join("test.tar.gz");

        // Create a temporary binary file
        let binary_dir = TempDir::new().unwrap();
        let binary_path = binary_dir.path().join(binary_name);
        let mut file = File::create(&binary_path).unwrap();
        file.write_all(content.as_bytes()).unwrap();

        // Create tar.gz
        let tar_gz = File::create(&archive_path).unwrap();
        let enc = GzEncoder::new(tar_gz, Compression::default());
        let mut tar = Builder::new(enc);
        tar.append_path_with_name(&binary_path, binary_name)
            .unwrap();
        tar.finish().unwrap();

        (temp_dir, archive_path)
    }

    #[cfg(unix)]
    #[test]
    fn test_extract_targz_with_vibe_coding_tracker() {
        use vibe_coding_tracker::update::extract_targz;

        let content = "#!/bin/bash\necho 'test binary'";
        let (_temp_dir, archive_path) = create_test_targz(content, "vibe_coding_tracker");

        let extract_dir = TempDir::new().unwrap();
        let result = extract_targz(&archive_path, extract_dir.path());

        assert!(result.is_ok());
        let binary_path = result.unwrap();
        assert!(binary_path.exists());
        assert_eq!(binary_path.file_name().unwrap(), "vibe_coding_tracker");

        // Verify content
        let extracted_content = fs::read_to_string(&binary_path).unwrap();
        assert_eq!(extracted_content, content);
    }

    #[cfg(unix)]
    #[test]
    fn test_extract_targz_with_vct() {
        use vibe_coding_tracker::update::extract_targz;

        let content = "#!/bin/bash\necho 'test binary'";
        let (_temp_dir, archive_path) = create_test_targz(content, "vct");

        let extract_dir = TempDir::new().unwrap();
        let result = extract_targz(&archive_path, extract_dir.path());

        assert!(result.is_ok());
        let binary_path = result.unwrap();
        assert!(binary_path.exists());
        assert_eq!(binary_path.file_name().unwrap(), "vct");
    }

    #[cfg(unix)]
    #[test]
    fn test_extract_targz_binary_not_found() {
        use vibe_coding_tracker::update::extract_targz;

        let content = "some content";
        let (_temp_dir, archive_path) = create_test_targz(content, "other_binary");

        let extract_dir = TempDir::new().unwrap();
        let result = extract_targz(&archive_path, extract_dir.path());

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Binary not found in archive")
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_extract_targz_permissions() {
        use std::os::unix::fs::PermissionsExt;
        use vibe_coding_tracker::update::extract_targz;

        let content = "#!/bin/bash\necho 'test'";
        let (_temp_dir, archive_path) = create_test_targz(content, "vct");

        let extract_dir = TempDir::new().unwrap();
        let binary_path = extract_targz(&archive_path, extract_dir.path()).unwrap();

        // Check that binary is executable
        let metadata = fs::metadata(&binary_path).unwrap();
        let permissions = metadata.permissions();
        assert_eq!(permissions.mode() & 0o111, 0o111); // Check execute bits
    }

    #[cfg(windows)]
    fn create_test_zip(content: &str, binary_name: &str) -> (TempDir, PathBuf) {
        use zip::ZipWriter;
        use zip::write::SimpleFileOptions;

        let temp_dir = TempDir::new().unwrap();
        let archive_path = temp_dir.path().join("test.zip");

        let file = File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);

        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file(binary_name, options).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
        zip.finish().unwrap();

        (temp_dir, archive_path)
    }

    #[cfg(windows)]
    #[test]
    fn test_extract_zip_with_exe() {
        use vibe_coding_tracker::update::extract_zip;

        let content = "test binary content";
        let (temp_dir, archive_path) = create_test_zip(content, "vibe_coding_tracker.exe");

        let extract_dir = TempDir::new().unwrap();
        let result = extract_zip(&archive_path, extract_dir.path());

        assert!(result.is_ok());
        let binary_path = result.unwrap();
        assert!(binary_path.exists());
        assert_eq!(binary_path.file_name().unwrap(), "vibe_coding_tracker.exe");

        let extracted_content = fs::read_to_string(&binary_path).unwrap();
        assert_eq!(extracted_content, content);
    }

    #[cfg(windows)]
    #[test]
    fn test_extract_zip_with_vct_exe() {
        use vibe_coding_tracker::update::extract_zip;

        let content = "test binary content";
        let (temp_dir, archive_path) = create_test_zip(content, "vct.exe");

        let extract_dir = TempDir::new().unwrap();
        let result = extract_zip(&archive_path, extract_dir.path());

        assert!(result.is_ok());
        let binary_path = result.unwrap();
        assert_eq!(binary_path.file_name().unwrap(), "vct.exe");
    }

    #[cfg(windows)]
    #[test]
    fn test_extract_zip_binary_not_found() {
        use vibe_coding_tracker::update::extract_zip;

        let content = "test content";
        let (temp_dir, archive_path) = create_test_zip(content, "other.exe");

        let extract_dir = TempDir::new().unwrap();
        let result = extract_zip(&archive_path, extract_dir.path());

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Binary not found in archive")
        );
    }
}

#[test]
fn test_github_release_with_multiple_assets() {
    let json = r#"{
        "tag_name": "v1.0.0",
        "name": "Major Release",
        "body": "First stable release",
        "assets": [
            {
                "name": "vibe_coding_tracker-v1.0.0-linux-x64-gnu.tar.gz",
                "browser_download_url": "https://example.com/linux.tar.gz",
                "size": 5000000
            },
            {
                "name": "vibe_coding_tracker-v1.0.0-macos-arm64.tar.gz",
                "browser_download_url": "https://example.com/macos.tar.gz",
                "size": 4500000
            },
            {
                "name": "vibe_coding_tracker-v1.0.0-windows-x64.zip",
                "browser_download_url": "https://example.com/windows.zip",
                "size": 4000000
            }
        ]
    }"#;

    let release: GitHubRelease = serde_json::from_str(json).unwrap();

    assert_eq!(release.assets.len(), 3);

    // Find specific asset
    let linux_asset = release
        .assets
        .iter()
        .find(|a| a.name.contains("linux"))
        .unwrap();
    assert!(linux_asset.name.ends_with(".tar.gz"));

    let windows_asset = release
        .assets
        .iter()
        .find(|a| a.name.contains("windows"))
        .unwrap();
    assert!(windows_asset.name.ends_with(".zip"));
}

#[test]
fn test_asset_finding_logic() {
    let assets = vec![
        GitHubAsset {
            name: "vibe_coding_tracker-v0.1.6-linux-x64-gnu.tar.gz".to_string(),
            browser_download_url: "https://example.com/linux.tar.gz".to_string(),
            size: 5000000,
        },
        GitHubAsset {
            name: "vibe_coding_tracker-v0.1.6-macos-arm64.tar.gz".to_string(),
            browser_download_url: "https://example.com/macos.tar.gz".to_string(),
            size: 4500000,
        },
        GitHubAsset {
            name: "vibe_coding_tracker-v0.1.6-windows-x64.zip".to_string(),
            browser_download_url: "https://example.com/windows.zip".to_string(),
            size: 4000000,
        },
    ];

    let release = GitHubRelease {
        tag_name: "v0.1.6".to_string(),
        name: "Release v0.1.6".to_string(),
        body: None,
        assets,
    };

    #[cfg(target_os = "linux")]
    {
        let pattern = get_asset_pattern("0.1.6").unwrap();
        let asset = release.assets.iter().find(|a| a.name == pattern);
        assert!(asset.is_some());
        assert!(asset.unwrap().name.contains("linux"));
    }

    #[cfg(target_os = "macos")]
    {
        let pattern = get_asset_pattern("0.1.6").unwrap();
        let asset = release.assets.iter().find(|a| a.name == pattern);
        assert!(asset.is_some());
        assert!(asset.unwrap().name.contains("macos"));
    }

    #[cfg(target_os = "windows")]
    {
        let pattern = get_asset_pattern("0.1.6").unwrap();
        let asset = release.assets.iter().find(|a| a.name == pattern);
        assert!(asset.is_some());
        assert!(asset.unwrap().name.contains("windows"));
    }
}

#[test]
fn test_version_comparison_edge_cases() {
    use semver::Version;

    // Test major version differences
    let v1 = Version::parse("2.0.0").unwrap();
    let v2 = Version::parse("1.9.9").unwrap();
    assert!(v1 > v2);

    // Test minor version differences
    let v3 = Version::parse("1.2.0").unwrap();
    let v4 = Version::parse("1.1.9").unwrap();
    assert!(v3 > v4);

    // Test patch version differences
    let v5 = Version::parse("1.0.2").unwrap();
    let v6 = Version::parse("1.0.1").unwrap();
    assert!(v5 > v6);

    // Test equality
    let v7 = Version::parse("1.0.0").unwrap();
    let v8 = Version::parse("1.0.0").unwrap();
    assert_eq!(v7, v8);
}
