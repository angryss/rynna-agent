use assert_cmd::Command;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

#[tokio::test(flavor = "multi_thread")]
async fn cli_uses_persisted_toolset_switches() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"choices":[{"message":{"role":"assistant","content":"done"}}]}),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    std::fs::write(&config, format!("version: 1\ndefault_profile: test\nproviders:\n  local:\n    kind: openai-compatible\n    api_base: '{}/v1'\nprofiles:\n  test:\n    providers:\n      - provider: local\n        model: test\n    capabilities: [files]\n    disabled_toolsets: [file_operations]\ncapabilities:\n  files:\n    kind: filesystem\n    root: '{}'\n    read_only: false\n", server.uri(), dir.path().display())).unwrap();
    Command::cargo_bin("rynna")
        .unwrap()
        .args([
            "--config",
            config.to_str().unwrap(),
            "run",
            "--prompt",
            "hi",
        ])
        .assert()
        .success();
    let requests = server.received_requests().await.unwrap();
    let request: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let names: Vec<_> = request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["code_search"]);
}
