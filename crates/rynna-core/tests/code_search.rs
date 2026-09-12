use async_trait::async_trait;
use rynna_core::{
    Agent, AgentProfiles, Completion, CompletionRequest, Message, ModelProvider, Profile,
    ProviderError, Tool, ToolCall, ToolDefinition, ToolError, ToolSource,
};
use serde_json::{Value, json};
use std::sync::Arc;

struct Search(&'static str);
#[async_trait]
impl Tool for Search {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new("code_search", self.0, json!({"type":"object"}))
    }
    async fn execute(&self, _: Value) -> Result<Value, ToolError> {
        Ok(json!({"provider":self.0}))
    }
}
struct Source {
    replace: bool,
    present: bool,
}
#[async_trait]
impl ToolSource for Source {
    fn replaces_code_search(&self) -> bool {
        self.replace
    }
    async fn discover(&self) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        Ok(if self.present {
            vec![Arc::new(Search("plugin"))]
        } else {
            vec![]
        })
    }
}
struct Model;
#[async_trait]
impl ModelProvider for Model {
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        assert_eq!(request.tools.len(), 1);
        if let Some(result) = request.messages.iter().find(|m| m.tool_call_id.is_some()) {
            Ok(Completion::new(Message::assistant(result.content.clone())))
        } else {
            Ok(Completion::with_tool_calls(vec![ToolCall::new(
                "search",
                "code_search",
                json!({}),
            )]))
        }
    }
}
fn profiles() -> AgentProfiles {
    let profile: Profile = serde_json::from_value(json!({"name":"test","providers":[]})).unwrap();
    AgentProfiles::new(
        "test",
        [(
            profile,
            Agent::with_tools(Arc::new(Model), "policy", vec![Arc::new(Search("builtin"))])
                .unwrap(),
        )],
    )
    .unwrap()
}
#[tokio::test]
async fn builtin_is_default_and_only_explicit_plugin_selection_can_replace_it() {
    let mut profiles = profiles();
    let result = profiles.respond(None, &[], "search").await.unwrap();
    assert!(result.content.contains("builtin"));
    profiles
        .set_tool_source(
            "test",
            Some(Arc::new(Source {
                replace: true,
                present: true,
            })),
        )
        .unwrap();
    let result = profiles.respond(None, &[], "search").await.unwrap();
    assert!(result.content.contains("plugin"));
    profiles
        .set_tool_source(
            "test",
            Some(Arc::new(Source {
                replace: false,
                present: true,
            })),
        )
        .unwrap();
    assert!(matches!(
        profiles.respond(None, &[], "search").await.unwrap_err(),
        rynna_core::ProfileAgentError::Agent(rynna_core::AgentError::DuplicateTool(name)) if name == "code_search"
    ));
    profiles
        .set_tool_source(
            "test",
            Some(Arc::new(Source {
                replace: true,
                present: false,
            })),
        )
        .unwrap();
    assert!(profiles.respond(None, &[], "search").await.is_err());
    profiles.set_tool_source("test", None).unwrap();
    assert!(
        profiles
            .respond(None, &[], "search")
            .await
            .unwrap()
            .content
            .contains("builtin")
    );
}
