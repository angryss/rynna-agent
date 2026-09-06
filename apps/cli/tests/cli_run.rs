use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use wiremock::matchers::{body_json, body_partial_json, body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread")]
async fn run_text_removes_terminal_control_characters() {
    let server = MockServer::start().await;
    let malicious = "safe\u{1b}[2J\u{1b}]0;owned\u{7}\r\u{8}\u{9b}31m";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": malicious}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut command = Command::cargo_bin("rynna").unwrap();
    command
        .args(["run", "--prompt", "Do the work", "--output", "text"])
        .env("RYNNA_API_BASE", format!("{}/v1", server.uri()))
        .env("RYNNA_MODEL", "test-model");

    command
        .assert()
        .success()
        .stdout(predicate::eq("safe[2J]0;owned31m\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_error_body_removes_terminal_control_characters() {
    let server = MockServer::start().await;
    let malicious = "safe\u{1b}[2J\u{1b}]0;owned\u{7}\r\u{8}\u{9b}31m";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string(malicious))
        .expect(1)
        .mount(&server)
        .await;

    // An error response must not fall through to a real locally configured provider.
    let config = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        config.path(),
        format!(
            r#"
version = 1
default_profile = "fixture"
[providers.fixture]
kind = "openai-compatible"
api_base = "{}/v1"
[profiles.fixture]
provider = "fixture"
model = "test-model"
"#,
            server.uri()
        ),
    )
    .unwrap();
    let mut command = Command::cargo_bin("rynna").unwrap();
    command
        .args(["run", "--prompt", "Do the work", "--output", "text"])
        .env("RYNNA_CONFIG", config.path())
        .env("RYNNA_API_BASE", format!("{}/v1", server.uri()))
        .env("RYNNA_MODEL", "test-model");

    command.assert().failure().stderr(
        predicate::str::contains("safe[2J]0;owned31m")
            .and(predicate::str::contains('\u{1b}').not())
            .and(predicate::str::contains('\u{7}').not())
            .and(predicate::str::contains('\r').not())
            .and(predicate::str::contains('\u{8}').not())
            .and(predicate::str::contains('\u{9b}').not()),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_json_escapes_control_characters_without_changing_content() {
    let server = MockServer::start().await;
    let malicious = "safe\u{1b}[2J\u{1b}]0;owned\u{7}\r\u{8}\u{9b}31m";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": malicious}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut command = Command::cargo_bin("rynna").unwrap();
    let output = command
        .args(["run", "--prompt", "Do the work", "--output", "json"])
        .env("RYNNA_API_BASE", format!("{}/v1", server.uri()))
        .env("RYNNA_MODEL", "test-model")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\\u001b"));
    assert!(stdout.contains("\\u0007"));
    assert!(stdout.contains("\\r"));
    assert!(stdout.contains("\\b"));
    assert!(!stdout.contains('\u{1b}'));
    assert!(!stdout.contains('\u{7}'));
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["message"]["content"], malicious);
}

#[tokio::test(flavor = "multi_thread")]
async fn run_emits_one_json_response_for_unattended_use() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({
            "model": "test-model",
            "messages": [
                {"role": "system", "content": "You are Rynna."},
                {"role": "user", "content": "Do the work"}
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "Automated."}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut command = Command::cargo_bin("rynna").unwrap();
    command
        .args(["run", "--prompt", "Do the work", "--output", "json"])
        .env("RYNNA_API_BASE", format!("{}/v1", server.uri()))
        .env("RYNNA_MODEL", "test-model")
        .env("RYNNA_SYSTEM_PROMPT", "You are Rynna.");

    command
        .assert()
        .success()
        .stdout(predicate::eq(
            "{\"message\":{\"role\":\"assistant\",\"content\":\"Automated.\"}}\n",
        ))
        .stderr(predicate::str::is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn run_reads_the_prompt_from_stdin_when_the_flag_is_omitted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({
            "model": "test-model",
            "messages": [
                {"role": "system", "content": "You are Rynna."},
                {"role": "user", "content": "Prompt from stdin"}
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "Read it."}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut command = Command::cargo_bin("rynna").unwrap();
    command
        .args(["run", "--output", "json"])
        .write_stdin("Prompt from stdin\n")
        .env("RYNNA_API_BASE", format!("{}/v1", server.uri()))
        .env("RYNNA_MODEL", "test-model")
        .env("RYNNA_SYSTEM_PROMPT", "You are Rynna.");

    command.assert().success().stdout(predicate::eq(
        "{\"message\":{\"role\":\"assistant\",\"content\":\"Read it.\"}}\n",
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn run_keeps_diagnostics_off_json_stdout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "Machine readable."}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut command = Command::cargo_bin("rynna").unwrap();
    command
        .args(["run", "--prompt", "Do the work", "--output", "json"])
        .env("RYNNA_API_BASE", format!("{}/v1", server.uri()))
        .env("RYNNA_MODEL", "test-model")
        .env("RUST_LOG", "trace");

    command.assert().success().stdout(predicate::eq(
        "{\"message\":{\"role\":\"assistant\",\"content\":\"Machine readable.\"}}\n",
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn run_uses_the_selected_profiles_provider_model_and_system_prompt() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_json(json!({
            "model": "work-model",
            "messages": [
                {"role": "system", "content": "Work profile policy"},
                {"role": "user", "content": "Use my profile"}
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "Profile selected."}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r#"
version = 1
default_profile = "local"

[providers.local]
kind = "openai-compatible"
api_base = "http://127.0.0.1:11434/v1"

[providers.work]
kind = "openai-compatible"
api_base = "{server}/v1"

[[profiles.local.providers]]
provider = "local"
model = "local-model"
enabled = true
default = true

[[profiles.work.providers]]
provider = "work"
model = "work-model"
enabled = true
default = true
[profiles.work]
system_prompt = "Work profile policy"
active_skills = []
mcp_servers = ["filesystem"]

[mcp_servers.filesystem]
transport = "stdio"
command = "mcp-filesystem"
"#,
            server = server.uri()
        ),
    )
    .unwrap();

    let mut command = Command::cargo_bin("rynna").unwrap();
    command.args([
        "--config",
        config.to_str().unwrap(),
        "--profile",
        "work",
        "run",
        "--prompt",
        "Use my profile",
    ]);

    command
        .assert()
        .success()
        .stdout(predicate::eq("Profile selected.\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn run_does_not_require_credentials_for_an_inactive_profile() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "Local profile."}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r#"
version = 1
default_profile = "local"

[providers.local]
kind = "openai-compatible"
api_base = "{server}/v1"

[providers.remote]
kind = "openai-compatible"
api_base = "https://example.com/v1"
api_key_env = "RYNNA_TEST_MISSING_REMOTE_KEY"

[[profiles.local.providers]]
provider = "local"
model = "local-model"
enabled = true
default = true

[[profiles.remote.providers]]
provider = "remote"
model = "remote-model"
enabled = true
default = true
"#,
            server = server.uri()
        ),
    )
    .unwrap();

    let mut command = Command::cargo_bin("rynna").unwrap();
    command.args([
        "--config",
        config.to_str().unwrap(),
        "run",
        "--prompt",
        "Stay local",
    ]);

    command
        .assert()
        .success()
        .stdout(predicate::eq("Local profile.\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn run_executes_the_selected_profiles_filesystem_capability() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("\"name\":\"read_file\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"README.md\"}"
                        }
                    }]
                }
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("\"role\":\"tool\""))
        .and(body_string_contains("# Rynna"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "Read the workspace file."}
            }]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("README.md"), "# Rynna\n").unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r#"
version = 1
default_profile = "local"

[providers.local]
kind = "openai-compatible"
api_base = "{server}/v1"

[[profiles.local.providers]]
provider = "local"
model = "test-model"
enabled = true
default = true
[profiles.local]
capabilities = ["workspace"]

[capabilities.workspace]
kind = "filesystem"
root = "{root}"
read_only = true
"#,
            server = server.uri(),
            root = directory.path().display()
        ),
    )
    .unwrap();

    let mut command = Command::cargo_bin("rynna").unwrap();
    command.args([
        "--config",
        config.to_str().unwrap(),
        "run",
        "--prompt",
        "Read README.md",
    ]);

    command
        .assert()
        .success()
        .stdout(predicate::eq("Read the workspace file.\n"));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn run_executes_the_selected_profiles_command_capability() {
    use std::os::unix::fs::PermissionsExt;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("\"name\":\"run_command\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "run_command",
                            "arguments": "{\"program\":\"inspect_os\"}"
                        }
                    }]
                }
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("\"role\":\"tool\""))
        .and(body_string_contains("TestOS 26.6"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "TestOS 26.6 is installed."}
            }]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;

    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("inspect-os");
    std::fs::write(&program, "#!/bin/sh\nprintf 'TestOS 26.6\\n'\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r#"
version = 1
default_profile = "local"

[providers.local]
kind = "openai-compatible"
api_base = "{server}/v1"

[[profiles.local.providers]]
provider = "local"
model = "test-model"
enabled = true
default = true
[profiles.local]
capabilities = ["host-commands"]

[capabilities.host-commands]
kind = "command"
working_directory = "{root}"
programs = {{ inspect_os = "{program}" }}
timeout_seconds = 5
max_output_bytes = 8192
"#,
            server = server.uri(),
            root = directory.path().display(),
            program = program.display()
        ),
    )
    .unwrap();

    let mut command = Command::cargo_bin("rynna").unwrap();
    command.args([
        "--config",
        config.to_str().unwrap(),
        "run",
        "--prompt",
        "What operating system is installed on this computer?",
    ]);

    command
        .assert()
        .success()
        .stdout(predicate::eq("TestOS 26.6 is installed.\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn one_shot_run_drains_queued_memory_before_exiting() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"choices":[{"message":{"role":"assistant","content":"Saved answer"}}]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version":"0.5.0"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/default/banks/test/memories/recall"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[]})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/default/banks/test/memories"))
        .and(body_partial_json(json!({"async":true})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(100))
                .set_body_json(json!({"success":true})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("memory.toml"), format!("version = 1\n[profiles.default]\nkind = 'hindsight'\ndeployment = 'self_hosted'\napi_base = '{}'\nbank_id = 'test'\n", server.uri())).unwrap();
    Command::cargo_bin("rynna")
        .unwrap()
        .args(["run", "--prompt", "Remember this"])
        .env("RYNNA_PROVIDER_CONFIG", dir.path().join("providers.toml"))
        .env("RYNNA_API_BASE", format!("{}/v1", server.uri()))
        .env("RYNNA_MODEL", "test-model")
        .assert()
        .success()
        .stdout(predicate::eq("Saved answer\n"));
    let writes = server.received_requests().await.unwrap();
    let body: serde_json::Value = writes
        .iter()
        .find(|r| r.url.path().ends_with("/memories"))
        .unwrap()
        .body_json()
        .unwrap();
    assert!(
        body["items"][0]["document_id"]
            .as_str()
            .unwrap()
            .starts_with("rynna-")
    );
    let content: serde_json::Value =
        serde_json::from_str(body["items"][0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(content[0]["content"], "Remember this");
    assert_eq!(content[1]["content"], "Saved answer");
}

#[tokio::test(flavor = "multi_thread")]
async fn run_loads_selected_skill_through_the_real_provider_tool_loop() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("\"name\":\"read_skill\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": null, "tool_calls": [{
                "id": "skill-1", "type": "function",
                "function": {"name": "read_skill", "arguments": "{\"name\":\"review\"}"}
            }]}}]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("Private review instructions"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "Skill loaded."}}]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    let skill = directory.path().join("skills/review");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: review\ndescription: Review code\n---\nPrivate review instructions",
    )
    .unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r#"
version = 1
default_profile = "work"
[providers.local]
kind = "openai-compatible"
api_base = "{}/v1"
[profiles.work]
provider = "local"
model = "test"
active_skills = ["review"]
"#,
            server.uri()
        ),
    )
    .unwrap();
    Command::cargo_bin("rynna")
        .unwrap()
        .args([
            "--config",
            config.to_str().unwrap(),
            "--provider-config",
            directory.path().join("providers.toml").to_str().unwrap(),
            "run",
            "--prompt",
            "Use $review",
        ])
        .assert()
        .success()
        .stdout(predicate::eq("Skill loaded.\n"));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(!String::from_utf8_lossy(&requests[0].body).contains("Private review instructions"));
}
