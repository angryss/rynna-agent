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
async fn inherited_mcp_servers_are_disabled_before_starting_a_turn() {
    let directory = tempfile::tempdir().unwrap();
    let provider = CodexAppServerProvider::with_home(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex_ambient.py"),
        directory.path().join("home"),
        None,
    );
    let agent = Agent::new(Arc::new(provider), "Test policy");
    let answer = agent
        .respond(&[], "What operating system is installed on this computer?")
        .await
        .unwrap();
    assert_eq!(answer, Message::assistant("No ambient tools available"));
}

#[tokio::test]
async fn invalid_mcp_inventory_fails_closed_without_disclosing_configuration() {
    for (scenario, expected) in [
        ("inventory-failed", "Codex MCP inventory failed"),
        (
            "inventory-malformed",
            "Codex returned invalid MCP inventory",
        ),
        (
            "inventory-oversized",
            "Codex MCP inventory exceeded the size limit",
        ),
        ("inventory-missing-name", "Codex omitted an MCP server name"),
        (
            "inventory-unsupported",
            "Codex returned an unsupported MCP transport",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join(scenario);
        symlink(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex_ambient.py"),
            &program,
        )
        .unwrap();
        let provider =
            CodexAppServerProvider::with_home(program, directory.path().join("home"), None);
        let error = provider
            .complete(CompletionRequest {
                messages: vec![Message::user("Hello")],
                tools: vec![],
            })
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            format!("model provider failed: {expected}"),
            "{scenario}"
        );
        assert!(!error.contains("private-config-canary"));
    }
}

#[tokio::test]
async fn dynamic_tools_round_trip_through_rynna_history() {
    let directory = tempfile::tempdir().unwrap();
    let provider = CodexAppServerProvider::with_home(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex_tools.py"),
        directory.path().join("home"),
        None,
    );
    let tools = vec![ToolDefinition::new(
        "inspect_host",
        "Inspect",
        serde_json::json!({"type":"object"}),
    )];
    let user = Message::user("Inspect host");
    let completion = provider
        .complete(CompletionRequest {
            messages: vec![user.clone()],
            tools: tools.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        completion.message.tool_calls,
        vec![ToolCall::new(
            "call-1",
            "inspect_host",
            serde_json::json!({"kind":"os"})
        )]
    );
    let answer = provider
        .complete(CompletionRequest {
            messages: vec![
                user,
                completion.message,
                Message::tool("call-1", "Authorized host result"),
            ],
            tools,
        })
        .await
        .unwrap();
    assert_eq!(answer.message, Message::assistant("Host verified"));
    assert!(provider.supports_external_tools());
}

// The fixture records the actual wire requests, not the caller's history. Checking
// each replay catches dropped, duplicated, reordered, or cross-conversation items.
async fn replay_conversation(
    provider: &CodexAppServerProvider,
    home: &std::path::Path,
    identity: &str,
) {
    let prompt = format!("Inspect host for {identity}; retain this original request exactly once.");
    let mut messages = vec![
        Message::system(format!("Replay test: {identity}")),
        Message::user(&prompt),
    ];
    let tools = vec![ToolDefinition::new(
        "inspect_host",
        "Inspect",
        serde_json::json!({"type":"object"}),
    )];
    let mut expected_items = vec![serde_json::json!({
        "type":"message", "role":"user",
        "content":[{"type":"input_text", "text":prompt}]
    })];
    for round in 0..=3 {
        let completion = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            provider.complete(CompletionRequest {
                messages: messages.clone(),
                tools: tools.clone(),
            }),
        )
        .await
        .expect("fixture rendezvous or completion timed out")
        .unwrap();
        let wire: serde_json::Value = serde_json::from_slice(
            &std::fs::read(home.join(format!("{identity}-{round}.json")))
                .expect("fixture must record this conversation and round"),
        )
        .unwrap();
        assert_eq!(wire["turn"]["method"], "turn/start");
        if round == 0 {
            assert_eq!(wire["items"], serde_json::json!([]));
            assert_eq!(
                wire["turn"]["params"]["input"],
                serde_json::json!([
                    {"type":"text", "text":prompt}
                ])
            );
        } else {
            assert_eq!(
                wire["items"],
                serde_json::json!(expected_items),
                "{identity} round {round}"
            );
            let input = wire["turn"]["params"]["input"].as_array().unwrap();
            assert_eq!(input.len(), 1);
            assert_eq!(input[0]["type"], "text");
            let text = input[0]["text"].as_str().unwrap();
            assert!(!text.is_empty());
            assert!(
                !text.contains(&prompt),
                "original prompt must not be resubmitted"
            );
            for item in &expected_items {
                if item["type"] == "function_call_output" {
                    assert!(
                        !text.contains(item["output"].as_str().unwrap()),
                        "tool output must not become user input"
                    );
                }
            }
        }
        if round == 3 {
            assert_eq!(
                completion.message,
                Message::assistant(format!("Verified {identity}"))
            );
            break;
        }
        // IDs intentionally collide between conversations; arguments/results do not.
        let call_id = format!("call-{}", round + 1);
        let arguments = serde_json::json!({"conversation":identity,"step":round + 1});
        assert_eq!(
            completion.message.tool_calls,
            vec![ToolCall::new(&call_id, "inspect_host", arguments.clone())]
        );
        let result = format!("Private {identity} result {}", round + 1);
        expected_items.push(serde_json::json!({
            "type":"function_call", "call_id":call_id,
            "name":"inspect_host", "arguments":arguments.to_string()
        }));
        expected_items.push(serde_json::json!({
            "type":"function_call_output", "call_id":call_id, "output":result
        }));
        messages.push(completion.message);
        messages.push(Message::tool(call_id, result));
    }
}

#[tokio::test]
async fn continuation_replays_original_user_once_and_all_prior_tool_pairs() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let provider = CodexAppServerProvider::with_home(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex_tools.py"),
        &home,
        None,
    );
    replay_conversation(&provider, &home, "solo").await;
}

#[tokio::test]
async fn concurrent_conversations_isolate_replayed_history_with_colliding_call_ids() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let provider = CodexAppServerProvider::with_home(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex_tools.py"),
        &home,
        None,
    );
    // Share the exact provider and home. A fixture rendezvous at every round
    // requires overlapping subprocesses, rather than relying on scheduling luck.
    tokio::join!(
        replay_conversation(&provider, &home, "alpha"),
        replay_conversation(&provider, &home, "beta"),
    );
}

struct HostTool {
    allowed: bool,
    executions: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl rynna_core::Tool for HostTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "inspect_host",
            "Inspect",
            serde_json::json!({"type":"object"}),
        )
    }
    async fn execute(
        &self,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, rynna_core::ToolError> {
        assert_eq!(arguments, serde_json::json!({"kind":"os"}));
        if !self.allowed {
            return Err(rynna_core::ToolError::new("permission denied"));
        }
        self.executions
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(serde_json::json!("Authorized host result"))
    }
}

#[tokio::test]
async fn agent_executes_only_authorized_tools_and_preserves_denials() {
    for allowed in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let provider = Arc::new(CodexAppServerProvider::with_home(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex_tools.py"),
            directory.path().join("home"),
            None,
        ));
        let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agent = Agent::with_tools(
            provider.clone(),
            "Policy",
            vec![Arc::new(HostTool {
                allowed,
                executions: executions.clone(),
            })],
        )
        .unwrap();
        let mut deltas = Vec::new();
        let answer = agent
            .respond_stream(&[], "Inspect host", &mut |d| deltas.push(d.clone()))
            .await
            .unwrap();
        assert_eq!(
            answer.content,
            if allowed {
                "Host verified"
            } else {
                "Permission denied"
            }
        );
        assert_eq!(
            executions.load(std::sync::atomic::Ordering::SeqCst),
            usize::from(allowed)
        );
        assert!(
            deltas
                .iter()
                .any(|d| matches!(d, CompletionDelta::ToolStarted(_)))
        );
        let unavailable = Agent::new(provider, "No tools")
            .respond(&[], "Inspect host")
            .await
            .unwrap_err();
        assert!(unavailable.to_string().contains("unadvertised Rynna tool"));
    }
}

#[tokio::test]
async fn continuation_reused_call_id_is_rejected_before_second_execution() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Arc::new(CodexAppServerProvider::with_home(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex_tools.py"),
        directory.path().join("home"),
        None,
    ));
    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let agent = Agent::with_tools(
        provider,
        "Reused call test",
        vec![Arc::new(HostTool {
            allowed: true,
            executions: executions.clone(),
        })],
    )
    .unwrap();
    let error = agent.respond(&[], "Inspect host").await.unwrap_err();
    assert_eq!(
        executions.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a completed tool call must not execute again on continuation"
    );
    assert_eq!(
        error.to_string(),
        "model provider failed: Codex reused a tool call ID"
    );
}

#[tokio::test]
async fn invalid_dynamic_requests_fail_closed() {
    for scenario in [
        "missing-id",
        "unsupported",
        "unadvertised",
        "wrong-turn",
        "malformed-arguments",
        "namespace",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let provider = CodexAppServerProvider::with_home(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex_tools.py"),
            directory.path().join("home"),
            None,
        );
        let error = provider
            .complete(CompletionRequest {
                messages: vec![Message::user(scenario)],
                tools: vec![ToolDefinition::new(
                    "inspect_host",
                    "Inspect",
                    serde_json::json!({"type":"object"}),
                )],
            })
            .await
            .unwrap_err();
        let expected = if scenario == "unsupported" {
            "unsupported operation"
        } else {
            "invalid or unadvertised Rynna tool"
        };
        assert!(error.to_string().contains(expected), "{scenario}: {error}");
    }
}

#[tokio::test]
async fn malformed_tool_history_is_rejected_before_launch() {
    let directory = tempfile::tempdir().unwrap();
    let provider = CodexAppServerProvider::new(directory.path().join("does-not-exist"), None);
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
            "model provider failed: Codex request contains invalid tool history"
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
