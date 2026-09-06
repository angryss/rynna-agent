//! Sequential profile-owned workflow definitions. Runtime revisions are assigned by the catalog.
use crate::Subagent;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const BUILTIN_ID: &str = "rynna-default";
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepRole {
    Work,
    Verify,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Executor {
    Instructions,
    Subagent,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    pub role: StepRole,
    pub executor: Executor,
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helper: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_target: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub revision: u64,
    pub steps: Vec<Step>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowMetadata {
    pub id: String,
    pub name: String,
    pub description: String,
    pub revision: u64,
    pub read_only: bool,
}
impl Workflow {
    pub fn metadata(&self) -> WorkflowMetadata {
        WorkflowMetadata {
            id: self.id.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            revision: self.revision,
            read_only: self.id == BUILTIN_ID,
        }
    }
    pub fn validate(&self, helpers: &[Subagent]) -> Result<(), String> {
        if !valid_id(&self.id)
            || self.name.trim().is_empty()
            || self.name.len() > 256
            || self.description.len() > 1024
            || self.revision == 0
        {
            return Err("workflow requires an ASCII ID (1–64 bytes), name (1–256 bytes), description up to 1024 bytes, and positive revision".into());
        }
        if !(2..=16).contains(&self.steps.len()) {
            return Err("workflow requires 2–16 ordered steps".into());
        }
        let mut ids = BTreeSet::new();
        for (index, step) in self.steps.iter().enumerate() {
            if !valid_id(&step.id) || !ids.insert(&step.id) {
                return Err("step IDs must be valid and unique".into());
            }
            if step.instructions.trim().is_empty() || step.instructions.len() > 32_000 {
                return Err("step instructions require 1–32000 bytes".into());
            }
            match step.executor {
                Executor::Instructions if step.helper.is_some() => {
                    return Err("direct steps cannot reference helpers".into());
                }
                Executor::Subagent
                    if !helpers
                        .iter()
                        .any(|h| Some(&h.name) == step.helper.as_ref()) =>
                {
                    return Err("step references a missing same-profile helper".into());
                }
                _ => {}
            }
            if index + 1 == self.steps.len() {
                if step.role != StepRole::Verify
                    || !self.steps[..index]
                        .iter()
                        .any(|s| Some(&s.id) == step.repeat_target.as_ref())
                {
                    return Err("final step must verify and repeat to an earlier work step".into());
                }
            } else if step.role != StepRole::Work || step.repeat_target.is_some() {
                return Err("only the final verification step may repeat".into());
            }
        }
        Ok(())
    }
}
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
pub fn validate_custom(workflows: &[Workflow], helpers: &[Subagent]) -> Result<(), String> {
    if workflows.len() > 32 {
        return Err("at most 32 custom workflows per profile".into());
    }
    let mut ids = BTreeSet::new();
    for workflow in workflows {
        if workflow.id == BUILTIN_ID || !ids.insert(&workflow.id) {
            return Err("reserved or duplicate workflow ID".into());
        }
        workflow.validate(helpers)?;
    }
    Ok(())
}
pub fn default_workflow() -> Workflow {
    Workflow { id:BUILTIN_ID.into(), name:"Rynna default".into(), description:"Plan, execute, verify, and repeat execution until criteria are met.".into(), revision:1,
        steps:vec![
            Step { id:"plan".into(), role:StepRole::Work, executor:Executor::Instructions, instructions:"Plan concrete work and checks for the goal and every success criterion.".into(), helper:None, repeat_target:None },
            Step { id:"execute".into(), role:StepRole::Work, executor:Executor::Instructions, instructions:"Perform the planned work. Address failed verification criteria using the available permitted tools.".into(), helper:None, repeat_target:None },
            Step { id:"verify".into(), role:StepRole::Verify, executor:Executor::Instructions, instructions:"Check every success criterion using objective checks when possible. Record evidence and label qualitative judgments.".into(), helper:None, repeat_target:Some("execute".into()) }
        ] }
}
