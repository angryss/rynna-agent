use async_trait::async_trait;
use rynna_core::*;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Recorder(Mutex<Vec<CompletionRequest>>);
#[async_trait]
impl ModelProvider for Recorder {
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        self.0.lock().unwrap().push(request);
        Ok(Completion::new(Message::assistant("Done")))
    }
}

struct NativeTool;
#[async_trait]
impl Tool for NativeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "native_tool",
            "Native operation",
            serde_json::json!({"type":"object"}),
        )
    }
    async fn execute(&self, _: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        Ok(serde_json::Value::Null)
    }
}

struct NoExternalTools(Recorder);
#[async_trait]
impl ModelProvider for NoExternalTools {
    fn supports_external_tools(&self) -> bool {
        false
    }
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        self.0.complete(request).await
    }
}

#[tokio::test]
async fn conversation_size_excludes_native_tools_when_provider_cannot_use_them() {
    let provider = Arc::new(NoExternalTools(Recorder::default()));
    let agent = Agent::with_tools(
        provider.clone(),
        "Configured policy",
        vec![Arc::new(NativeTool)],
    )
    .unwrap();
    let size = agent.conversation_size(&[], "Fix a bug").unwrap();
    agent.respond(&[], "Fix a bug").await.unwrap();
    let requests = provider.0.0.lock().unwrap();
    assert!(requests[0].tools.is_empty());
    assert_eq!(
        size.current_tokens,
        ThresholdContextManager::estimate(&requests[0], None)
    );
}

struct PrependingManager;
impl ContextManagement for PrependingManager {
    fn prepare(&self, mut request: CompletionRequest, _: Option<ServerCompaction>) -> ContextPlan {
        request
            .messages
            .insert(0, Message::system("context manager policy"));
        request.messages.push(request.messages[1].clone());
        request.tools.clear();
        ContextPlan {
            request,
            size: self.current_size(),
            server_compaction_threshold: None,
            compacted: false,
        }
    }
    fn current_size(&self) -> ContextSize {
        ContextSize {
            current_tokens: 0,
            max_tokens: 8192,
        }
    }
}

#[tokio::test]
async fn prepended_context_preserves_unrelated_messages_and_reconciles_changed_inventory() {
    let provider = Arc::new(Recorder::default());
    let agent = Agent::with_tools(
        provider.clone(),
        "Configured policy",
        vec![Arc::new(NativeTool)],
    )
    .unwrap()
    .with_context_manager(Arc::new(PrependingManager));
    agent.respond(&[], "Fix a bug").await.unwrap();
    let requests = provider.0.lock().unwrap();
    let request = &requests[0];
    assert_policy(request);
    assert_eq!(
        request
            .messages
            .iter()
            .filter(|m| m.content.contains("Available tools for this request"))
            .count(),
        1
    );
    assert!(!request.messages[0].content.contains("native_tool"));
    assert_eq!(
        request.messages[1],
        Message::system("context manager policy")
    );
    assert_eq!(request.messages[2], Message::user("Fix a bug"));
    assert_eq!(request.messages.len(), 3);
}

mod common;
use common::assert_policy;

struct FailingRecorder(Mutex<Vec<CompletionRequest>>);
#[async_trait]
impl ModelProvider for FailingRecorder {
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        self.0.lock().unwrap().push(request);
        Err(ProviderError::new("retry on fallback"))
    }
}

#[tokio::test]
async fn fallback_attempts_keep_the_same_policy_in_both_response_modes() {
    for stream in [false, true] {
        let failed = Arc::new(FailingRecorder(Mutex::new(Vec::new())));
        let succeeded = Arc::new(Recorder::default());
        let provider =
            Arc::new(FallbackProvider::new(vec![failed.clone(), succeeded.clone()]).unwrap());
        let agent = Agent::new(provider, "Configured policy");
        if stream {
            agent
                .respond_stream(&[], "Fix a bug", &mut |_| {})
                .await
                .unwrap();
        } else {
            agent.respond(&[], "Fix a bug").await.unwrap();
        }
        let failed = failed.0.lock().unwrap();
        let succeeded = succeeded.0.lock().unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(succeeded.len(), 1);
        assert_policy(&failed[0]);
        assert_policy(&succeeded[0]);
        assert_eq!(failed[0], succeeded[0]);
    }
}

#[tokio::test]
async fn summary_requests_include_policy_without_tools_and_keep_summary_instructions() {
    let provider = Arc::new(Recorder::default());
    let agent = Agent::new(provider.clone(), "Configured policy");
    agent
        .compact_history(&[Message::user("Fix a bug"), Message::assistant("Done")])
        .await
        .unwrap();
    let requests = provider.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_policy(&requests[0]);
    assert!(requests[0].tools.is_empty());
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|m| m.role == Role::System && m.content.contains("Return only a concise summary"))
    );
}

#[tokio::test]
async fn conversation_size_counts_the_same_policy_and_inventory_as_dispatch() {
    let provider = Arc::new(Recorder::default());
    let agent = Agent::new(provider.clone(), "Configured policy");
    let size = agent.conversation_size(&[], "Fix a bug").unwrap();
    agent.respond(&[], "Fix a bug").await.unwrap();
    let requests = provider.0.lock().unwrap();
    assert_eq!(
        size.current_tokens,
        ThresholdContextManager::estimate(&requests[0], None)
    );
}

#[tokio::test]
async fn ordinary_requests_include_development_policy_and_preserve_configured_context() {
    let provider = Arc::new(Recorder::default());
    let agent = Agent::new(
        provider.clone(),
        "Configured policy\nProject instructions\nActive skill instructions",
    );
    agent.respond(&[], "Fix a bug").await.unwrap();
    let requests = provider.0.lock().unwrap();
    assert_policy(&requests[0]);
    assert!(
        requests[0].messages[0]
            .content
            .contains("Configured policy\nProject instructions\nActive skill instructions")
    );
}
