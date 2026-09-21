use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use rynna_config::{ProfileCatalog, ProviderSettingsStore};
use rynna_core::{
    Agent, AgentProfiles, Completion, CompletionRequest, Message, ModelProvider, ProviderError,
    Tool, ToolDefinition, ToolError,
};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;
#[derive(Default)]
struct Model(Mutex<Vec<CompletionRequest>>);
#[async_trait]
impl ModelProvider for Model {
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        self.0.lock().unwrap().push(request);
        Ok(Completion::new(Message::assistant("ok")))
    }
}
struct Reader;
#[async_trait]
impl Tool for Reader {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new("read_file", "read", json!({}))
    }
    async fn execute(&self, _: Value) -> Result<Value, ToolError> {
        panic!("not called")
    }
}
async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    local: bool,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    if local {
        req.extensions_mut()
            .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
    }
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    (
        status,
        serde_json::from_slice(&to_bytes(res.into_body(), 1024 * 1024).await.unwrap()).unwrap(),
    )
}
#[tokio::test]
async fn saved_toolsets_apply_immediately_and_are_local_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rynna.yaml");
    std::fs::write(&path, "version: 1\ndefault_profile: test\nproviders:\n  local:\n    kind: openai-compatible\n    api_base: http://localhost:11434\nprofiles:\n  test:\n    providers:\n      - provider: local\n        model: test\n").unwrap();
    let catalog = ProfileCatalog::load(&path).unwrap();
    let profile = catalog.resolve("test").unwrap().profile;
    let model = Arc::new(Model::default());
    let runtime = AgentProfiles::new(
        "test",
        [(
            profile.clone(),
            Agent::with_tools(model.clone(), "", vec![Arc::new(Reader)]).unwrap(),
        )],
    )
    .unwrap();
    let app = rynna_server::router_with_profiles_provider_settings_and_catalog(
        runtime,
        ProviderSettingsStore::load(dir.path().join("providers.yaml")).unwrap(),
        catalog,
    );
    let mut body = serde_json::to_value(profile).unwrap();
    body["disabled_toolsets"] = json!(["file_operations"]);
    assert_eq!(
        request(&app, "PUT", "/v1/profiles/test", body.clone(), false)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, "PUT", "/v1/profiles/test", body.clone(), true)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/respond",
            json!({"prompt":"hi", "history":[]}),
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(model.0.lock().unwrap().last().unwrap().tools.is_empty());
    assert_eq!(
        serde_json::to_value(
            ProfileCatalog::load(&path)
                .unwrap()
                .resolve("test")
                .unwrap()
                .profile
        )
        .unwrap()["disabled_toolsets"],
        body["disabled_toolsets"]
    );
    body["disabled_toolsets"] = json!([]);
    assert_eq!(
        request(&app, "PUT", "/v1/profiles/test", body, true)
            .await
            .0,
        StatusCode::OK
    );
    request(
        &app,
        "POST",
        "/v1/respond",
        json!({"prompt":"hi", "history":[]}),
        true,
    )
    .await;
    assert_eq!(
        model.0.lock().unwrap().last().unwrap().tools[0].name,
        "read_file"
    );
}

#[tokio::test]
async fn saved_yolo_applies_native_tools_immediately_and_can_be_disabled() {
    for forced in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rynna.yaml");
        std::fs::write(&path, "version: 1\ndefault_profile: test\nproviders:\n  local:\n    kind: openai-compatible\n    api_base: http://localhost:11434\nprofiles:\n  test:\n    providers:\n      - provider: local\n        model: test\n").unwrap();
        let catalog = ProfileCatalog::load(&path).unwrap();
        let profile = catalog.resolve("test").unwrap().profile;
        let model = Arc::new(Model::default());
        let runtime = AgentProfiles::new(
            "test",
            [(
                profile.clone(),
                Agent::with_tools(model.clone(), "", vec![Arc::new(Reader)])
                    .unwrap()
                    .with_yolo_override(forced),
            )],
        )
        .unwrap();
        let app = rynna_server::router_with_profiles_provider_settings_and_catalog(
            runtime,
            ProviderSettingsStore::load(dir.path().join("providers.yaml")).unwrap(),
            catalog,
        );
        let mut body = serde_json::to_value(profile).unwrap();
        body["disabled_toolsets"] = json!(["file_operations", "commands", "code_search"]);
        for yolo in [true, false] {
            body["yolo"] = json!(yolo);
            assert_eq!(
                request(&app, "PUT", "/v1/profiles/test", body.clone(), true)
                    .await
                    .0,
                StatusCode::OK
            );
            assert_eq!(
                ProfileCatalog::load(&path)
                    .unwrap()
                    .resolve("test")
                    .unwrap()
                    .yolo,
                yolo
            );
            assert_eq!(
                request(
                    &app,
                    "POST",
                    "/v1/respond",
                    json!({"prompt":"hi","history":[]}),
                    true
                )
                .await
                .0,
                StatusCode::OK
            );
            let guard = model.0.lock().unwrap();
            let names = guard
                .last()
                .unwrap()
                .tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>();
            assert_eq!(names.contains(&"run_command"), yolo || forced);
            assert_eq!(names.contains(&"write_file"), yolo || forced);
            assert_eq!(names.contains(&"read_file"), yolo || forced);
        }
    }
}
