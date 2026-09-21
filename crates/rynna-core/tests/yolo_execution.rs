use async_trait::async_trait;
use rynna_core::{
    Agent, AgentProfiles, Completion, CompletionRequest, Message, ModelProvider, Profile,
    ProviderError, Tool, ToolCall, ToolDefinition, ToolError,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct Caller(AtomicUsize);
#[async_trait]
impl ModelProvider for Caller {
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            assert!(request.tools.iter().any(|t| t.name == "read_file"));
            Ok(Completion::with_tool_calls(
                (0..65)
                    .map(|i| ToolCall::new(i.to_string(), "read_file", json!({})))
                    .collect(),
            ))
        } else {
            Ok(Completion::new(Message::assistant("done")))
        }
    }
}
struct Slow(Arc<AtomicUsize>);
#[async_trait]
impl Tool for Slow {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new("read_file", "fixture", json!({"type":"object"}))
    }
    async fn execute(&self, _: Value) -> Result<Value, ToolError> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_secs(301)).await;
        }
        Ok(json!("fixture"))
    }
}
#[tokio::test(start_paused = true)]
async fn yolo_bypasses_disabled_toolsets_call_budget_and_execution_deadline() {
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::with_tools(
        Arc::new(Caller(AtomicUsize::new(0))),
        "",
        vec![Arc::new(Slow(calls.clone()))],
    )
    .unwrap()
    .with_yolo(true);
    let profile: Profile = serde_json::from_value(
        json!({"name":"test","providers":[],"disabled_toolsets":["file_operations"]}),
    )
    .unwrap();
    let profiles = AgentProfiles::new("test", [(profile, agent)]).unwrap();
    assert_eq!(
        profiles.respond(None, &[], "read").await.unwrap().content,
        "done"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 65);
}
