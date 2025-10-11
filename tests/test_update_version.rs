// Unit tests for update/mod.rs
//
// Tests version extraction and comparison logic

use vibe_coding_tracker::update::extract_semver_version;

#[test]
fn test_extract_semver_version_clean() {
    // Test extracting clean semver version
    let version = "0.1.6";
    assert_eq!(extract_semver_version(version), "0.1.6");
}

#[test]
fn test_extract_semver_version_with_git_metadata() {
    // Test extracting version with git metadata
    let version = "0.1.6-5-g1234567";
    assert_eq!(extract_semver_version(version), "0.1.6");
}

#[test]
fn test_extract_semver_version_with_dirty_flag() {
    // Test extracting version with dirty flag
    let version = "0.1.6-5-g1234567-dirty";
    assert_eq!(extract_semver_version(version), "0.1.6");
}

#[test]
fn test_extract_semver_version_rc() {
    // Test extracting release candidate version
    let version = "1.0.0-rc.1";
    assert_eq!(extract_semver_version(version), "1.0.0");
}

#[test]
fn test_extract_semver_version_beta() {
    // Test extracting beta version
    let version = "2.3.4-beta.2";
    assert_eq!(extract_semver_version(version), "2.3.4");
}

#[test]
fn test_extract_semver_version_alpha() {
    // Test extracting alpha version
    let version = "0.5.0-alpha";
    assert_eq!(extract_semver_version(version), "0.5.0");
}

#[test]
fn test_extract_semver_version_complex() {
    // Test extracting complex version string
    let version = "1.2.3-45-gabcdef0-modified";
    assert_eq!(extract_semver_version(version), "1.2.3");
}

#[test]
fn test_extract_semver_version_single_digit() {
    // Test single digit versions
    assert_eq!(extract_semver_version("1.0.0"), "1.0.0");
    assert_eq!(extract_semver_version("0.0.1"), "0.0.1");
}

#[test]
fn test_extract_semver_version_large_numbers() {
    // Test with large version numbers
    assert_eq!(extract_semver_version("10.20.30"), "10.20.30");
    assert_eq!(extract_semver_version("100.200.300-1-g123"), "100.200.300");
}

#[test]
fn test_extract_semver_version_empty() {
    // Test with empty string (edge case)
    assert_eq!(extract_semver_version(""), "");
}

#[test]
fn test_extract_semver_version_no_dashes() {
    // Test version without any dashes
    let version = "2.4.8";
    assert_eq!(extract_semver_version(version), "2.4.8");
}

#[test]
fn test_extract_semver_version_multiple_dashes() {
    // Test with multiple dashes
    let version = "1.0.0-pre-release-candidate";
    assert_eq!(extract_semver_version(version), "1.0.0");
}

#[test]
fn test_extract_semver_version_only_major_minor() {
    // Test incomplete version (not standard semver, but should handle gracefully)
    let version = "1.2";
    assert_eq!(extract_semver_version(version), "1.2");
}

#[test]
fn test_extract_semver_version_with_v_prefix() {
    // Test with v prefix (common in git tags)
    // Note: This function doesn't strip 'v', that's done elsewhere
    let version = "v1.2.3-dirty";
    assert_eq!(extract_semver_version(version), "v1.2.3");
}

#[test]
fn test_extract_semver_version_consistency() {
    // Test that calling twice gives same result
    let version = "3.1.4-15-g926535-dirty";
    let result1 = extract_semver_version(version);
    let result2 = extract_semver_version(version);
    assert_eq!(result1, result2);
}

#[test]
fn test_extract_semver_version_zero_version() {
    // Test zero versions
    assert_eq!(extract_semver_version("0.0.0"), "0.0.0");
    assert_eq!(extract_semver_version("0.0.0-dev"), "0.0.0");
}

#[test]
fn test_extract_semver_version_patch_zero() {
    // Test with patch version zero
    assert_eq!(extract_semver_version("1.5.0"), "1.5.0");
    assert_eq!(extract_semver_version("2.0.0-rc1"), "2.0.0");
}
