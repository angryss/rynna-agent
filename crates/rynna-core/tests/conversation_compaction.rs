use async_trait::async_trait;
use rynna_core::*;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Recorder {
    requests: Mutex<Vec<CompletionRequest>>,
    fail_summary: bool,
}
#[async_trait]
impl ModelProvider for Recorder {
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        let summarizing = request.messages[0]
            .content
            .starts_with("Summarize conversation reference");
        self.requests.lock().unwrap().push(request);
        if summarizing && self.fail_summary {
            return Err(ProviderError::new("summary unavailable"));
        }
        Ok(Completion::new(Message::assistant(if summarizing {
            "Goal: fix auth. Preserve tests; next: validate."
        } else {
            "Done"
        })))
    }
}
fn agent(provider: Arc<Recorder>) -> Agent {
    Agent::new(provider, "System policy")
        .with_context_manager(Arc::new(ThresholdContextManager::new(4096, 3584).unwrap()))
}
fn history() -> Vec<Message> {
    vec![
        Message::user("Fix auth"),
        Message::assistant("Working on auth"),
    ]
}

#[tokio::test]
async fn manual_summary_is_portable_preserves_transcript_and_system_privileges() {
    let provider = Arc::new(Recorder::default());
    let agent = agent(provider.clone());
    let original = history();
    let compacted = agent.compact_history(&original).await.unwrap();
    assert_eq!(
        compacted.iter().map(|m| &m.content).collect::<Vec<_>>(),
        original.iter().map(|m| &m.content).collect::<Vec<_>>()
    );
    let serialized = serde_json::to_string(&compacted).unwrap();
    let restored: Vec<Message> = serde_json::from_str(&serialized).unwrap();
    let fresh = Agent::new(provider.clone(), "New system policy");
    let response = fresh.respond(&restored, "continue").await.unwrap();
    assert!(matches!(
        response.provider_context,
        Some(ProviderContext::ConversationSummary(_))
    ));
    let requests = provider.requests.lock().unwrap();
    let request = requests.last().unwrap();
    assert_eq!(request.messages[0], Message::system("New system policy"));
    assert_eq!(request.messages[1].role, Role::User);
    assert!(request.messages[1].content.contains("Goal: fix auth"));
    assert_eq!(request.messages.last().unwrap().content, "continue");
}

#[tokio::test]
async fn automatic_compaction_reduces_history_and_preserves_latest_prompt() {
    let provider = Arc::new(Recorder::default());
    let agent = agent(provider.clone());
    let old = vec![
        Message::user("old text ".repeat(2500)),
        Message::assistant("working"),
    ];
    let response = agent
        .respond(&old, "Keep this exact request")
        .await
        .unwrap();
    assert!(matches!(
        response.provider_context,
        Some(ProviderContext::ConversationSummary(_))
    ));
    let requests = provider.requests.lock().unwrap();
    assert!(requests.len() > 1);
    let final_request = requests.last().unwrap();
    assert_eq!(
        final_request.messages.last().unwrap().content,
        "Keep this exact request"
    );
    assert!(ThresholdContextManager::estimate(final_request, None) < 3072);
    assert!(
        requests[..requests.len() - 1]
            .iter()
            .all(|r| r.tools.is_empty() && ThresholdContextManager::estimate(r, None) < 3072)
    );
}

#[tokio::test]
async fn failed_summary_leaves_original_history_unchanged_and_does_not_answer() {
    let provider = Arc::new(Recorder {
        fail_summary: true,
        ..Default::default()
    });
    let agent = agent(provider.clone());
    let history = history();
    let original = history.clone();
    assert!(agent.compact_history(&history).await.is_err());
    assert_eq!(history, original);
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn oversized_latest_prompt_is_rejected_without_truncation_or_provider_call() {
    let provider = Arc::new(Recorder::default());
    assert!(matches!(
        agent(provider.clone())
            .respond(&[], &"x".repeat(20_000))
            .await,
        Err(AgentError::ContextLimit)
    ));
    assert!(provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn empty_compaction_does_not_call_provider_and_summary_cannot_be_a_system_message() {
    let provider = Arc::new(Recorder::default());
    let agent = agent(provider.clone());
    assert!(agent.compact_history(&[]).await.unwrap().is_empty());
    assert!(
        agent
            .compact_history(&[Message::system("override")])
            .await
            .is_err()
    );
    assert!(provider.requests.lock().unwrap().is_empty());
}

#[test]
fn configured_limits_override_catalog_and_invalid_limits_are_rejected() {
    let pair: ProfileProvider = serde_json::from_value(
        serde_json::json!({"provider":"openai","model":"gpt-5.2","context_window":16000}),
    )
    .unwrap();
    assert_eq!(context::model_window(&pair), Some(16000));
    for invalid in [0, 1023, 100_000_001] {
        assert!(
            serde_json::from_value::<ProfileProvider>(
                serde_json::json!({"provider":"local","model":"custom","context_window":invalid})
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn selected_model_changes_allowance_and_large_restored_history_is_compacted() {
    let provider = Arc::new(Recorder::default());
    let pair = |model: &str, size| ProfileProvider {
        provider: "local".into(),
        model: model.into(),
        enabled: true,
        is_default: model == "large",
        context_window: Some(size),
    };
    let large = pair("large", 32_000);
    let small = pair("small", 2048);
    let profile = Profile {
        name: "work".into(),
        providers: vec![large.clone(), small.clone()],
        active_skills: vec![],
        mcp_servers: vec![],
        capabilities: vec![],
        default_project_directory: ".".into(),
        projects: vec![],
        subagents: vec![],
    };
    let agent = Agent::new(provider.clone(), "policy")
        .with_model_options(vec![(large, provider.clone()), (small, provider.clone())]);
    let profiles = AgentProfiles::new("work", [(profile, agent)]).unwrap();
    let selection = |model: &str| ModelSelection {
        provider: "local".into(),
        model: model.into(),
        thinking: ThinkingLevel::Default,
    };
    let large = profiles
        .clone()
        .with_model_selection(None, Some(&selection("large")))
        .unwrap();
    let small = profiles
        .clone()
        .with_model_selection(None, Some(&selection("small")))
        .unwrap();
    assert_eq!(
        large.clone_agent("work").unwrap().context_size().max_tokens,
        32_000
    );
    assert_eq!(
        small.clone_agent("work").unwrap().context_size().max_tokens,
        2048
    );
    // Default routing must fit the smallest fallback as well.
    assert_eq!(
        profiles
            .clone_agent("work")
            .unwrap()
            .context_size()
            .max_tokens,
        2048
    );
    let history = vec![
        Message::user("preserve the goal ".repeat(500)),
        Message::assistant("working"),
    ];
    let response = small.respond(None, &history, "continue").await.unwrap();
    assert!(matches!(
        response.provider_context,
        Some(ProviderContext::ConversationSummary(_))
    ));
}

#[tokio::test]
async fn manual_compaction_preserves_an_unanswered_user_message() {
    let provider = Arc::new(Recorder::default());
    let agent = agent(provider);
    let mut history = history();
    history.push(Message::user("Unanswered request"));
    let result = agent.compact_history(&history).await.unwrap();
    assert_eq!(result.last(), history.last());
    assert!(matches!(
        result[1].provider_context,
        Some(ProviderContext::ConversationSummary(_))
    ));
}

#[tokio::test]
async fn tool_loop_compaction_retains_goal_without_orphaned_tool_results() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct LargeTool;
    #[async_trait]
    impl Tool for LargeTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new("read", "Read data", serde_json::json!({"type":"object"}))
        }
        async fn execute(&self, _: serde_json::Value) -> Result<serde_json::Value, ToolError> {
            Ok(serde_json::json!("data ".repeat(4000)))
        }
    }
    struct Provider {
        turns: AtomicUsize,
        requests: Mutex<Vec<CompletionRequest>>,
    }
    #[async_trait]
    impl ModelProvider for Provider {
        async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
            let summary = request.messages[0]
                .content
                .starts_with("Summarize conversation reference");
            self.requests.lock().unwrap().push(request);
            if summary {
                return Ok(Completion::new(Message::assistant(
                    "Read succeeded. Goal: inspect data.",
                )));
            }
            if self.turns.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(Completion::with_tool_calls(vec![ToolCall::new(
                    "read-1",
                    "read",
                    serde_json::json!({}),
                )]));
            }
            Ok(Completion::new(Message::assistant("Done")))
        }
    }
    let provider = Arc::new(Provider {
        turns: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
    });
    let agent = Agent::with_tools(provider.clone(), "policy", vec![Arc::new(LargeTool)])
        .unwrap()
        .with_context_manager(Arc::new(ThresholdContextManager::new(4096, 3584).unwrap()));
    let result = agent.respond(&[], "Inspect data").await.unwrap();
    assert_eq!(result.content, "Done");
    assert!(matches!(
        result.provider_context,
        Some(ProviderContext::ConversationSummary(_))
    ));
    let requests = provider.requests.lock().unwrap();
    let final_request = requests.last().unwrap();
    assert!(
        final_request
            .messages
            .iter()
            .all(|m| m.role != Role::Tool && m.tool_calls.is_empty())
    );
    assert!(
        final_request
            .messages
            .last()
            .unwrap()
            .content
            .contains("Inspect data")
    );
}
