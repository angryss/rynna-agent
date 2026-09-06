//! Shared Axum/Tauri composition and scoped operations.
use async_trait::async_trait;
use rynna_config::ProfileCatalog;
use rynna_core::{Agent, AgentProfiles, workflow_runs::*, workflows::*};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Weak},
};
use tokio::sync::{Mutex, OnceCell};
use uuid::Uuid;

pub struct Host {
    pub profiles: Arc<Mutex<AgentProfiles>>,
    pub catalog: Option<Arc<Mutex<ProfileCatalog>>>,
    path: PathBuf,
    runner: OnceCell<Result<Arc<Runner>, String>>,
    // Serializes admission and destructive configuration changes across both transports.
    pub admission: Mutex<()>,
    sessions: Mutex<BTreeMap<Uuid, Weak<Mutex<()>>>>,
}
impl Host {
    pub fn new(
        profiles: Arc<Mutex<AgentProfiles>>,
        catalog: Option<Arc<Mutex<ProfileCatalog>>>,
        path: PathBuf,
    ) -> Self {
        Self {
            profiles,
            catalog,
            path,
            runner: OnceCell::new(),
            admission: Mutex::new(()),
            sessions: Mutex::new(BTreeMap::new()),
        }
    }
    pub async fn runner(&self) -> Result<Arc<Runner>, String> {
        self.runner
            .get_or_init(|| async {
                let store = Arc::new(crate::FileRunStore::open(&self.path)?);
                Runner::open(
                    store,
                    Arc::new(ProfileExecutor {
                        profiles: self.profiles.clone(),
                    }),
                )
                .await
            })
            .await
            .clone()
    }
    pub async fn definitions(&self, profile: &str) -> Result<Vec<Workflow>, String> {
        if let Some(catalog) = &self.catalog {
            match catalog.lock().await.workflows(profile) {
                Ok(definitions) => return Ok(definitions),
                Err(rynna_config::ConfigError::UnknownProfile(_)) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        if !self.profiles.lock().await.contains(profile) {
            return Err("unknown profile".into());
        }
        Ok(vec![default_workflow()])
    }
    pub async fn save(&self, profile: &str, workflow: Workflow) -> Result<Workflow, String> {
        self.catalog
            .as_ref()
            .ok_or("workflow catalog unavailable")?
            .lock()
            .await
            .save_workflow(profile, workflow)
            .map_err(|e| e.to_string())
    }
    pub async fn delete(&self, profile: &str, id: &str) -> Result<(), String> {
        self.catalog
            .as_ref()
            .ok_or("workflow catalog unavailable")?
            .lock()
            .await
            .delete_workflow(profile, id)
            .map_err(|e| e.to_string())
    }
    pub async fn start(&self, start: Start) -> Result<Run, String> {
        let _guard = self.admission.lock().await;
        let _session = self.session_lock(start.session_id).await;
        let runner = self.runner().await?;
        // Idempotent retries need not re-resolve a definition edited after the accepted start.
        if let Some(run) = runner.retry(&start).await? {
            return Ok(run);
        }
        let workflow = self
            .definitions(&start.profile)
            .await?
            .into_iter()
            .find(|w| w.id == start.workflow_id)
            .ok_or("unknown workflow")?;
        let (agent, metadata) = resolve(&self.profiles, &start).await?;
        let fingerprint = agent.workflow_fingerprint(&metadata)?;
        runner
            .start(start, workflow, metadata.subagents, fingerprint)
            .await
    }
    pub async fn list(&self, profile: &str, session: Uuid) -> Result<Vec<Run>, String> {
        Ok(self.runner().await?.list(profile, session).await)
    }
    pub async fn read(&self, id: Uuid, profile: &str, session: Uuid) -> Result<Run, String> {
        self.runner()
            .await?
            .read(id, profile, session)
            .await
            .ok_or("run unavailable".into())
    }
    pub async fn control(&self, id: Uuid, control: Control) -> Result<Run, String> {
        let _guard = self.admission.lock().await;
        let _session = self.session_lock(control.session_id).await;
        self.runner().await?.control(id, control).await
    }
    pub async fn ensure_profile_idle(&self, profile: &str) -> Result<(), String> {
        if self.runner().await?.profile_busy(profile).await {
            Err(
                "cancel nonterminal workflow runs before changing this profile or its projects"
                    .into(),
            )
        } else {
            Ok(())
        }
    }
    async fn session_lock(&self, session: Uuid) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut sessions = self.sessions.lock().await;
            sessions.retain(|_, value| value.strong_count() > 0);
            let lock = sessions
                .get(&session)
                .and_then(Weak::upgrade)
                .unwrap_or_else(|| Arc::new(Mutex::new(())));
            sessions.insert(session, Arc::downgrade(&lock));
            lock
        };
        lock.lock_owned().await
    }
    pub async fn chat_lease(
        &self,
        session: Option<Uuid>,
    ) -> Result<Option<tokio::sync::OwnedMutexGuard<()>>, String> {
        let lease = if let Some(session) = session {
            Some(self.session_lock(session).await)
        } else {
            None
        };
        self.ensure_chat_idle(session).await?;
        Ok(lease)
    }
    pub async fn ensure_chat_idle(&self, session: Option<Uuid>) -> Result<(), String> {
        if let Some(session) = session {
            // Unavailable workflow storage must not disable ordinary chat.
            if let Some(Ok(runner)) = self.runner.get()
                && runner.session_busy(session).await
            {
                return Err("pause the workflow before sending ordinary chat".into());
            }
        }
        Ok(())
    }
    pub async fn shutdown(&self) {
        if let Some(Ok(runner)) = self.runner.get() {
            runner.pause_all().await;
        }
    }
}
struct ProfileExecutor {
    profiles: Arc<Mutex<AgentProfiles>>,
}
async fn resolve(
    profiles: &Mutex<AgentProfiles>,
    start: &Start,
) -> Result<(Agent, rynna_core::Profile), String> {
    let profiles = profiles
        .lock()
        .await
        .clone()
        .with_project(Some(&start.profile), start.project.as_deref())
        .map_err(|e| e.to_string())?
        .with_model_selection(Some(&start.profile), Some(&start.selection))
        .map_err(|e| e.to_string())?;
    let metadata = profiles
        .profiles()
        .into_iter()
        .find(|p| p.name == start.profile)
        .ok_or("profile unavailable")?;
    Ok((
        profiles
            .clone_agent(&start.profile)
            .ok_or("profile unavailable")?,
        metadata,
    ))
}
#[async_trait]
impl WorkflowExecutor for ProfileExecutor {
    async fn preflight(&self, run: &Run) -> Result<(), String> {
        let (agent, metadata) = resolve(&self.profiles, &run.start).await?;
        if agent.workflow_fingerprint(&metadata)? != run.fingerprint {
            return Err(
                "execution context changed; restore configuration or cancel and start a new run"
                    .into(),
            );
        }
        Ok(())
    }
    async fn execute(&self, run: &Run, allowance: usize) -> Result<ExecutionResult, String> {
        self.preflight(run).await?;
        let (agent, _) = resolve(&self.profiles, &run.start).await?;
        agent.execute_workflow_step(run, allowance).await
    }
}
