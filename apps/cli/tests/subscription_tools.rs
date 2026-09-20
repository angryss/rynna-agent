#![cfg(unix)]
use assert_cmd::Command;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

#[test]
fn every_builtin_provider_keeps_tools_in_mixed_fallback_chains() {
    use rynna_core::{FallbackProvider, ModelProvider};
    use rynna_provider_anthropic::{AnthropicMessagesProvider, ClaudeCodeProvider};
    use rynna_provider_openai::{CodexAppServerProvider, OpenAiCompatibleProvider};
    use std::sync::Arc;
    let providers: Vec<Arc<dyn ModelProvider>> = vec![
        Arc::new(OpenAiCompatibleProvider::new("http://127.0.0.1:1/v1", "local", None).unwrap()),
        Arc::new(
            AnthropicMessagesProvider::with_base_url(
                "http://127.0.0.1:1",
                "claude",
                "test-not-a-credential",
            )
            .unwrap(),
        ),
        Arc::new(ClaudeCodeProvider::new("/nonexistent/claude", "sonnet")),
        Arc::new(CodexAppServerProvider::new(
            "/nonexistent/codex",
            Some("test".into()),
        )),
    ];
    for provider in &providers {
        assert!(provider.supports_external_tools());
    }
    assert!(
        FallbackProvider::new(providers)
            .unwrap()
            .supports_external_tools()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn subscription_and_fallback_preserve_opt_in_capabilities_and_disabled_toolsets() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&server)
        .await;
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude_profile_tools.py");
    // Test both explicit Claude selection and API -> Claude profile-default fallback.
    for fallback in [false, true] {
        for (capabilities, disabled, prompt, expected) in [
            ("[]", "[]", "list", "no-tools"),
            ("[files]", "[file_operations]", "list", "code_search"),
            (
                "[files, host]",
                "[file_operations, code_search]",
                "list",
                "run_command",
            ),
            (
                "[files, host]",
                "[commands, file_operations]",
                "list",
                "code_search",
            ),
            ("[host]", "[]", "inspect", "Linux"),
            (
                "[host]",
                "[]",
                "inspect-denied",
                "not allowed by command policy",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join("config.yaml");
            let first = if fallback {
                "      - provider: api\n        model: unavailable\n"
            } else {
                ""
            };
            std::fs::write(
                &config,
                format!(
                    r#"version: 1
default_profile: test
providers:
  api:
    kind: openai-compatible
    api_base: '{}/v1'
  claude:
    kind: claude-subscription
    claude_program: '{}'
profiles:
  test:
    providers:
{}      - provider: claude
        model: sonnet
    capabilities: {}
    disabled_toolsets: {}
capabilities:
  files:
    kind: filesystem
    root: '{}'
    read_only: true
  host:
    kind: command
    working_directory: '{}'
    programs:
      inspect_os: /usr/bin/uname
    timeout_seconds: 5
    max_output_bytes: 4096
"#,
                    server.uri(),
                    fixture.display(),
                    first,
                    capabilities,
                    disabled,
                    dir.path().display(),
                    dir.path().display()
                ),
            )
            .unwrap();
            let output = Command::cargo_bin("rynna")
                .unwrap()
                .env("XDG_CONFIG_HOME", dir.path().join("xdg"))
                .args([
                    "--config",
                    config.to_str().unwrap(),
                    "--provider-config",
                    dir.path().join("providers.yaml").to_str().unwrap(),
                    "run",
                    "--prompt",
                    prompt,
                ])
                .assert()
                .success()
                .get_output()
                .stdout
                .clone();
            let output = String::from_utf8(output).unwrap();
            assert!(
                output.contains(expected),
                "fallback={fallback}, {capabilities}, {disabled}: {output}"
            );
            if prompt == "list" {
                assert_eq!(output.trim(), expected);
            }
        }
    }
    let requests = server.received_requests().await.unwrap();
    assert!(!requests.is_empty());
    assert!(
        requests
            .iter()
            .any(|r| String::from_utf8_lossy(&r.body).contains("run_command")),
        "fallback must retain tools on the primary request, too"
    );
}
