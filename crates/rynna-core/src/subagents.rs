//! Profile-owned, single-level task delegation using the parent's execution authority.
use super::*;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Subagent {
    pub name: String,
    pub description: String,
    pub instructions: String,
}

pub fn validate(subagents: &[Subagent]) -> Result<(), ProfileError> {
    let mut names = std::collections::BTreeSet::new();
    if subagents.len() > 32 {
        return Err(ProfileError::InvalidSubagents(
            "at most 32 helpers per profile".into(),
        ));
    }
    for helper in subagents {
        if helper.name.is_empty()
            || helper.name.len() > 64
            || !helper
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        {
            return Err(ProfileError::InvalidSubagents(
                "names must contain 1–64 letters, digits, underscores or hyphens".into(),
            ));
        }
        if !names.insert(&helper.name) {
            return Err(ProfileError::InvalidSubagents(format!(
                "duplicate name `{}`",
                helper.name
            )));
        }
        if helper.description.trim().is_empty()
            || helper.description.len() > 1024
            || helper.instructions.trim().is_empty()
            || helper.instructions.len() > 32_000
        {
            return Err(ProfileError::InvalidSubagents(
                "description and instructions are required (maximum 1024 and 32000 bytes)".into(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn delegation_tool(
    parent: &Agent,
    tools: &BTreeMap<String, Arc<dyn Tool>>,
    tool_budget: Arc<ToolBudget>,
) -> Arc<dyn Tool> {
    // Snapshot the selected provider, policy/project/skills, and already-discovered tools.
    // Constructing a fresh Agent leaves history, memory and further delegation disabled.
    let mut child = Agent::new(parent.provider.clone(), parent.system_prompt.clone());
    child.context_manager = parent.context_manager.clone();
    child.tools = Arc::new(tools.clone());
    child.tool_budget = Some(tool_budget);
    Arc::new(Delegate {
        child,
        helpers: parent.subagents.clone(),
    })
}

struct Delegate {
    child: Agent,
    helpers: Arc<Vec<Subagent>>,
}

#[async_trait]
impl Tool for Delegate {
    fn definition(&self) -> ToolDefinition {
        let helpers = self
            .helpers
            .iter()
            .map(|helper| format!("{}: {}", helper.name, helper.description))
            .collect::<Vec<_>>()
            .join("\n");
        ToolDefinition::new(
            "delegate_task",
            format!(
                "Delegate a self-contained task to a helper from this profile. Provide all relevant context in task; helpers do not see this conversation. Each call waits for its result. Helpers share your model, project and permitted tools, and cannot delegate.\nAvailable helpers:\n{helpers}"
            ),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "subagent": {"type": "string", "enum": self.helpers.iter().map(|helper| &helper.name).collect::<Vec<_>>()},
                    "task": {"type": "string", "minLength": 1, "maxLength": 32000}
                },
                "required": ["subagent", "task"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Arguments {
            subagent: String,
            task: String,
        }
        let args: Arguments = serde_json::from_value(arguments)
            .map_err(|_| ToolError::new("expected subagent and task strings"))?;
        if args.task.trim().is_empty() || args.task.len() > 32_000 {
            return Err(ToolError::new(
                "task must contain 1–32000 bytes of non-blank text",
            ));
        }
        let helper = self
            .helpers
            .iter()
            .find(|helper| helper.name == args.subagent)
            .ok_or_else(|| ToolError::new("unknown subagent in this profile"))?;
        let mut child = self.child.clone();
        child.system_prompt = format!("{}\n\nSubagent role: {}\n{}\nComplete the delegated task and return your findings to the parent agent.", child.system_prompt, helper.name, helper.instructions).into();
        // The parent's aggregate tool deadline also covers the entire child model/tool loop.
        let reply = child
            .respond(&[], &args.task)
            .await
            .map_err(|error| ToolError::new(error.to_string()))?;
        Ok(serde_json::json!({"subagent": helper.name, "result": reply.content}))
    }
}
