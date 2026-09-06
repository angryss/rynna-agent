use async_trait::async_trait;
use rynna_core::{workflow_runs::*, *};
use rynna_workflows::host::Host;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Mutex;
struct Provider {
    calls: Arc<AtomicUsize>,
    supported: bool,
}
#[async_trait]
impl ModelProvider for Provider {
    fn supports_external_tools(&self) -> bool {
        self.supported
    }
    async fn complete(&self, _: CompletionRequest) -> Result<Completion, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Completion::new(Message::assistant("result")))
    }
}
fn request() -> Start {
    serde_json::from_value(serde_json::json!({"request_id":uuid::Uuid::new_v4(),"session_id":uuid::Uuid::new_v4(),"profile":"default","project":null,"selection":{"provider":"fake","model":"fake","thinking":"default"},"workflow_id":"rynna-default","goal":"goal","criteria":[{"id":"criterion","text":"criterion met"}],"limits":{"steps":50,"tool_calls":512,"active_seconds":1800}})).unwrap()
}
fn profiles(supported: bool, calls: Arc<AtomicUsize>) -> Arc<Mutex<AgentProfiles>> {
    let profile:Profile=serde_json::from_value(serde_json::json!({"name":"default","providers":[{"provider":"fake","model":"fake","default":true}]})).unwrap();
    Arc::new(Mutex::new(
        AgentProfiles::new(
            "default",
            [(
                profile,
                Agent::new(Arc::new(Provider { calls, supported }), "policy"),
            )],
        )
        .unwrap(),
    ))
}
#[tokio::test]
async fn unsupported_providers_fail_before_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let host = Host::new(profiles(false, calls.clone()), None, dir.path().into());
    let request = request();
    assert!(
        host.start(request.clone())
            .await
            .unwrap_err()
            .contains("accounting")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        host.list("default", request.session_id)
            .await
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn blocked_runs_protect_profiles_and_context_drift_blocks_resume() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let profiles = profiles(true, calls.clone());
    let host = Host::new(profiles.clone(), None, dir.path().into());
    let request = request();
    let first = host.start(request.clone()).await.unwrap();
    let run = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let r = host
                .list("default", request.session_id)
                .await
                .unwrap()
                .pop()
                .unwrap();
            if r.status == Status::Blocked {
                break r;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(host.ensure_profile_idle("default").await.is_err());
    assert!(
        host.read(first.id, "other", request.session_id)
            .await
            .is_err()
    );
    let lease = host.chat_lease(Some(request.session_id)).await.unwrap();
    let control = Control {
        profile: "default".into(),
        session_id: request.session_id,
        expected_revision: run.revision,
        action: Action::Resume {
            acknowledge_uncertain: false,
        },
    };
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(10),
            host.control(first.id, control.clone())
        )
        .await
        .is_err()
    );
    drop(lease);
    profiles
        .lock()
        .await
        .set_project_configuration("default", dir.path().into(), vec![])
        .unwrap();
    let blocked = host.control(first.id, control).await.unwrap();
    assert_eq!(blocked.status, Status::Blocked);
    assert!(blocked.reason.unwrap().contains("context changed"));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}
