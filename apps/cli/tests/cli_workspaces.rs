use assert_cmd::Command;
use serde_json::Value;

fn write_config(path: &std::path::Path) {
    std::fs::write(
        path,
        r#"
version = 1
default_profile = "local"
[providers.ollama]
kind = "openai-compatible"
api_base = "http://127.0.0.1:11434/v1"
[profiles.local]
provider = "ollama"
model = "qwen3:8b"
"#,
    )
    .unwrap();
}

fn run(config: &std::path::Path, arguments: &[&str]) {
    Command::cargo_bin("rynna")
        .unwrap()
        .arg("--config")
        .arg(config)
        .args(arguments)
        .assert()
        .success();
}

#[test]
fn workspace_commands_create_update_list_and_delete_profile_owned_workspaces() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.toml");
    write_config(&config);

    run(
        &config,
        &[
            "workspaces",
            "create",
            "rynna",
            "--directory",
            "/projects/rynna",
            "--directory",
            "/projects/shared",
        ],
    );
    run(
        &config,
        &[
            "workspaces",
            "update",
            "rynna",
            "--default-directory",
            "/projects/shared",
        ],
    );
    run(&config, &["workspaces", "set-default", "/projects/home"]);

    let output = Command::cargo_bin("rynna")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .args(["workspaces", "list", "--output", "json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["profile"], "local");
    assert_eq!(value["default_workspace_directory"], "/projects/home");
    assert_eq!(value["workspaces"][0]["name"], "rynna");
    assert_eq!(
        value["workspaces"][0]["default_directory"],
        "/projects/shared"
    );

    run(&config, &["workspaces", "delete", "rynna"]);
    let catalog = rynna_config::ProfileCatalog::load(&config).unwrap();
    assert!(
        catalog
            .resolve("local")
            .unwrap()
            .profile
            .workspaces
            .is_empty()
    );
}
