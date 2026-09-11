use rynna_config::profile_update::save_provider_with_catalog;
use rynna_config::{
    ConfiguredProvider, OPENAI_ACCOUNT_PROVIDER, OpenAiAuthentication, ProfileCatalog,
    ProviderKind, ProviderSettingsStore,
};

fn account() -> ConfiguredProvider {
    ConfiguredProvider::OpenAi {
        authentication: OpenAiAuthentication::Chatgpt,
        reuse_existing: false,
    }
}

#[test]
fn existing_accounts_register_disabled_models_only_in_their_profile_and_preserve_edits() {
    let directory = tempfile::tempdir().unwrap();
    let mut settings =
        ProviderSettingsStore::load(directory.path().join("providers.yaml")).unwrap();
    settings.add("default", account()).unwrap();
    let mut catalog = ProfileCatalog::built_in();
    let mut other = catalog.resolve("default").unwrap().profile;
    other.name = "other".into();
    catalog.add_profile(other.clone()).unwrap();
    catalog.register_openai_accounts(&settings).unwrap();
    let mut profile = catalog.resolve("default").unwrap().profile;
    assert_eq!(profile.providers.len(), 2);
    assert!(profile.providers[0].is_default);
    assert!(!profile.providers[1].enabled);
    assert_eq!(profile.providers[1].provider, OPENAI_ACCOUNT_PROVIDER);
    assert_eq!(catalog.resolve("other").unwrap().profile, other);
    profile.providers[1].model = "account-model".into();
    profile.providers[1].enabled = true;
    catalog.update_profile("default", profile.clone()).unwrap();
    catalog.register_openai_accounts(&settings).unwrap();
    assert_eq!(catalog.resolve("default").unwrap().profile, profile);
    assert_eq!(
        catalog.resolve("default").unwrap().providers[1].provider_kind,
        ProviderKind::OpenAiAccount
    );
}

#[test]
fn provider_save_persists_catalog_and_does_not_commit_settings_on_catalog_failure() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.yaml");
    std::fs::write(&path, "version: 1\ndefault_profile: default\nproviders:\n  local:\n    kind: openai-compatible\n    api_base: http://localhost:11434/v1\nprofiles:\n  default:\n    provider: local\n    model: test\n").unwrap();
    let mut catalog = ProfileCatalog::load(&path).unwrap();
    let settings_path = directory.path().join("providers.yaml");
    let mut settings = ProviderSettingsStore::load(&settings_path).unwrap();
    // Invalid catalog at save time must not persist an otherwise-valid provider change.
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, "broken: [").unwrap();
    assert!(
        save_provider_with_catalog(&mut catalog, &mut settings, "default", account(), false)
            .is_err()
    );
    assert!(!settings_path.exists());
    assert!(settings.get("default", "openai").is_none());
    std::fs::write(&path, original).unwrap();
    save_provider_with_catalog(&mut catalog, &mut settings, "default", account(), false).unwrap();
    let reloaded = ProfileCatalog::load(&path).unwrap();
    assert!(
        reloaded
            .provider_ids()
            .contains(&OPENAI_ACCOUNT_PROVIDER.to_owned())
    );
    assert_eq!(
        reloaded.resolve("default").unwrap().profile.providers.len(),
        2
    );
    assert!(
        ProviderSettingsStore::load(&settings_path)
            .unwrap()
            .get("default", "openai")
            .is_some()
    );
}

#[test]
fn account_provider_rejects_api_routing_and_existing_provider_name_collisions() {
    let source = "version: 1\ndefault_profile: default\nproviders:\n  openai-account:\n    kind: openai-account\n    api_base: https://api.openai.com/v1\nprofiles:\n  default:\n    provider: openai-account\n    model: test\n";
    assert!(ProfileCatalog::from_yaml(source).is_err());
    let mut catalog = ProfileCatalog::from_yaml(
        &source.replace("kind: openai-account", "kind: openai-compatible"),
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut settings =
        ProviderSettingsStore::load(directory.path().join("providers.yaml")).unwrap();
    settings.add("default", account()).unwrap();
    assert!(catalog.register_openai_accounts(&settings).is_err());
    assert_eq!(
        catalog.resolve("default").unwrap().providers[0].provider_kind,
        ProviderKind::OpenAiCompatible
    );
}
