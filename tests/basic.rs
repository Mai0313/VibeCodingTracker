#[test]
fn version_info_works() {
    let version_info = vibe_coding_tracker::get_version_info();
    assert!(!version_info.version.is_empty());
    assert_eq!(version_info.version, vibe_coding_tracker::VERSION);
}
