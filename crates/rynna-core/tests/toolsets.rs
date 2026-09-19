use async_trait::async_trait;
use rynna_core::{
    Agent, AgentProfiles, Completion, CompletionRequest, ModelProvider, Profile, ProviderError,
    Tool, ToolCall, ToolDefinition, ToolError,
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Provider {
    requests: Mutex<Vec<CompletionRequest>>,
}
#[async_trait]
impl ModelProvider for Provider {
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        self.requests.lock().unwrap().push(request);
        Ok(Completion::with_tool_calls(vec![ToolCall::new(
            "1",
            "read_file",
            json!({}),
        )]))
    }
}
struct Reader(Arc<AtomicUsize>);
#[async_trait]
impl Tool for Reader {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new("read_file", "read", json!({}))
    }
    async fn execute(&self, _: Value) -> Result<Value, ToolError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(json!("secret"))
    }
}
fn profile(disabled: Value) -> Profile {
    serde_json::from_value(json!({"name":"test", "providers":[], "disabled_toolsets":disabled}))
        .unwrap()
}

#[test]
fn groups_use_real_native_names_and_reject_unknown_ids() {
    use rynna_core::toolsets::ToolsetId;
    for (id, names) in [
        (
            ToolsetId::FileOperations,
            vec![
                "read_file",
                "write_file",
                "edit_file",
                "search_files",
                "find_files",
                "list_directory",
                "create_directory",
                "file_info",
            ],
        ),
        (ToolsetId::CodeSearch, vec!["code_search"]),
        (ToolsetId::Commands, vec!["run_command"]),
        (ToolsetId::Skills, vec!["read_skill"]),
        (ToolsetId::Subagents, vec!["delegate_task"]),
    ] {
        for name in names {
            assert!(id.contains(name));
        }
        assert!(!id.contains("patch"));
        assert!(!id.contains("mcp_server__read_file"));
    }
    assert!(
        serde_json::from_value::<Profile>(
            json!({"name":"test", "providers":[], "disabled_toolsets":["typo"]})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<Profile>(json!({"name":"test", "providers":[]}))
            .unwrap()
            .disabled_toolsets
            .is_empty()
    );
}

#[tokio::test]
async fn workflow_cannot_bypass_disabled_subagents() {
    let provider = Arc::new(Provider::default());
    let profiles = AgentProfiles::new(
        "test",
        [(
            profile(json!(["subagents"])),
            Agent::new(provider.clone(), ""),
        )],
    )
    .unwrap();
    let mut workflow = rynna_core::workflows::default_workflow();
    workflow.steps[0].executor = rynna_core::workflows::Executor::Subagent;
    workflow.steps[0].helper = Some("helper".into());
    let run = serde_json::from_value(json!({
        "version":1,"created_at":0,"id":uuid::Uuid::new_v4(),
        "start":{"request_id":uuid::Uuid::new_v4(),"session_id":uuid::Uuid::new_v4(),"profile":"test","project":null,"selection":{"provider":"test","model":"test","thinking":"default"},"workflow_id":"rynna-default","goal":"work","criteria":[],"limits":{"steps":50,"tool_calls":64,"active_seconds":300},"initial_context":""},
        "workflow":workflow,"helpers":[{"name":"helper","description":"help","instructions":"help"}],"fingerprint":"test","cursor":0,"status":"running","reason":null,"revision":1,"consumed":{"steps":0,"tool_calls":0,"active_seconds":0},"in_flight":false,"uncertain":false,"events":[],"verification":null,"steering":[]
    })).unwrap();
    assert!(
        profiles
            .clone_agent("test")
            .unwrap()
            .execute_workflow_step(&run, 64)
            .await
            .is_err()
    );
    assert!(
        provider.requests.lock().unwrap().is_empty(),
        "disabled helpers must not reach the provider"
    );
}
#[tokio::test]
async fn disabled_file_operations_are_not_advertised_or_invokable() {
    let provider = Arc::new(Provider::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let agent =
        Agent::with_tools(provider.clone(), "", vec![Arc::new(Reader(calls.clone()))]).unwrap();
    let profiles =
        AgentProfiles::new("test", [(profile(json!(["file_operations"])), agent)]).unwrap();
    let result = profiles.respond(None, &[], "read").await;
    assert!(result.is_err());
    assert!(provider.requests.lock().unwrap()[0].tools.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn replacing_a_profile_applies_its_saved_toolsets() {
    let provider = Arc::new(Provider::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let agent =
        Agent::with_tools(provider.clone(), "", vec![Arc::new(Reader(calls.clone()))]).unwrap();
    let mut profiles = AgentProfiles::new("test", [(profile(json!([])), agent.clone())]).unwrap();
    profiles
        .upsert(profile(json!(["file_operations"])), agent)
        .unwrap();
    assert!(profiles.respond(None, &[], "read").await.is_err());
    assert!(provider.requests.lock().unwrap()[0].tools.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
