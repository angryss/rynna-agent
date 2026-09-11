#![cfg(unix)]
use serde_json::{Value, json};
use std::{
    net::TcpListener,
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Server(Child, String);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
async fn start(directory: &Path) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let child = Command::new(assert_cmd::cargo::cargo_bin!("rynna"))
        .args([
            "--config",
            directory.join("config.yaml").to_str().unwrap(),
            "--provider-config",
            directory.join("providers.yaml").to_str().unwrap(),
            "serve",
            "--bind",
            &address.to_string(),
        ])
        .env(
            "RYNNA_CODEX_PATH",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex_account.py"),
        )
        .env("RYNNA_CODEX_HOME", directory.join("rynna-codex"))
        .env("RYNNA_TEST_LOG", directory.join("calls.jsonl"))
        .env("CODEX_HOME", "must-not-leak")
        .env_remove("RYNNA_PROFILE")
        .env_remove("RYNNA_MODEL")
        .env_remove("RYNNA_API_KEY")
        .env_remove("RYNNA_API_BASE")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut server = Server(child, format!("http://{address}"));
    let client = reqwest::Client::new();
    for _ in 0..250 {
        if client
            .get(format!("{}/v1/profiles", server.1))
            .send()
            .await
            .is_ok()
        {
            return server;
        }
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "server exited before readiness"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("server did not become ready")
}
fn initial_config(directory: &Path) {
    std::fs::write(
        directory.join("config.yaml"),
        format!(
            r#"
version: 1
default_profile: default
providers:
  local:
    kind: openai-compatible
    api_base: http://127.0.0.1:1/v1
profiles:
  default:
    provider: local
    model: local-test
    capabilities: [files]
capabilities:
  files:
    kind: filesystem
    root: '{}'
"#,
            directory.display()
        ),
    )
    .unwrap();
}
async fn profiles(client: &reqwest::Client, server: &Server) -> Value {
    client
        .get(format!("{}/v1/profiles", server.1))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn web_login_registers_models_and_restart_streams_through_private_account() {
    let directory = tempfile::tempdir().unwrap();
    let directory = directory.path().canonicalize().unwrap();
    initial_config(&directory);
    let client = reqwest::Client::new();
    let server = start(&directory).await;
    client
        .post(format!("{}/v1/profiles/default/providers", server.1))
        .json(&json!({"kind":"openai","authentication":"chatgpt"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let models: Vec<String> = client
        .get(format!(
            "{}/v1/profiles/default/providers/openai-account/models",
            server.1
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(models, ["account-model", "second-model"]);
    assert!(
        !directory.join("calls.jsonl").exists(),
        "model lookup must not start inference"
    );
    let listed = profiles(&client, &server).await;
    assert!(
        listed["provider_ids"]
            .as_array()
            .unwrap()
            .contains(&json!("openai-account"))
    );
    let mut profile = listed["configured_profiles"][0].clone();
    assert_eq!(profile["providers"][1]["enabled"], false);
    assert_eq!(
        listed["profiles"][0]["providers"].as_array().unwrap().len(),
        1
    );
    profile["providers"][0]["default"] = json!(false);
    profile["providers"][1]["enabled"] = json!(true);
    profile["providers"][1]["default"] = json!(true);
    profile["providers"][1]["model"] = json!("account-model");
    client
        .put(format!("{}/v1/profiles/default", server.1))
        .json(&profile)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    drop(server);
    let server = start(&directory).await;
    let response = client
        .post(format!("{}/v1/respond/stream", server.1))
        .json(&json!({"profile":"default","prompt":"Hello", "history":[],
            "selection":{"provider":"openai-account","model":"account-model"}}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(response.contains("Account answer."), "{response}");
    assert!(response.contains("Checking."), "{response}");
    let calls: Value = serde_json::from_str(
        std::fs::read_to_string(directory.join("calls.jsonl"))
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(calls["model"], "account-model");
    assert_eq!(
        calls["home"],
        directory.join("rynna-codex").to_str().unwrap()
    );
    client
        .delete(format!("{}/v1/profiles/default/providers/openai", server.1))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let response = client
        .post(format!("{}/v1/respond", server.1))
        .json(&json!({"profile":"default","prompt":"Hello", "history":[],
            "selection":{"provider":"openai-account","model":"account-model"}}))
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    let error = response.text().await.unwrap();
    assert!(error.contains("provider_error"), "{error}");
    assert_eq!(
        std::fs::read_to_string(directory.join("calls.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[tokio::test]
async fn existing_credentials_backfill_and_reused_account_ignores_private_home() {
    let directory = tempfile::tempdir().unwrap();
    let directory = directory.path().canonicalize().unwrap();
    initial_config(&directory);
    std::fs::write(directory.join("providers.yaml"), "version: 1\nprofiles:\n  default:\n  - kind: openai\n    authentication: chatgpt\n    reuse_existing: true\n").unwrap();
    let client = reqwest::Client::new();
    let server = start(&directory).await;
    let listed = profiles(&client, &server).await;
    let mut profile = listed["configured_profiles"][0].clone();
    profile["providers"][0]["default"] = json!(false);
    profile["providers"][1]["enabled"] = json!(true);
    profile["providers"][1]["default"] = json!(true);
    client
        .put(format!("{}/v1/profiles/default", server.1))
        .json(&profile)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    // Real Codex writes login status to stderr.
    let status: Value = client
        .get(format!("{}/v1/providers/openai/existing-account", server.1))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["connected"], true);
    drop(server);
    let server = start(&directory).await;
    let response = client
        .post(format!("{}/v1/respond", server.1))
        .json(&json!({"profile":"default","prompt":"Hello", "history":[],
            "selection":{"provider":"openai-account","model":"Codex default"}}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(response.contains("Account answer."), "{response}");
    let calls: Value = serde_json::from_str(
        std::fs::read_to_string(directory.join("calls.jsonl"))
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert!(calls["home"].is_null());
    assert!(calls["model"].is_null());
}

#[tokio::test]
async fn pending_browser_login_does_not_lock_profile_discovery() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().canonicalize().unwrap();
    initial_config(&directory);
    std::fs::write(directory.join("delay-login"), "").unwrap();
    let client = reqwest::Client::new();
    let server = start(&directory).await;
    let request = client
        .post(format!("{}/v1/profiles/default/providers", server.1))
        .json(&json!({"kind":"openai","authentication":"chatgpt"}));
    let login = tokio::spawn(async move { request.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(3), async {
        while !directory.join("login-started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let listed = tokio::time::timeout(Duration::from_secs(1), profiles(&client, &server)).await;
    std::fs::remove_file(directory.join("delay-login")).unwrap();
    login.await.unwrap().error_for_status().unwrap();
    assert!(listed.is_ok(), "catalog was locked during browser login");
}
