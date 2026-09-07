use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread")]
async fn chat_removes_terminal_control_characters() {
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
        .arg("chat")
        .write_stdin("Hello\n/quit\n")
        .env("RYNNA_API_BASE", format!("{}/v1", server.uri()))
        .env("RYNNA_MODEL", "test-model");

    command.assert().success().stdout(predicate::eq(
        "Rynna interactive mode. /compact summarizes context; /model selects a model; /thinking sets effort; /quit exits.\nyou> rynna> safe[2J]0;owned31m\nContext: ~1% used (127 / 8192 estimated tokens).\nyou> ",
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_keeps_history_until_the_user_quits() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({
            "model": "test-model",
            "messages": [
                {"role": "system", "content": "You are Rynna."},
                {"role": "user", "content": "Hello"}
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "Ready."}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({
            "model": "test-model",
            "messages": [
                {"role": "system", "content": "You are Rynna."},
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Ready."},
                {"role": "user", "content": "Next"}
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "Done."}
            }]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;

    let mut command = Command::cargo_bin("rynna").unwrap();
    command
        .arg("chat")
        .write_stdin("Hello\nNext\n/quit\n")
        .env("RYNNA_API_BASE", format!("{}/v1", server.uri()))
        .env("RYNNA_MODEL", "test-model")
        .env("RYNNA_SYSTEM_PROMPT", "You are Rynna.");

    command.assert().success().stdout(
        predicate::str::contains("Rynna interactive mode")
            .and(predicate::str::contains("Ready."))
            .and(predicate::str::contains("Done.")),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_alias_quits_without_contacting_the_provider() {
    let server = MockServer::start().await;
    let mut command = Command::cargo_bin("rynna").unwrap();
    command
        .arg("chat")
        .write_stdin("/exit\n")
        .env("RYNNA_API_BASE", format!("{}/v1", server.uri()))
        .env("RYNNA_MODEL", "test-model");

    command.assert().success().stdout(predicate::eq(
        "Rynna interactive mode. /compact summarizes context; /model selects a model; /thinking sets effort; /quit exits.\nyou> ",
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_switches_provider_model_and_effort_while_preserving_history() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(&config, format!(r#"
version = 1
default_profile = "local"
[providers.first]
kind = "openai-compatible"
api_base = "{}/v1"
[providers.second]
kind = "openai-compatible"
api_base = "{}/v1"
[profiles.local]
providers = [{{provider = "second", model = "second-model"}}, {{provider = "first", model = "first-model", default = true}}]
"#, server.uri(), server.uri())).unwrap();
    Mock::given(path("/v1/chat/completions"))
        .and(body_partial_json(json!({"model":"overridden-model"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"choices":[{"message":{"role":"assistant","content":"first answer"}}]}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/v1/chat/completions"))
        .and(body_partial_json(json!({"model":"second-model","reasoning_effort":"high","messages":[{"role":"system","content":"You are Rynna, a careful and capable AI software agent."},{"role":"user","content":"Hello"},{"role":"assistant","content":"first answer"},{"role":"user","content":"Continue"}]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices":[{"message":{"role":"assistant","content":"second answer"}}]})))
        .expect(1).mount(&server).await;
    Command::cargo_bin("rynna")
        .unwrap()
        .args(["--config", config.to_str().unwrap(), "chat"])
        .env("RYNNA_MODEL", "overridden-model")
        .write_stdin("Hello\n/model 1\n/thinking high\n/model missing unavailable\n/thinking invalid\nContinue\n/quit\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("second answer"));
}
