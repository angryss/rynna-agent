use rynna_config::ProfileCatalog;
use rynna_core::toolsets::ToolsetId;

#[test]
fn toolset_switches_survive_save_reload_and_rename() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rynna.yaml");
    std::fs::write(&path, "version: 1\ndefault_profile: test\nproviders:\n  local:\n    kind: openai-compatible\n    api_base: http://localhost:11434\nprofiles:\n  test:\n    providers:\n      - provider: local\n        model: test\n").unwrap();
    let mut catalog = ProfileCatalog::load(&path).unwrap();
    let mut profile = catalog.resolve("test").unwrap().profile;
    assert!(profile.disabled_toolsets.is_empty());
    profile.disabled_toolsets = vec![ToolsetId::FileOperations];
    catalog.update_profile("test", profile).unwrap();
    let mut catalog = ProfileCatalog::load(&path).unwrap();
    let mut profile = catalog.resolve("test").unwrap().profile;
    assert_eq!(profile.disabled_toolsets, vec![ToolsetId::FileOperations]);
    profile.name = "renamed".into();
    catalog.update_profile("test", profile).unwrap();
    let catalog = ProfileCatalog::load(&path).unwrap();
    assert_eq!(
        catalog
            .resolve("renamed")
            .unwrap()
            .profile
            .disabled_toolsets,
        vec![ToolsetId::FileOperations]
    );
}
