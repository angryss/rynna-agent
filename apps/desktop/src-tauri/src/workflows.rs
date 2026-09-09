use rynna_core::{
    workflow_runs::{Control, Run, Start},
    workflows::{Workflow, WorkflowMetadata},
};
use rynna_workflows::host::Host;
use std::sync::Arc;
use tauri::State;
#[tauri::command]
pub async fn list_workflows(
    host: State<'_, Arc<Host>>,
    profile: String,
) -> Result<Vec<WorkflowMetadata>, String> {
    Ok(host
        .definitions(&profile)
        .await?
        .iter()
        .map(Workflow::metadata)
        .collect())
}
#[tauri::command]
pub async fn read_workflow(
    host: State<'_, Arc<Host>>,
    profile: String,
    id: String,
) -> Result<Workflow, String> {
    host.definitions(&profile)
        .await?
        .into_iter()
        .find(|w| w.id == id)
        .ok_or("workflow unavailable".into())
}
#[tauri::command]
pub async fn save_workflow(
    host: State<'_, Arc<Host>>,
    profile: String,
    workflow: Workflow,
) -> Result<Workflow, String> {
    host.save(&profile, workflow).await
}
#[tauri::command]
pub async fn delete_workflow(
    host: State<'_, Arc<Host>>,
    profile: String,
    id: String,
) -> Result<(), String> {
    host.delete(&profile, &id).await
}
#[tauri::command]
pub async fn start_workflow(host: State<'_, Arc<Host>>, request: Start) -> Result<Run, String> {
    host.start(request).await
}
#[tauri::command]
pub async fn list_workflow_runs(
    host: State<'_, Arc<Host>>,
    profile: String,
    session_id: uuid::Uuid,
) -> Result<Vec<Run>, String> {
    host.list(&profile, session_id).await
}
#[tauri::command]
pub async fn control_workflow(
    host: State<'_, Arc<Host>>,
    id: uuid::Uuid,
    request: Control,
) -> Result<Run, String> {
    host.control(id, request).await
}

#[tauri::command]
pub async fn read_workflow_run(
    host: State<'_, Arc<Host>>,
    id: uuid::Uuid,
    profile: String,
    session_id: uuid::Uuid,
) -> Result<Run, String> {
    host.read(id, &profile, session_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tauri::Manager;
    struct Provider(AtomicUsize);
    #[async_trait]
    impl rynna_core::ModelProvider for Provider {
        async fn complete(
            &self,
            request: rynna_core::CompletionRequest,
        ) -> Result<rynna_core::Completion, rynna_core::ProviderError> {
            let prompt = &request.messages.last().unwrap().content;
            assert!(prompt.contains("criterion"));
            let text = if prompt.contains("Return only JSON") {
                assert!(
                    request.messages[0]
                        .content
                        .contains("Subagent role: reviewer")
                );
                assert!(request.tools.is_empty());
                let met = self.0.fetch_add(1, Ordering::SeqCst) > 0;
                serde_json::json!({"results":[{"criterion_id":"criterion","verdict":if met {"met"}else{"unmet"},"kind":"test","reference":"fake check","excerpt":"deterministic evidence"}],"summary":"checked","can_continue":true}).to_string()
            } else {
                "implementation result".into()
            };
            Ok(rynna_core::Completion::new(rynna_core::Message::assistant(
                text,
            )))
        }
    }
    #[tokio::test]
    async fn desktop_commands_repeat_and_complete_with_the_http_evidence_contract() {
        let dir = tempfile::tempdir().unwrap();
        let mut catalog = rynna_config::ProfileCatalog::from_yaml(
            r#"
version: 1
default_profile: default
providers:
  fake:
    kind: openai-compatible
    api_base: http://localhost:3999/v1
profiles:
  default:
    provider: fake
    model: fake
    subagents:
    - name: reviewer
      description: Verify results
      instructions: Use the requested evidence envelope.
"#,
        )
        .unwrap();
        let mut workflow = rynna_core::workflows::default_workflow();
        workflow.id = "custom".into();
        workflow.steps[2].executor = rynna_core::workflows::Executor::Subagent;
        workflow.steps[2].helper = Some("reviewer".into());
        catalog.save_workflow("default", workflow).unwrap();
        let profile = catalog.resolve("default").unwrap().profile;
        let agent = rynna_core::Agent::new(Arc::new(Provider(AtomicUsize::new(0))), "policy");
        let profiles = rynna_core::AgentProfiles::new("default", [(profile, agent)]).unwrap();
        let host = Arc::new(Host::new(
            Arc::new(tokio::sync::Mutex::new(profiles)),
            Some(Arc::new(tokio::sync::Mutex::new(catalog))),
            dir.path().into(),
        ));
        let app = tauri::test::mock_app();
        app.manage(host);
        let session = uuid::Uuid::new_v4();
        let request:Start=serde_json::from_value(serde_json::json!({"request_id":uuid::Uuid::new_v4(),"session_id":session,"profile":"default","project":null,"selection":{"provider":"fake","model":"fake","thinking":"default"},"workflow_id":"custom","goal":"goal","criteria":[{"id":"criterion","text":"criterion met"}],"limits":{"steps":50,"tool_calls":512,"active_seconds":1800}})).unwrap();
        let first = start_workflow(app.state(), request.clone()).await.unwrap();
        let run = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let run = list_workflow_runs(app.state(), "default".into(), session)
                    .await
                    .unwrap()
                    .pop()
                    .unwrap();
                if run.status == rynna_core::workflow_runs::Status::Completed {
                    break run;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(run.consumed.steps, 5);
        assert_eq!(run.consumed.tool_calls, 0);
        assert_eq!(
            run.events
                .iter()
                .map(|e| e.step_id.as_str())
                .collect::<Vec<_>>(),
            vec!["plan", "execute", "verify", "execute", "verify"]
        );
        assert_eq!(
            run.verification.unwrap().results[0].excerpt,
            "deterministic evidence"
        );
        assert_eq!(
            start_workflow(app.state(), request).await.unwrap().id,
            first.id
        );
    }
}
