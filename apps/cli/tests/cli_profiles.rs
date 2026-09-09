use assert_cmd::Command;
use predicates::prelude::predicate;
use serde_json::Value;

#[test]
fn profiles_lists_configured_profiles_without_contacting_providers() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.yaml");
    std::fs::write(
        &config,
        r#"
version: 1
default_profile: local
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  local:
    provider: ollama
    model: qwen3:8b
    active_skills:
    - rust
    subagents:
    - name: reviewer
      description: Review code
      instructions: Find bugs
  work:
    provider: ollama
    model: qwen3:14b
    active_skills:
    - github
"#,
    )
    .unwrap();

    let output = Command::cargo_bin("rynna")
        .unwrap()
        .args([
            "--config",
            config.to_str().unwrap(),
            "profiles",
            "--output",
            "json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["default_profile"], "local");
    assert_eq!(value["profiles"][0]["name"], "local");
    assert_eq!(value["profiles"][1]["name"], "work");
    assert_eq!(value["profiles"][1]["providers"][0]["model"], "qwen3:14b");
    assert_eq!(value["profiles"][1]["active_skills"][0], "github");
    assert_eq!(value["profiles"][0]["subagents"][0]["name"], "reviewer");
    assert_eq!(value["profiles"][1]["subagents"], serde_json::json!([]));
}

#[test]
fn profiles_reports_the_effective_model_for_the_selected_default() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.yaml");
    std::fs::write(
        &config,
        r#"
version: 1
default_profile: local
providers:
  offline:
    kind: openai-compatible
    api_base: https://offline.invalid/v1
profiles:
  local:
    provider: offline
    model: catalog-model
  work:
    provider: offline
    model: work-model
"#,
    )
    .unwrap();

    let output = Command::cargo_bin("rynna")
        .unwrap()
        .args([
            "--config",
            config.to_str().unwrap(),
            "--profile",
            "work",
            "--model",
            "effective-model",
            "profiles",
            "--output",
            "json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["default_profile"], "work");
    assert_eq!(
        value["profiles"][0]["providers"][0]["model"],
        "catalog-model"
    );
    assert_eq!(
        value["profiles"][1]["providers"][0]["model"],
        "effective-model"
    );
}

#[test]
fn profiles_rejects_a_blank_effective_model() {
    let mut command = Command::cargo_bin("rynna").unwrap();
    command.args(["--model", "   ", "profiles"]);

    command
        .assert()
        .failure()
        .stderr(predicate::str::contains("provider model must not be blank"));
}
