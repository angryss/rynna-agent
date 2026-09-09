use async_trait::async_trait;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{ConnectInfo, State},
    http::{Request, StatusCode, Uri},
    routing::post,
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
async fn settings_are_local_only_default_to_none_and_never_return_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("providers.yaml");
    let app = app(&path, Arc::new(Model::default()));
    for method in ["GET", "PUT"] {
        assert_eq!(
            request(
                &app,
                method,
                "/v1/profiles/test/memory",
                json!({"kind":"none"}),
                false
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        request(&app, "GET", "/v1/profiles/test/memory", Value::Null, true)
            .await
            .1,
        json!({"kind":"none"})
    );
    let input = json!({"kind":"hindsight", "deployment":"cloud", "api_base":"https://api.hindsight.vectorize.io", "bank_id":"rynna", "api_key":"test-secret"});
    let (status, saved) = request(&app, "PUT", "/v1/profiles/test/memory", input, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["api_key_configured"], true);
    assert!(!saved.to_string().contains("test-secret"));
    assert!(saved.get("api_key").is_none());
    assert_eq!(
        request(&app, "GET", "/v1/profiles/test/memory", Value::Null, true)
            .await
            .1,
        saved
    );
    let mut malformed = saved.clone();
    malformed["deployment"] = json!("test-secret");
    let (status, body) = request(&app, "PUT", "/v1/profiles/test/memory", malformed, true).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!body.to_string().contains("test-secret"));
    assert_eq!(
        request(&app, "GET", "/v1/profiles/test/memory", Value::Null, true)
            .await
            .1,
        saved
    );
    assert_eq!(
        request(
            &app,
            "PUT",
            "/v1/profiles/test/memory",
            json!({"kind":"none"}),
            true
        )
        .await
        .1,
        json!({"kind":"none"})
    );
}

type Calls = Arc<Mutex<Vec<String>>>;
async fn memory(State(calls): State<Calls>, uri: Uri, Json(_): Json<Value>) -> Json<Value> {
    calls.lock().unwrap().push(uri.path().into());
    Json(json!({"results":[{"text":"Remembered fact"}],"success":true}))
}
async fn wait_calls(calls: &Calls, count: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while calls.lock().unwrap().len() < count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("queued writes should finish");
}
#[tokio::test]
async fn saving_and_disabling_change_live_sync_and_stream_requests_and_survive_restart() {
    let calls = Calls::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let memory_app = Router::new()
        .route("/{*path}", post(memory))
        .with_state(calls.clone());
    let server = tokio::spawn(async move { axum::serve(listener, memory_app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("providers.yaml");
    let model = Arc::new(Model::default());
    let router = app(&path, model.clone());
    let settings =
        json!({"kind":"hindsight","deployment":"self_hosted","api_base":base,"bank_id":"test"});
    assert_eq!(
        request(&router, "PUT", "/v1/profiles/test/memory", settings, true)
            .await
            .0,
        StatusCode::OK
    );
    let prompt = json!({"prompt":"hello","history":[]});
    assert_eq!(
        request(&router, "POST", "/v1/respond", prompt.clone(), true)
            .await
            .0,
        StatusCode::OK
    );
    {
        let requests = model.0.lock().unwrap();
        assert_eq!(requests[0].messages[0].content, "policy");
        assert_eq!(requests[0].messages[1].role, rynna_core::Role::User);
        assert!(requests[0].messages[1].content.contains("Remembered fact"));
    }
    wait_calls(&calls, 2).await;
    assert_eq!(calls.lock().unwrap().len(), 2);
    let restarted = app(&path, model.clone());
    let stream = Request::post("/v1/respond/stream")
        .header("content-type", "application/json")
        .body(Body::from(prompt.to_string()))
        .unwrap();
    let response = restarted.oneshot(stream).await.unwrap();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("answer"));
    wait_calls(&calls, 4).await;
    assert_eq!(calls.lock().unwrap().len(), 4);
    assert_eq!(
        request(
            &router,
            "PUT",
            "/v1/profiles/test/memory",
            json!({"kind":"none"}),
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    request(&router, "POST", "/v1/respond", prompt, true).await;
    wait_calls(&calls, 4).await;
    assert_eq!(calls.lock().unwrap().len(), 4);
    assert_eq!(
        model.0.lock().unwrap().last().unwrap().messages[0].content,
        "policy"
    );
    server.abort();
}

#[tokio::test]
async fn failed_persistence_is_reported_and_does_not_enable_memory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("providers.yaml");
    let router = app(&path, Arc::new(Model::default()));
    std::fs::create_dir(dir.path().join("memory.yaml")).unwrap();
    assert_eq!(
        request(
            &router,
            "PUT",
            "/v1/profiles/test/memory",
            json!({"kind":"none"}),
            true
        )
        .await
        .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn unknown_profiles_and_global_endpoint_cannot_read_or_write_memory() {
    let dir = tempfile::tempdir().unwrap();
    let router = app(
        &dir.path().join("providers.yaml"),
        Arc::new(Model::default()),
    );
    for path in ["/v1/profiles/missing/memory", "/v1/settings/memory"] {
        for method in ["GET", "PUT"] {
            assert!(
                !request(&router, method, path, json!({"kind":"none"}), true)
                    .await
                    .0
                    .is_success()
            );
        }
    }
    assert!(!dir.path().join("memory.yaml").exists());
}

#[tokio::test]
async fn two_profiles_route_to_their_own_banks_and_disabling_one_keeps_the_other_enabled() {
    let calls = Calls::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let memory_app = Router::new()
        .route("/{*path}", post(memory))
        .with_state(calls.clone());
    let server = tokio::spawn(async move { axum::serve(listener, memory_app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("providers.yaml");
    let model = Arc::new(Model::default());
    let router = app(&path, model.clone());
    for name in ["test", "other"] {
        assert_eq!(
            request(
                &router,
                "GET",
                &format!("/v1/profiles/{name}/memory"),
                Value::Null,
                true
            )
            .await
            .1,
            json!({"kind":"none"})
        );
        let settings = json!({"kind":"hindsight", "deployment":"self_hosted", "api_base":base, "bank_id": name});
        assert_eq!(
            request(
                &router,
                "PUT",
                &format!("/v1/profiles/{name}/memory"),
                settings,
                true
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let restarted = app(&path, model);
    for name in ["test", "other"] {
        let prompt = json!({"profile":name, "prompt":"hello", "history":[]});
        assert_eq!(
            request(&restarted, "POST", "/v1/respond", prompt, true)
                .await
                .0,
            StatusCode::OK
        );
    }
    wait_calls(&calls, 4).await;
    let mut actual = calls.lock().unwrap().clone();
    actual.sort();
    let mut expected = vec![
        "/v1/default/banks/test/memories/recall",
        "/v1/default/banks/test/memories",
        "/v1/default/banks/other/memories/recall",
        "/v1/default/banks/other/memories",
    ];
    expected.sort();
    assert_eq!(actual, expected);
    assert_eq!(
        request(
            &restarted,
            "PUT",
            "/v1/profiles/test/memory",
            json!({"kind":"none"}),
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    for name in ["test", "other"] {
        request(
            &restarted,
            "POST",
            "/v1/respond",
            json!({"profile":name,"prompt":"again","history":[]}),
            true,
        )
        .await;
    }
    wait_calls(&calls, 6).await;
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 6);
    assert!(calls[4..].iter().all(|path| path.contains("/banks/other/")));
    server.abort();
}

#[tokio::test]
async fn catalog_profile_rename_and_delete_move_and_remove_memory_settings() {
    use rynna_config::{
        ProfileCatalog,
        memory::{MemorySettings, MemorySettingsStore},
    };
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
    let settings = json!({"kind":"hindsight", "deployment":"cloud", "api_base":"https://api.hindsight.vectorize.io", "bank_id":"new-bank", "api_key":"new-secret"});
    assert_eq!(
        request(&router, "PUT", "/v1/profiles/new/memory", settings, true)
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
    let store = MemorySettingsStore::new(dir.path().join("memory.yaml"));
    assert!(matches!(store.load("new").unwrap(), MemorySettings::None));
    assert!(
        matches!(store.load("renamed").unwrap(), MemorySettings::Hindsight { api_key: Some(key), .. } if key == "new-secret")
    );
    assert_eq!(
        request(
            &router,
            "GET",
            "/v1/profiles/renamed/memory",
            Value::Null,
            true
        )
        .await
        .1["bank_id"],
        "new-bank"
    );
    assert_eq!(
        request(&router, "DELETE", "/v1/profiles/renamed", Value::Null, true)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert!(matches!(
        store.load("renamed").unwrap(),
        MemorySettings::None
    ));
    assert!(matches!(store.load("test").unwrap(), MemorySettings::None));
}

#[tokio::test]
async fn sync_and_sse_forward_session_ids_to_hindsight_documents() {
    let writes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = writes.clone();
    let memory_app = Router::new()
        .route(
            "/version",
            axum::routing::get(|| async { Json(json!({"version":"0.5.0"})) }),
        )
        .route(
            "/v1/default/banks/test/memories/recall",
            post(|| async { Json(json!({"results":[]})) }),
        )
        .route(
            "/v1/default/banks/test/memories",
            post(move |Json(body): Json<Value>| {
                let captured = captured.clone();
                async move {
                    captured.lock().unwrap().push(body);
                    Json(json!({"success":true}))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, memory_app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let router = app(
        &dir.path().join("providers.yaml"),
        Arc::new(Model::default()),
    );
    request(
        &router,
        "PUT",
        "/v1/profiles/test/memory",
        json!({"kind":"hindsight","deployment":"self_hosted","api_base":base,"bank_id":"test"}),
        true,
    )
    .await;
    let session = uuid::Uuid::new_v4();
    let (_, first) = request(
        &router,
        "POST",
        "/v1/respond",
        json!({"session_id":session,"prompt":"first"}),
        true,
    )
    .await;
    let stream = Request::post("/v1/respond/stream").header("content-type", "application/json")
        .body(Body::from(json!({"session_id":session,"prompt":"second","history":[{"role":"user","content":"first"},first["message"]]}).to_string())).unwrap();
    let response = router.clone().oneshot(stream).await.unwrap();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("done"));
    let other = uuid::Uuid::new_v4();
    request(
        &router,
        "POST",
        "/v1/respond",
        json!({"session_id":other,"prompt":"other chat"}),
        true,
    )
    .await;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while writes.lock().unwrap().len() < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let writes = writes.lock().unwrap();
    assert_eq!(
        writes[0]["items"][0]["document_id"],
        format!("rynna-{session}")
    );
    assert_eq!(
        writes[1]["items"][0]["document_id"],
        writes[0]["items"][0]["document_id"]
    );
    assert_eq!(
        writes[2]["items"][0]["document_id"],
        format!("rynna-{other}")
    );
    assert_eq!(writes[1]["items"][0]["metadata"]["turn_index"], "2");
    let messages: Value =
        serde_json::from_str(writes[1]["items"][0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(messages[0]["content"], "second");
    server.abort();
}
