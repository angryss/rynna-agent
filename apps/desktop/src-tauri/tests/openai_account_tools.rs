use async_trait::async_trait;
use rynna_core::{
    AgentProfiles, Completion, CompletionRequest, Message, ModelProvider, ProviderError, Role,
    ToolCall,
};
use serde_json::{Value, json};
use std::sync::Arc;

struct NativeProbe {
    tool: &'static str,
    arguments: Value,
}

#[async_trait]
impl ModelProvider for NativeProbe {
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        if let Some(result) = request.messages.last().filter(|m| m.role == Role::Tool) {
            return Ok(Completion::new(Message::assistant(&result.content)));
        }
        let names: Vec<_> = request.tools.iter().map(|t| t.name.as_str()).collect();
        for name in ["read_file", "list_directory", "search_files", "code_search"] {
            assert!(
                names.contains(&name),
                "account profile missing {name}: {names:?}"
            );
        }
        for name in [
            "host_info",
            "run_command",
            "write_file",
            "edit_file",
            "create_directory",
        ] {
            assert!(
                !names.contains(&name),
                "account profile unexpectedly grants {name}"
            );
        }
        Ok(Completion::with_tool_calls(vec![ToolCall {
            id: "native-probe".into(),
            name: self.tool.into(),
            arguments: self.arguments.clone(),
        }]))
    }
}

#[tokio::test]
async fn runtime_only_openai_account_executes_shared_native_defaults() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("sample.txt"), "account native read").unwrap();
    std::fs::write(directory.path().join(".env"), "synthetic protected data").unwrap();
    for (tool, arguments) in [
        ("read_file", json!({"path":"sample.txt"})),
        ("read_file", json!({"path":".env"})),
    ] {
        let (mut profile, agent) =
            rynna_desktop::compose_openai_account_agent(Arc::new(NativeProbe {
                tool,
                arguments: arguments.clone(),
            }))
            .unwrap();
        assert_eq!(profile.name, "openai-account");
        assert!(!profile.yolo);
        assert!(profile.capabilities.is_empty());
        assert_eq!(profile.providers[0].provider, "openai");
        assert_eq!(profile.providers[0].model, "Codex default");
        profile.default_project_directory = directory.path().into();
        let runtime = AgentProfiles::new("openai-account", [(profile, agent)]).unwrap();
        let response = rynna_desktop::respond_with_profiles(
            &runtime,
            serde_json::from_value(json!({"profile":"openai-account","prompt":"probe"})).unwrap(),
        )
        .await
        .unwrap();
        let result: Value = serde_json::from_str(&response.message.content).unwrap();
        match (tool, arguments["path"].as_str()) {
            (_, Some("sample.txt")) => assert_eq!(result["content"], "account native read"),
            (_, Some(".env")) => {
                assert!(
                    result.get("error").is_some(),
                    "protected read succeeded: {result}"
                );
                assert!(
                    !response
                        .message
                        .content
                        .contains("synthetic protected data")
                );
            }
            _ => unreachable!(),
        }
    }
}
