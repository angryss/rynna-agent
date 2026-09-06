use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use rynna_config::ProviderSettingsStore;
use rynna_core::{
    Agent, AgentProfiles, Completion, CompletionRequest, Message, ModelProvider, Profile,
    ProviderError,
};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tower::ServiceExt;
struct Provider(AtomicUsize);
#[async_trait]
impl ModelProvider for Provider {
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        let prompt = &request.messages.last().unwrap().content;
        assert!(prompt.contains("goal"));
        assert!(prompt.contains("criterion"));
        let response = if prompt.contains("Return only JSON") {
            let pass = self.0.fetch_add(1, Ordering::SeqCst) > 0;
            serde_json::json!({"results":[{"criterion_id":"criterion","verdict":if pass {"met"}else{"unmet"},"kind":"test","reference":"fake check","excerpt":"deterministic evidence"}],"summary":"checked","can_continue":true}).to_string()
        } else {
            "implementation result".into()
        };
        Ok(Completion::new(Message::assistant(response)))
    }
}
fn request(method: &str, path: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}
#[tokio::test]
async fn http_run_survives_observer_disconnect_retries_and_redacts_definitions() {
    let dir = tempfile::tempdir().unwrap();
    let profile:Profile=serde_json::from_value(serde_json::json!({"name":"default","providers":[{"provider":"fake","model":"fake","default":true}]})).unwrap();
    let agent = Agent::new(Arc::new(Provider(AtomicUsize::new(0))), "policy");
    let profiles = AgentProfiles::new("default", [(profile, agent)]).unwrap();
    let app = rynna_server::router_with_profiles_and_provider_runtime(
        profiles,
        ProviderSettingsStore::load(dir.path().join("providers.toml")).unwrap(),
        "unused",
        dir.path().join("codex"),
    );
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/profiles/default/workflows",
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 100000).await.unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("instructions"));
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/profiles/default/workflows/rynna-default",
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let mut admin = request(
        "GET",
        "/v1/profiles/default/workflows/rynna-default",
        serde_json::Value::Null,
    );
    admin
        .extensions_mut()
        .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
    assert_eq!(
        app.clone().oneshot(admin).await.unwrap().status(),
        StatusCode::OK
    );
    let session = uuid::Uuid::new_v4();
    let body = serde_json::json!({"request_id":uuid::Uuid::new_v4(),"session_id":session,"profile":"default","project":null,"selection":{"provider":"fake","model":"fake","thinking":"default"},"workflow_id":"rynna-default","goal":"goal","criteria":[{"id":"criterion","text":"criterion met"}],"limits":{"steps":50,"tool_calls":512,"active_seconds":1800}});
    // Drop the accepted response without reading it; the worker belongs to the host.
    let response = app
        .clone()
        .oneshot(request("POST", "/v1/workflow-runs", body.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    drop(response);
    let path = format!("/v1/workflow-runs?profile=default&session_id={session}");
    let run = loop {
        let response = app
            .clone()
            .oneshot(request("GET", &path, serde_json::Value::Null))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        let runs: Vec<rynna_core::workflow_runs::Run> = serde_json::from_slice(&bytes).unwrap();
        let run = runs.into_iter().next().unwrap();
        if run.status == rynna_core::workflow_runs::Status::Completed {
            break run;
        }
        tokio::task::yield_now().await;
    };
    assert_eq!(run.consumed.steps, 5);
    assert_eq!(run.events.len(), 5);
    assert_eq!(run.consumed.tool_calls, 0);
    let retry = app
        .clone()
        .oneshot(request("POST", "/v1/workflow-runs", body))
        .await
        .unwrap();
    let bytes = to_bytes(retry.into_body(), 4 * 1024 * 1024).await.unwrap();
    let retry: rynna_core::workflow_runs::Run = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(retry.id, run.id);
    assert_eq!(retry.events.len(), 5);
    let wrong = format!(
        "/v1/workflow-runs/{}?profile=wrong&session_id={session}",
        run.id
    );
    assert_eq!(
        app.oneshot(request("GET", &wrong, serde_json::Value::Null))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
}
