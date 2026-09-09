use async_trait::async_trait;
use rynna_core::workflow_runs::*;
use rynna_workflows::FileRunStore;
use std::sync::Arc;
struct Executor;
#[async_trait]
impl WorkflowExecutor for Executor {
    async fn preflight(&self, _: &Run) -> Result<(), String> {
        Ok(())
    }
    async fn execute(&self, _: &Run, _: usize) -> Result<ExecutionResult, ExecutionError> {
        std::future::pending().await
    }
}
#[tokio::test]
async fn private_single_owner_and_corrupt_records_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::open(dir.path()).unwrap();
    assert!(FileRunStore::open(dir.path()).is_err());
    assert!(store.load().await.unwrap().is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    std::fs::write(dir.path().join("bad.json"), "{bad").unwrap();
    assert!(store.load().await.is_err());
    assert!(dir.path().join("bad.json").exists());
    drop(store);
    assert!(FileRunStore::open(dir.path()).is_ok());
}
#[tokio::test]
async fn durable_reservations_recover_without_execution() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileRunStore::open(dir.path()).unwrap());
    let runner = Runner::open(store.clone(), Arc::new(Executor))
        .await
        .unwrap();
    let start:Start=serde_json::from_value(serde_json::json!({"request_id":uuid::Uuid::new_v4(),"session_id":uuid::Uuid::new_v4(),"profile":"default","project":null,"selection":{"provider":"fake","model":"fake","thinking":"default"},"workflow_id":"rynna-default","goal":"goal","criteria":[{"id":"done","text":"done"}],"limits":{"steps":50,"tool_calls":512,"active_seconds":1800}})).unwrap();
    let run = runner
        .start(
            start,
            rynna_core::workflows::default_workflow(),
            vec![],
            "snapshot".into(),
        )
        .await
        .unwrap();
    for _ in 0..100 {
        if store.load().await.unwrap()[0].in_flight {
            break;
        }
        tokio::task::yield_now().await;
    }
    let checkpoint = store.load().await.unwrap().pop().unwrap();
    assert!(checkpoint.in_flight);
    assert_eq!(checkpoint.consumed.tool_calls, 64);
    let second = Runner::open(store.clone(), Arc::new(Executor))
        .await
        .unwrap();
    let recovered = second
        .list("default", run.start.session_id)
        .await
        .pop()
        .unwrap();
    assert_eq!(recovered.status, Status::Paused);
    assert!(recovered.uncertain);
    assert_eq!(recovered.consumed.active_seconds, 300);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(dir.path().join(format!("{}.json", run.id)))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
