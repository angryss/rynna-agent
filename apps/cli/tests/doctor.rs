use assert_cmd::Command;
use predicates::prelude::*;

/// Writes a catalog and returns its path plus the owning directory.
fn catalog(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("config.toml");
    std::fs::write(&path, body).expect("write catalog");
    (directory, path)
}

fn doctor(config: &std::path::Path) -> Command {
    let mut command = Command::cargo_bin("rynna").expect("rynna binary");
    command.arg("--config").arg(config).arg("doctor");
    // Keep the developer's real environment from deciding the result.
    command.env_remove("DOCTOR_TEST_KEY");
    command
}

const HEALTHY: &str = r#"
version = 1
default_profile = "local"
[providers.ollama]
kind = "openai-compatible"
api_base = "http://localhost:11434/v1"
[profiles.local]
provider = "ollama"
model = "llama3"
"#;

#[test]
fn a_healthy_catalog_reports_no_problems_and_exits_zero() {
    let (_directory, config) = catalog(HEALTHY);
    doctor(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("No problems found"));
}

#[test]
fn a_missing_credential_variable_fails_without_naming_its_value() {
    let (_directory, config) = catalog(
        r#"
version = 1
default_profile = "work"
[providers.remote]
kind = "anthropic-messages"
api_base = "https://api.anthropic.com"
api_key_env = "DOCTOR_TEST_KEY"
[profiles.work]
provider = "remote"
model = "claude-opus-5"
"#,
    );
    doctor(&config)
        .assert()
        .failure()
        .stdout(predicate::str::contains("$DOCTOR_TEST_KEY"))
        .stdout(predicate::str::contains("is not set"));
}

#[test]
fn a_set_credential_variable_passes_and_its_value_never_appears() {
    let (_directory, config) = catalog(
        r#"
version = 1
default_profile = "work"
[providers.remote]
kind = "anthropic-messages"
api_base = "https://api.anthropic.com"
api_key_env = "DOCTOR_TEST_KEY"
[profiles.work]
provider = "remote"
model = "claude-opus-5"
"#,
    );
    let mut command = doctor(&config);
    command.env("DOCTOR_TEST_KEY", "super-secret-value");
    command
        .assert()
        .success()
        // Presence is reported; the value must never be echoed.
        .stdout(predicate::str::contains("reads $DOCTOR_TEST_KEY"))
        .stdout(predicate::str::contains("super-secret-value").not());
}

#[test]
fn a_missing_filesystem_root_fails() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let config = directory.path().join("config.toml");
    let missing = directory.path().join("absent");
    std::fs::write(
        &config,
        format!(
            r#"
version = 1
default_profile = "work"
[providers.ollama]
kind = "openai-compatible"
api_base = "http://localhost:11434/v1"
[profiles.work]
provider = "ollama"
model = "llama3"
capabilities = ["files"]
[capabilities.files]
kind = "filesystem"
root = "{}"
allowed_patterns = ["**/*"]
"#,
            missing.display()
        ),
    )
    .expect("write catalog");

    doctor(&config)
        .assert()
        .failure()
        .stdout(predicate::str::contains("does not exist"));
}

#[test]
fn an_existing_filesystem_root_passes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let config = directory.path().join("config.toml");
    let root = directory.path().join("workspace");
    std::fs::create_dir(&root).expect("create root");
    std::fs::write(
        &config,
        format!(
            r#"
version = 1
default_profile = "work"
[providers.ollama]
kind = "openai-compatible"
api_base = "http://localhost:11434/v1"
[profiles.work]
provider = "ollama"
model = "llama3"
capabilities = ["files"]
[capabilities.files]
kind = "filesystem"
root = "{}"
allowed_patterns = ["**/*"]
"#,
            root.display()
        ),
    )
    .expect("write catalog");

    doctor(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("No problems found"));
}

#[test]
fn json_output_is_machine_readable() {
    let (_directory, config) = catalog(HEALTHY);
    let output = doctor(&config)
        .arg("--output")
        .arg("json")
        .output()
        .unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor emits valid JSON");
    assert_eq!(report["default_profile"], "local");
    assert!(
        report["findings"]
            .as_array()
            .expect("findings array")
            .iter()
            .all(|finding| finding["severity"] == "ok")
    );
}

#[test]
fn every_problem_is_reported_in_one_run() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let config = directory.path().join("config.toml");
    // Two independent problems: the run must not stop at the first.
    std::fs::write(
        &config,
        format!(
            r#"
version = 1
default_profile = "work"
[providers.remote]
kind = "anthropic-messages"
api_base = "https://api.anthropic.com"
api_key_env = "DOCTOR_TEST_KEY"
[profiles.work]
provider = "remote"
model = "claude-opus-5"
capabilities = ["files"]
[capabilities.files]
kind = "filesystem"
root = "{}"
allowed_patterns = ["**/*"]
"#,
            directory.path().join("absent").display()
        ),
    )
    .expect("write catalog");

    doctor(&config)
        .assert()
        .failure()
        .stdout(predicate::str::contains("$DOCTOR_TEST_KEY"))
        .stdout(predicate::str::contains("does not exist"))
        .stdout(predicate::str::contains("2 of 2 checks need attention"));
}
