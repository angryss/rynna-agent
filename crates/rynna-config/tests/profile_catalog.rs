use rynna_config::{
    AnthropicAuthentication, ConfiguredProvider, OpenAiAuthentication, ProfileCatalog,
    ProviderKind, ProviderSettingsStore, ResolvedCapability, secure_private_directory,
};
use rynna_core::{Profile, ProfileProvider};

#[test]
fn parses_anthropic_api_and_subscription_profiles() {
    let catalog = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: api
providers:
  anthropic_api:
    kind: anthropic-messages
    api_base: https://api.anthropic.com
    api_key_env: ANTHROPIC_API_KEY
  claude_subscription:
    kind: claude-subscription
    claude_program: /usr/local/bin/claude
profiles:
  api:
    providers:
    - provider: anthropic_api
      model: claude-sonnet-4-5
      enabled: true
      default: true
  subscription:
    providers:
    - provider: claude_subscription
      model: sonnet
      enabled: true
      default: true
"#,
    )
    .unwrap();

    let api = catalog.resolve("api").unwrap();
    assert_eq!(
        api.providers[0].provider_kind,
        ProviderKind::AnthropicMessages
    );
    assert_eq!(
        api.providers[0].api_key_env.as_deref(),
        Some("ANTHROPIC_API_KEY")
    );
    let subscription = catalog.resolve("subscription").unwrap();
    assert_eq!(
        subscription.providers[0].provider_kind,
        ProviderKind::ClaudeSubscription
    );
    assert_eq!(
        subscription.providers[0].claude_program.to_string_lossy(),
        "/usr/local/bin/claude"
    );
}

#[test]
fn anthropic_provider_settings_never_serialize_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("providers.yaml");
    let mut store = ProviderSettingsStore::load(&path).unwrap();
    store
        .add(
            "work",
            ConfiguredProvider::Anthropic {
                authentication: AnthropicAuthentication::Subscription,
            },
        )
        .unwrap();
    let encoded = std::fs::read_to_string(path).unwrap();
    assert!(encoded.contains("kind: anthropic"));
    assert!(!encoded.contains("token"));
    assert!(!encoded.contains("api_key"));
}

#[test]
fn openrouter_provider_settings_persist_only_credential_readiness() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("providers.yaml");
    let mut store = ProviderSettingsStore::load(&path).unwrap();
    store.add("work", ConfiguredProvider::OpenRouter).unwrap();

    let encoded = std::fs::read_to_string(&path).unwrap();
    assert!(encoded.contains("kind: openrouter"));
    assert!(!encoded.contains("OPENROUTER_API_KEY"));
    assert_eq!(
        ProviderSettingsStore::load(path).unwrap().list("work"),
        vec![ConfiguredProvider::OpenRouter]
    );
}

#[test]
fn example_catalog_configures_openrouter_through_the_openai_compatible_adapter() {
    let catalog = ProfileCatalog::from_yaml(include_str!("../../../rynna.example.yaml")).unwrap();
    let profile = catalog.resolve("openrouter").unwrap();

    assert_eq!(profile.providers[0].name, "openrouter");
    assert_eq!(
        profile.providers[0].provider_kind,
        ProviderKind::OpenAiCompatible
    );
    assert_eq!(
        profile.providers[0].api_base,
        "https://openrouter.ai/api/v1"
    );
    assert_eq!(
        profile.providers[0].api_key_env.as_deref(),
        Some("OPENROUTER_API_KEY")
    );
}

#[test]
fn claude_subscription_profiles_reject_rynna_context() {
    let error = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: subscription
providers:
  claude_subscription:
    kind: claude-subscription
profiles:
  subscription:
    providers:
    - provider: claude_subscription
      model: sonnet
      enabled: true
      default: true
    active_skills:
    - rust
"#,
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("cannot declare skills, MCP servers, or capabilities")
    );
}

#[test]
fn claude_subscription_provider_rejects_api_credentials_and_endpoint() {
    let error = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: claude
providers:
  claude:
    kind: claude-subscription
    api_base: https://api.anthropic.com
    api_key_env: ANTHROPIC_API_KEY
profiles:
  claude:
    providers:
    - provider: claude
      model: sonnet
      enabled: true
      default: true
"#,
    )
    .expect_err("subscription and direct API configuration must remain separate");

    assert!(
        error
            .to_string()
            .contains("cannot declare an API base URL or API key"),
        "unexpected error: {error}"
    );
}

#[test]
fn parses_profile_provider_model_skills_and_mcp_servers() {
    let catalog = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: work
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
    api_key_env: OLLAMA_API_KEY
profiles:
  work:
    providers:
    - provider: ollama
      model: qwen3:14b
      enabled: true
      default: true
    system_prompt: You are Rynna at work.
    active_skills:
    - rust
    - github
    mcp_servers:
    - filesystem
    capabilities:
    - workspace
capabilities:
  workspace:
    kind: filesystem
    root: .
    read_only: true
    denied_patterns:
    - private/**
    max_traversal_files: 200
    max_traversal_depth: 12
    max_search_bytes: 4096
mcp_servers:
  filesystem:
    transport: stdio
    command: mcp-filesystem
    args:
    - /workspace
"#,
    )
    .unwrap();

    let profile = catalog.resolve("work").unwrap();

    assert_eq!(catalog.default_profile(), "work");
    assert_eq!(profile.profile.name, "work");
    assert_eq!(profile.profile.providers[0].provider, "ollama");
    assert_eq!(profile.profile.providers[0].model, "qwen3:14b");
    assert_eq!(profile.profile.active_skills, ["rust", "github"]);
    assert_eq!(profile.profile.mcp_servers, ["filesystem"]);
    assert_eq!(profile.profile.capabilities, ["workspace"]);
    let ResolvedCapability::FileSystem(filesystem) = &profile.capabilities[0] else {
        panic!("expected filesystem capability");
    };
    assert_eq!(filesystem.root.to_string_lossy(), ".");
    assert!(filesystem.read_only);
    assert_eq!(
        filesystem.denied_patterns.as_deref(),
        Some(&["private/**".to_owned()][..])
    );
    assert_eq!(filesystem.max_traversal_files, Some(200));
    assert_eq!(filesystem.max_traversal_depth, Some(12));
    assert_eq!(filesystem.max_search_bytes, Some(4096));
    assert_eq!(
        profile.providers[0].provider_kind,
        ProviderKind::OpenAiCompatible
    );
    assert_eq!(profile.providers[0].api_base, "http://127.0.0.1:11434/v1");
    assert_eq!(
        profile.providers[0].api_key_env.as_deref(),
        Some("OLLAMA_API_KEY")
    );
    assert_eq!(profile.system_prompt, "You are Rynna at work.");
    assert_eq!(catalog.resolve_all().unwrap(), vec![profile]);
    assert_eq!(
        catalog.mcp_server("filesystem").unwrap()["command"].as_str(),
        Some("mcp-filesystem")
    );
}

#[test]
fn parses_ordered_profile_providers_for_runtime_fallback() {
    let catalog = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: work
providers:
  primary:
    kind: openai-compatible
    api_base: https://primary.example/v1
  secondary:
    kind: anthropic-messages
    api_base: https://secondary.example
    api_key_env: ANTHROPIC_API_KEY
profiles:
  work:
    providers:
    - provider: primary
      model: primary-model
      enabled: true
      default: true
    - provider: secondary
      model: secondary-model
      enabled: true
      default: false
"#,
    )
    .unwrap();

    let profile = catalog.resolve("work").unwrap();

    assert_eq!(profile.profile.providers.len(), 2);
    assert_eq!(profile.profile.providers[0].provider, "primary");
    assert_eq!(profile.profile.providers[0].model, "primary-model");
    assert_eq!(profile.providers[1].name, "secondary");
    assert_eq!(profile.providers[1].model, "secondary-model");
    assert_eq!(
        profile.providers[1].provider_kind,
        ProviderKind::AnthropicMessages
    );
}

#[test]
fn resolves_only_enabled_models_with_the_default_model_first() {
    let catalog = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: work
providers:
  local:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
  openai:
    kind: openai-compatible
    api_base: https://api.openai.com/v1
profiles:
  work:
    providers:
    - provider: local
      model: qwen3:8b
      enabled: false
    - provider: local
      model: qwen3:14b
      enabled: true
    - provider: openai
      model: gpt-5
      enabled: true
      default: true
"#,
    )
    .unwrap();

    let profile = catalog.resolve("work").unwrap();

    assert_eq!(profile.profile.providers.len(), 3);
    assert!(!profile.profile.providers[0].enabled);
    assert_eq!(profile.providers.len(), 2);
    assert_eq!(profile.providers[0].model, "gpt-5");
    assert_eq!(profile.providers[1].model, "qwen3:14b");
}

#[test]
fn rejects_multiple_default_models_for_one_profile() {
    let error = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: work
providers:
  local:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  work:
    providers:
    - provider: local
      model: qwen3:8b
      default: true
    - provider: local
      model: qwen3:14b
      default: true
"#,
    )
    .unwrap_err();

    assert!(error.to_string().contains("one default model"));
}

#[test]
fn rejects_duplicate_models_within_one_profile() {
    let error = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: local
providers:
  ollama:
    kind: openai-compatible
    api_base: http://localhost:11434/v1
profiles:
  local:
    providers:
    - provider: ollama
      model: qwen3:8b
      enabled: true
      default: true
    - provider: ollama
      model: qwen3:8b
      enabled: false
"#,
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("duplicate model `ollama/qwen3:8b`")
    );
}

#[test]
fn parses_a_bounded_command_capability() {
    let catalog = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: local
providers:
  local:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  local:
    providers:
    - provider: local
      model: test-model
      enabled: true
      default: true
    capabilities:
    - host-commands
capabilities:
  host-commands:
    kind: command
    working_directory: .
    programs:
      uname: /usr/bin/uname
      sw_vers: /usr/bin/sw_vers
    timeout_seconds: 5
    max_output_bytes: 8192
"#,
    )
    .unwrap();

    let profile = catalog.resolve("local").unwrap();
    let ResolvedCapability::Command(command) = &profile.capabilities[0] else {
        panic!("expected command capability");
    };
    assert_eq!(command.working_directory.to_string_lossy(), ".");
    assert_eq!(
        command.programs["uname"].to_string_lossy(),
        "/usr/bin/uname"
    );
    assert_eq!(
        command.programs["sw_vers"].to_string_lossy(),
        "/usr/bin/sw_vers"
    );
    assert_eq!(command.timeout_seconds, 5);
    assert_eq!(command.max_output_bytes, 8192);
}

#[test]
fn built_in_catalog_preserves_the_existing_local_ollama_defaults() {
    let catalog = ProfileCatalog::built_in();
    let profile = catalog.resolve(catalog.default_profile()).unwrap();

    assert_eq!(catalog.default_profile(), "default");
    assert_eq!(profile.profile.providers[0].provider, "ollama");
    assert_eq!(profile.profile.providers[0].model, "qwen3:8b");
    assert_eq!(profile.providers[0].api_base, "http://127.0.0.1:11434/v1");
    assert!(profile.profile.active_skills.is_empty());
    assert!(profile.profile.mcp_servers.is_empty());
}

#[test]
fn loads_a_profile_catalog_from_an_explicit_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
version: 1
default_profile: local
providers:
  local:
    kind: openai-compatible
    api_base: http://localhost:1234/v1
profiles:
  local:
    providers:
    - provider: local
      model: test-model
      enabled: true
      default: true
"#,
    )
    .unwrap();

    let catalog = ProfileCatalog::load(&path).unwrap();

    assert_eq!(
        catalog.resolve("local").unwrap().profile.providers[0].model,
        "test-model"
    );
}

#[test]
fn default_configuration_path_uses_the_platform_configuration_directory() {
    let path = ProfileCatalog::default_path().unwrap();

    assert!(path.ends_with("rynna/config.yaml"));
}

#[test]
fn rejects_profiles_that_reference_unknown_providers_or_mcp_servers() {
    let unknown_provider = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: broken
providers: {}
profiles:
  broken:
    providers:
    - provider: missing
      model: model
      enabled: true
      default: true
"#,
    )
    .unwrap_err();
    assert!(
        unknown_provider
            .to_string()
            .contains("references unknown provider `missing`")
    );

    let unknown_mcp = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: broken
providers:
  local:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  broken:
    providers:
    - provider: local
      model: model
      enabled: true
      default: true
    mcp_servers:
    - missing
"#,
    )
    .unwrap_err();
    assert!(
        unknown_mcp
            .to_string()
            .contains("references unknown MCP server `missing`")
    );
}

#[test]
fn rejects_profiles_that_reference_unknown_capabilities() {
    let error = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: broken
providers:
  local:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  broken:
    providers:
    - provider: local
      model: model
      enabled: true
      default: true
    capabilities:
    - missing
"#,
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("references unknown capability `missing`")
    );
}

#[test]
fn filesystem_capabilities_are_unique_per_profile_not_globally() {
    let valid = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: one
providers:
  local:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  one:
    providers:
    - provider: local
      model: model
      enabled: true
      default: true
    capabilities:
    - workspace-one
  two:
    providers:
    - provider: local
      model: model
      enabled: true
      default: true
    capabilities:
    - workspace-two
capabilities:
  workspace-one:
    kind: filesystem
    root: .
  workspace-two:
    kind: filesystem
    root: ..
"#,
    )
    .unwrap();
    assert_eq!(valid.resolve_all().unwrap().len(), 2);

    let error = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: broken
providers:
  local:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  broken:
    providers:
    - provider: local
      model: model
      enabled: true
      default: true
    capabilities:
    - workspace-one
    - workspace-two
capabilities:
  workspace-one:
    kind: filesystem
    root: .
  workspace-two:
    kind: filesystem
    root: ..
"#,
    )
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        "profile `broken` activates multiple filesystem capabilities, whose tool names would conflict"
    );
}

#[test]
fn command_capabilities_are_unique_per_profile() {
    let error = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: broken
providers:
  local:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  broken:
    providers:
    - provider: local
      model: model
      enabled: true
      default: true
    capabilities:
    - commands-one
    - commands-two
capabilities:
  commands-one:
    kind: command
    working_directory: .
    programs:
      uname: /usr/bin/uname
    timeout_seconds: 5
    max_output_bytes: 8192
  commands-two:
    kind: command
    working_directory: .
    programs:
      sw_vers: /usr/bin/sw_vers
    timeout_seconds: 5
    max_output_bytes: 8192
"#,
    )
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        "profile `broken` activates multiple command capabilities, whose tool names would conflict"
    );
}

#[test]
fn rejects_structurally_invalid_command_capabilities() {
    let invalid = [
        (
            "programs: {}\ntimeout_seconds: 5\nmax_output_bytes: 8192",
            "at least one program",
        ),
        (
            "programs:\n  uname: uname\ntimeout_seconds: 5\nmax_output_bytes: 8192",
            "absolute path",
        ),
        (
            "programs:\n  uname: /usr/bin/uname\ntimeout_seconds: 0\nmax_output_bytes: 8192",
            "greater than zero",
        ),
    ];

    for (command_config, expected) in invalid {
        let command_config = command_config
            .lines()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let error = ProfileCatalog::from_yaml(&format!(
            r#"
version: 1
default_profile: broken
providers:
  local:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  broken:
    providers:
    - provider: local
      model: model
      enabled: true
      default: true
    capabilities:
    - commands
capabilities:
  commands:
    kind: command
    working_directory: .
{command_config}
"#
        ))
        .unwrap_err();

        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn rejects_command_limits_above_the_safe_hard_maximum() {
    let error = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: broken
providers:
  local:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  broken:
    providers:
    - provider: local
      model: model
      enabled: true
      default: true
    capabilities:
    - commands
capabilities:
  commands:
    kind: command
    working_directory: .
    programs:
      uname: /usr/bin/uname
    timeout_seconds: 301
    max_output_bytes: 8388609
"#,
    )
    .unwrap_err();

    assert!(error.to_string().contains("safe maximum"));
}

#[test]
fn rejects_provider_urls_with_embedded_credentials() {
    let error = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: unsafe
providers:
  remote:
    kind: openai-compatible
    api_base: http://user:password@remote.example/v1
profiles:
  unsafe:
    providers:
    - provider: remote
      model: model
      enabled: true
      default: true
"#,
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("provider `remote` base URL must not contain embedded credentials")
    );
}

#[test]
fn provider_settings_start_blank_and_support_add_update_and_delete() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("providers.yaml");
    let mut store = ProviderSettingsStore::load(&path).unwrap();

    assert!(store.list("work").is_empty());
    assert!(!path.exists());

    store
        .add(
            "work",
            ConfiguredProvider::Ollama {
                api_base: "http://127.0.0.1:11434/v1".to_owned(),
            },
        )
        .unwrap();
    store
        .add(
            "work",
            ConfiguredProvider::OpenAi {
                authentication: OpenAiAuthentication::Chatgpt,
                reuse_existing: true,
            },
        )
        .unwrap();

    assert_eq!(
        ProviderSettingsStore::load(&path).unwrap().list("work"),
        vec![
            ConfiguredProvider::Ollama {
                api_base: "http://127.0.0.1:11434/v1".to_owned(),
            },
            ConfiguredProvider::OpenAi {
                authentication: OpenAiAuthentication::Chatgpt,
                reuse_existing: true,
            },
        ]
    );

    store
        .update(
            "work",
            ConfiguredProvider::OpenAi {
                authentication: OpenAiAuthentication::ApiKey,
                reuse_existing: false,
            },
        )
        .unwrap();
    assert_eq!(
        store.get("work", "openai"),
        Some(&ConfiguredProvider::OpenAi {
            authentication: OpenAiAuthentication::ApiKey,
            reuse_existing: false,
        })
    );

    store.delete("work", "ollama").unwrap();
    assert_eq!(
        ProviderSettingsStore::load(path).unwrap().list("work"),
        vec![ConfiguredProvider::OpenAi {
            authentication: OpenAiAuthentication::ApiKey,
            reuse_existing: false,
        }]
    );
}

#[test]
fn provider_settings_are_isolated_by_profile() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("providers.yaml");
    let mut store = ProviderSettingsStore::load(&path).unwrap();

    store
        .add(
            "alpha",
            ConfiguredProvider::Ollama {
                api_base: "http://127.0.0.1:11434/v1".to_owned(),
            },
        )
        .unwrap();
    store
        .add(
            "beta",
            ConfiguredProvider::Ollama {
                api_base: "http://127.0.0.1:22434/v1".to_owned(),
            },
        )
        .unwrap();

    let reloaded = ProviderSettingsStore::load(path).unwrap();
    assert_eq!(
        reloaded.list("alpha"),
        vec![ConfiguredProvider::Ollama {
            api_base: "http://127.0.0.1:11434/v1".to_owned(),
        }]
    );
    assert_eq!(
        reloaded.list("beta"),
        vec![ConfiguredProvider::Ollama {
            api_base: "http://127.0.0.1:22434/v1".to_owned(),
        }]
    );
}

#[test]
fn provider_settings_follow_profile_rename_and_delete() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("providers.yaml");
    let mut store = ProviderSettingsStore::load(&path).unwrap();
    store
        .add(
            "work",
            ConfiguredProvider::Ollama {
                api_base: "http://localhost:11434/v1".to_owned(),
            },
        )
        .unwrap();

    store.rename_profile("work", "renamed-work").unwrap();
    assert!(store.list("work").is_empty());
    assert_eq!(store.list("renamed-work").len(), 1);

    store.delete_profile("renamed-work").unwrap();
    assert!(store.list("renamed-work").is_empty());
}

#[test]
fn provider_settings_reject_the_obsolete_global_provider_format() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("providers.yaml");
    std::fs::write(
        &path,
        r#"
version: 1
providers:
- kind: ollama
  api_base: http://localhost:11434/v1
"#,
    )
    .unwrap();

    let error = ProviderSettingsStore::load(path).unwrap_err();

    assert!(error.to_string().contains("unknown field `providers`"));
}

#[test]
fn profile_catalog_migrates_the_legacy_single_provider_format() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
version: 1
default_profile: alpha
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  alpha:
    provider: ollama
    model: qwen3:8b
"#,
    )
    .unwrap();

    let catalog = ProfileCatalog::load(&path).unwrap();
    let resolved = catalog.resolve("alpha").unwrap();

    assert_eq!(resolved.profile.providers.len(), 1);
    assert!(resolved.profile.providers[0].enabled);
    assert!(resolved.profile.providers[0].is_default);
}

#[test]
fn provider_settings_reject_duplicates_unknown_updates_and_invalid_ollama_urls() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("providers.yaml");
    let mut store = ProviderSettingsStore::load(path).unwrap();
    store
        .add(
            "work",
            ConfiguredProvider::Ollama {
                api_base: "http://localhost:11434/v1".to_owned(),
            },
        )
        .unwrap();

    assert!(
        store
            .add(
                "work",
                ConfiguredProvider::Ollama {
                    api_base: "http://localhost:11434/v1".to_owned(),
                }
            )
            .unwrap_err()
            .to_string()
            .contains("already configured")
    );
    assert!(
        store
            .update(
                "work",
                ConfiguredProvider::OpenAi {
                    authentication: OpenAiAuthentication::Chatgpt,
                    reuse_existing: false,
                }
            )
            .unwrap_err()
            .to_string()
            .contains("is not configured")
    );
    assert!(
        ProviderSettingsStore::load(directory.path().join("invalid.yaml"))
            .unwrap()
            .add(
                "work",
                ConfiguredProvider::Ollama {
                    api_base: "not a URL".to_owned(),
                }
            )
            .unwrap_err()
            .to_string()
            .contains("URL is invalid")
    );
}

#[test]
fn provider_settings_do_not_change_in_memory_when_persistence_fails() {
    let directory = tempfile::tempdir().unwrap();
    let settings_path = directory.path().join("providers.yaml");
    let mut store = ProviderSettingsStore::load(&settings_path).unwrap();
    std::fs::create_dir(&settings_path).unwrap();

    assert!(
        store
            .add(
                "work",
                ConfiguredProvider::Ollama {
                    api_base: "http://localhost:11434/v1".to_owned(),
                }
            )
            .is_err()
    );
    assert!(store.list("work").is_empty());

    std::fs::remove_dir(&settings_path).unwrap();
    store
        .add(
            "work",
            ConfiguredProvider::Ollama {
                api_base: "http://localhost:11434/v1".to_owned(),
            },
        )
        .expect("a failed replacement must not poison future writes");
    assert_eq!(store.list("work").len(), 1);
}

#[cfg(unix)]
#[test]
fn provider_settings_ignore_stale_temporary_file_symlinks() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let settings_path = directory.path().join("providers.yaml");
    let temporary_path = settings_path.with_extension("yaml.tmp");
    let victim_path = directory.path().join("victim.txt");
    std::fs::write(&victim_path, "keep me").unwrap();
    symlink(&victim_path, temporary_path).unwrap();
    let mut store = ProviderSettingsStore::load(&settings_path).unwrap();

    store
        .add(
            "work",
            ConfiguredProvider::Ollama {
                api_base: "http://localhost:11434/v1".to_owned(),
            },
        )
        .unwrap();
    assert_eq!(std::fs::read_to_string(victim_path).unwrap(), "keep me");
    assert_eq!(store.list("work").len(), 1);
}

#[cfg(unix)]
#[test]
fn secure_private_directory_rejects_a_symlink_ancestor() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let real = directory.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = directory.path().join("link");
    symlink(&real, &link).unwrap();

    let error = secure_private_directory(link.join("codex")).unwrap_err();

    assert!(error.to_string().contains("symbolic link"));
    assert!(!real.join("codex").exists());
}

#[cfg(unix)]
#[test]
fn secure_private_directory_rejects_parent_components_and_root() {
    assert!(secure_private_directory("/").is_err());
    assert!(secure_private_directory("/tmp/rynna/../codex").is_err());
}

#[test]
fn provider_settings_merge_mutations_from_separate_store_instances() {
    let directory = tempfile::tempdir().unwrap();
    let settings_path = directory.path().join("providers.yaml");
    let mut first = ProviderSettingsStore::load(&settings_path).unwrap();
    let mut second = ProviderSettingsStore::load(&settings_path).unwrap();

    first
        .add(
            "alpha",
            ConfiguredProvider::Ollama {
                api_base: "http://localhost:11434/v1".to_owned(),
            },
        )
        .unwrap();
    second
        .add(
            "beta",
            ConfiguredProvider::OpenAi {
                authentication: OpenAiAuthentication::Chatgpt,
                reuse_existing: false,
            },
        )
        .unwrap();

    let reloaded = ProviderSettingsStore::load(settings_path).unwrap();
    assert_eq!(reloaded.list("alpha").len(), 1);
    assert_eq!(reloaded.list("beta").len(), 1);
}

#[test]
fn provider_settings_support_a_bare_relative_filename() {
    let file_name = format!(".rynna-provider-settings-test-{}.yaml", std::process::id());
    let lock_name = format!(
        ".rynna-provider-settings-test-{}.yaml.lock",
        std::process::id()
    );
    let _ = std::fs::remove_file(&file_name);
    let _ = std::fs::remove_file(&lock_name);
    let mut store = ProviderSettingsStore::load(&file_name).unwrap();

    store
        .add(
            "work",
            ConfiguredProvider::Ollama {
                api_base: "http://localhost:11434/v1".to_owned(),
            },
        )
        .unwrap();

    assert_eq!(
        ProviderSettingsStore::load(&file_name)
            .unwrap()
            .list("work")
            .len(),
        1
    );
    std::fs::remove_file(file_name).unwrap();
    std::fs::remove_file(lock_name).unwrap();
}

fn editable_profile(name: &str, model: &str) -> Profile {
    Profile {
        name: name.to_owned(),
        providers: vec![ProfileProvider {
            provider: "ollama".to_owned(),
            model: model.to_owned(),
            enabled: true,
            is_default: true,
            context_window: None,
        }],
        active_skills: Vec::new(),
        mcp_servers: Vec::new(),
        capabilities: Vec::new(),
        default_project_directory: ".".into(),
        projects: Vec::new(),
        subagents: Vec::new(),
    }
}

#[test]
fn parses_profile_projects_and_defaults_profiles_to_the_current_directory() {
    let catalog = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: work
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  work:
    provider: ollama
    model: qwen3:8b
    default_project_directory: /projects/home
    projects:
    - name: rynna
      directories:
      - /projects/rynna
      - /projects/shared
      default_directory: /projects/rynna
"#,
    )
    .unwrap();
    let profile = catalog.resolve("work").unwrap().profile;
    assert_eq!(
        profile.default_project_directory.to_string_lossy(),
        "/projects/home"
    );
    assert_eq!(profile.projects[0].name, "rynna");
    assert_eq!(profile.projects[0].directories.len(), 2);

    let defaults = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: work
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  work:
    provider: ollama
    model: qwen3:8b
"#,
    )
    .unwrap();
    assert_eq!(
        defaults
            .resolve("work")
            .unwrap()
            .profile
            .default_project_directory
            .to_string_lossy(),
        "."
    );
}

#[test]
fn rejects_retired_workspace_profile_fields() {
    let error = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: work
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  work:
    provider: ollama
    model: qwen3:8b
    default_workspace_directory: /projects/home
    workspaces: []
"#,
    )
    .unwrap_err();

    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn rejects_a_project_default_directory_outside_its_directory_list() {
    let error = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: work
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  work:
    provider: ollama
    model: qwen3:8b
    projects:
    - name: rynna
      directories:
      - /projects/rynna
      default_directory: /projects/other
"#,
    )
    .unwrap_err();
    assert!(error.to_string().contains("must be one of its directories"));
}

#[test]
fn adds_updates_and_deletes_profiles_and_persists_them() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
version: 1
default_profile: alpha
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  alpha:
    providers:
    - provider: ollama
      model: qwen3:8b
      enabled: true
      default: true
"#,
    )
    .unwrap();

    let mut catalog = ProfileCatalog::load(&path).unwrap();
    catalog
        .add_profile(editable_profile("zeta", "gpt-5"))
        .unwrap();
    catalog
        .update_profile("zeta", editable_profile("work", "gpt-5.2"))
        .unwrap();
    catalog.delete_profile("work").unwrap();

    let reloaded = ProfileCatalog::load(&path).unwrap();
    let names: Vec<_> = reloaded
        .resolve_all()
        .unwrap()
        .into_iter()
        .map(|profile| profile.profile.name)
        .collect();
    assert_eq!(names, vec!["alpha".to_owned()]);
    assert_eq!(
        reloaded.resolve("alpha").unwrap().profile.providers[0].model,
        "qwen3:8b"
    );
}

#[test]
fn adding_a_profile_does_not_change_memory_when_persistence_fails() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
version: 1
default_profile: alpha
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  alpha:
    providers:
    - provider: ollama
      model: qwen3:8b
      enabled: true
      default: true
"#,
    )
    .unwrap();

    let mut catalog = ProfileCatalog::load(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();

    let error = catalog
        .add_profile(editable_profile("work", "gpt-5"))
        .unwrap_err();

    assert!(error.to_string().contains("failed to write"));
    let names: Vec<_> = catalog
        .resolve_all()
        .unwrap()
        .into_iter()
        .map(|profile| profile.profile.name)
        .collect();
    assert_eq!(names, vec!["alpha".to_owned()]);
}

#[test]
fn profile_catalog_merges_mutations_from_separate_instances() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
version: 1
default_profile: alpha
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  alpha:
    providers:
    - provider: ollama
      model: qwen3:8b
      enabled: true
      default: true
"#,
    )
    .unwrap();
    let mut first = ProfileCatalog::load(&path).unwrap();
    let mut second = ProfileCatalog::load(&path).unwrap();

    first
        .add_profile(editable_profile("work", "gpt-5"))
        .unwrap();
    second
        .add_profile(editable_profile("personal", "qwen3:14b"))
        .unwrap();

    let names: Vec<_> = ProfileCatalog::load(path)
        .unwrap()
        .resolve_all()
        .unwrap()
        .into_iter()
        .map(|profile| profile.profile.name)
        .collect();
    assert_eq!(names, ["alpha", "personal", "work"]);
}

#[test]
fn profile_catalog_lists_all_catalog_provider_identifiers() {
    let catalog = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: alpha
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
  unused_custom:
    kind: openai-compatible
    api_base: https://custom.example/v1
profiles:
  alpha:
    providers:
    - provider: ollama
      model: qwen3:8b
      enabled: true
      default: true
"#,
    )
    .unwrap();

    assert_eq!(catalog.provider_ids(), ["ollama", "unused_custom"]);
}

#[test]
fn reserved_runtime_profile_name_is_rejected_by_catalog_crud() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
version: 1
default_profile: alpha
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  alpha:
    providers:
    - provider: ollama
      model: qwen3:8b
      enabled: true
      default: true
  openai-account:
    providers:
    - provider: ollama
      model: legacy
      enabled: true
      default: true
"#,
    )
    .unwrap();
    let original = std::fs::read_to_string(&path).unwrap();
    let mut catalog = ProfileCatalog::load(&path).unwrap();

    assert!(
        catalog
            .add_profile(editable_profile("openai-account", "new"))
            .unwrap_err()
            .to_string()
            .contains("reserved")
    );
    assert!(
        catalog
            .update_profile("alpha", editable_profile("openai-account", "renamed"))
            .unwrap_err()
            .to_string()
            .contains("reserved")
    );
    assert!(
        catalog
            .update_profile(
                "openai-account",
                editable_profile("openai-account", "edited")
            )
            .unwrap_err()
            .to_string()
            .contains("reserved")
    );
    assert!(
        catalog
            .delete_profile("openai-account")
            .unwrap_err()
            .to_string()
            .contains("reserved")
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}

#[test]
fn rejects_deleting_the_last_profile() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
version: 1
default_profile: alpha
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  alpha:
    providers:
    - provider: ollama
      model: qwen3:8b
      enabled: true
      default: true
"#,
    )
    .unwrap();

    let mut catalog = ProfileCatalog::load(&path).unwrap();
    let error = catalog.delete_profile("alpha").unwrap_err();
    assert!(error.to_string().contains("last profile"));
}

#[test]
fn profile_skills_persist_independently_through_rename_and_delete() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
version: 1
default_profile: personal
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  personal:
    provider: ollama
    model: test
    active_skills:
    - personal-skill
"#,
    )
    .unwrap();
    let mut catalog = ProfileCatalog::load(&path).unwrap();
    let mut work = editable_profile("work", "test");
    work.active_skills = vec!["code-review".into(), "./skills/rust".into()];
    catalog.add_profile(work.clone()).unwrap();
    work.name = "renamed-work".into();
    catalog.update_profile("work", work).unwrap();
    let reloaded = ProfileCatalog::load(&path).unwrap();
    assert_eq!(
        reloaded
            .resolve("renamed-work")
            .unwrap()
            .profile
            .active_skills,
        ["code-review", "./skills/rust"]
    );
    assert_eq!(
        reloaded.resolve("personal").unwrap().profile.active_skills,
        ["personal-skill"]
    );
    assert_eq!(
        reloaded.resolve("personal").unwrap().skills_directory,
        directory.path()
    );
    catalog.delete_profile("renamed-work").unwrap();
    catalog
        .add_profile(editable_profile("renamed-work", "test"))
        .unwrap();
    assert!(
        catalog
            .resolve("renamed-work")
            .unwrap()
            .profile
            .active_skills
            .is_empty()
    );
    assert_eq!(
        catalog.resolve("personal").unwrap().profile.active_skills,
        ["personal-skill"]
    );
}

#[test]
fn model_override_updates_the_resolved_default_and_matching_metadata() {
    let catalog = ProfileCatalog::from_yaml(
        r#"
version: 1
default_profile: local
providers:
  local:
    kind: openai-compatible
    api_base: http://localhost:11434/v1
profiles:
  local:
    providers:
    - provider: local
      model: first
    - provider: local
      model: default-model
      default: true
"#,
    )
    .unwrap();
    let mut resolved = catalog.resolve("local").unwrap();
    resolved.override_default_model("override");
    assert_eq!(resolved.providers[0].model, "override");
    assert_eq!(resolved.providers[1].model, "first");
    assert_eq!(resolved.profile.providers[0].model, "first");
    assert_eq!(resolved.profile.providers[1].model, "override");
    assert!(resolved.profile.providers[1].is_default);
}

#[test]
fn mlx_custom_models_round_trip_with_default_and_custom_endpoints() {
    for endpoint in ["", "api_base: http://localhost:8080/v1"] {
        let source = format!(
            r#"
version: 1
default_profile: local
providers:
  mlx:
    kind: mlx
    {endpoint}
profiles:
  local:
    provider: mlx
    model: mlx-community/Qwen3.8-27B-8bit
"#
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        std::fs::write(&path, source).unwrap();
        let mut catalog = ProfileCatalog::load(&path).unwrap();
        let mut profile = catalog.resolve("local").unwrap().profile;
        profile.providers.push(ProfileProvider {
            provider: "mlx".to_owned(),
            model: "/models/my-custom-model".to_owned(),
            enabled: true,
            is_default: false,
            context_window: None,
        });
        catalog.update_profile("local", profile).unwrap();
        let resolved = ProfileCatalog::load(&path)
            .unwrap()
            .resolve("local")
            .unwrap();
        assert_eq!(resolved.providers[0].provider_kind, ProviderKind::Mlx);
        assert_eq!(
            resolved.providers[0].api_base,
            if endpoint.is_empty() {
                "http://127.0.0.1:8000/v1"
            } else {
                "http://localhost:8080/v1"
            }
        );
        assert_eq!(
            resolved.profile.providers[0].model,
            "mlx-community/Qwen3.8-27B-8bit"
        );
        assert_eq!(
            resolved.profile.providers[1].model,
            "/models/my-custom-model"
        );
    }
}

#[test]
fn built_in_catalog_accepts_custom_mlx_models_without_changing_ollama_default() {
    let mut catalog = ProfileCatalog::built_in();
    let mut profile = catalog.resolve("default").unwrap().profile;
    profile.providers.push(ProfileProvider {
        provider: "mlx".to_owned(),
        model: "my-custom-mlx-model".to_owned(),
        enabled: true,
        is_default: false,
        context_window: None,
    });
    catalog.update_profile("default", profile).unwrap();
    let resolved = catalog.resolve("default").unwrap();
    assert_eq!(resolved.profile.providers[0].model, "qwen3:8b");
    assert_eq!(resolved.providers[1].provider_kind, ProviderKind::Mlx);
}

#[test]
fn mlx_settings_validate_urls_and_persist_per_profile() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("providers.yaml");
    let mut store = ProviderSettingsStore::load(&path).unwrap();
    for url in [
        "invalid",
        "ftp://localhost",
        "http://user:password@localhost:8000/v1",
    ] {
        assert!(
            store
                .add(
                    "local",
                    ConfiguredProvider::Mlx {
                        api_base: url.to_owned()
                    }
                )
                .is_err()
        );
    }
    let provider = ConfiguredProvider::Mlx {
        api_base: "http://localhost:8000/v1".to_owned(),
    };
    store.add("local", provider.clone()).unwrap();
    let reloaded = ProviderSettingsStore::load(&path).unwrap();
    assert_eq!(reloaded.list("local"), vec![provider]);
    assert!(reloaded.list("other").is_empty());
}

#[test]
fn subagents_persist_per_profile_and_follow_profile_lifecycle() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
version: 1
default_profile: default
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  default:
    provider: ollama
    model: qwen3:8b
"#,
    )
    .unwrap();
    let mut catalog = ProfileCatalog::load(&path).unwrap();
    let mut work = editable_profile("work", "qwen3:8b");
    work.subagents = vec![rynna_core::Subagent {
        name: "reviewer".into(),
        description: "Review code".into(),
        instructions: "Find bugs".into(),
    }];
    catalog.add_profile(work.clone()).unwrap();
    let mut personal = work.clone();
    personal.name = "personal".into();
    personal.subagents[0].instructions = "Review prose".into();
    catalog.add_profile(personal).unwrap();
    let mut reloaded = ProfileCatalog::load(&path).unwrap();
    assert!(
        reloaded
            .resolve("default")
            .unwrap()
            .profile
            .subagents
            .is_empty()
    );
    assert_eq!(
        reloaded.resolve("work").unwrap().profile.subagents[0].instructions,
        "Find bugs"
    );
    assert_eq!(
        reloaded.resolve("personal").unwrap().profile.subagents[0].instructions,
        "Review prose"
    );
    work.name = "renamed".into();
    reloaded.update_profile("work", work.clone()).unwrap();
    assert!(reloaded.resolve("work").is_err());
    assert_eq!(
        reloaded.resolve("renamed").unwrap().profile.subagents,
        work.subagents
    );
    reloaded.delete_profile("renamed").unwrap();
    let reloaded = ProfileCatalog::load(&path).unwrap();
    assert!(reloaded.resolve("renamed").is_err());
    assert_eq!(
        reloaded.resolve("personal").unwrap().profile.subagents[0].instructions,
        "Review prose"
    );
}

#[test]
fn invalid_subagents_do_not_modify_the_saved_catalog() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
version: 1
default_profile: default
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  default:
    provider: ollama
    model: qwen3:8b
"#,
    )
    .unwrap();
    let mut catalog = ProfileCatalog::load(&path).unwrap();
    let mut work = editable_profile("work", "qwen3:8b");
    catalog.add_profile(work.clone()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let helper = rynna_core::Subagent {
        name: "reviewer".into(),
        description: "Review code".into(),
        instructions: "Find bugs".into(),
    };
    for helpers in [
        vec![helper.clone(), helper.clone()],
        vec![rynna_core::Subagent {
            name: "bad name".into(),
            ..helper.clone()
        }],
        vec![rynna_core::Subagent {
            instructions: " ".into(),
            ..helper.clone()
        }],
        vec![rynna_core::Subagent {
            description: "x".repeat(1025),
            ..helper.clone()
        }],
        vec![helper; 33],
    ] {
        work.subagents = helpers;
        assert!(catalog.update_profile("work", work.clone()).is_err());
        assert!(
            catalog
                .resolve("work")
                .unwrap()
                .profile
                .subagents
                .is_empty()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn yaml_catalog_preserves_multiline_prompts_and_string_model_names() {
    let source = r#"
version: 1
default_profile: local
providers:
  ollama:
    kind: openai-compatible
    api_base: http://localhost:11434/v1
profiles:
  local:
    provider: ollama
    model: "123"
    system_prompt: |
      Review carefully.
      Preserve user changes.
"#;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom.yml");
    std::fs::write(&path, source).unwrap();
    let mut catalog = ProfileCatalog::load(&path).unwrap();
    let profile = catalog.resolve("local").unwrap().profile;
    assert_eq!(profile.providers[0].model, "123");
    assert_eq!(
        catalog.resolve("local").unwrap().system_prompt,
        "Review carefully.\nPreserve user changes.\n"
    );
    catalog.update_profile("local", profile.clone()).unwrap();
    assert_eq!(
        ProfileCatalog::load(&path)
            .unwrap()
            .resolve("local")
            .unwrap()
            .profile,
        profile
    );
}

#[test]
fn yaml_catalog_rejects_duplicate_mapping_keys() {
    let source = "version: 1\nversion: 1\ndefault_profile: local\n";
    assert!(ProfileCatalog::from_yaml(source).is_err());
    let source = r#"
version: 1
default_profile: local
providers:
  ollama:
    kind: openai-compatible
    api_base: http://localhost:11434/v1
profiles:
  local:
    provider: ollama
    model: first
  local:
    provider: ollama
    model: second
"#;
    assert!(ProfileCatalog::from_yaml(source).is_err());
}

#[test]
fn legacy_provider_settings_require_conversion_and_yaml_takes_precedence() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("providers.yaml");
    let legacy = directory.path().join("providers.toml");
    std::fs::write(&legacy, "private legacy settings").unwrap();
    let error = ProviderSettingsStore::load(&path).unwrap_err().to_string();
    assert!(error.contains("convert it to YAML"));
    assert!(!path.exists());
    std::fs::write(&path, "version: 1\nprofiles: {}\n").unwrap();
    assert!(
        ProviderSettingsStore::load(&path)
            .unwrap()
            .list("local")
            .is_empty()
    );
    assert_eq!(
        std::fs::read_to_string(legacy).unwrap(),
        "private legacy settings"
    );
}

#[test]
fn yolo_defaults_off_and_survives_profile_edits() {
    let source = r#"
version: 1
default_profile: default
providers:
  local:
    kind: openai-compatible
    api_base: http://localhost:11434/v1
profiles:
  default:
    provider: local
    model: test
"#;
    let catalog = ProfileCatalog::from_yaml(source).unwrap();
    assert!(!catalog.resolve("default").unwrap().yolo);
    let mut catalog = ProfileCatalog::from_yaml(&format!("{source}    yolo: true\n")).unwrap();
    let mut profile = catalog.resolve("default").unwrap().profile;
    assert!(catalog.resolve("default").unwrap().yolo);
    profile.name = "renamed".to_owned();
    catalog.update_profile("default", profile).unwrap();
    assert!(catalog.resolve("renamed").unwrap().yolo);
}
