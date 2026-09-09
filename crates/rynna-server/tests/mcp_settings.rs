use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use rynna_config::ProviderSettingsStore;
use rynna_core::{
    Agent, AgentProfiles, Completion, CompletionRequest, Message, ModelProvider, Profile,
    ProviderError,
};
use rynna_server::router_with_profiles_and_provider_settings;
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
        Ok(Completion::new(Message::assistant("answer")))
    }
}
fn app(path: &std::path::Path, model: Arc<Model>) -> Router {
    let profile = Profile {
        name: "test".into(),
        providers: vec![],
        active_skills: vec![],
        mcp_servers: vec![],
        capabilities: vec![],
        default_project_directory: ".".into(),
        projects: vec![],
        subagents: Vec::new(),
    };
    let profiles = AgentProfiles::new(
        "test",
        [
            (profile.clone(), Agent::new(model.clone(), "policy")),
            (
                Profile {
                    name: "other".into(),
                    ..profile
                },
                Agent::new(model, "policy"),
            ),
        ],
    )
    .unwrap();
    router_with_profiles_and_provider_settings(profiles, ProviderSettingsStore::load(path).unwrap())
}
async fn request(
    app: &Router,
    method: &str,
    path: &str,
    value: Value,
    local: bool,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap();
    if local {
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
    }
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap()
        },
    )
}

#[tokio::test]
async fn settings_are_local_profile_specific_validated_and_persistent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("providers.yaml");
    let app = app(&path, Arc::new(Model::default()));
    let config = json!({"mcpServers":{"tools":{"transport":"stdio","command":"does-not-run-on-save","enabled":false}}});
    for method in ["GET", "PUT"] {
        assert_eq!(
            request(&app, method, "/v1/profiles/test/mcp", config.clone(), false)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        request(&app, "GET", "/v1/profiles/test/mcp", Value::Null, true)
            .await
            .1,
        json!({"mcpServers":{}})
    );
    assert_eq!(
        request(&app, "PUT", "/v1/profiles/test/mcp", config.clone(), true)
            .await
            .0,
        StatusCode::OK
    );
    let saved = request(&app, "GET", "/v1/profiles/test/mcp", Value::Null, true)
        .await
        .1;
    assert_eq!(
        saved["mcpServers"]["tools"]["command"],
        "does-not-run-on-save"
    );
    assert_eq!(
        request(&app, "GET", "/v1/profiles/other/mcp", Value::Null, true)
            .await
            .1,
        json!({"mcpServers":{}})
    );
    assert_eq!(
        request(
            &app,
            "PUT",
            "/v1/profiles/test/mcp",
            json!({"mcpServers":{"tools":{"transport":"stdio","command":""}}}),
            true
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&app, "GET", "/v1/profiles/test/mcp", Value::Null, true)
            .await
            .1,
        saved
    );
    assert_ne!(
        request(&app, "PUT", "/v1/profiles/unknown/mcp", config, true)
            .await
            .0,
        StatusCode::OK
    );
    let store = rynna_config::mcp::McpSettingsStore::new(dir.path().join("mcp.yaml"));
    assert_eq!(store.load("test").unwrap().servers.len(), 1);
    assert!(store.load("unknown").unwrap().servers.is_empty());
    assert_eq!(
        request(
            &app,
            "PUT",
            "/v1/profiles/test/mcp",
            json!({"mcpServers":{}}),
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(store.load("test").unwrap().servers.is_empty());
}

#[tokio::test]
async fn catalog_profile_rename_and_delete_move_and_remove_mcp_settings() {
    use rynna_config::{ProfileCatalog, mcp::McpSettingsStore};
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.yaml");
    std::fs::write(
        &config_path,
        r#"
version: 1
default_profile: test
providers:
  ollama:
    kind: openai-compatible
    api_base: http://127.0.0.1:11434/v1
profiles:
  test:
    provider: ollama
    model: model
"#,
    )
    .unwrap();
    let catalog = ProfileCatalog::load(&config_path).unwrap();
    let profile = catalog.resolve("test").unwrap().profile;
    let profiles = AgentProfiles::new(
        "test",
        [(profile, Agent::new(Arc::new(Model::default()), "policy"))],
    )
    .unwrap();
    let router = rynna_server::router_with_profiles_provider_settings_and_catalog(
        profiles,
        ProviderSettingsStore::load(dir.path().join("providers.yaml")).unwrap(),
        catalog,
    );
    let draft = json!({"name":"new", "providers":[{"provider":"ollama", "model":"model"}], "active_skills":[], "mcp_servers":[], "capabilities":[]});
    assert_eq!(
        request(&router, "POST", "/v1/profiles", draft.clone(), true)
            .await
            .0,
        StatusCode::OK
    );
    let settings = json!({"mcpServers":{"tools":{"transport":"stdio","command":"new-command","enabled":false}}});
    assert_eq!(
        request(&router, "PUT", "/v1/profiles/new/mcp", settings, true)
            .await
            .0,
        StatusCode::OK
    );
    let mut renamed = draft.clone();
    renamed["name"] = json!("renamed");
    assert_eq!(
        request(&router, "PUT", "/v1/profiles/new", renamed, true)
            .await
            .0,
        StatusCode::OK
    );
    let store = McpSettingsStore::new(dir.path().join("mcp.yaml"));
    assert!(store.load("new").unwrap().servers.is_empty());
    assert_eq!(store.load("renamed").unwrap().servers.len(), 1);
    assert_eq!(
        request(
            &router,
            "GET",
            "/v1/profiles/renamed/mcp",
            Value::Null,
            true
        )
        .await
        .1["mcpServers"]["tools"]["command"],
        "new-command"
    );
    assert_eq!(
        request(&router, "DELETE", "/v1/profiles/renamed", Value::Null, true)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert!(store.load("renamed").unwrap().servers.is_empty());
    assert!(store.load("test").unwrap().servers.is_empty());
}

#[tokio::test]
async fn saved_changes_apply_to_the_selected_profiles_next_request() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(
        &dir.path().join("providers.yaml"),
        Arc::new(Model::default()),
    );
    let enabled = json!({"mcpServers":{"broken":{"transport":"stdio","command":"rynna-test-nonexistent-mcp-program"}}});
    assert_eq!(
        request(&app, "PUT", "/v1/profiles/test/mcp", enabled, true)
            .await
            .0,
        StatusCode::OK
    );
    let prompt = |profile| json!({"profile":profile,"prompt":"hello","history":[]});
    let failed = request(&app, "POST", "/v1/respond", prompt("test"), true).await;
    assert_eq!(failed.0, StatusCode::BAD_GATEWAY);
    assert!(failed.1.to_string().contains("broken"));
    assert!(
        !failed
            .1
            .to_string()
            .contains("rynna-test-nonexistent-mcp-program")
    );
    assert_eq!(
        request(&app, "POST", "/v1/respond", prompt("other"), true)
            .await
            .0,
        StatusCode::OK
    );
    request(
        &app,
        "PUT",
        "/v1/profiles/test/mcp",
        json!({"mcpServers":{}}),
        true,
    )
    .await;
    assert_eq!(
        request(&app, "POST", "/v1/respond", prompt("test"), true)
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn failed_mcp_rename_preserves_catalog_credentials_memory_and_runtime_and_can_retry() {
    use rynna_config::{
        ConfiguredProvider, ProfileCatalog,
        mcp::McpSettingsStore,
        memory::{MemorySettings, MemorySettingsStore},
    };
    for failure in ["malformed", "directory", "destination"] {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(&config_path, "version: 1\ndefault_profile: test\nproviders:\n  ollama:\n    kind: openai-compatible\n    api_base: http://127.0.0.1:11434/v1\nprofiles:\n  test:\n    provider: ollama\n    model: model\n").unwrap();
        let catalog = ProfileCatalog::load(&config_path).unwrap();
        let profile = catalog.resolve("test").unwrap().profile;
        let runtime = AgentProfiles::new(
            "test",
            [(
                profile.clone(),
                Agent::new(Arc::new(Model::default()), "policy"),
            )],
        )
        .unwrap();
        let mut providers = ProviderSettingsStore::load(dir.path().join("providers.yaml")).unwrap();
        providers
            .add(
                "test",
                ConfiguredProvider::Ollama {
                    api_base: "http://127.0.0.1:11434/v1".into(),
                },
            )
            .unwrap();
        let memory = MemorySettingsStore::new(providers.memory_settings_path());
        memory.save("test", MemorySettings::None).unwrap();
        let mcp_path = providers.mcp_settings_path();
        let mcp = McpSettingsStore::new(&mcp_path);
        mcp.save("test", serde_json::from_value(json!({"mcpServers":{"tools":{"transport":"stdio","command":"original","enabled":false}}})).unwrap()).unwrap();
        let original_mcp = std::fs::read(&mcp_path).unwrap();
        let router = rynna_server::router_with_profiles_provider_settings_and_catalog(
            runtime, providers, catalog,
        );
        match failure {
            "malformed" => std::fs::write(&mcp_path, "malformed: [").unwrap(),
            "directory" => {
                std::fs::remove_file(&mcp_path).unwrap();
                std::fs::create_dir(&mcp_path).unwrap();
            }
            "destination" => {
                mcp.save("renamed", Default::default()).unwrap();
            }
            _ => unreachable!(),
        }
        let paths = [
            config_path.clone(),
            dir.path().join("providers.yaml"),
            dir.path().join("memory.yaml"),
        ];
        let before = paths
            .iter()
            .map(|path| std::fs::read(path).unwrap())
            .collect::<Vec<_>>();
        let broken_mcp = std::fs::read(&mcp_path).ok();
        let renamed = serde_json::to_value(Profile {
            name: "renamed".into(),
            ..profile
        })
        .unwrap();
        let response = request(&router, "PUT", "/v1/profiles/test", renamed.clone(), true).await;
        assert!(!response.0.is_success(), "{failure}");
        for (path, bytes) in paths.iter().zip(&before) {
            assert_eq!(&std::fs::read(path).unwrap(), bytes, "{failure}");
        }
        assert_eq!(std::fs::read(&mcp_path).ok(), broken_mcp);
        let listing = request(&router, "GET", "/v1/profiles", Value::Null, true)
            .await
            .1;
        assert_eq!(listing["default_profile"], "test");
        assert_eq!(listing["configured_profiles"][0]["name"], "test");
        assert_eq!(
            request(
                &router,
                "POST",
                "/v1/respond",
                json!({"profile":"test","prompt":"hello","history":[]}),
                true
            )
            .await
            .0,
            StatusCode::OK
        );
        if failure == "directory" {
            std::fs::remove_dir(&mcp_path).unwrap();
        }
        std::fs::write(&mcp_path, &original_mcp).unwrap();
        assert_eq!(
            request(&router, "PUT", "/v1/profiles/test", renamed, true)
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            ProfileCatalog::load(&config_path)
                .unwrap()
                .default_profile(),
            "renamed"
        );
        assert_eq!(mcp.load("renamed").unwrap().servers.len(), 1);
        assert!(mcp.load("test").unwrap().servers.is_empty());
        assert_eq!(
            ProviderSettingsStore::load(dir.path().join("providers.yaml"))
                .unwrap()
                .list("renamed")
                .len(),
            1
        );
        let memory_file = std::fs::read_to_string(dir.path().join("memory.yaml")).unwrap();
        assert!(memory_file.contains("  renamed:"));
        assert!(!memory_file.contains("  test:"));
    }
}
