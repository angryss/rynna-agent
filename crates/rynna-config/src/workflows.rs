//! Atomic profile-owned definition administration.
use crate::{ConfigError, ProfileCatalog};
use rynna_core::workflows::{BUILTIN_ID, Workflow, default_workflow};

fn invalid(message: impl Into<String>) -> ConfigError {
    ConfigError::InvalidWorkflow(message.into())
}
impl ProfileCatalog {
    pub fn workflows(&self, profile: &str) -> Result<Vec<Workflow>, ConfigError> {
        let profile = self
            .profiles
            .get(profile)
            .ok_or_else(|| ConfigError::UnknownProfile(profile.into()))?;
        let mut workflows = vec![default_workflow()];
        workflows.extend(profile.workflows.clone());
        Ok(workflows)
    }
    pub fn save_workflow(
        &mut self,
        profile: &str,
        mut workflow: Workflow,
    ) -> Result<Workflow, ConfigError> {
        if workflow.id == BUILTIN_ID {
            return Err(invalid("duplicate the built-in to customize it"));
        }
        let _lock = self.lock_exclusive()?;
        let mut file = self.fresh_file()?;
        let target = file
            .profiles
            .get_mut(profile)
            .ok_or_else(|| ConfigError::UnknownProfile(profile.into()))?;
        if let Some(existing) = target.workflows.iter_mut().find(|w| w.id == workflow.id) {
            if workflow.revision != existing.revision {
                return Err(invalid("stale workflow revision; refresh before saving"));
            }
            workflow.revision = existing
                .revision
                .checked_add(1)
                .ok_or_else(|| invalid("revision exhausted"))?;
            *existing = workflow.clone();
        } else {
            workflow.revision = 1;
            target.workflows.push(workflow.clone());
        }
        self.apply_file(file)?;
        Ok(workflow)
    }
    pub fn delete_workflow(&mut self, profile: &str, id: &str) -> Result<(), ConfigError> {
        if id == BUILTIN_ID {
            return Err(invalid("built-in workflow is read-only"));
        }
        let _lock = self.lock_exclusive()?;
        let mut file = self.fresh_file()?;
        let target = file
            .profiles
            .get_mut(profile)
            .ok_or_else(|| ConfigError::UnknownProfile(profile.into()))?;
        if !target.workflows.iter().any(|w| w.id == id) {
            return Err(invalid("unknown workflow"));
        }
        target.workflows.retain(|w| w.id != id);
        self.apply_file(file)
    }
}
