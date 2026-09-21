use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rynna_core::{
    Agent, Completion, CompletionRequest, ContextManagement, ContextPlan, ContextSize, Message,
    ModelProvider, ProviderContext, ProviderError, ServerCompaction, ThresholdContextManager,
    ToolCall,
};
use serde_json::json;

#[derive(Default)]
struct ManagedProvider {
    plans: Mutex<Vec<ContextPlan>>,
    server_compaction: Option<ServerCompaction>,
}

#[async_trait]
impl ModelProvider for ManagedProvider {
    async fn complete(&self, _request: CompletionRequest) -> Result<Completion, ProviderError> {
        panic!("the agent must use the context-aware provider entrypoint")
    }

    fn server_compaction(&self) -> Option<ServerCompaction> {
        self.server_compaction
    }

    async fn complete_managed(&self, plan: ContextPlan) -> Result<Completion, ProviderError> {
        self.plans.lock().unwrap().push(plan);
        Ok(Completion::new(Message::assistant("done")))
    }
}

struct InjectedContextManager;

impl ContextManagement for InjectedContextManager {
    fn prepare(
        &self,
        mut request: CompletionRequest,
        _server_compaction: Option<ServerCompaction>,
    ) -> ContextPlan {
        request.messages = vec![Message::system("injected context")];
        ContextPlan {
            request,
            size: ContextSize {
                current_tokens: 17,
                max_tokens: 4096,
            },
            server_compaction_threshold: None,
            compacted: true,
        }
    }

    fn current_size(&self) -> ContextSize {
        ContextSize {
            current_tokens: 17,
            max_tokens: 4096,
        }
    }
}

#[tokio::test]
async fn agent_context_behavior_can_be_injected_through_the_contract() {
    let provider = Arc::new(ManagedProvider::default());
    let agent = Agent::new(
        Arc::clone(&provider) as Arc<dyn ModelProvider>,
        "original context",
    )
    .with_context_manager(Arc::new(InjectedContextManager));

    agent.respond(&[], "hello").await.unwrap();

    let plans = provider.plans.lock().unwrap();
    assert!(
        plans[0].request.messages[0]
            .content
            .contains("Reason -> Act -> Observe")
    );
    assert_eq!(
        plans[0].request.messages[1],
        Message::system("injected context")
    );
    assert_eq!(agent.context_size().current_tokens, 17);
}

#[test]
fn threshold_manager_tracks_size_and_requests_server_compaction_near_the_limit() {
    let manager = ThresholdContextManager::new(100, 80).unwrap();
    let request = CompletionRequest {
        messages: vec![Message::user("x".repeat(340))],
        tools: vec![],
    };

    let plan = manager.prepare(request.clone(), Some(ServerCompaction::Other));

    assert_eq!(plan.request, request);
    assert!(plan.size.current_tokens >= 80);
    assert_eq!(plan.server_compaction_threshold, Some(80));
    assert!(!plan.compacted);
    assert_eq!(manager.current_size(), plan.size);
}

#[test]
fn threshold_manager_counts_provider_continuation_state() {
    let manager = ThresholdContextManager::new(1_000, 800).unwrap();
    let mut message = Message::assistant("answer");
    message.provider_context = Some(ProviderContext::OpenAi(vec![json!({
        "type": "compaction",
        "encrypted_content": "x".repeat(4_000)
    })]));

    let plan = manager.prepare(
        CompletionRequest {
            messages: vec![message],
            tools: vec![],
        },
        Some(ServerCompaction::OpenAi),
    );

    assert!(plan.size.current_tokens >= 800);
    assert_eq!(plan.server_compaction_threshold, Some(800));
}

#[test]
fn threshold_manager_ignores_history_superseded_by_provider_compaction() {
    let manager = ThresholdContextManager::new(1_000, 800).unwrap();
    let mut compacted = Message::assistant("summary");
    compacted.provider_context = Some(ProviderContext::AnthropicCompaction(Some(
        "short summary".to_owned(),
    )));

    let plan = manager.prepare(
        CompletionRequest {
            messages: vec![
                Message::user("x".repeat(4_000)),
                compacted,
                Message::user("continue"),
            ],
            tools: vec![],
        },
        Some(ServerCompaction::Anthropic),
    );

    assert!(plan.size.current_tokens < 800);
    assert_eq!(plan.server_compaction_threshold, None);
}

#[test]
fn local_fallback_counts_history_that_only_a_server_can_supersede() {
    let manager = ThresholdContextManager::new(1_000, 800).unwrap();
    let mut compacted = Message::assistant("summary");
    compacted.provider_context = Some(ProviderContext::AnthropicCompaction(Some(
        "short summary".to_owned(),
    )));

    let plan = manager.prepare(
        CompletionRequest {
            messages: vec![
                Message::user("x".repeat(4_000)),
                compacted,
                Message::user("continue"),
            ],
            tools: vec![],
        },
        None,
    );

    assert!(plan.compacted);
    assert!(plan.size.current_tokens < 800);
}

#[test]
fn a_provider_does_not_trust_another_providers_compaction_marker() {
    let manager = ThresholdContextManager::new(1_000, 800).unwrap();
    let mut compacted = Message::assistant("summary");
    compacted.provider_context = Some(ProviderContext::AnthropicCompaction(Some(
        "compact summary".into(),
    )));
    let request = CompletionRequest {
        messages: vec![
            Message::user("x".repeat(4_000)),
            compacted,
            Message::user("continue"),
        ],
        tools: vec![],
    };

    let plan = manager.prepare(request, Some(ServerCompaction::OpenAi));

    assert!(plan.size.current_tokens >= 800);
    assert_eq!(plan.server_compaction_threshold, Some(800));
}

#[test]
fn failed_anthropic_compaction_does_not_supersede_history() {
    let manager = ThresholdContextManager::new(1_000, 800).unwrap();
    let mut failed_compaction = Message::assistant("");
    failed_compaction.provider_context = Some(ProviderContext::AnthropicCompaction(None));
    let request = CompletionRequest {
        messages: vec![
            Message::user("x".repeat(4_000)),
            failed_compaction,
            Message::user("continue"),
        ],
        tools: vec![],
    };

    let plan = manager.prepare(request, Some(ServerCompaction::Anthropic));

    assert!(plan.size.current_tokens >= 800);
    assert_eq!(plan.server_compaction_threshold, Some(800));
}

#[test]
fn threshold_manager_compacts_locally_when_the_provider_cannot() {
    let manager = ThresholdContextManager::new(100, 80).unwrap();
    let request = CompletionRequest {
        messages: vec![
            Message::system("policy"),
            Message::user("old".repeat(80)),
            Message::assistant("old answer".repeat(30)),
            Message::user("latest question"),
        ],
        tools: vec![],
    };

    let plan = manager.prepare(request, None);

    assert!(plan.compacted);
    assert_eq!(plan.server_compaction_threshold, None);
    assert_eq!(
        plan.request.messages.first(),
        Some(&Message::system("policy"))
    );
    assert_eq!(
        plan.request.messages.last(),
        Some(&Message::user("latest question"))
    );
    assert!(plan.size.current_tokens < 80);
}

#[test]
fn local_compaction_never_separates_tool_calls_from_their_results() {
    let manager = ThresholdContextManager::new(100, 80).unwrap();
    let request = CompletionRequest {
        messages: vec![
            Message::system("policy"),
            Message::user("old".repeat(100)),
            Message::assistant_with_tool_calls(vec![ToolCall::new(
                "call-1",
                "read_file",
                json!({"path": "README.md"}),
            )]),
            Message::tool("call-1", "x".repeat(4_000)),
        ],
        tools: vec![],
    };

    let plan = manager.prepare(request, None);

    let retained_tool_result = plan
        .request
        .messages
        .iter()
        .any(|message| message.tool_call_id.as_deref() == Some("call-1"));
    let retained_tool_call = plan.request.messages.iter().any(|message| {
        message
            .tool_calls
            .iter()
            .any(|call| call.id.as_str() == "call-1")
    });
    assert_eq!(retained_tool_result, retained_tool_call);
    assert!(plan.size.current_tokens < 80);
}
