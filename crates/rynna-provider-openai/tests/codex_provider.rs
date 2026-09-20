#![cfg(unix)]

use std::{os::unix::fs::symlink, path::PathBuf, sync::Arc};

use rynna_core::{
    Agent, CompletionDelta, CompletionRequest, Message, ModelProvider, ToolCall, ToolDefinition,
};
use rynna_provider_openai::CodexAppServerProvider;

fn fixture_agent(scenario: &str) -> (tempfile::TempDir, Agent) {
    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join(scenario);
    // Execute a checked-in fixture via symlink to avoid parallel write/exec races.
    symlink(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex.sh"),
        &program,
    )
    .unwrap();
    let provider =
        CodexAppServerProvider::with_home(program, directory.path().join("codex-home"), None);
    (directory, Agent::new(Arc::new(provider), "Test policy"))
}

#[tokio::test]
async fn incompatible_app_server_fails_without_an_answer() {
    for (scenario, expected) in [
        ("malformed", "Codex app-server returned invalid JSON"),
        (
            "incompatible",
            "Codex app-server request failed: initialize unsupported",
        ),
        ("missing-thread", "Codex app-server omitted the thread id"),
        ("missing-turn", "Codex app-server omitted the turn id"),
        ("disabled-tool", "Codex attempted to start a disabled tool"),
    ] {
        let (_directory, agent) = fixture_agent(scenario);
        let mut deltas = Vec::new();
        let error = agent
            .respond_stream(&[], "Answer without tools", &mut |delta| {
                deltas.push(delta.clone())
            })
            .await
            .unwrap_err();
        assert!(
            deltas.is_empty(),
            "{scenario} emitted an answer before failing"
        );
        assert_eq!(
            error.to_string(),
            format!("model provider failed: {expected}"),
            "{scenario}"
        );
    }
}

#[tokio::test]
async fn tool_requests_are_rejected_before_launch() {
    let directory = tempfile::tempdir().unwrap();
    let provider = CodexAppServerProvider::new(directory.path().join("does-not-exist"), None);
    assert!(!provider.supports_external_tools());
    let mut tool_message = Message::assistant("");
    tool_message.tool_calls.push(ToolCall {
        id: "call-1".into(),
        name: "shell".into(),
        arguments: serde_json::json!({}),
    });
    let mut tool_result = Message::user("tool output");
    tool_result.tool_call_id = Some("call-1".into());
    for request in [
        CompletionRequest {
            messages: vec![Message::user("Use a tool")],
            tools: vec![ToolDefinition {
                name: "shell".into(),
                description: "Disabled".into(),
                input_schema: serde_json::json!({}),
            }],
        },
        CompletionRequest {
            messages: vec![tool_message, Message::user("Continue")],
            tools: vec![],
        },
        CompletionRequest {
            messages: vec![tool_result, Message::user("Continue")],
            tools: vec![],
        },
    ] {
        let error = provider.complete(request).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "model provider failed: Codex account profiles do not accept Rynna tool calls"
        );
    }
}

#[tokio::test]
async fn compatible_app_server_does_not_require_a_specific_version_banner() {
    for scenario in ["current", "future", "custom", "no-version"] {
        let (_directory, agent) = fixture_agent(scenario);
        let answer = agent
            .respond(&[], "Answer without tools")
            .await
            .unwrap_or_else(|error| panic!("{scenario}: {error}"));
        assert_eq!(
            answer,
            Message::assistant("Compatible answer"),
            "{scenario}"
        );
        let mut deltas = Vec::new();
        let streamed = agent
            .respond_stream(&[], "Answer without tools", &mut |delta| {
                deltas.push(delta.clone())
            })
            .await
            .unwrap_or_else(|error| panic!("{scenario}: {error}"));
        assert_eq!(streamed, answer);
        assert_eq!(
            deltas,
            [CompletionDelta::Content("Compatible answer".into())]
        );
    }
}
