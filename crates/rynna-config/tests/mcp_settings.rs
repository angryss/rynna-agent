use rynna_config::mcp::{McpSettings, McpSettingsStore};

fn config(command: &str) -> McpSettings {
    serde_yaml_ng::from_str(&format!(
        r#"
mcpServers:
  tools:
    transport: stdio
    command: '{command}'
    args:
    - hello
    env:
      TOKEN: private-value
"#
    ))
    .unwrap()
}

#[test]
fn profiles_persist_independently_and_follow_rename_delete() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.yaml");
    let store = McpSettingsStore::new(&path);
    assert!(store.load("new").unwrap().servers.is_empty());
    store.save("work", config("work-command")).unwrap();
    store.save("personal", config("personal-command")).unwrap();
    let reloaded = McpSettingsStore::new(&path);
    let encoded = serde_yaml_ng::to_string(&reloaded.load("work").unwrap()).unwrap();
    assert!(encoded.contains("work-command"));
    assert!(!encoded.contains("personal-command"));
    store.save("work", McpSettings::default()).unwrap();
    assert!(
        serde_yaml_ng::to_string(&store.load("personal").unwrap())
            .unwrap()
            .contains("personal-command")
    );
    store.rename_profile("personal", "renamed").unwrap();
    assert!(store.load("personal").unwrap().servers.is_empty());
    assert_eq!(store.load("renamed").unwrap().servers.len(), 1);
    store.delete_profile("renamed").unwrap();
    assert!(store.load("renamed").unwrap().servers.is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn invalid_save_preserves_file_and_errors_hide_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.yaml");
    let store = McpSettingsStore::new(&path);
    store.save("work", config("valid")).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(store.save("work", config("")).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    std::fs::write(&path, "secret-invalid-source").unwrap();
    let error = store
        .save("work", McpSettings::default())
        .unwrap_err()
        .to_string();
    assert!(!error.contains("secret-invalid-source"));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "secret-invalid-source"
    );
}

#[test]
fn validates_http_and_unknown_fields() {
    let settings: McpSettings = serde_yaml_ng::from_str(
        r#"
mcpServers:
  remote:
    transport: streamable_http
    url: https://example.com/mcp
    bearer_token_env: MCP_TOKEN
"#,
    )
    .unwrap();
    settings.validate().unwrap();
    for url in [
        "file:///tmp/a",
        "https://user:secret@example.com/mcp",
        "not-a-url",
    ] {
        let text =
            format!("mcpServers:\n  remote:\n    transport: streamable_http\n    url: '{url}'");
        assert!(
            serde_yaml_ng::from_str::<McpSettings>(&text)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    assert!(
        serde_yaml_ng::from_str::<McpSettings>(
            "mcpServers:\n  remote:\n    transport: stdio\n    command: x\n    unknown: true"
        )
        .is_err()
    );
}

#[test]
fn legacy_mcp_file_cannot_be_silently_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.yaml");
    let legacy = dir.path().join("mcp.toml");
    std::fs::write(&legacy, "private legacy settings").unwrap();
    let store = McpSettingsStore::new(&path);
    assert!(
        store
            .load("work")
            .unwrap_err()
            .to_string()
            .contains("convert it to YAML")
    );
    assert!(store.save("work", config("valid")).is_err());
    assert!(!path.exists());
    assert_eq!(
        std::fs::read_to_string(legacy).unwrap(),
        "private legacy settings"
    );
}
