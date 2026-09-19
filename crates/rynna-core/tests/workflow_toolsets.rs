use async_trait::async_trait;
use rynna_core::{
    Agent, AgentProfiles, Completion, CompletionRequest, Message, ModelProvider, Profile,
    ProviderError, ThresholdContextManager, Tool, ToolDefinition, ToolError,
};
use serde_json::{Value, json};
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

struct LargeReader;

#[async_trait]
impl Tool for LargeReader {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new("read_file", "schema documentation ".repeat(4000), json!({}))
    }

    async fn execute(&self, _: Value) -> Result<Value, ToolError> {
        panic!("this test must not execute tools")
    }
}

#[tokio::test]
async fn workflow_preflight_counts_only_enabled_toolsets() {
    for disabled in [false, true] {
        let provider = Arc::new(Recorder::default());
        let agent = Agent::with_tools(provider.clone(), "", vec![Arc::new(LargeReader)])
            .unwrap()
            .with_context_manager(Arc::new(ThresholdContextManager::new(4096, 3584).unwrap()));
        let profile: Profile = serde_json::from_value(json!({
            "name": "test", "providers": [],
            "disabled_toolsets": if disabled { vec!["file_operations"] } else { vec![] }
        }))
        .unwrap();
        let profiles = AgentProfiles::new("test", [(profile, agent)]).unwrap();
        let run = serde_json::from_value(json!({
            "version":1,"created_at":0,"id":uuid::Uuid::new_v4(),
            "start":{"request_id":uuid::Uuid::new_v4(),"session_id":uuid::Uuid::new_v4(),"profile":"test","project":null,"selection":{"provider":"test","model":"test","thinking":"default"},"workflow_id":"rynna-default","goal":"work","criteria":[],"limits":{"steps":50,"tool_calls":64,"active_seconds":300},"initial_context":""},
            "workflow":rynna_core::workflows::default_workflow(),"helpers":[],"fingerprint":"test","cursor":0,"status":"running","reason":null,"revision":1,"consumed":{"steps":0,"tool_calls":0,"active_seconds":0},"in_flight":false,"uncertain":false,"events":[],"verification":null,"steering":[]
        })).unwrap();
        let result = profiles
            .clone_agent("test")
            .unwrap()
            .execute_workflow_step(&run, 64)
            .await;
        let requests = provider.0.lock().unwrap();
        if disabled {
            assert_eq!(result.unwrap().content, "Done");
            assert_eq!(requests.len(), 1);
            assert!(requests[0].tools.is_empty());
        } else {
            assert_eq!(
                result
                    .err()
                    .expect("enabled oversized schema must be rejected")
                    .message,
                "workflow context exceeds the model context allowance"
            );
            assert!(requests.is_empty());
        }
    }
}
