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
#[test]
fn composed_profiles_do_not_advertise_host_info() {
    for yolo in [false, true] {
        let mut profile = ProfileCatalog::built_in().resolve("default").unwrap();
        profile.yolo = yolo;
        let names: Vec<_> = rynna_runtime::native_tools(&profile)
            .unwrap()
            .iter()
            .map(|tool| tool.definition().name)
            .collect();
        assert!(
            !names.iter().any(|name| name == "host_info"),
            "yolo={yolo}: {names:?}"
        );
        assert_eq!(names.iter().any(|name| name == "run_command"), yolo);
        assert!(names.iter().any(|name| name == "read_file"));
    }
}

#[tokio::test]
async fn default_code_search_searches_all_selected_repositories() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    let outside = root.path().join("outside");
    for directory in [&first, &second, &outside] {
        std::fs::create_dir_all(directory.join(".git")).unwrap();
        std::fs::write(directory.join("item.rs"), "fn needle() {}").unwrap();
        std::fs::write(directory.join(".env"), "needle restricted").unwrap();
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.join("item.rs"), second.join("linked.rs")).unwrap();
    for yolo in [false, true] {
        let mut profile = ProfileCatalog::built_in().resolve("default").unwrap();
        profile.yolo = yolo;
        profile.profile.default_project_directory = first.clone();
        let tools = rynna_runtime::native_tools(&profile).unwrap();
        let search = tool(&tools, "code_search");
        let scoped = search
            .for_project(&[first.clone(), second.clone()])
            .unwrap();
        for arguments in [
            json!({"query":"needle"}),
            json!({"query":"needle", "path":"."}),
        ] {
            let result = scoped.execute(arguments).await.unwrap();
            let text = result.to_string();
            let matches = result["matches"].as_array().unwrap();
            for directory in [&first, &second] {
                assert!(
                    matches
                        .iter()
                        .any(|hit| hit["repository"] == directory.to_str().unwrap()
                            && hit["repository_path"] == "item.rs"),
                    "yolo={yolo}: {result}"
                );
            }
            assert!(!text.contains(outside.to_str().unwrap()), "{result}");
            if !yolo {
                assert!(!text.contains("restricted"), "{result}");
                assert!(!text.contains("linked.rs"), "{result}");
            }
        }
        let result = search.execute(json!({"query":"needle"})).await.unwrap();
        assert!(!result.to_string().contains(second.to_str().unwrap()));
        if !yolo {
            for path in [
                outside.to_string_lossy().into_owned(),
                "../outside".to_owned(),
            ] {
                assert!(
                    scoped
                        .execute(json!({"query":"needle", "path":path}))
                        .await
                        .is_err()
                );
            }
        }
    }
}

#[tokio::test]
async fn stale_inactive_profile_defaults_are_lazy() {
    let parent = tempfile::tempdir().unwrap();
    let selected = tempfile::tempdir().unwrap();
    std::fs::write(selected.path().join("item.txt"), "selected").unwrap();
    for yolo in [false, true] {
        let mut profile = ProfileCatalog::built_in().resolve("default").unwrap();
        profile.yolo = yolo;
        profile.profile.default_project_directory = parent.path().join("unmounted");
        let tools = rynna_runtime::native_tools(&profile)
            .unwrap_or_else(|error| panic!("inactive profile yolo={yolo}: {error}"));
        let read = tool(&tools, "read_file");
        assert!(read.execute(json!({"path":"item.txt"})).await.is_err());
        assert!(
            tool(&tools, "code_search")
                .execute(json!({"query":"selected"}))
                .await
                .is_err()
        );
        let scoped = read.for_project(&[selected.path().into()]).unwrap();
        assert!(
            scoped
                .execute(json!({"path":"item.txt"}))
                .await
                .unwrap()
                .to_string()
                .contains("selected")
        );
        if !yolo {
            assert!(!tools.iter().any(|tool| matches!(
                tool.definition().name.as_str(),
                "write_file" | "edit_file" | "create_directory" | "run_command"
            )));
        }
    }
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
