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

struct ProjectProbe {
    name: &'static str,
    arguments: serde_json::Value,
}
#[async_trait]
impl ModelProvider for ProjectProbe {
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        if let Some(result) = request
            .messages
            .last()
            .filter(|m| m.role == rynna_core::Role::Tool)
        {
            return Ok(Completion::new(Message::assistant(&result.content)));
        }
        Ok(Completion::with_tool_calls(vec![rynna_core::ToolCall {
            id: "project-probe".into(),
            name: self.name.into(),
            arguments: self.arguments.clone(),
        }]))
    }
}

async fn probe_project(runtime: &AgentProfiles) -> serde_json::Value {
    let response = rynna_desktop::respond_with_profiles(
        runtime,
        serde_json::from_value(serde_json::json!({"prompt":"probe project"})).unwrap(),
    )
    .await
    .unwrap();
    serde_json::from_str(&response.message.content).unwrap()
}

#[tokio::test]
async fn desktop_live_default_project_update_to_cwd_rebinds_normal_files() {
    let old = tempfile::tempdir().unwrap();
    std::fs::write(old.path().join("Cargo.toml"), "old project").unwrap();
    let mut catalog = ProfileCatalog::built_in();
    let mut resolved = catalog.resolve("default").unwrap();
    resolved.profile.default_project_directory = old.path().into();
    catalog
        .update_profile("default", resolved.profile.clone())
        .unwrap();
    let agent = rynna_desktop::compose_agent(
        &resolved,
        Arc::new(ProjectProbe {
            name: "read_file",
            arguments: serde_json::json!({"path":"Cargo.toml"}),
        }),
    )
    .unwrap();
    let mut runtime = AgentProfiles::new("default", [(resolved.profile.clone(), agent)]).unwrap();
    assert_eq!(probe_project(&runtime).await["content"], "old project");
    resolved.profile.default_project_directory = ".".into();
    rynna_desktop::update_saved_profile(
        &mut catalog,
        &mut runtime,
        None,
        "default",
        resolved.profile,
    )
    .unwrap();
    assert_eq!(
        runtime.profiles()[0].default_project_directory,
        std::path::Path::new(".")
    );
    assert_eq!(
        probe_project(&runtime).await["content"],
        std::fs::read_to_string("Cargo.toml").unwrap()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn desktop_live_default_project_update_to_cwd_rebinds_yolo_command() {
    let old = tempfile::tempdir().unwrap();
    let mut catalog = ProfileCatalog::built_in();
    let mut resolved = catalog.resolve("default").unwrap();
    resolved.yolo = true;
    resolved.profile.yolo = true;
    resolved.profile.default_project_directory = old.path().into();
    catalog
        .update_profile("default", resolved.profile.clone())
        .unwrap();
    let agent = rynna_desktop::compose_agent(
        &resolved,
        Arc::new(ProjectProbe {
            name: "run_command",
            arguments: serde_json::json!({"program":"/bin/pwd","arguments":[]}),
        }),
    )
    .unwrap();
    let mut runtime = AgentProfiles::new("default", [(resolved.profile.clone(), agent)]).unwrap();
    assert_eq!(
        probe_project(&runtime).await["stdout"]
            .as_str()
            .unwrap()
            .trim(),
        old.path().canonicalize().unwrap().to_str().unwrap()
    );
    resolved.profile.default_project_directory = ".".into();
    rynna_desktop::update_saved_profile(
        &mut catalog,
        &mut runtime,
        None,
        "default",
        resolved.profile,
    )
    .unwrap();
    assert!(runtime.profiles()[0].yolo);
    assert_eq!(
        probe_project(&runtime).await["stdout"]
            .as_str()
            .unwrap()
            .trim(),
        std::env::current_dir()
            .unwrap()
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
    );
}

#[tokio::test]
async fn desktop_mode_edits_replace_native_tools_without_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut catalog = ProfileCatalog::built_in();
    let mut resolved = catalog.resolve("default").unwrap();
    resolved.profile.default_project_directory = dir.path().to_owned();
    let model = Arc::new(Model::default());
    let agent = rynna_desktop::compose_agent(&resolved, model.clone()).unwrap();
    let mut runtime = AgentProfiles::new("default", [(resolved.profile.clone(), agent)]).unwrap();
    runtime.respond(None, &[], "host").await.unwrap();
    {
        let requests = model.0.lock().unwrap();
        let names = requests
            .last()
            .unwrap()
            .tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"read_file"));
        assert!(!names.contains(&"run_command") && !names.contains(&"write_file"));
    }
    for yolo in [true, false] {
        let mut profile = resolved.profile.clone();
        profile.yolo = yolo;
        profile.disabled_toolsets = vec![
            ToolsetId::FileOperations,
            ToolsetId::Commands,
            ToolsetId::CodeSearch,
        ];
        rynna_desktop::update_saved_profile(&mut catalog, &mut runtime, None, "default", profile)
            .unwrap();
        runtime.respond(None, &[], "host").await.unwrap();
        let requests = model.0.lock().unwrap();
        let names = requests
            .last()
            .unwrap()
            .tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names.contains(&"read_file"), yolo);
        assert_eq!(names.contains(&"run_command"), yolo);
        assert_eq!(names.contains(&"write_file"), yolo);
        assert_eq!(runtime.profiles()[0].yolo, yolo);
    }
}
