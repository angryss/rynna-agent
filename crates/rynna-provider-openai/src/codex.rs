use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;
use rynna_core::{
    Completion, CompletionDelta, CompletionRequest, Message, ModelProvider, ProviderError, Role,
};
use tokio::io::BufReader;
use tokio::process::Command;
use tokio::time::{Duration, Instant};

use super::models::{ProviderModel, context_window};

use super::codex_protocol::{
    OpenAiCredentialSelection, read_codex_message, read_codex_response, secure_codex_home,
    write_codex_message,
};

const CODEX_OPERATION_TIMEOUT: Duration = Duration::from_secs(180);
const SUPPORTED_CODEX_VERSION: &str = "codex-cli 0.149.1";
const MAX_CODEX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_CODEX_TURN_MESSAGES: usize = 4096;

#[derive(Clone)]
pub struct CodexAppServerProvider {
    thinking: rynna_core::ThinkingLevel,
    context_window: Option<usize>,
    program: PathBuf,
    codex_home: Option<PathBuf>,
    credential_selection: Option<OpenAiCredentialSelection>,
    model: Option<String>,
    profile_settings: Option<(PathBuf, String)>,
}

impl CodexAppServerProvider {
    pub fn new(program: impl Into<PathBuf>, model: Option<String>) -> Self {
        Self {
            thinking: rynna_core::ThinkingLevel::Default,
            context_window: None,
            program: program.into(),
            codex_home: None,
            credential_selection: None,
            model,
            profile_settings: None,
        }
    }

    pub fn with_home(
        program: impl Into<PathBuf>,
        codex_home: impl Into<PathBuf>,
        model: Option<String>,
    ) -> Self {
        Self {
            thinking: rynna_core::ThinkingLevel::Default,
            context_window: None,
            program: program.into(),
            codex_home: Some(codex_home.into()),
            credential_selection: None,
            model,
            profile_settings: None,
        }
    }

    pub fn with_selectable_home(
        program: impl Into<PathBuf>,
        codex_home: impl Into<PathBuf>,
        credential_selection: OpenAiCredentialSelection,
        model: Option<String>,
    ) -> Self {
        Self {
            thinking: rynna_core::ThinkingLevel::Default,
            context_window: None,
            program: program.into(),
            codex_home: Some(codex_home.into()),
            credential_selection: Some(credential_selection),
            model,
            profile_settings: None,
        }
    }

    pub fn for_profile(settings_path: PathBuf, profile: &str, model: &str) -> Result<Self, String> {
        let program = std::env::var_os("RYNNA_CODEX_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| "codex".into());
        let home = std::env::var_os("RYNNA_CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::config_dir().map(|path| path.join("rynna").join("codex")))
            .ok_or("Rynna could not determine its configuration directory")?;
        let mut provider = Self::with_home(
            program,
            home,
            (model != rynna_config::OPENAI_ACCOUNT_MODEL).then(|| model.to_owned()),
        );
        provider.profile_settings = Some((settings_path, profile.to_owned()));
        Ok(provider)
    }

    pub fn with_context_window(mut self, context_window: Option<usize>) -> Self {
        self.context_window = context_window;
        self
    }

    fn account_command(&self) -> Result<Command, ProviderError> {
        let mut command = Command::new(&self.program);
        let reuse_existing = if let Some((path, profile)) = &self.profile_settings {
            let settings =
                rynna_config::ProviderSettingsStore::load(path).map_err(provider_error)?;
            match settings.get(profile, "openai") {
                Some(rynna_config::ConfiguredProvider::OpenAi { reuse_existing, .. }) => {
                    *reuse_existing
                }
                _ => {
                    return Err(ProviderError::new(
                        "Connect OpenAI in this profile's Provider Credentials settings before using account models",
                    ));
                }
            }
        } else {
            self.credential_selection
                .as_ref()
                .is_some_and(OpenAiCredentialSelection::reuses_existing)
        };
        if reuse_existing {
            command
                .env_remove("CODEX_HOME")
                .env_remove("RYNNA_CODEX_HOME");
        } else if let Some(codex_home) = &self.codex_home {
            command.env(
                "CODEX_HOME",
                secure_codex_home(codex_home.clone()).map_err(ProviderError::new)?,
            );
        } else {
            command
                .env_remove("CODEX_HOME")
                .env_remove("RYNNA_CODEX_HOME");
        }
        Ok(command)
    }

    /// Read-only discovery: no thread or turn is started, so the inference version pin does not apply.
    pub async fn list_models(&self) -> Result<Vec<ProviderModel>, ProviderError> {
        let deadline = Instant::now() + Duration::from_secs(15);
        let workspace = tempfile::tempdir().map_err(provider_error)?;
        let mut command = self.account_command()?;
        let cache_home = command
            .as_std()
            .get_envs()
            .find(|(key, _)| *key == "CODEX_HOME")
            .and_then(|(_, value)| value.map(PathBuf::from))
            .or_else(|| dirs::home_dir().map(|home| home.join(".codex")));
        command
            .arg("app-server")
            .current_dir(workspace.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let (mut child, _group) =
            rynna_core::process::ProcessGroup::spawn(&mut command).map_err(provider_error)?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| ProviderError::new("Codex stdin unavailable"))?;
        let mut stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| ProviderError::new("Codex stdout unavailable"))?,
        );
        write_codex_message(
            &mut stdin,
            &serde_json::json!({"method":"initialize","id":1,"params":{
                "clientInfo":{"name":"rynna","version":env!("CARGO_PKG_VERSION")}
            }}),
            deadline,
        )
        .await
        .map_err(ProviderError::new)?;
        read_codex_response(&mut stdout, 1, deadline)
            .await
            .map_err(ProviderError::new)?;
        write_codex_message(
            &mut stdin,
            &serde_json::json!({"method":"initialized","params":{}}),
            deadline,
        )
        .await
        .map_err(ProviderError::new)?;
        let mut models = Vec::new();
        let mut recommended_model: Option<String> = None;
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();
        for id in 2..22 {
            write_codex_message(
                &mut stdin,
                &serde_json::json!({"method":"model/list","id":id,
                "params":{"limit":100,"includeHidden":false,"cursor":cursor}}),
                deadline,
            )
            .await
            .map_err(ProviderError::new)?;
            let response = read_codex_response(&mut stdout, id, deadline)
                .await
                .map_err(ProviderError::new)?;
            let data = response
                .pointer("/result/data")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| ProviderError::new("Codex returned invalid model data"))?;
            for entry in data {
                if entry.get("hidden").and_then(serde_json::Value::as_bool) == Some(true) {
                    continue;
                }
                let model = entry
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(|| ProviderError::new("Codex omitted a model ID"))?;
                if entry.get("isDefault").and_then(serde_json::Value::as_bool) == Some(true) {
                    recommended_model = Some(model.to_owned());
                }
                models.push(ProviderModel {
                    id: model.to_owned(),
                    context_window: None,
                });
            }
            cursor = response
                .pointer("/result/nextCursor")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let Some(next) = &cursor else {
                models.sort_by(|a, b| a.id.cmp(&b.id));
                models.dedup_by(|a, b| a.id == b.id);
                if let Some(home) = cache_home {
                    enrich_context_windows(&mut models, &home).await;
                }
                // Resolve the sentinel against the same layered config used by a new session.
                let config_deadline = deadline.min(Instant::now() + Duration::from_secs(2));
                if write_codex_message(
                    &mut stdin,
                    &serde_json::json!({
                        "method":"config/read", "id":22, "params":{"includeLayers":false}
                    }),
                    config_deadline,
                )
                .await
                .is_ok()
                    && let Ok(config) = read_codex_response(&mut stdout, 22, config_deadline).await
                {
                    let default_model = config
                        .pointer("/result/config/model")
                        .and_then(serde_json::Value::as_str)
                        .or(recommended_model.as_deref());
                    if let Some(window) = default_model
                        .and_then(|id| models.iter().find(|model| model.id == id))
                        .and_then(|model| model.context_window)
                    {
                        models.push(ProviderModel {
                            id: rynna_config::OPENAI_ACCOUNT_MODEL.into(),
                            context_window: Some(window),
                        });
                    }
                }
                models.sort_by(|a, b| a.id.cmp(&b.id));
                return Ok(models);
            };
            if !seen.insert(next.clone()) {
                break;
            }
        }
        Err(ProviderError::new(
            "Codex model listing exceeded its page limit",
        ))
    }

    async fn run(
        &self,
        request: CompletionRequest,
        on_delta: &mut (dyn for<'delta> FnMut(&'delta CompletionDelta) + Send),
    ) -> Result<Completion, ProviderError> {
        let deadline = Instant::now() + CODEX_OPERATION_TIMEOUT;
        verify_codex_version(&self.program, deadline).await?;
        if !request.tools.is_empty()
            || request
                .messages
                .iter()
                .any(|message| !message.tool_calls.is_empty() || message.tool_call_id.is_some())
        {
            return Err(ProviderError::new(
                "Codex account profiles do not accept Rynna tool calls",
            ));
        }

        let system_prompt = request
            .messages
            .iter()
            .find(|message| message.role == Role::System)
            .map(|message| message.content.as_str())
            .unwrap_or("You are Rynna, a careful AI software agent.");
        let last_user = request
            .messages
            .iter()
            .rposition(|message| message.role == Role::User)
            .ok_or_else(|| ProviderError::new("Codex request has no user message"))?;
        let prompt = request.messages[last_user].content.clone();
        let history_items = request.messages[..last_user]
            .iter()
            .filter_map(history_item)
            .collect::<Vec<_>>();
        let workspace = tempfile::tempdir().map_err(provider_error)?;
        let mut command = self.account_command()?;
        command
            .arg("app-server")
            .current_dir(workspace.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let (mut child, _process_group) =
            rynna_core::process::ProcessGroup::spawn(&mut command).map_err(provider_error)?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| ProviderError::new("Codex app-server stdin is unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProviderError::new("Codex app-server stdout is unavailable"))?;
        let mut stdout = BufReader::new(stdout);

        write_codex_message(
            &mut stdin,
            &serde_json::json!({
                "method": "initialize",
                "id": 1,
                "params": {
                    "clientInfo": {"name": "rynna", "title": "Rynna", "version": env!("CARGO_PKG_VERSION")},
                    "capabilities": {"experimentalApi": true}
                }
            }),
            deadline,
        )
        .await
        .map_err(ProviderError::new)?;
        read_codex_response(&mut stdout, 1, deadline)
            .await
            .map_err(ProviderError::new)?;
        write_codex_message(
            &mut stdin,
            &serde_json::json!({"method": "initialized", "params": {}}),
            deadline,
        )
        .await
        .map_err(ProviderError::new)?;

        let mut thread_params = serde_json::json!({
            "cwd": workspace.path(),
            "environments": [],
            "approvalPolicy": "never",
            "sandbox": "read-only",
            "config": {
                "features": {"shell_tool": false, "view_image": false},
                "tools": {"update_plan": {"enabled": false}},
                "web_search": "disabled"
            },
            "ephemeral": true,
            "baseInstructions": format!(
                "{system_prompt}\n\nDo not run commands, inspect files, or use tools. Answer only from the supplied conversation."
            ),
            "serviceName": "rynna"
        });
        if let Some(window) = self.context_window {
            thread_params["config"]["model_context_window"] = serde_json::json!(window);
        }
        if let Some(model) = &self.model {
            thread_params["model"] = serde_json::Value::String(model.clone());
        }
        write_codex_message(
            &mut stdin,
            &serde_json::json!({"method": "thread/start", "id": 2, "params": thread_params}),
            deadline,
        )
        .await
        .map_err(ProviderError::new)?;
        let thread = read_codex_response(&mut stdout, 2, deadline)
            .await
            .map_err(ProviderError::new)?;
        let thread_id = thread
            .pointer("/result/thread/id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ProviderError::new("Codex app-server omitted the thread id"))?;

        let mut next_id = 3;
        if !history_items.is_empty() {
            write_codex_message(
                &mut stdin,
                &serde_json::json!({
                    "method": "thread/inject_items",
                    "id": next_id,
                    "params": {"threadId": thread_id, "items": history_items}
                }),
                deadline,
            )
            .await
            .map_err(ProviderError::new)?;
            read_codex_response(&mut stdout, next_id, deadline)
                .await
                .map_err(ProviderError::new)?;
            next_id += 1;
        }
        let mut turn_params =
            serde_json::json!({"threadId": thread_id, "input": [{"type": "text", "text": prompt}]});
        if self.thinking != rynna_core::ThinkingLevel::Default {
            turn_params["effort"] = serde_json::json!(self.thinking.as_str());
        }
        write_codex_message(
            &mut stdin,
            &serde_json::json!({
                "method": "turn/start",
                "id": next_id,
                "params": turn_params
            }),
            deadline,
        )
        .await
        .map_err(ProviderError::new)?;
        let turn = read_codex_response(&mut stdout, next_id, deadline)
            .await
            .map_err(ProviderError::new)?;
        let turn_id = turn
            .pointer("/result/turn/id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ProviderError::new("Codex app-server omitted the turn id"))?;

        let mut content = String::new();
        let mut reasoning_bytes = 0;
        let mut message_count = 0;
        let mut agent_item_ids = HashSet::new();
        loop {
            let message = read_codex_message(&mut stdout, deadline)
                .await
                .map_err(ProviderError::new)?;
            count_turn_message(&mut message_count)?;
            match message.get("method").and_then(serde_json::Value::as_str) {
                Some("item/started") if message_matches_turn(&message, thread_id, turn_id) => {
                    let item = message
                        .pointer("/params/item")
                        .ok_or_else(|| ProviderError::new("Codex omitted a started item"))?;
                    if let Some(item_id) = started_agent_item_id(item)? {
                        agent_item_ids.insert(item_id.to_owned());
                    }
                }
                Some("item/agentMessage/delta") => {
                    if !message_matches_turn(&message, thread_id, turn_id) {
                        continue;
                    }
                    let Some(item_id) = message
                        .pointer("/params/itemId")
                        .and_then(serde_json::Value::as_str)
                    else {
                        continue;
                    };
                    if !agent_item_ids.contains(item_id) {
                        continue;
                    }
                    if let Some(delta) = message
                        .pointer("/params/delta")
                        .and_then(serde_json::Value::as_str)
                    {
                        append_content(&mut content, delta)?;
                        on_delta(&CompletionDelta::Content(delta.to_owned()));
                    }
                }
                Some("item/reasoning/summaryTextDelta") => {
                    if !message_matches_turn(&message, thread_id, turn_id) {
                        continue;
                    }
                    if let Some(delta) = message
                        .pointer("/params/delta")
                        .and_then(serde_json::Value::as_str)
                    {
                        append_reasoning(&mut reasoning_bytes, delta)?;
                        on_delta(&CompletionDelta::Thinking(delta.to_owned()));
                    }
                }
                Some("turn/completed") => {
                    if message
                        .pointer("/params/threadId")
                        .and_then(serde_json::Value::as_str)
                        != Some(thread_id)
                        || message
                            .pointer("/params/turn/id")
                            .and_then(serde_json::Value::as_str)
                            != Some(turn_id)
                    {
                        continue;
                    }
                    let status = message
                        .pointer("/params/turn/status")
                        .and_then(serde_json::Value::as_str);
                    if status != Some("completed") {
                        let error = message
                            .pointer("/params/turn/error/message")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("Codex turn did not complete");
                        return Err(ProviderError::new(sanitize_error(error)));
                    }
                    break;
                }
                _ => {}
            }
        }
        if content.is_empty() {
            return Err(ProviderError::new("Codex returned an empty response"));
        }
        Ok(Completion::new(Message::assistant(content)))
    }
}

async fn verify_codex_version(
    program: &std::path::Path,
    deadline: Instant,
) -> Result<(), ProviderError> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| ProviderError::new("Codex version check timed out"))?;
    let output = tokio::time::timeout(
        remaining.min(Duration::from_secs(5)),
        Command::new(program)
            .arg("--version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| ProviderError::new("Codex version check timed out"))?
    .map_err(provider_error)?;
    if !output.status.success()
        || output.stdout.len() > 128
        || String::from_utf8_lossy(&output.stdout).trim() != SUPPORTED_CODEX_VERSION
    {
        return Err(ProviderError::new(format!(
            "unsupported Codex CLI version; Rynna requires {SUPPORTED_CODEX_VERSION}"
        )));
    }
    Ok(())
}

#[async_trait]
impl ModelProvider for CodexAppServerProvider {
    fn with_thinking(
        &self,
        level: rynna_core::ThinkingLevel,
    ) -> Result<std::sync::Arc<dyn ModelProvider>, ProviderError> {
        let mut provider = self.clone();
        provider.thinking = level;
        Ok(std::sync::Arc::new(provider))
    }

    fn supports_external_tools(&self) -> bool {
        false
    }

    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        self.run(request, &mut |_| {}).await
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
        on_delta: &mut (dyn for<'delta> FnMut(&'delta CompletionDelta) + Send),
    ) -> Result<Completion, ProviderError> {
        self.run(request, on_delta).await
    }
}

fn history_item(message: &Message) -> Option<serde_json::Value> {
    match message.role {
        Role::User => Some(serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": message.content}]
        })),
        Role::Assistant => Some(serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": message.content}]
        })),
        _ => None,
    }
}

fn message_matches_turn(message: &serde_json::Value, thread_id: &str, turn_id: &str) -> bool {
    message
        .pointer("/params/threadId")
        .and_then(serde_json::Value::as_str)
        == Some(thread_id)
        && message
            .pointer("/params/turnId")
            .and_then(serde_json::Value::as_str)
            == Some(turn_id)
}

fn started_agent_item_id(item: &serde_json::Value) -> Result<Option<&str>, ProviderError> {
    match item.get("type").and_then(serde_json::Value::as_str) {
        Some("agentMessage") => item
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(Some)
            .ok_or_else(|| ProviderError::new("Codex omitted the agent message item id")),
        Some("reasoning" | "userMessage") => Ok(None),
        _ => Err(ProviderError::new(
            "Codex attempted to start a disabled tool",
        )),
    }
}

fn append_content(content: &mut String, delta: &str) -> Result<(), ProviderError> {
    if content.len().saturating_add(delta.len()) > MAX_CODEX_RESPONSE_BYTES {
        return Err(ProviderError::new(
            "Codex response exceeded the aggregate size limit",
        ));
    }
    content.push_str(delta);
    Ok(())
}

fn append_reasoning(bytes: &mut usize, delta: &str) -> Result<(), ProviderError> {
    if bytes.saturating_add(delta.len()) > MAX_CODEX_RESPONSE_BYTES {
        return Err(ProviderError::new(
            "Codex reasoning exceeded the aggregate size limit",
        ));
    }
    *bytes += delta.len();
    Ok(())
}

fn count_turn_message(count: &mut usize) -> Result<(), ProviderError> {
    if *count >= MAX_CODEX_TURN_MESSAGES {
        return Err(ProviderError::new("Codex turn exceeded the message limit"));
    }
    *count += 1;
    Ok(())
}

fn provider_error(error: impl std::fmt::Display) -> ProviderError {
    ProviderError::new(format!("Codex app-server failed: {error}"))
}

fn sanitize_error(error: &str) -> String {
    error
        .chars()
        .filter(|character| !character.is_control())
        .take(512)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_CODEX_RESPONSE_BYTES, MAX_CODEX_TURN_MESSAGES, append_content, append_reasoning,
        count_turn_message, started_agent_item_id,
    };

    #[test]
    fn codex_response_content_has_an_aggregate_size_limit() {
        let mut content = "x".repeat(MAX_CODEX_RESPONSE_BYTES);

        let error = append_content(&mut content, "x").unwrap_err();

        assert_eq!(
            error.to_string(),
            "model provider failed: Codex response exceeded the aggregate size limit"
        );
    }

    #[test]
    fn codex_turn_has_an_aggregate_message_limit() {
        let mut count = MAX_CODEX_TURN_MESSAGES;

        let error = count_turn_message(&mut count).unwrap_err();

        assert_eq!(
            error.to_string(),
            "model provider failed: Codex turn exceeded the message limit"
        );
    }

    #[test]
    fn codex_reasoning_has_an_aggregate_size_limit() {
        let mut bytes = MAX_CODEX_RESPONSE_BYTES;

        let error = append_reasoning(&mut bytes, "x").unwrap_err();

        assert_eq!(
            error.to_string(),
            "model provider failed: Codex reasoning exceeded the aggregate size limit"
        );
    }

    #[test]
    fn codex_tool_lifecycle_items_are_rejected() {
        let item = serde_json::json!({"type": "commandExecution", "id": "tool-1"});

        let error = started_agent_item_id(&item).unwrap_err();

        assert_eq!(
            error.to_string(),
            "model provider failed: Codex attempted to start a disabled tool"
        );
    }
}

// model/list omits token limits. Codex refreshes this metadata in the selected home.
// The detected maximum is saved and passed as model_context_window when starting a session.
async fn enrich_context_windows(models: &mut [ProviderModel], home: &std::path::Path) {
    use tokio::io::AsyncReadExt;
    let Ok(file) = tokio::fs::File::open(home.join("models_cache.json")).await else {
        return;
    };
    let mut bytes = Vec::new();
    if file
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .is_err()
        || bytes.len() > 4 * 1024 * 1024
    {
        return;
    }
    let Ok(cache) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return;
    };
    let Some(entries) = cache.get("models").and_then(serde_json::Value::as_array) else {
        return;
    };
    for model in models {
        model.context_window = entries
            .iter()
            .find(|entry| entry.get("slug").and_then(serde_json::Value::as_str) == Some(&model.id))
            .and_then(|entry| {
                entry
                    .get("max_context_window")
                    .and_then(context_window)
                    .or_else(|| entry.get("context_window").and_then(context_window))
            });
    }
}

#[cfg(test)]
mod context_metadata_tests {
    use super::*;

    #[tokio::test]
    async fn context_metadata_matches_exact_model_and_prefers_supported_maximum() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("models_cache.json"),
            serde_json::json!({"models": [
                {"slug":"known", "context_window":128000, "max_context_window":1000000},
                {"slug":"invalid", "context_window":-1},
                {"slug":"only-maximum", "max_context_window":1000000}
            ]})
            .to_string(),
        )
        .unwrap();
        let mut models = ["known", "unknown", "invalid", "only-maximum"].map(|id| ProviderModel {
            id: id.into(),
            context_window: None,
        });
        enrich_context_windows(&mut models, home.path()).await;
        assert_eq!(models[0].context_window, Some(1000000));
        assert!(
            models[1..3]
                .iter()
                .all(|model| model.context_window.is_none())
        );
        std::fs::write(home.path().join("models_cache.json"), "invalid json").unwrap();
        enrich_context_windows(&mut models, home.path()).await;
        assert_eq!(models[0].context_window, Some(1000000));
    }
}
