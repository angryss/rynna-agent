use rynna_config::ProfileCatalog;
use rynna_core::Tool;
use serde_json::json;
use std::sync::Arc;
fn tool(tools: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
    tools
        .iter()
        .find(|t| t.definition().name == name)
        .unwrap_or_else(|| panic!("missing {name}"))
        .clone()
}
#[cfg(unix)]
#[tokio::test]
async fn yolo_injects_native_write_and_arbitrary_commands_without_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let mut profile = ProfileCatalog::built_in().resolve("default").unwrap();
    profile.yolo = true;
    profile.profile.default_project_directory = dir.path().to_owned();
    let tools = rynna_runtime::native_tools(&profile).unwrap();
    let destination = outside.path().join(".env");
    tool(&tools, "write_file")
        .execute(json!({"path":destination,"content":"synthetic"}))
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), "synthetic");
    assert!(
        tool(&tools, "read_file")
            .execute(json!({"path":destination}))
            .await
            .unwrap()
            .to_string()
            .contains("synthetic")
    );
    let result = tool(&tools,"run_command").execute(json!({"program":"/bin/sh","arguments":["-c", "printf command-fixture > command.txt; printf executed"]})).await.unwrap();
    assert_eq!(result["stdout"], "executed");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("command.txt")).unwrap(),
        "command-fixture"
    );
    assert!(
        tool(&tools, "run_command")
            .execute(json!({"program":42}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn yolo_retains_path_argument_validation() {
    let mut profile = ProfileCatalog::built_in().resolve("default").unwrap();
    profile.yolo = true;
    let tools = rynna_runtime::native_tools(&profile).unwrap();
    for arguments in [
        json!({}),
        json!({"path":42}),
        json!({"path":".","unexpected":true}),
    ] {
        assert!(
            tool(&tools, "file_info")
                .execute(arguments.clone())
                .await
                .is_err(),
            "{arguments}"
        );
    }
}
#[tokio::test]
async fn default_files_follow_request_project_but_cannot_escape_it() {
    let first = tempfile::tempdir().unwrap();
    let selected = tempfile::tempdir().unwrap();
    std::fs::write(first.path().join("item.txt"), "first").unwrap();
    std::fs::write(selected.path().join("item.txt"), "selected").unwrap();
    std::fs::write(selected.path().join(".env"), "synthetic restricted").unwrap();
    let mut profile = ProfileCatalog::built_in().resolve("default").unwrap();
    profile.profile.default_project_directory = first.path().into();
    let tools = rynna_runtime::native_tools(&profile).unwrap();
    let read = tool(&tools, "read_file");
    let scoped = read.for_project(&[selected.path().into()]).unwrap();
    assert!(
        scoped
            .execute(json!({"path":"item.txt"}))
            .await
            .unwrap()
            .to_string()
            .contains("selected")
    );
    assert!(
        read.execute(json!({"path":"item.txt"}))
            .await
            .unwrap()
            .to_string()
            .contains("first")
    );
    assert!(
        scoped
            .execute(json!({"path":first.path().join("item.txt")}))
            .await
            .is_err()
    );
    assert!(scoped.execute(json!({"path":"../item.txt"})).await.is_err());
    assert!(scoped.execute(json!({"path":".env"})).await.is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            first.path().join("item.txt"),
            selected.path().join("linked"),
        )
        .unwrap();
        assert!(scoped.execute(json!({"path":"linked"})).await.is_err());
    }
}
#[cfg(unix)]
#[tokio::test]
async fn yolo_ignores_explicit_permissions_and_configured_resource_limits() {
    let dir = tempfile::tempdir().unwrap();
    let source = format!(
        r#"version: 1
default_profile: test
providers:
  local:
    kind: openai-compatible
    api_base: http://localhost:11434/v1
profiles:
  test:
    provider: local
    model: test
    capabilities: [files, command]
    default_project_directory: '{}'
    yolo: true
    disabled_toolsets: [file_operations, commands, code_search]
capabilities:
  files:
    kind: filesystem
    root: '{}'
    read_only: true
    allowed_patterns: ['nothing']
    denied_patterns: ['**']
    protected_patterns: ['**']
    max_read_bytes: 1
  command:
    kind: command
    working_directory: '{}'
    programs:
      forbidden: /nonexistent-program
    timeout_seconds: 1
    max_output_bytes: 1
"#,
        dir.path().display(),
        dir.path().display(),
        dir.path().display()
    );
    let profile = ProfileCatalog::from_yaml(&source)
        .unwrap()
        .resolve("test")
        .unwrap();
    let tools = rynna_runtime::native_tools(&profile).unwrap();
    tool(&tools, "write_file")
        .execute(json!({"path":".env","content":"synthetic yolo fixture"}))
        .await
        .unwrap();
    assert!(
        tool(&tools, "read_file")
            .execute(json!({"path":".env"}))
            .await
            .unwrap()
            .to_string()
            .contains("synthetic yolo fixture")
    );
    let result = tool(&tools, "run_command")
        .execute(json!({"program":"sh","arguments":["-c","sleep 2; printf resource-fixture"]}))
        .await
        .unwrap();
    assert_eq!(result["stdout"], "resource-fixture");
    assert_eq!(
        std::fs::read_to_string(dir.path().join(".env")).unwrap(),
        "synthetic yolo fixture"
    );
}
