use async_trait::async_trait;
use rynna_core::{ModelSelection, ThinkingLevel, workflow_runs::*, workflows::default_workflow};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use uuid::Uuid;
#[derive(Default)]
struct Store {
    runs: Mutex<Vec<Run>>,
    fail: AtomicUsize,
}
#[async_trait]
impl RunStore for Store {
    async fn load(&self) -> Result<Vec<Run>, String> {
        Ok(self.runs.lock().unwrap().clone())
    }
    async fn save(&self, r: &Run) -> Result<(), String> {
        if self.fail.load(Ordering::SeqCst) > 0 {
            return Err("disk full".into());
        }
        let mut all = self.runs.lock().unwrap();
        all.retain(|x| x.id != r.id);
        all.push(r.clone());
        Ok(())
    }
}
struct Executor {
    calls: AtomicUsize,
    malformed: bool,
}
#[async_trait]
impl WorkflowExecutor for Executor {
    async fn preflight(&self, _: &Run) -> Result<(), String> {
        Ok(())
    }
    async fn execute(&self, r: &Run, _: usize) -> Result<ExecutionResult, String> {
        let count = self.calls.fetch_add(1, Ordering::SeqCst);
        let content = if r.cursor == 2 && !self.malformed {
            serde_json::json!({"results":[{"criterion_id":"works","verdict":if count>=4 {"met"}else{"unmet"},"kind":"test","reference":"tests","excerpt":"deterministic check"}],"summary":"checked","can_continue":true}).to_string()
        } else {
            "work result".into()
        };
        assert!(r.prompt().unwrap().contains("works"));
        Ok(ExecutionResult {
            content,
            tool_calls: 1,
        })
    }
}
fn start() -> Start {
    Start {
        request_id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        profile: "default".into(),
        project: None,
        selection: ModelSelection {
            provider: "fake".into(),
            model: "fake".into(),
            thinking: ThinkingLevel::Default,
        },
        workflow_id: "rynna-default".into(),
        goal: "work".into(),
        criteria: vec![Criterion {
            id: "works".into(),
            text: "it works".into(),
        }],
        limits: Limits::default(),
        initial_context: String::new(),
    }
}
async fn settled(runner: &Runner, start: &Start) -> Run {
    for _ in 0..500 {
        let run = runner
            .list(&start.profile, start.session_id)
            .await
            .pop()
            .unwrap();
        if !run.status.executing() {
            return run;
        }
        tokio::task::yield_now().await;
    }
    panic!("run did not settle")
}
#[tokio::test]
async fn repeats_then_completes_with_evidence_and_exact_accounting() {
    let store = Arc::new(Store::default());
    let executor = Arc::new(Executor {
        calls: AtomicUsize::new(0),
        malformed: false,
    });
    let runner = Runner::open(store, executor.clone()).await.unwrap();
    let request = start();
    let first = runner
        .start(
            request.clone(),
            default_workflow(),
            vec![],
            "snapshot".into(),
        )
        .await
        .unwrap();
    let retry = runner
        .start(request.clone(), default_workflow(), vec![], "other".into())
        .await
        .unwrap();
    assert_eq!(first.id, retry.id);
    let run = settled(&runner, &request).await;
    assert_eq!(run.status, Status::Completed);
    assert_eq!(run.consumed.steps, 5);
    assert_eq!(run.consumed.tool_calls, 5);
    assert_eq!(
        run.events
            .iter()
            .map(|e| e.step_id.as_str())
            .collect::<Vec<_>>(),
        vec!["plan", "execute", "verify", "execute", "verify"]
    );
    assert_eq!(executor.calls.load(Ordering::SeqCst), 5);
    assert!(run.verification.is_some());
}
#[tokio::test]
async fn malformed_verification_blocks_and_stale_controls_conflict() {
    let runner = Runner::open(
        Arc::new(Store::default()),
        Arc::new(Executor {
            calls: AtomicUsize::new(0),
            malformed: true,
        }),
    )
    .await
    .unwrap();
    let request = start();
    runner
        .start(request.clone(), default_workflow(), vec![], "x".into())
        .await
        .unwrap();
    let run = settled(&runner, &request).await;
    assert_eq!(run.status, Status::Blocked);
    let control = Control {
        profile: request.profile.clone(),
        session_id: request.session_id,
        expected_revision: run.revision - 1,
        action: Action::Cancel,
    };
    assert!(runner.control(run.id, control).await.is_err());
    let mut other = request.clone();
    other.request_id = Uuid::new_v4();
    assert!(
        runner
            .start(other, default_workflow(), vec![], "x".into())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn limits_and_recovery_never_reset_or_replay() {
    let store = Arc::new(Store::default());
    let executor = Arc::new(Executor {
        calls: AtomicUsize::new(0),
        malformed: false,
    });
    let runner = Runner::open(store.clone(), executor.clone()).await.unwrap();
    let mut request = start();
    request.limits.steps = 1;
    runner
        .start(request.clone(), default_workflow(), vec![], "x".into())
        .await
        .unwrap();
    let run = settled(&runner, &request).await;
    assert_eq!(run.status, Status::BudgetExhausted);
    let mut interrupted = run.clone();
    interrupted.status = Status::Running;
    interrupted.in_flight = true;
    interrupted.consumed.tool_calls = 64;
    store.save(&interrupted).await.unwrap();
    let recovered = Runner::open(store, executor.clone()).await.unwrap();
    let run = settled(&recovered, &request).await;
    assert_eq!(run.status, Status::Paused);
    assert!(run.uncertain);
    assert_eq!(run.consumed.tool_calls, 64);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    assert!(
        recovered
            .control(
                run.id,
                Control {
                    profile: request.profile,
                    session_id: request.session_id,
                    expected_revision: run.revision,
                    action: Action::Resume {
                        acknowledge_uncertain: false
                    }
                }
            )
            .await
            .is_err()
    );
}

#[test]
fn transport_control_contract_roundtrips() {
    let control = Control {
        profile: "default".into(),
        session_id: Uuid::new_v4(),
        expected_revision: 1,
        action: Action::Resume {
            acknowledge_uncertain: true,
        },
    };
    let json = serde_json::to_value(&control).unwrap();
    assert_eq!(json["action"], "resume");
    let decoded: Control = serde_json::from_value(json).unwrap();
    assert!(matches!(
        decoded.action,
        Action::Resume {
            acknowledge_uncertain: true
        }
    ));
}

struct GatedExecutor {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    calls: AtomicUsize,
}
#[async_trait]
impl WorkflowExecutor for GatedExecutor {
    async fn preflight(&self, _: &Run) -> Result<(), String> {
        Ok(())
    }
    async fn execute(&self, run: &Run, _: usize) -> Result<ExecutionResult, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
        let content = if run.cursor + 1 == run.workflow.steps.len() {
            serde_json::json!({"results":[{"criterion_id":run.start.criteria[0].id,"verdict":"met","kind":"qualitative","reference":"review","excerpt":"checked"}],"summary":"met","can_continue":true}).to_string()
        } else {
            "step finished".into()
        };
        Ok(ExecutionResult {
            content,
            tool_calls: 2,
        })
    }
}
fn gated() -> Arc<GatedExecutor> {
    Arc::new(GatedExecutor {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        calls: AtomicUsize::new(0),
    })
}
#[tokio::test]
async fn pause_settles_steering_invalidates_and_cancel_wins_completion() {
    let executor = gated();
    let runner = Runner::open(Arc::new(Store::default()), executor.clone())
        .await
        .unwrap();
    let request = start();
    let mut definition = default_workflow();
    definition.steps.remove(0);
    runner
        .start(request.clone(), definition, vec![], "x".into())
        .await
        .unwrap();
    executor.entered.notified().await;
    let run = runner
        .list(&request.profile, request.session_id)
        .await
        .pop()
        .unwrap();
    let pausing = runner
        .control(
            run.id,
            Control {
                profile: request.profile.clone(),
                session_id: request.session_id,
                expected_revision: run.revision,
                action: Action::Pause,
            },
        )
        .await
        .unwrap();
    assert_eq!(pausing.status, Status::Pausing);
    executor.release.notify_one();
    let paused = settled(&runner, &request).await;
    assert_eq!(paused.status, Status::Paused);
    assert_eq!(paused.cursor, 1);
    assert_eq!(paused.events.len(), 1);
    let steered = runner
        .control(
            run.id,
            Control {
                profile: request.profile.clone(),
                session_id: request.session_id,
                expected_revision: paused.revision,
                action: Action::Steer {
                    text: "amended".into(),
                    criteria: Some(vec![Criterion {
                        id: "new".into(),
                        text: "new criterion".into(),
                    }]),
                },
            },
        )
        .await
        .unwrap();
    assert!(steered.verification.is_none());
    assert!(steered.prompt().unwrap().contains("new criterion"));
    runner
        .control(
            run.id,
            Control {
                profile: request.profile.clone(),
                session_id: request.session_id,
                expected_revision: steered.revision,
                action: Action::Resume {
                    acknowledge_uncertain: false,
                },
            },
        )
        .await
        .unwrap();
    executor.entered.notified().await;
    let verifying = runner
        .list(&request.profile, request.session_id)
        .await
        .pop()
        .unwrap();
    let cancelling = runner
        .control(
            run.id,
            Control {
                profile: request.profile.clone(),
                session_id: request.session_id,
                expected_revision: verifying.revision,
                action: Action::Cancel,
            },
        )
        .await
        .unwrap();
    assert_eq!(cancelling.status, Status::Cancelling);
    executor.release.notify_one();
    let done = settled(&runner, &request).await;
    assert_eq!(done.status, Status::Cancelled);
    assert_eq!(done.consumed.steps, 2);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn failed_result_checkpoint_stops_dispatch_and_keeps_reservations() {
    let executor = gated();
    let store = Arc::new(Store::default());
    let runner = Runner::open(store.clone(), executor.clone()).await.unwrap();
    let request = start();
    runner
        .start(request.clone(), default_workflow(), vec![], "x".into())
        .await
        .unwrap();
    executor.entered.notified().await;
    store.fail.store(1, Ordering::SeqCst);
    executor.release.notify_one();
    let run = settled(&runner, &request).await;
    assert_eq!(run.status, Status::Paused);
    assert!(run.uncertain);
    assert!(run.reason.unwrap().contains("persistence failure"));
    assert!(run.events.is_empty());
    assert_eq!(run.consumed.tool_calls, 64);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn concurrent_starts_admit_only_one_request() {
    let executor = gated();
    let runner = Runner::open(Arc::new(Store::default()), executor)
        .await
        .unwrap();
    let request = start();
    let mut other = request.clone();
    other.request_id = Uuid::new_v4();
    let (a, b) = tokio::join!(
        runner.start(request, default_workflow(), vec![], "x".into()),
        runner.start(other, default_workflow(), vec![], "x".into())
    );
    assert_ne!(a.is_ok(), b.is_ok());
}

#[tokio::test]
async fn cancel_interrupts_a_blocked_step_without_waiting_for_completion() {
    let executor = gated();
    let runner = Runner::open(Arc::new(Store::default()), executor.clone())
        .await
        .unwrap();
    let request = start();
    runner
        .start(
            request.clone(),
            default_workflow(),
            vec![],
            "snapshot".into(),
        )
        .await
        .unwrap();
    executor.entered.notified().await;
    let active = runner
        .list(&request.profile, request.session_id)
        .await
        .pop()
        .unwrap();
    runner
        .control(
            active.id,
            Control {
                profile: request.profile.clone(),
                session_id: request.session_id,
                expected_revision: active.revision,
                action: Action::Cancel,
            },
        )
        .await
        .unwrap();
    // Deliberately never release the executor. Cancellation must drop the step.
    let stopped = settled(&runner, &request).await;
    assert_eq!(stopped.status, Status::Cancelled);
    assert!(!stopped.in_flight);
    assert!(stopped.events.is_empty());
    assert_eq!(stopped.cursor, 0);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}
