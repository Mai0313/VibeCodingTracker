use vibe_coding_tracker::{get_version_info, VersionInfo, VERSION, PKG_NAME, PKG_DESCRIPTION};

#[test]
fn test_version_constants() {
    assert!(!VERSION.is_empty(), "VERSION should not be empty");
    assert!(!PKG_NAME.is_empty(), "PKG_NAME should not be empty");
    assert!(!PKG_DESCRIPTION.is_empty(), "PKG_DESCRIPTION should not be empty");

    // Verify package name
    assert_eq!(PKG_NAME, "vibe_coding_tracker");
}

#[test]
fn test_get_version_info() {
    let version_info = get_version_info();

    // Version should match constant
    assert_eq!(version_info.version, VERSION);

    // Rust and Cargo versions should not be empty
    assert!(!version_info.rust_version.is_empty());
    assert!(!version_info.cargo_version.is_empty());

    // Rust version should be valid (either a version number or "unknown")
    assert!(
        version_info.rust_version.contains('.') || version_info.rust_version == "unknown",
        "Rust version should be valid format or 'unknown'"
    );

    // Cargo version should be valid (either a version number or "unknown")
    assert!(
        version_info.cargo_version.contains('.') || version_info.cargo_version == "unknown",
        "Cargo version should be valid format or 'unknown'"
    );
}

#[test]
fn test_version_info_serialization() {
    let version_info = get_version_info();

    // Test serialization
    let json = serde_json::to_string(&version_info).unwrap();
    assert!(json.contains("version"));
    assert!(json.contains("rust_version"));
    assert!(json.contains("cargo_version"));

    // Test deserialization
    let deserialized: VersionInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.version, version_info.version);
    assert_eq!(deserialized.rust_version, version_info.rust_version);
    assert_eq!(deserialized.cargo_version, version_info.cargo_version);
}

#[test]
fn test_version_info_clone() {
    let version_info = get_version_info();
    let cloned = version_info.clone();

    assert_eq!(cloned.version, version_info.version);
    assert_eq!(cloned.rust_version, version_info.rust_version);
    assert_eq!(cloned.cargo_version, version_info.cargo_version);
}

#[test]
fn test_version_info_debug() {
    let version_info = get_version_info();
    let debug_str = format!("{:?}", version_info);

    // Debug output should contain "VersionInfo"
    assert!(debug_str.contains("VersionInfo"));
    assert!(debug_str.contains(&version_info.version));
}

#[test]
fn test_version_info_consistency() {
    // Multiple calls should return the same version
    let v1 = get_version_info();
    let v2 = get_version_info();

    assert_eq!(v1.version, v2.version);
    assert_eq!(v1.rust_version, v2.rust_version);
    assert_eq!(v1.cargo_version, v2.cargo_version);
}
