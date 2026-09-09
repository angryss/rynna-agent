use rynna_config::memory::{
    HINDSIGHT_CLOUD_URL, HindsightDeployment, MemorySettings, MemorySettingsStore,
};

fn hindsight(base: &str, cloud: bool, key: Option<&str>) -> MemorySettings {
    MemorySettings::Hindsight {
        deployment: if cloud {
            HindsightDeployment::Cloud
        } else {
            HindsightDeployment::SelfHosted
        },
        api_base: base.into(),
        bank_id: "rynna".into(),
        api_key: key.map(str::to_owned),
    }
}

#[test]
fn default_none_and_cloud_key_round_trip_preserve_and_disable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("memory.yaml");
    let store = MemorySettingsStore::new(&path);
    assert!(matches!(store.load("test").unwrap(), MemorySettings::None));
    assert!(!path.exists());
    store
        .save(
            "test",
            hindsight(HINDSIGHT_CLOUD_URL, true, Some("test-secret")),
        )
        .unwrap();
    let reloaded = MemorySettingsStore::new(&path);
    let saved = reloaded
        .save("test", hindsight(HINDSIGHT_CLOUD_URL, true, None))
        .unwrap();
    assert!(
        matches!(saved, MemorySettings::Hindsight { api_key: Some(ref key), .. } if key == "test-secret")
    );
    // Private response has no secret-bearing field.
    let response = serde_yaml_ng::to_string(&saved.response()).unwrap();
    assert!(!response.contains("test-secret"));
    assert!(response.contains("api_key_configured: true"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    store.save("test", MemorySettings::None).unwrap();
    assert!(
        !std::fs::read_to_string(&path)
            .unwrap()
            .contains("test-secret")
    );
    assert!(matches!(
        reloaded.load("test").unwrap(),
        MemorySettings::None
    ));
}

#[test]
fn self_hosted_optional_key_can_be_cleared_and_never_moves_to_another_host() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemorySettingsStore::new(dir.path().join("memory.yaml"));
    store
        .save(
            "test",
            hindsight("http://localhost:8888", false, Some("secret")),
        )
        .unwrap();
    let moved = store
        .save("test", hindsight("https://other.example", false, None))
        .unwrap();
    assert!(matches!(
        moved,
        MemorySettings::Hindsight { api_key: None, .. }
    ));
    store
        .save(
            "test",
            hindsight("https://other.example", false, Some("secret")),
        )
        .unwrap();
    let cleared = store
        .save("test", hindsight("https://other.example", false, Some("")))
        .unwrap();
    assert!(matches!(
        cleared,
        MemorySettings::Hindsight { api_key: None, .. }
    ));
}

#[test]
fn invalid_settings_leave_saved_state_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemorySettingsStore::new(dir.path().join("memory.yaml"));
    store
        .save("test", hindsight(HINDSIGHT_CLOUD_URL, true, Some("secret")))
        .unwrap();
    for base in [
        "file:///tmp/memory",
        "https://user:password@example.com",
        "https://example.com?token=secret",
        "https://example.com#secret",
    ] {
        assert!(store.save("test", hindsight(base, false, None)).is_err());
    }
    assert!(
        store
            .save("test", hindsight(HINDSIGHT_CLOUD_URL, true, Some("")))
            .is_err()
    );
    assert!(
        store
            .save(
                "test",
                hindsight("https://other.example", true, Some("secret"))
            )
            .is_err()
    );
    assert!(
        matches!(store.load("test").unwrap(), MemorySettings::Hindsight { api_key: Some(key), .. } if key == "secret")
    );
}

#[test]
fn corrupt_file_and_write_failure_do_not_leak_credentials_or_report_success() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.yaml");
    std::fs::write(&path, "api_key: 'super-secret-invalid-yaml").unwrap();
    let store = MemorySettingsStore::new(&path);
    assert!(
        !store
            .load("test")
            .err()
            .unwrap()
            .to_string()
            .contains("super-secret")
    );
    let unwritable = MemorySettingsStore::new(path.join("memory.yaml"));
    assert!(unwritable.save("test", MemorySettings::None).is_err());
}

#[test]
fn profile_settings_credentials_and_disabling_are_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemorySettingsStore::new(dir.path().join("memory.yaml"));
    store
        .save(
            "work",
            hindsight(HINDSIGHT_CLOUD_URL, true, Some("work-secret")),
        )
        .unwrap();
    assert!(matches!(
        store.load("personal").unwrap(),
        MemorySettings::None
    ));
    assert!(
        store
            .save("personal", hindsight(HINDSIGHT_CLOUD_URL, true, None))
            .is_err()
    );
    store
        .save(
            "personal",
            hindsight(HINDSIGHT_CLOUD_URL, true, Some("personal-secret")),
        )
        .unwrap();
    let saved = store
        .save("work", hindsight(HINDSIGHT_CLOUD_URL, true, None))
        .unwrap();
    assert!(
        matches!(saved, MemorySettings::Hindsight { api_key: Some(key), .. } if key == "work-secret")
    );
    store.save("work", MemorySettings::None).unwrap();
    assert!(
        matches!(store.load("personal").unwrap(), MemorySettings::Hindsight { api_key: Some(key), .. } if key == "personal-secret")
    );
    assert!(store.save(" ", MemorySettings::None).is_err());
}

#[test]
fn rename_moves_only_its_profile_and_delete_does_not_leak_into_recreated_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemorySettingsStore::new(dir.path().join("memory.yaml"));
    store
        .save(
            "work",
            hindsight(HINDSIGHT_CLOUD_URL, true, Some("work-secret")),
        )
        .unwrap();
    store.save("personal", MemorySettings::None).unwrap();
    assert!(store.rename_profile("work", "personal").is_err());
    store.rename_profile("work", "renamed").unwrap();
    assert!(matches!(store.load("work").unwrap(), MemorySettings::None));
    assert!(
        matches!(store.load("renamed").unwrap(), MemorySettings::Hindsight { api_key: Some(key), .. } if key == "work-secret")
    );
    store.delete_profile("renamed").unwrap();
    assert!(matches!(
        store.load("renamed").unwrap(),
        MemorySettings::None
    ));
    assert!(matches!(
        store.load("personal").unwrap(),
        MemorySettings::None
    ));
}

#[test]
fn unreleased_global_settings_are_not_implicitly_shared_with_any_profile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.yaml");
    let previous =
        serde_yaml_ng::to_string(&hindsight(HINDSIGHT_CLOUD_URL, true, Some("secret"))).unwrap();
    std::fs::write(&path, &previous).unwrap();
    let store = MemorySettingsStore::new(&path);
    assert!(
        store
            .load("work")
            .err()
            .unwrap()
            .to_string()
            .contains("assign any previous global settings to a profile")
    );
    assert!(store.save("work", MemorySettings::None).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), previous);
}

#[test]
fn repairs_invalid_profiles_without_changing_other_profiles() {
    for invalid in [
        "kind: unknown",
        "kind: hindsight\ndeployment: self_hosted\napi_base: not-a-url\nbank_id: bank",
    ] {
        for replacement in [
            MemorySettings::None,
            hindsight("http://localhost:8888", false, None),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("memory.yaml");
            let invalid = invalid.replace("\n", "\n    ");
            let source = format!(
                "version: 1\nprofiles:\n  broken:\n    {invalid}\n  other:\n    kind: hindsight\n    deployment: cloud\n    api_base: {HINDSIGHT_CLOUD_URL}\n    bank_id: other-bank\n    api_key: other-secret\n  also_broken:\n    kind: future-provider\n"
            );
            std::fs::write(&path, &source).unwrap();
            let store = MemorySettingsStore::new(&path);
            assert!(store.load("broken").is_err());
            assert!(store.load("other").is_ok());
            store.save("broken", replacement).unwrap();
            assert!(store.load("broken").is_ok());
            let before: serde_yaml_ng::Value = serde_yaml_ng::from_str(&source).unwrap();
            let after: serde_yaml_ng::Value =
                serde_yaml_ng::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(before["profiles"]["other"], after["profiles"]["other"]);
            assert_eq!(
                before["profiles"]["also_broken"],
                after["profiles"]["also_broken"]
            );
        }
    }
}

#[test]
fn malformed_file_cannot_be_overwritten_by_a_single_profile_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.yaml");
    let source = "version: 1\nprofiles:\n  other:\n    api_key: 'unterminated-secret";
    std::fs::write(&path, source).unwrap();
    let store = MemorySettingsStore::new(&path);
    assert!(store.save("broken", MemorySettings::None).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}

#[test]
fn legacy_memory_file_cannot_be_silently_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.yaml");
    let legacy = dir.path().join("memory.toml");
    std::fs::write(&legacy, "private legacy settings").unwrap();
    let store = MemorySettingsStore::new(&path);
    assert!(
        store
            .load("work")
            .err()
            .unwrap()
            .to_string()
            .contains("convert it to YAML")
    );
    assert!(store.save("work", MemorySettings::None).is_err());
    assert!(!path.exists());
    assert_eq!(
        std::fs::read_to_string(legacy).unwrap(),
        "private legacy settings"
    );
}

#[test]
fn duplicate_profiles_cannot_be_overwritten_by_a_single_profile_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.yaml");
    let source = "version: 1\nprofiles:\n  other:\n    kind: none\n  other:\n    kind: none\n";
    std::fs::write(&path, source).unwrap();
    let store = MemorySettingsStore::new(&path);
    assert!(store.save("work", MemorySettings::None).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}
