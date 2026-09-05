use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use rynna_config::{ProfileCatalog, ProviderSettingsStore};
use rynna_core::{
    Agent, AgentProfiles, Completion, CompletionDelta, CompletionRequest, Message, ModelProvider,
    Profile, ProfileProvider, ProviderError,
};
use rynna_server::{
    router, router_with_profiles, router_with_profiles_and_provider_runtime,
    router_with_profiles_and_provider_settings, router_with_profiles_provider_settings_and_catalog,
    router_with_web,
};
use serde_json::Value;
use tower::ServiceExt;

use tokio::sync::Notify;
use tokio::time::{Duration, timeout};

fn local_provider_request(mut request: Request<Body>) -> Request<Body> {
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:42000".parse::<SocketAddr>().unwrap(),
    ));
    request
}

struct FixedProvider;

#[async_trait]
impl ModelProvider for FixedProvider {
    async fn complete(&self, _request: CompletionRequest) -> Result<Completion, ProviderError> {
        Ok(Completion::new(Message::assistant("Ready.")))
    }
}

struct InvalidRoleProvider;

#[async_trait]
impl ModelProvider for InvalidRoleProvider {
    async fn complete(&self, _request: CompletionRequest) -> Result<Completion, ProviderError> {
        Ok(Completion::new(Message::user("not an assistant response")))
    }
}

struct EmptyProvider;

#[async_trait]
impl ModelProvider for EmptyProvider {
    async fn complete(&self, _request: CompletionRequest) -> Result<Completion, ProviderError> {
        Ok(Completion::new(Message::assistant(" \n")))
    }
}

struct ReplyProvider(&'static str);

#[async_trait]
impl ModelProvider for ReplyProvider {
    async fn complete(&self, _request: CompletionRequest) -> Result<Completion, ProviderError> {
        Ok(Completion::new(Message::assistant(self.0)))
    }
}

struct ThinkingProvider;

#[async_trait]
impl ModelProvider for ThinkingProvider {
    async fn complete(&self, _request: CompletionRequest) -> Result<Completion, ProviderError> {
        Ok(Completion::new(Message::assistant("Answer")))
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
        on_delta: &mut (dyn for<'delta> FnMut(&'delta CompletionDelta) + Send),
    ) -> Result<Completion, ProviderError> {
        on_delta(&CompletionDelta::Thinking("Inspect".to_owned()));
        on_delta(&CompletionDelta::Content("Answer".to_owned()));
        Ok(Completion::new(Message::assistant("Answer")))
    }
}

fn test_app() -> axum::Router {
    let agent = rynna_core::Agent::new(Arc::new(FixedProvider), "You are Rynna.");
    router(agent)
}

fn profile(name: &str, reply: &'static str) -> (Profile, Agent) {
    (
        Profile {
            name: name.to_owned(),
            providers: vec![ProfileProvider {
                provider: format!("{name}-provider"),
                model: format!("{name}-model"),
                enabled: true,
                is_default: true,
            }],
            active_skills: vec![format!("{name}-skill")],
            mcp_servers: vec![format!("{name}-mcp")],
            capabilities: Vec::new(),
            default_workspace_directory: ".".into(),
            workspaces: Vec::new(),
        },
        Agent::new(Arc::new(ReplyProvider(reply)), "You are Rynna."),
    )
}

fn profiles_app() -> axum::Router {
    router_with_profiles(
        AgentProfiles::new(
            "local",
            vec![profile("local", "Local."), profile("work", "Work.")],
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn health_endpoint_reports_the_service_is_ready() {
    let response = test_app()
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value, serde_json::json!({"status": "ok"}));
}

#[tokio::test]
async fn respond_endpoint_returns_an_assistant_message() {
    let response = test_app()
        .oneshot(
            Request::post("/v1/respond")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "prompt": "Hello",
                        "history": [{"role": "user", "content": "Earlier"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "message": {"role": "assistant", "content": "Ready."}
        })
    );
}

#[tokio::test]
async fn respond_stream_endpoint_emits_reasoning_content_and_completion_events() {
    let agent = Agent::new(Arc::new(ThinkingProvider), "You are Rynna.");
    let response = router(agent)
        .oneshot(
            Request::post("/v1/respond/stream")
                .header("content-type", "application/json")
                .header("accept", "text/event-stream")
                .body(Body::from(r#"{"prompt":"Hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        body.contains(r#"data: {"kind":"thinking","content":"Inspect"}"#),
        "{body}"
    );
    assert!(
        body.contains(r#"data: {"kind":"content","content":"Answer"}"#),
        "{body}"
    );
    assert!(
        body.contains(r#"data: {"kind":"done","message":{"role":"assistant","content":"Answer"}}"#),
        "{body}"
    );
}

#[tokio::test]
async fn profiles_endpoint_lists_profiles_and_respond_dispatches_to_the_requested_profile() {
    let profiles_response = profiles_app()
        .oneshot(Request::get("/v1/profiles").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(profiles_response.status(), StatusCode::OK);
    let profiles_body = to_bytes(profiles_response.into_body(), 4096).await.unwrap();
    let profiles: Value = serde_json::from_slice(&profiles_body).unwrap();
    assert_eq!(profiles["default_profile"], "local");
    assert_eq!(profiles["profiles"][1]["name"], "work");
    assert_eq!(profiles["profiles"][1]["active_skills"][0], "work-skill");
    assert_eq!(profiles["profiles"][1]["mcp_servers"][0], "work-mcp");

    let response = profiles_app()
        .oneshot(
            Request::post("/v1/respond")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"profile":"work","prompt":"Hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(value["message"]["content"], "Work.");
}

#[tokio::test]
async fn respond_endpoint_rejects_an_unknown_profile() {
    let response = profiles_app()
        .oneshot(
            Request::post("/v1/respond")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"profile":"missing","prompt":"Hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "error": {
                "code": "unknown_profile",
                "message": "profile `missing` is not defined"
            }
        })
    );
}

#[tokio::test]
async fn respond_endpoint_rejects_a_workspace_outside_the_selected_profile() {
    let response = profiles_app()
        .oneshot(
            Request::post("/v1/respond")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"profile":"work","workspace":"missing","prompt":"Hello"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"]["code"], "invalid_request");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("workspace `missing`")
    );
}

#[tokio::test]
async fn respond_endpoint_returns_json_for_malformed_json() {
    let response = test_app()
        .oneshot(
            Request::post("/v1/respond")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"prompt":}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "error": {
                "code": "invalid_request",
                "message": "request body must be valid JSON"
            }
        })
    );
}

#[tokio::test]
async fn respond_endpoint_requires_a_json_content_type() {
    let response = test_app()
        .oneshot(
            Request::post("/v1/respond")
                .body(Body::from(r#"{"prompt":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "error": {
                "code": "unsupported_media_type",
                "message": "content type must be application/json"
            }
        })
    );
}

#[tokio::test]
async fn respond_endpoint_returns_json_for_an_oversized_body() {
    let oversized_body = serde_json::json!({
        "prompt": "x".repeat(2 * 1024 * 1024)
    })
    .to_string();
    let response = test_app()
        .oneshot(
            Request::post("/v1/respond")
                .header("content-type", "application/json")
                .body(Body::from(oversized_body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "error": {
                "code": "payload_too_large",
                "message": "request body is too large"
            }
        })
    );
}

#[tokio::test]
async fn respond_endpoint_returns_json_when_the_method_is_not_allowed() {
    let response = test_app()
        .oneshot(Request::get("/v1/respond").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "error": {
                "code": "method_not_allowed",
                "message": "HTTP method is not allowed for this API route"
            }
        })
    );
}

#[tokio::test]
async fn respond_endpoint_rejects_a_blank_prompt_as_bad_input() {
    let response = test_app()
        .oneshot(
            Request::post("/v1/respond")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"prompt":"   "}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "error": {
                "code": "invalid_request",
                "message": "user input must not be blank"
            }
        })
    );
}

#[tokio::test]
async fn respond_endpoint_hides_invalid_provider_responses() {
    let agent = rynna_core::Agent::new(Arc::new(InvalidRoleProvider), "You are Rynna.");
    let response = router(agent)
        .oneshot(
            Request::post("/v1/respond")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"prompt":"hello","history":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "error": {"code": "provider_error", "message": "model provider request failed"}
        })
    );
}

#[tokio::test]
async fn respond_endpoint_maps_exhausted_empty_responses_to_a_safe_provider_error() {
    let agent = rynna_core::Agent::new(Arc::new(EmptyProvider), "You are Rynna.");
    let response = router(agent)
        .oneshot(
            Request::post("/v1/respond")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"prompt":"hello","history":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "error": {"code": "provider_error", "message": "model provider request failed"}
        })
    );
}

#[tokio::test]
async fn web_router_serves_the_spa_for_browser_routes() {
    let web_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        web_dir.path().join("index.html"),
        "<!doctype html><title>Rynna</title>",
    )
    .unwrap();
    let agent = rynna_core::Agent::new(Arc::new(FixedProvider), "You are Rynna.");

    let response = router_with_web(agent, web_dir.path())
        .oneshot(
            Request::get("/conversations/new")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    assert!(String::from_utf8(body.to_vec()).unwrap().contains("Rynna"));
}

#[tokio::test]
async fn web_router_does_not_hide_unknown_api_routes_behind_the_spa() {
    let web_dir = tempfile::tempdir().unwrap();
    std::fs::write(web_dir.path().join("index.html"), "<title>Rynna</title>").unwrap();
    let agent = rynna_core::Agent::new(Arc::new(FixedProvider), "You are Rynna.");

    let response = router_with_web(agent, web_dir.path())
        .oneshot(Request::get("/v1/missing").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "error": {"code": "not_found", "message": "API route not found"}
        })
    );
}

#[tokio::test]
async fn provider_settings_endpoints_start_empty_and_persist_crud() {
    let directory = tempfile::tempdir().unwrap();
    let settings_path = directory.path().join("providers.toml");
    let settings = ProviderSettingsStore::load(&settings_path).unwrap();
    let profiles = AgentProfiles::new("local", vec![profile("local", "Local.")]).unwrap();
    let app = router_with_profiles_and_provider_settings(profiles, settings);

    let response = app
        .clone()
        .oneshot(local_provider_request(
            Request::get("/v1/profiles/local/providers")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        serde_json::json!([])
    );

    let response = app
        .clone()
        .oneshot(local_provider_request(
            Request::post("/v1/profiles/local/providers")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"kind":"openrouter"}"#))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(local_provider_request(
            Request::post("/v1/profiles/local/providers")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"kind":"ollama","api_base":"http://localhost:11434/v1"}"#,
                ))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(local_provider_request(
            Request::put("/v1/profiles/local/providers/ollama")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"kind":"ollama","api_base":"http://localhost:11435/v1"}"#,
                ))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let reloaded = ProviderSettingsStore::load(&settings_path).unwrap();
    assert_eq!(
        serde_json::to_value(reloaded.list("local")).unwrap(),
        serde_json::json!([
            { "kind": "openrouter" },
            {
                "kind": "ollama",
                "api_base": "http://localhost:11435/v1"
            }
        ])
    );

    let response = app
        .oneshot(local_provider_request(
            Request::delete("/v1/profiles/local/providers/ollama")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        serde_json::to_value(
            ProviderSettingsStore::load(&settings_path)
                .unwrap()
                .list("local")
        )
        .unwrap(),
        serde_json::json!([{ "kind": "openrouter" }])
    );
}

#[tokio::test]
async fn provider_settings_reject_non_loopback_clients() {
    let directory = tempfile::tempdir().unwrap();
    let settings = ProviderSettingsStore::load(directory.path().join("providers.toml")).unwrap();
    let profiles = AgentProfiles::new("local", vec![profile("local", "Local.")]).unwrap();
    let app = router_with_profiles_and_provider_settings(profiles, settings);
    let mut request = Request::post("/v1/profiles/local/providers")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"kind":"ollama","api_base":"http://localhost:11434/v1"}"#,
        ))
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "203.0.113.10:42000".parse::<SocketAddr>().unwrap(),
    ));

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn provider_settings_report_persistence_failures_as_server_errors() {
    let directory = tempfile::tempdir().unwrap();
    let settings_path = directory.path().join("providers.toml");
    let settings = ProviderSettingsStore::load(&settings_path).unwrap();
    std::fs::create_dir(&settings_path).unwrap();
    let profiles = AgentProfiles::new("local", vec![profile("local", "Local.")]).unwrap();
    let app = router_with_profiles_and_provider_settings(profiles, settings);
    let request = local_provider_request(
        Request::post("/v1/profiles/local/providers")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"kind":"ollama","api_base":"http://localhost:11434/v1"}"#,
            ))
            .unwrap(),
    );

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[cfg(unix)]
#[tokio::test]
async fn concurrent_openai_creates_authenticate_only_once() {
    let directory = tempfile::tempdir().unwrap();
    let settings = ProviderSettingsStore::load(directory.path().join("providers.toml")).unwrap();
    let login_log = directory.path().join("login.log");
    let codex = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake_codex_concurrent_login.sh");
    let profiles = AgentProfiles::new("local", vec![profile("local", "Local.")]).unwrap();
    let app = router_with_profiles_and_provider_runtime(
        profiles,
        settings,
        codex,
        directory.path().canonicalize().unwrap().join("codex-home"),
    );
    let request = |key: &'static str| {
        local_provider_request(
            Request::post("/v1/profiles/local/providers")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"kind":"openai","authentication":"api_key","api_key":"{key}"}}"#
                )))
                .unwrap(),
        )
    };

    let (first, second) = tokio::join!(
        app.clone().oneshot(request("first")),
        app.oneshot(request("second"))
    );
    let statuses = [first.unwrap().status(), second.unwrap().status()];

    assert!(statuses.contains(&StatusCode::OK));
    assert!(statuses.contains(&StatusCode::BAD_REQUEST));
    assert_eq!(
        std::fs::read_to_string(login_log).unwrap().lines().count(),
        1
    );
}

#[cfg(unix)]
#[tokio::test]
async fn openai_provider_can_reuse_existing_chatgpt_credentials_without_starting_login() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let settings = ProviderSettingsStore::load(directory.path().join("providers.toml")).unwrap();
    let invocation_log = directory.path().join("invocation.log");
    let codex = directory.path().join("codex");
    std::fs::write(
        &codex,
        format!(
            "#!/bin/sh\nprintf '%s\\n%s\\n' \"$*\" \"${{CODEX_HOME-unset}}\" >> '{}'\n[ \"$1 $2\" = 'login status' ] || exit 1\nprintf '%s\\n' 'Logged in using ChatGPT'\n",
            invocation_log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let profiles = AgentProfiles::new("local", vec![profile("local", "Local.")]).unwrap();
    let app = router_with_profiles_and_provider_runtime(
        profiles,
        settings,
        codex,
        directory.path().join("codex-home"),
    );

    let discovery = app
        .clone()
        .oneshot(local_provider_request(
            Request::get("/v1/providers/openai/existing-account")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(discovery.status(), StatusCode::OK);
    let discovery_body = to_bytes(discovery.into_body(), 1024).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&discovery_body).unwrap(),
        serde_json::json!({"connected": true, "method": "chatgpt"})
    );

    let response = app
        .oneshot(local_provider_request(
            Request::post("/v1/profiles/local/providers")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"kind":"openai","authentication":"chatgpt","reuse_existing":true}"#,
                ))
                .unwrap(),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let response_body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&response_body).unwrap(),
        serde_json::json!({
            "kind": "openai",
            "authentication": "chatgpt",
            "reuse_existing": true
        })
    );
    assert_eq!(
        std::fs::read_to_string(invocation_log).unwrap(),
        "login status\nunset\nlogin status\nunset\n"
    );
}

#[tokio::test]
async fn profiles_endpoint_creates_updates_and_deletes_catalog_profiles() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
version = 1
default_profile = "alpha"
[providers.ollama]
kind = "openai-compatible"
api_base = "http://127.0.0.1:11434/v1"
[profiles.alpha]
provider = "ollama"
model = "qwen3:8b"
"#,
    )
    .unwrap();
    let catalog = ProfileCatalog::load(&path).unwrap();
    let profiles = AgentProfiles::new("alpha", vec![profile("alpha", "Alpha.")]).unwrap();
    let settings = ProviderSettingsStore::load(directory.path().join("providers.toml")).unwrap();
    let app = router_with_profiles_provider_settings_and_catalog(profiles, settings, catalog);

    let created = app
        .clone()
        .oneshot(local_provider_request(
            Request::post("/v1/profiles")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "work",
                        "providers": [{ "provider": "ollama", "model": "gpt-5" }],
                        "active_skills": [],
                        "mcp_servers": [],
                        "capabilities": []
                    })
                    .to_string(),
                ))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created_body = to_bytes(created.into_body(), 4096).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&created_body).unwrap()["name"],
        "work"
    );

    let provider = app
        .clone()
        .oneshot(local_provider_request(
            Request::post("/v1/profiles/work/providers")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"kind":"ollama","api_base":"http://127.0.0.1:11434/v1"}"#,
                ))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(provider.status(), StatusCode::OK);

    let updated = app
        .clone()
        .oneshot(local_provider_request(
            Request::put("/v1/profiles/work")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "renamed-work",
                        "providers": [{ "provider": "ollama", "model": "gpt-5.2" }],
                        "active_skills": [],
                        "mcp_servers": [],
                        "capabilities": []
                    })
                    .to_string(),
                ))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);

    let renamed_providers = app
        .clone()
        .oneshot(local_provider_request(
            Request::get("/v1/profiles/renamed-work/providers")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(renamed_providers.status(), StatusCode::OK);
    let renamed_providers = to_bytes(renamed_providers.into_body(), 4096).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&renamed_providers)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let deleted = app
        .clone()
        .oneshot(local_provider_request(
            Request::delete("/v1/profiles/renamed-work")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    let listed = app
        .oneshot(Request::get("/v1/profiles").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let listed_body = to_bytes(listed.into_body(), 4096).await.unwrap();
    let listed: Value = serde_json::from_slice(&listed_body).unwrap();
    assert_eq!(listed["profiles"].as_array().unwrap().len(), 1);
    assert_eq!(listed["profiles"][0]["name"], "alpha");
    let settings = ProviderSettingsStore::load(directory.path().join("providers.toml")).unwrap();
    assert!(settings.list("work").is_empty());
    assert!(settings.list("renamed-work").is_empty());
}

#[tokio::test]
async fn updated_workspace_metadata_is_available_to_subsequent_runtime_requests() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
version = 1
default_profile = "alpha"
[providers.ollama]
kind = "openai-compatible"
api_base = "http://127.0.0.1:11434/v1"
[profiles.alpha]
provider = "ollama"
model = "qwen3:8b"
"#,
    )
    .unwrap();
    let catalog = ProfileCatalog::load(&path).unwrap();
    let profiles = AgentProfiles::new("alpha", vec![profile("alpha", "Alpha.")]).unwrap();
    let settings = ProviderSettingsStore::load(directory.path().join("providers.toml")).unwrap();
    let app = router_with_profiles_provider_settings_and_catalog(profiles, settings, catalog);

    let updated = app
        .clone()
        .oneshot(local_provider_request(
            Request::put("/v1/profiles/alpha")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "alpha",
                        "providers": [{ "provider": "ollama", "model": "qwen3:8b" }],
                        "active_skills": [],
                        "mcp_servers": [],
                        "capabilities": [],
                        "default_workspace_directory": "/projects/home",
                        "workspaces": [{
                            "name": "rynna",
                            "directories": ["/projects/rynna", "/projects/shared"],
                            "default_directory": "/projects/rynna"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::post("/v1/respond")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"profile":"alpha","workspace":"rynna","prompt":"Hello"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn created_profile_metadata_is_not_attached_to_an_existing_runtime_agent() {
    struct CountingProvider(AtomicUsize);

    #[async_trait]
    impl ModelProvider for CountingProvider {
        async fn complete(&self, _request: CompletionRequest) -> Result<Completion, ProviderError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Completion::new(Message::assistant("sensitive runtime")))
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
version = 1
default_profile = "alpha"
[providers.ollama]
kind = "openai-compatible"
api_base = "http://127.0.0.1:11434/v1"
[profiles.alpha]
provider = "ollama"
model = "qwen3:8b"
"#,
    )
    .unwrap();
    let catalog = ProfileCatalog::load(path).unwrap();
    let provider = Arc::new(CountingProvider(AtomicUsize::new(0)));
    let runtime_profile = Profile {
        name: "alpha".to_owned(),
        providers: vec![ProfileProvider {
            provider: "ollama".to_owned(),
            model: "qwen3:8b".to_owned(),
            enabled: true,
            is_default: true,
        }],
        active_skills: vec!["sensitive-skill".to_owned()],
        mcp_servers: Vec::new(),
        capabilities: vec!["sensitive-capability".to_owned()],
        default_workspace_directory: ".".into(),
        workspaces: Vec::new(),
    };
    let profiles = AgentProfiles::new(
        "alpha",
        [(
            runtime_profile,
            Agent::new(provider.clone(), "sensitive system prompt"),
        )],
    )
    .unwrap();
    let settings = ProviderSettingsStore::load(directory.path().join("providers.toml")).unwrap();
    let app = router_with_profiles_provider_settings_and_catalog(profiles, settings, catalog);
    let new_profile = serde_json::json!({
        "name": "new",
        "providers": [{"provider": "ollama", "model": "other"}],
        "active_skills": [],
        "mcp_servers": [],
        "capabilities": []
    });

    let created = app
        .clone()
        .oneshot(local_provider_request(
            Request::post("/v1/profiles")
                .header("content-type", "application/json")
                .body(Body::from(new_profile.to_string()))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let response = app
        .oneshot(
            Request::post("/v1/respond")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"profile":"new","prompt":"run"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(provider.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn updated_catalog_metadata_does_not_replace_running_profile_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
version = 1
default_profile = "alpha"
[providers.ollama]
kind = "openai-compatible"
api_base = "http://127.0.0.1:11434/v1"
[profiles.alpha]
provider = "ollama"
model = "qwen3:8b"
"#,
    )
    .unwrap();
    let catalog = ProfileCatalog::load(path).unwrap();
    let runtime_profile = Profile {
        name: "alpha".to_owned(),
        providers: vec![ProfileProvider {
            provider: "ollama".to_owned(),
            model: "runtime-model".to_owned(),
            enabled: true,
            is_default: true,
        }],
        active_skills: Vec::new(),
        mcp_servers: Vec::new(),
        capabilities: vec!["runtime-capability".to_owned()],
        default_workspace_directory: ".".into(),
        workspaces: Vec::new(),
    };
    let profiles = AgentProfiles::new(
        "alpha",
        [(
            runtime_profile,
            Agent::new(Arc::new(FixedProvider), "runtime policy"),
        )],
    )
    .unwrap();
    let settings = ProviderSettingsStore::load(directory.path().join("providers.toml")).unwrap();
    let app = router_with_profiles_provider_settings_and_catalog(profiles, settings, catalog);

    let updated = app
        .clone()
        .oneshot(local_provider_request(
            Request::put("/v1/profiles/alpha")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"name":"alpha","providers":[{"provider":"ollama","model":"saved-model"}],"active_skills":[],"mcp_servers":[],"capabilities":[]}"#,
                ))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);
    let listed = app
        .oneshot(Request::get("/v1/profiles").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let listed: Value =
        serde_json::from_slice(&to_bytes(listed.into_body(), 4096).await.unwrap()).unwrap();

    assert_eq!(
        listed["profiles"][0]["providers"][0]["model"],
        "runtime-model"
    );
    assert_eq!(
        listed["profiles"][0]["capabilities"],
        serde_json::json!(["runtime-capability"])
    );
    assert_eq!(
        listed["configured_profiles"][0]["providers"][0]["model"],
        "saved-model"
    );
}

#[tokio::test]
async fn profile_catalog_response_includes_unused_custom_provider_ids() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
version = 1
default_profile = "alpha"
[providers.ollama]
kind = "openai-compatible"
api_base = "http://127.0.0.1:11434/v1"
[providers.unused-custom]
kind = "openai-compatible"
api_base = "https://custom.example/v1"
[profiles.alpha]
providers = [
  { provider = "ollama", model = "qwen3:8b", enabled = true, default = true },
  { provider = "ollama", model = "qwen3:14b", enabled = false },
]
"#,
    )
    .unwrap();
    let catalog = ProfileCatalog::load(path).unwrap();
    let mut alpha = profile("alpha", "Alpha.");
    alpha.0.providers.push(ProfileProvider {
        provider: "ollama".to_owned(),
        model: "qwen3:14b".to_owned(),
        enabled: false,
        is_default: false,
    });
    let profiles = AgentProfiles::new("alpha", vec![alpha]).unwrap();
    let settings = ProviderSettingsStore::load(directory.path().join("providers.toml")).unwrap();
    let response = router_with_profiles_provider_settings_and_catalog(profiles, settings, catalog)
        .oneshot(Request::get("/v1/profiles").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(
        value["provider_ids"],
        serde_json::json!(["ollama", "unused-custom"])
    );
    assert_eq!(
        value["profiles"][0]["providers"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        value["configured_profiles"][0]["providers"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn profile_mutations_preserve_runtime_default_and_active_delete_selects_a_runtime_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
version = 1
default_profile = "alpha"
[providers.ollama]
kind = "openai-compatible"
api_base = "http://127.0.0.1:11434/v1"
[profiles.alpha]
provider = "ollama"
model = "qwen3:8b"
[profiles.work]
provider = "ollama"
model = "qwen3:14b"
"#,
    )
    .unwrap();
    let catalog = ProfileCatalog::load(path).unwrap();
    let profiles = AgentProfiles::new(
        "work",
        vec![
            profile("alpha", "Alpha."),
            profile("work", "Work."),
            profile("runtime-only", "Runtime fallback."),
        ],
    )
    .unwrap();
    let settings = ProviderSettingsStore::load(directory.path().join("providers.toml")).unwrap();
    let app = router_with_profiles_provider_settings_and_catalog(profiles, settings, catalog);

    let created = app
        .clone()
        .oneshot(local_provider_request(
            Request::post("/v1/profiles")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"name":"personal","providers":[{"provider":"ollama","model":"qwen3"}],"active_skills":[],"mcp_servers":[],"capabilities":[]}"#,
                ))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let listed = app
        .clone()
        .oneshot(Request::get("/v1/profiles").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let listed: Value =
        serde_json::from_slice(&to_bytes(listed.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(listed["default_profile"], "work");

    let deleted = app
        .clone()
        .oneshot(local_provider_request(
            Request::delete("/v1/profiles/work")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    let listed = app
        .oneshot(Request::get("/v1/profiles").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let listed: Value =
        serde_json::from_slice(&to_bytes(listed.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(listed["default_profile"], "alpha");
}

#[tokio::test]
async fn profile_persistence_errors_are_5xx_and_validation_errors_remain_4xx() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
version = 1
default_profile = "alpha"
[providers.ollama]
kind = "openai-compatible"
api_base = "http://127.0.0.1:11434/v1"
[profiles.alpha]
provider = "ollama"
model = "qwen3:8b"
"#,
    )
    .unwrap();
    let catalog = ProfileCatalog::load(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let profiles = AgentProfiles::new("alpha", vec![profile("alpha", "Alpha.")]).unwrap();
    let settings = ProviderSettingsStore::load(directory.path().join("providers.toml")).unwrap();
    let app = router_with_profiles_provider_settings_and_catalog(profiles, settings, catalog);
    let body = r#"{"name":"new","providers":[{"provider":"ollama","model":"qwen3"}],"active_skills":[],"mcp_servers":[],"capabilities":[]}"#;
    let persistence = app
        .oneshot(local_provider_request(
            Request::post("/v1/profiles")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(persistence.status(), StatusCode::INTERNAL_SERVER_ERROR);

    std::fs::remove_dir(&path).unwrap();
    std::fs::write(
        &path,
        r#"
version = 1
default_profile = "alpha"
[providers.ollama]
kind = "openai-compatible"
api_base = "http://127.0.0.1:11434/v1"
[profiles.alpha]
provider = "ollama"
model = "qwen3:8b"
"#,
    )
    .unwrap();
    let catalog = ProfileCatalog::load(&path).unwrap();
    let profiles = AgentProfiles::new("alpha", vec![profile("alpha", "Alpha.")]).unwrap();
    let settings =
        ProviderSettingsStore::load(directory.path().join("other-providers.toml")).unwrap();
    let validation = router_with_profiles_provider_settings_and_catalog(profiles, settings, catalog)
        .oneshot(local_provider_request(
            Request::post("/v1/profiles")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"name":"alpha","providers":[{"provider":"ollama","model":"qwen3"}],"active_skills":[],"mcp_servers":[],"capabilities":[]}"#,
                ))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(validation.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn non_streaming_response_releases_profiles_lock_while_provider_is_pending() {
    struct BlockingProvider {
        started: Notify,
        release: Notify,
    }

    #[async_trait]
    impl ModelProvider for BlockingProvider {
        async fn complete(&self, _request: CompletionRequest) -> Result<Completion, ProviderError> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(Completion::new(Message::assistant("released")))
        }
    }

    let provider = Arc::new(BlockingProvider {
        started: Notify::new(),
        release: Notify::new(),
    });
    let runtime_profile = Profile {
        name: "alpha".to_owned(),
        providers: vec![ProfileProvider {
            provider: "test".to_owned(),
            model: "test".to_owned(),
            enabled: true,
            is_default: true,
        }],
        active_skills: Vec::new(),
        mcp_servers: Vec::new(),
        capabilities: Vec::new(),
        default_workspace_directory: ".".into(),
        workspaces: Vec::new(),
    };
    let profiles = AgentProfiles::new(
        "alpha",
        [(runtime_profile, Agent::new(provider.clone(), "policy"))],
    )
    .unwrap();
    let app = router_with_profiles(profiles);
    let pending = tokio::spawn(
        app.clone().oneshot(
            Request::post("/v1/respond")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"prompt":"wait"}"#))
                .unwrap(),
        ),
    );
    provider.started.notified().await;

    let listed = timeout(
        Duration::from_millis(200),
        app.oneshot(Request::get("/v1/profiles").body(Body::empty()).unwrap()),
    )
    .await;
    provider.release.notify_one();
    let _ = pending.await.unwrap().unwrap();

    assert!(
        listed.is_ok(),
        "profiles lock remained held across provider await"
    );
}

#[tokio::test]
async fn selection_is_validated_before_starting_either_response_mode() {
    for endpoint in ["/v1/respond", "/v1/respond/stream"] {
        for selection in [
            serde_json::json!({"provider":"missing","model":"missing"}),
            serde_json::json!({"provider":"local-provider","model":"local-model","thinking":"invalid"}),
        ] {
            let response = profiles_app()
                .oneshot(
                    Request::post(endpoint)
                        .header("content-type", "application/json")
                        .body(Body::from(
                            serde_json::json!({"prompt":"hello","selection":selection}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }
}

#[tokio::test]
async fn both_http_response_modes_route_to_the_selected_pair() {
    let (mut metadata, agent) = profile("local", "default");
    let option = ProfileProvider {
        provider: "selected".into(),
        model: "selected-model".into(),
        enabled: true,
        is_default: false,
    };
    metadata.providers.push(option.clone());
    let agent =
        agent.with_model_options(vec![(option, Arc::new(ReplyProvider("selected answer")))]);
    let profiles = AgentProfiles::new("local", vec![(metadata, agent)]).unwrap();
    for endpoint in ["/v1/respond", "/v1/respond/stream"] {
        let response = router_with_profiles(profiles.clone()).oneshot(Request::post(endpoint)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({"prompt":"hello","selection":{"provider":"selected","model":"selected-model"}}).to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("selected answer"));
    }
    assert_eq!(
        profiles.respond(None, &[], "hello").await.unwrap().content,
        "default"
    );
}
