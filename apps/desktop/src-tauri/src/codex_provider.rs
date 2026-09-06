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

use crate::{
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
    program: PathBuf,
    codex_home: Option<PathBuf>,
    credential_selection: Option<OpenAiCredentialSelection>,
    model: Option<String>,
}

impl CodexAppServerProvider {
    pub fn new(program: impl Into<PathBuf>, model: Option<String>) -> Self {
        Self {
            thinking: rynna_core::ThinkingLevel::Default,
            program: program.into(),
            codex_home: None,
            credential_selection: None,
            model,
        }
    }

    pub fn with_home(
        program: impl Into<PathBuf>,
        codex_home: impl Into<PathBuf>,
        model: Option<String>,
    ) -> Self {
        Self {
            thinking: rynna_core::ThinkingLevel::Default,
            program: program.into(),
            codex_home: Some(codex_home.into()),
            credential_selection: None,
            model,
        }
    }

    pub(crate) fn with_selectable_home(
        program: impl Into<PathBuf>,
        codex_home: impl Into<PathBuf>,
        credential_selection: OpenAiCredentialSelection,
        model: Option<String>,
    ) -> Self {
        Self {
            thinking: rynna_core::ThinkingLevel::Default,
            program: program.into(),
            codex_home: Some(codex_home.into()),
            credential_selection: Some(credential_selection),
            model,
        }
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
        let mut command = Command::new(&self.program);
        let reuse_existing = self
            .credential_selection
            .as_ref()
            .is_some_and(OpenAiCredentialSelection::reuses_existing);
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
