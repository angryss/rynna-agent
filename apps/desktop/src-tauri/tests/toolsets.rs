use async_trait::async_trait;
use rynna_config::ProfileCatalog;
use rynna_core::{
    Agent, AgentProfiles, Completion, CompletionRequest, Message, ModelProvider, ProviderError,
    Subagent, toolsets::ToolsetId,
};
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Model(Mutex<Vec<CompletionRequest>>);
#[async_trait]
impl ModelProvider for Model {
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        self.0.lock().unwrap().push(request);
        Ok(Completion::new(Message::assistant("ok")))
    }
}
#[tokio::test]
async fn desktop_profile_save_updates_toolset_runtime() {
    let mut catalog = ProfileCatalog::from_yaml("version: 1\ndefault_profile: test\nproviders:\n  local:\n    kind: openai-compatible\n    api_base: http://localhost:11434\nprofiles:\n  test:\n    providers:\n      - provider: local\n        model: test\n").unwrap();
    let mut profile = catalog.resolve("test").unwrap().profile;
    profile.subagents = vec![Subagent {
        name: "helper".into(),
        description: "help".into(),
        instructions: "help".into(),
    }];
    let model = Arc::new(Model::default());
    let mut runtime =
        AgentProfiles::new("test", [(profile.clone(), Agent::new(model.clone(), ""))]).unwrap();
    profile.disabled_toolsets = vec![ToolsetId::Subagents];
    rynna_desktop::update_saved_profile(&mut catalog, &mut runtime, None, "test", profile).unwrap();
    runtime.respond(None, &[], "hi").await.unwrap();
    assert!(model.0.lock().unwrap()[0].tools.is_empty());
    assert_eq!(
        runtime.profiles()[0].disabled_toolsets,
        vec![ToolsetId::Subagents]
    );
}
