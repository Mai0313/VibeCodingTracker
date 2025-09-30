#[test]
fn version_info_works() {
    let version_info = codex_usage::get_version_info();
    assert!(!version_info.version.is_empty());
    assert_eq!(version_info.version, codex_usage::VERSION);
}
