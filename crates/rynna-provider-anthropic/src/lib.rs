use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, Url};
use rynna_core::retry::{Backoff, parse_retry_after};
use rynna_core::{
    CacheOptimizer, Completion, CompletionDelta, CompletionRequest, ContextPlan, Message,
    ModelProvider, PrefixCacheOptimizer, ProviderContext, ProviderError, Role, ServerCompaction,
    ToolCall,
};
use rynna_core::{ProviderErrorKind, classify_status, context_overflow_signal};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const COMPACTION_BETA: &str = "compact-2026-01-12";
const MIN_COMPACTION_TOKENS: usize = 50_000;
const COMPACTION_MODEL_PREFIXES: [&str; 8] = [
    "claude-fable-5",
    "claude-mythos-5",
    "claude-opus-4-6",
    "claude-opus-4-7",
    "claude-opus-4-8",
    "claude-opus-5",
    "claude-sonnet-4-6",
    "claude-sonnet-5",
];
const DEFAULT_MAX_TOKENS: u32 = 4096;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const CLAUDE_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_CLAUDE_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_CLAUDE_MESSAGES: usize = 4096;
const MAX_CLAUDE_PROMPT_BYTES: usize = 1024 * 1024;
pub const SUPPORTED_CLAUDE_CODE_VERSION: &str = "2.1.223";
const SUPPORTED_CLAUDE_CODE_VERSION_OUTPUT: &str = "2.1.223 (Claude Code)";

fn supports_compaction_model(model: &str) -> bool {
    COMPACTION_MODEL_PREFIXES.iter().any(|prefix| {
        model == *prefix
            || model
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with('-'))
    })
}

/// Environment settings that can override Claude account authentication,
/// redirect requests, or select a separately billed cloud provider.
pub const CLAUDE_SUBSCRIPTION_CONFLICTING_ENV_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AWS_API_KEY",
    "ANTHROPIC_AWS_BASE_URL",
    "ANTHROPIC_AWS_WORKSPACE_ID",
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_BEDROCK_MANTLE_BASE_URL",
    "AWS_BEARER_TOKEN_BEDROCK",
    "ANTHROPIC_FEDERATION_RULE_ID",
    "ANTHROPIC_ORGANIZATION_ID",
    "ANTHROPIC_WORKSPACE_ID",
    "ANTHROPIC_PROFILE",
    "ANTHROPIC_FOUNDRY_API_KEY",
    "ANTHROPIC_FOUNDRY_AUTH_TOKEN",
    "ANTHROPIC_FOUNDRY_BASE_URL",
    "ANTHROPIC_FOUNDRY_RESOURCE",
    "ANTHROPIC_VERTEX_BASE_URL",
    "ANTHROPIC_VERTEX_PROJECT_ID",
    "ANTHROPIC_CUSTOM_HEADERS",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
];

const CLAUDE_SUBSCRIPTION_ALLOWED_ENV_VARS: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TMPDIR",
    "TMP",
    "TEMP",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "BROWSER",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "https_proxy",
    "http_proxy",
    "all_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
    "DISABLE_TELEMETRY",
    "DISABLE_ERROR_REPORTING",
    "SYSTEMROOT",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PATHEXT",
    "COMSPEC",
];

pub fn claude_subscription_environment() -> Vec<(&'static str, std::ffi::OsString)> {
    CLAUDE_SUBSCRIPTION_ALLOWED_ENV_VARS
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (*name, value)))
        .collect()
}

pub fn isolate_claude_subscription_environment(command: &mut Command) {
    command.env_clear().envs(claude_subscription_environment());
}

pub async fn terminate_child(child: &mut Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[derive(Clone)]
pub struct AnthropicMessagesProvider {
    thinking: rynna_core::ThinkingLevel,
    client: Client,
    messages_url: Url,
    model: String,
    api_key: String,
    cache_optimizer: Arc<dyn CacheOptimizer>,
}

impl AnthropicMessagesProvider {
    pub fn new(
        model: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self, ProviderConfigError> {
        Self::with_base_url(DEFAULT_BASE_URL, model, api_key)
    }

    pub fn with_base_url(
        base_url: impl AsRef<str>,
        model: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self, ProviderConfigError> {
        let model = model.into();
        let api_key = api_key.into();
        if model.trim().is_empty() {
            return Err(ProviderConfigError::BlankModel);
        }
        if api_key.trim().is_empty() {
            return Err(ProviderConfigError::BlankApiKey);
        }
        let messages_url = Url::parse(&format!(
            "{}/v1/messages",
            base_url.as_ref().trim_end_matches('/')
        ))
        .map_err(|e| ProviderConfigError::InvalidBaseUrl(e.to_string()))?;
        if !messages_url.username().is_empty() || messages_url.password().is_some() {
            return Err(ProviderConfigError::EmbeddedCredentials);
        }
        if !matches!(messages_url.scheme(), "http" | "https") {
            return Err(ProviderConfigError::UnsupportedScheme);
        }
        if messages_url.scheme() == "http"
            && !matches!(
                messages_url.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]")
            )
        {
            return Err(ProviderConfigError::InsecureCredentials);
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            messages_url,
            model,
            api_key,
            thinking: rynna_core::ThinkingLevel::Default,
            cache_optimizer: Arc::new(PrefixCacheOptimizer),
        })
    }

    pub fn with_cache_optimizer(mut self, optimizer: Arc<dyn CacheOptimizer>) -> Self {
        self.cache_optimizer = optimizer;
        self
    }

    fn request(
        &self,
        request: CompletionRequest,
        stream: bool,
        compaction_threshold: Option<usize>,
    ) -> Result<serde_json::Value, ProviderError> {
        let cache = self.cache_optimizer.optimize(&request);
        let payload = build_messages_request(
            &self.model,
            request,
            stream,
            cache.use_server_cache,
            compaction_threshold,
        )?;
        let mut payload =
            serde_json::to_value(payload).map_err(|error| ProviderError::new(error.to_string()))?;
        if self.thinking != rynna_core::ThinkingLevel::Default {
            payload["thinking"] = serde_json::json!({"type": "adaptive"});
            payload["output_config"] = serde_json::json!({"effort": self.thinking.as_str()});
        }
        Ok(payload)
    }
}

#[derive(Debug, Error)]
pub enum ProviderConfigError {
    #[error("Anthropic API base URL is invalid: {0}")]
    InvalidBaseUrl(String),
    #[error("Anthropic API base URL must not contain embedded credentials")]
    EmbeddedCredentials,
    #[error("Anthropic API base URL must use HTTP or HTTPS")]
    UnsupportedScheme,
    #[error("Anthropic API credentials require HTTPS except for loopback test endpoints")]
    InsecureCredentials,
    #[error("Anthropic model must not be blank")]
    BlankModel,
    #[error("Anthropic API key must not be blank")]
    BlankApiKey,
    #[error("failed to build Anthropic HTTP client: {0}")]
    Client(#[from] reqwest::Error),
}

#[derive(Serialize)]
struct MessagesRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ApiTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_management: Option<AnthropicContextManagement>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream: bool,
}

#[derive(Serialize)]
struct CacheControl {
    #[serde(rename = "type")]
    kind: &'static str,
}
#[derive(Serialize)]
struct AnthropicContextManagement {
    edits: Vec<AnthropicContextEdit>,
}
#[derive(Serialize)]
struct AnthropicContextEdit {
    #[serde(rename = "type")]
    kind: &'static str,
    trigger: AnthropicContextTrigger,
    instructions: &'static str,
}
#[derive(Serialize)]
struct AnthropicContextTrigger {
    #[serde(rename = "type")]
    kind: &'static str,
    value: usize,
}
#[derive(Serialize)]
struct ApiMessage {
    role: &'static str,
    content: Vec<ContentBlock>,
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
    Compaction {
        content: Option<String>,
    },
}
#[derive(Serialize)]
struct ApiTool {
    name: String,
    description: String,
    input_schema: Value,
}

fn build_messages_request(
    model: &str,
    request: CompletionRequest,
    stream: bool,
    use_server_cache: bool,
    compaction_threshold: Option<usize>,
) -> Result<MessagesRequest, ProviderError> {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    let latest_compaction =
        request
            .messages
            .iter()
            .rposition(|message| match &message.provider_context {
                Some(ProviderContext::AnthropicCompaction(Some(_))) => true,
                Some(ProviderContext::Anthropic(blocks)) => blocks
                    .iter()
                    .any(|block| block["type"] == "compaction" && block["content"].is_string()),
                _ => false,
            });
    for (index, message) in request.messages.into_iter().enumerate() {
        if message.role == Role::System {
            system.push(message.content);
            continue;
        }
        if latest_compaction.is_some_and(|compaction| index < compaction) {
            continue;
        }
        match message.role {
            Role::System => unreachable!("system messages are handled before compaction pruning"),
            Role::User => messages.push(ApiMessage {
                role: "user",
                content: vec![ContentBlock::Text {
                    text: message.content,
                }],
            }),
            Role::Assistant => {
                if let Some(ProviderContext::Anthropic(blocks)) = &message.provider_context {
                    let content = blocks
                        .iter()
                        .cloned()
                        .map(serde_json::from_value)
                        .collect::<Result<Vec<ContentBlock>, _>>()
                        .map_err(|error| {
                            ProviderError::new(format!("invalid Anthropic continuation: {error}"))
                        })?;
                    messages.push(ApiMessage {
                        role: "assistant",
                        content,
                    });
                    continue;
                }
                if message.content.is_empty()
                    && message.tool_calls.is_empty()
                    && message.provider_context.is_none()
                {
                    continue;
                }
                let mut content = Vec::new();
                if let Some(ProviderContext::AnthropicCompaction(summary)) =
                    message.provider_context
                {
                    content.push(ContentBlock::Compaction { content: summary });
                }
                if !message.content.is_empty() {
                    content.push(ContentBlock::Text {
                        text: message.content,
                    });
                }
                content.extend(
                    message
                        .tool_calls
                        .into_iter()
                        .map(|c| ContentBlock::ToolUse {
                            id: c.id,
                            name: c.name,
                            input: c.arguments,
                        }),
                );
                messages.push(ApiMessage {
                    role: "assistant",
                    content,
                });
            }
            Role::Tool => messages.push(ApiMessage {
                role: "user",
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: message.tool_call_id.ok_or_else(|| {
                        ProviderError::new("Anthropic tool result is missing its tool-call id")
                    })?,
                    content: message.content,
                }],
            }),
        }
    }
    Ok(MessagesRequest {
        model: model.to_owned(),
        max_tokens: DEFAULT_MAX_TOKENS,
        cache_control: use_server_cache.then_some(CacheControl { kind: "ephemeral" }),
        system: (!system.is_empty()).then(|| system.join("\n\n")),
        messages,
        tools: request
            .tools
            .into_iter()
            .map(|t| ApiTool {
                name: t.name,
                description: t.description,
                input_schema: t.input_schema,
            })
            .collect(),
        context_management: compaction_threshold.map(|threshold| AnthropicContextManagement {
            edits: vec![AnthropicContextEdit {
                kind: "compact_20260112",
                trigger: AnthropicContextTrigger {
                    kind: "input_tokens",
                    value: threshold.max(MIN_COMPACTION_TOKENS),
                },
                instructions: "Summarize the transcript for continuity. Do not call tools.",
            }],
        }),
        stream,
    })
}

#[derive(Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    content: Vec<ResponseBlock>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseBlock {
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    Compaction {
        #[serde(rename = "content")]
        content: Option<String>,
    },
}

fn completion_from_blocks(blocks: Vec<ResponseBlock>) -> Result<Completion, ProviderError> {
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    let replay = if blocks.iter().any(|block| {
        matches!(
            block,
            ResponseBlock::Thinking { .. } | ResponseBlock::RedactedThinking { .. }
        )
    }) {
        Some(ProviderContext::Anthropic(
            blocks
                .iter()
                .map(serde_json::to_value)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| ProviderError::new(error.to_string()))?,
        ))
    } else {
        None
    };
    let mut provider_context = None;
    for block in blocks {
        match block {
            ResponseBlock::Thinking { .. } | ResponseBlock::RedactedThinking { .. } => {}
            ResponseBlock::Text { text } => content.push_str(&text),
            ResponseBlock::ToolUse { id, name, input } => {
                tool_calls.push(ToolCall::new(id, name, input))
            }
            ResponseBlock::Compaction { content: None } => {
                return Err(ProviderError::new(
                    "Anthropic compaction failed before tool execution",
                ));
            }
            ResponseBlock::Compaction {
                content: Some(content),
            } => {
                provider_context = Some(ProviderContext::AnthropicCompaction(Some(content)));
            }
        }
    }
    Ok(Completion::new(Message {
        role: Role::Assistant,
        content,
        tool_calls,
        tool_call_id: None,
        provider_context: replay.or(provider_context),
    }))
}

fn anthropic_context(request: &CompletionRequest) -> Option<ProviderContext> {
    request.messages.iter().find_map(|message| {
        if let Some(ProviderContext::AnthropicCompaction(summary)) = &message.provider_context {
            Some(ProviderContext::AnthropicCompaction(summary.clone()))
        } else if let Some(ProviderContext::Anthropic(blocks)) = &message.provider_context {
            blocks
                .iter()
                .find(|block| block["type"] == "compaction")
                .map(|block| {
                    ProviderContext::AnthropicCompaction(
                        block["content"].as_str().map(str::to_owned),
                    )
                })
        } else {
            None
        }
    })
}

#[async_trait]
impl ModelProvider for AnthropicMessagesProvider {
    fn workflow_policy(&self) -> String {
        format!(
            "{}:{}:{}",
            self.messages_url,
            self.model,
            self.thinking.as_str()
        )
    }
    fn with_thinking(
        &self,
        level: rynna_core::ThinkingLevel,
    ) -> Result<std::sync::Arc<dyn ModelProvider>, ProviderError> {
        let mut provider = self.clone();
        provider.thinking = level;
        Ok(std::sync::Arc::new(provider))
    }

    fn server_compaction(&self) -> Option<ServerCompaction> {
        supports_compaction_model(&self.model).then_some(ServerCompaction::Anthropic)
    }

    async fn complete_managed(&self, plan: ContextPlan) -> Result<Completion, ProviderError> {
        if self.server_compaction().is_none() {
            return self.complete(plan.request).await;
        }
        if plan.server_compaction_threshold.is_none() && anthropic_context(&plan.request).is_none()
        {
            return self.complete(plan.request).await;
        }
        let response = send_with_retry(
            self.client
                .post(self.messages_url.clone())
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("anthropic-beta", COMPACTION_BETA)
                .json(&self.request(plan.request, false, plan.server_compaction_threshold)?),
        )
        .await?;
        let bytes = checked_body(response, &self.api_key).await?;
        let parsed: MessagesResponse = serde_json::from_slice(&bytes)
            .map_err(|e| ProviderError::new(format!("invalid Anthropic response: {e}")))?;
        completion_from_blocks(parsed.content)
    }

    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        let uses_compaction = anthropic_context(&request).is_some();
        let mut request_builder = self
            .client
            .post(self.messages_url.clone())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&self.request(request, false, None)?);
        if uses_compaction {
            request_builder = request_builder.header("anthropic-beta", COMPACTION_BETA);
        }
        let response = send_with_retry(request_builder).await?;
        let bytes = checked_body(response, &self.api_key).await?;
        let parsed: MessagesResponse = serde_json::from_slice(&bytes)
            .map_err(|e| ProviderError::new(format!("invalid Anthropic response: {e}")))?;
        completion_from_blocks(parsed.content)
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a CompletionDelta) + Send),
    ) -> Result<Completion, ProviderError> {
        let uses_compaction = anthropic_context(&request).is_some();
        let mut request_builder = self
            .client
            .post(self.messages_url.clone())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&self.request(request, true, None)?);
        if uses_compaction {
            request_builder = request_builder.header("anthropic-beta", COMPACTION_BETA);
        }
        let mut response = send_with_retry(request_builder).await?;
        if !response.status().is_success() {
            return Err(http_error(response, &self.api_key).await);
        }
        let mut pending = Vec::new();
        let mut total = 0;
        let mut stopped = false;
        let mut tools: BTreeMap<usize, PendingTool> = BTreeMap::new();
        let mut blocks: BTreeMap<usize, BlockState> = BTreeMap::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| ProviderError::new(format!("failed to read Anthropic stream: {e}")))?
        {
            total += chunk.len();
            if total > MAX_RESPONSE_BYTES {
                return Err(too_large());
            }
            pending.extend_from_slice(&chunk);
            while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
                let mut line: Vec<u8> = pending.drain(..=pos).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if let Some(data) = line.strip_prefix(b"data: ")
                    && process_event(data, &self.api_key, &mut tools, &mut blocks, on_delta)?
                {
                    stopped = true;
                    break;
                }
            }
            if stopped {
                break;
            }
        }
        if !stopped {
            return Err(ProviderError::new(
                "Anthropic stream ended before message_stop",
            ));
        }
        let mut replay = Vec::new();
        for (index, block) in blocks {
            if let Some(mut response) = block.response {
                if let ResponseBlock::ToolUse { input, .. } = &mut response
                    && let Some(tool) = tools.remove(&index)
                {
                    *input = if tool.json.is_empty() {
                        tool.input
                    } else {
                        serde_json::from_str(&tool.json).map_err(|error| {
                            ProviderError::new(format!(
                                "invalid streamed Anthropic tool input: {error}"
                            ))
                        })?
                    };
                }
                replay.push(response);
            }
        }
        completion_from_blocks(replay)
    }
}

#[derive(Default)]
struct PendingTool {
    input: Value,
    json: String,
}
#[derive(Clone, Copy, Eq, PartialEq)]
enum BlockKind {
    Thinking,
    Text,
    Tool,
    Unknown,
}
struct BlockState {
    response: Option<ResponseBlock>,
    kind: BlockKind,
    stopped: bool,
}
#[derive(Deserialize)]
struct StreamEvent {
    #[serde(rename = "type")]
    kind: String,
    index: Option<usize>,
    content_block: Option<StreamBlock>,
    delta: Option<StreamDelta>,
    error: Option<Value>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StreamBlock {
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    Text {
        #[allow(dead_code)]
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(other)]
    Unknown,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StreamDelta {
    ThinkingDelta {
        thinking: String,
    },
    SignatureDelta {
        signature: String,
    },
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    #[serde(other)]
    Unknown,
}
fn process_event(
    data: &[u8],
    api_key: &str,
    tools: &mut BTreeMap<usize, PendingTool>,
    blocks: &mut BTreeMap<usize, BlockState>,
    on_delta: &mut (dyn for<'a> FnMut(&'a CompletionDelta) + Send),
) -> Result<bool, ProviderError> {
    let event: StreamEvent = serde_json::from_slice(data)
        .map_err(|e| ProviderError::new(format!("invalid Anthropic stream event: {e}")))?;
    if matches!(event.content_block, Some(StreamBlock::ToolUse { .. }))
        && event.kind != "content_block_start"
    {
        return Err(ProviderError::new(
            "Anthropic tool start used the wrong event kind",
        ));
    }
    if matches!(event.delta, Some(StreamDelta::InputJsonDelta { .. }))
        && event.kind != "content_block_delta"
    {
        return Err(ProviderError::new(
            "Anthropic tool delta used the wrong event kind",
        ));
    }
    if event.kind == "message_stop" {
        if blocks.values().any(|block| !block.stopped) {
            return Err(ProviderError::new(
                "Anthropic message stopped before tool block stop",
            ));
        }
        return Ok(true);
    }
    if event.kind == "error" {
        let message = event
            .error
            .as_ref()
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("unknown stream error")
            .replace(api_key, "[REDACTED]");
        return Err(ProviderError::new(format!(
            "Anthropic stream failed: {}",
            message.chars().take(512).collect::<String>()
        )));
    }
    if let Some(content_block) = event.content_block {
        let block_kind = match &content_block {
            StreamBlock::Thinking { .. } => BlockKind::Thinking,
            StreamBlock::RedactedThinking { .. } => BlockKind::Unknown,
            StreamBlock::Text { .. } => BlockKind::Text,
            StreamBlock::ToolUse { .. } => BlockKind::Tool,
            StreamBlock::Unknown => BlockKind::Unknown,
        };
        if event.kind != "content_block_start" {
            if block_kind == BlockKind::Tool {
                return Err(ProviderError::new(
                    "Anthropic tool start used the wrong event kind",
                ));
            }
        } else {
            let index = event.index.ok_or_else(|| {
                ProviderError::new(if block_kind == BlockKind::Tool {
                    "Anthropic tool start is missing its index"
                } else {
                    "Anthropic block start is missing its index"
                })
            })?;
            if blocks.contains_key(&index) {
                return Err(ProviderError::new(if block_kind == BlockKind::Tool {
                    "Anthropic stream repeated a tool block start"
                } else {
                    "Anthropic stream repeated a block start"
                }));
            }
            let response = match content_block {
                StreamBlock::Thinking {
                    thinking,
                    signature,
                } => {
                    if !thinking.is_empty() {
                        on_delta(&CompletionDelta::Thinking(thinking.clone()));
                    }
                    Some(ResponseBlock::Thinking {
                        thinking,
                        signature,
                    })
                }
                StreamBlock::RedactedThinking { data } => {
                    Some(ResponseBlock::RedactedThinking { data })
                }
                StreamBlock::Text { text } => {
                    if !text.is_empty() {
                        on_delta(&CompletionDelta::Content(text.clone()));
                    }
                    Some(ResponseBlock::Text { text })
                }
                StreamBlock::ToolUse { id, name, input } => {
                    if id.trim().is_empty() || name.trim().is_empty() {
                        return Err(ProviderError::new(
                            "Anthropic tool block is missing its id or name",
                        ));
                    }
                    tools.insert(
                        index,
                        PendingTool {
                            input: input.clone(),
                            json: String::new(),
                        },
                    );
                    Some(ResponseBlock::ToolUse { id, name, input })
                }
                StreamBlock::Unknown => None,
            };
            blocks.insert(
                index,
                BlockState {
                    response,
                    kind: block_kind,
                    stopped: false,
                },
            );
        }
    } else if event.kind == "content_block_start" {
        return Err(ProviderError::new(
            "Anthropic block start is missing its content block",
        ));
    }
    if event.kind == "content_block_stop" {
        let index = event
            .index
            .ok_or_else(|| ProviderError::new("Anthropic block stop is missing its index"))?;
        let block = blocks
            .get_mut(&index)
            .ok_or_else(|| ProviderError::new("Anthropic block stop arrived before block start"))?;
        if block.stopped {
            return Err(ProviderError::new(if block.kind == BlockKind::Tool {
                "Anthropic stream repeated a tool block stop"
            } else {
                "Anthropic stream repeated a block stop"
            }));
        }
        block.stopped = true;
    }
    if let Some(delta) = event.delta {
        let signature_delta = matches!(delta, StreamDelta::SignatureDelta { .. });
        match delta {
            StreamDelta::ThinkingDelta { thinking }
            | StreamDelta::SignatureDelta {
                signature: thinking,
            } => {
                if event.kind != "content_block_delta" {
                    return Err(ProviderError::new(
                        "Anthropic thinking delta used the wrong event kind",
                    ));
                }
                let index = event.index.ok_or_else(|| {
                    ProviderError::new("Anthropic thinking delta is missing its index")
                })?;
                require_active_block(blocks, index, BlockKind::Thinking, "thinking")?;
                if let Some(ResponseBlock::Thinking {
                    thinking: text,
                    signature,
                }) = &mut blocks.get_mut(&index).expect("validated block").response
                {
                    if signature_delta {
                        signature.push_str(&thinking);
                    } else {
                        text.push_str(&thinking);
                        on_delta(&CompletionDelta::Thinking(thinking));
                    }
                }
            }
            StreamDelta::TextDelta { text } => {
                if event.kind != "content_block_delta" {
                    return Err(ProviderError::new(
                        "Anthropic text delta used the wrong event kind",
                    ));
                }
                let index = event.index.ok_or_else(|| {
                    ProviderError::new("Anthropic text delta is missing its index")
                })?;
                require_active_block(blocks, index, BlockKind::Text, "text")?;
                if let Some(ResponseBlock::Text { text: content }) =
                    &mut blocks.get_mut(&index).expect("validated block").response
                {
                    content.push_str(&text);
                }
                on_delta(&CompletionDelta::Content(text));
            }
            StreamDelta::InputJsonDelta { partial_json } => {
                if event.kind != "content_block_delta" {
                    return Err(ProviderError::new(
                        "Anthropic tool delta used the wrong event kind",
                    ));
                }
                let index = event.index.ok_or_else(|| {
                    ProviderError::new("Anthropic tool delta is missing its index")
                })?;
                require_active_block(blocks, index, BlockKind::Tool, "tool input")?;
                let tool = tools.get_mut(&index).ok_or_else(|| {
                    ProviderError::new("Anthropic tool input arrived before tool start")
                })?;
                tool.json.push_str(&partial_json);
            }
            StreamDelta::Unknown => {}
        }
    }
    Ok(false)
}

fn require_active_block(
    blocks: &BTreeMap<usize, BlockState>,
    index: usize,
    expected: BlockKind,
    label: &str,
) -> Result<(), ProviderError> {
    let block = blocks.get(&index).ok_or_else(|| {
        ProviderError::new(if expected == BlockKind::Tool {
            "Anthropic tool input arrived before tool start".to_owned()
        } else {
            format!("Anthropic {label} arrived before block start")
        })
    })?;
    if block.kind != expected {
        return Err(ProviderError::new(format!(
            "Anthropic {label} arrived for the wrong block type"
        )));
    }
    if block.stopped {
        return Err(ProviderError::new(if expected == BlockKind::Tool {
            "Anthropic tool input arrived after tool block stop".to_owned()
        } else {
            format!("Anthropic {label} arrived after block stop")
        }));
    }
    Ok(())
}

async fn checked_body(
    response: reqwest::Response,
    api_key: &str,
) -> Result<Vec<u8>, ProviderError> {
    if !response.status().is_success() {
        return Err(http_error(response, api_key).await);
    }
    read_limited(response).await
}
async fn http_error(response: reqwest::Response, api_key: &str) -> ProviderError {
    let status = response.status();
    let retry_after = retry_after_of(&response);
    let body = match read_limited(response).await {
        Ok(body) => body,
        Err(error) => return error,
    };
    let text = String::from_utf8_lossy(&body).replace(api_key, "[REDACTED]");
    let error = ProviderError::new(format!(
        "Anthropic returned {status}: {}",
        text.chars().take(512).collect::<String>()
    ))
    .with_status(status.as_u16())
    .with_retry_after(retry_after);
    classify_body(error, &text)
}

/// Upgrades an otherwise-opaque rejection to `ContextOverflow` when the body says so.
fn classify_body(error: ProviderError, body: &str) -> ProviderError {
    if error.kind() == ProviderErrorKind::Permanent && context_overflow_signal(body) {
        return error.with_kind(ProviderErrorKind::ContextOverflow);
    }
    error
}

fn retry_after_of(response: &reqwest::Response) -> Option<std::time::Duration> {
    parse_retry_after(
        response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
    )
}

/// Sends `builder`, retrying only transient failures and only before any response
/// body is read, so a streamed turn is never restarted after deltas have been
/// emitted. Non-retryable responses are returned intact for the caller's existing
/// error path, which keeps the response body in the message.
async fn send_with_retry(
    builder: reqwest::RequestBuilder,
) -> Result<reqwest::Response, ProviderError> {
    let mut backoff = Backoff::new();
    loop {
        // Clone up front: a consumed builder cannot be reissued.
        let Some(attempt) = builder.try_clone() else {
            // Non-cloneable bodies cannot be retried safely.
            return builder.send().await.map_err(request_error);
        };
        let transport_error = match attempt.send().await {
            Ok(response) => {
                let status = response.status();
                if status.is_success()
                    || classify_status(status.as_u16()) != ProviderErrorKind::Transient
                {
                    return Ok(response);
                }
                // Decide while the response is still in hand, so giving up returns
                // this exact response (body included) to the caller's error path
                // rather than spending another request to rediscover it.
                let probe = ProviderError::new(String::new())
                    .with_status(status.as_u16())
                    .with_retry_after(retry_after_of(&response));
                match backoff.next_delay(&probe) {
                    Some(delay) => {
                        drop(response);
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    None => return Ok(response),
                }
            }
            Err(error) => request_error(error),
        };
        let Some(delay) = backoff.next_delay(&transport_error) else {
            return Err(transport_error);
        };
        tokio::time::sleep(delay).await;
    }
}
async fn read_limited(mut response: reqwest::Response) -> Result<Vec<u8>, ProviderError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(request_error)? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
fn too_large() -> ProviderError {
    ProviderError::new(format!(
        "Anthropic response exceeded {MAX_RESPONSE_BYTES}-byte limit"
    ))
}
fn request_error(e: reqwest::Error) -> ProviderError {
    // Connect and timeout faults usually clear on their own; decode faults do not.
    let kind = if e.is_timeout() || e.is_connect() {
        ProviderErrorKind::Transient
    } else {
        ProviderErrorKind::Permanent
    };
    ProviderError::new(format!("Anthropic request failed: {e}")).with_kind(kind)
}

async fn read_version_output<R: AsyncRead + Unpin>(reader: R) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    reader.take(1025).read_to_end(&mut output).await?;
    Ok(output)
}

#[derive(Clone)]
pub struct ClaudeCodeProvider {
    thinking: rynna_core::ThinkingLevel,
    program: PathBuf,
    model: String,
    timeout: Duration,
    environment: Vec<(String, PathBuf)>,
    secret_environment: Vec<(String, String)>,
}
impl ClaudeCodeProvider {
    pub fn new(program: impl Into<PathBuf>, model: impl Into<String>) -> Self {
        Self {
            thinking: rynna_core::ThinkingLevel::Default,
            program: program.into(),
            model: model.into(),
            timeout: CLAUDE_TIMEOUT,
            environment: Vec::new(),
            secret_environment: Vec::new(),
        }
    }
    #[doc(hidden)]
    pub fn with_test_environment(
        mut self,
        name: impl Into<String>,
        value: impl AsRef<Path>,
    ) -> Self {
        self.environment
            .push((name.into(), value.as_ref().to_owned()));
        self
    }
    #[doc(hidden)]
    pub fn with_test_secret_environment(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.secret_environment.push((name.into(), value.into()));
        self
    }
    #[doc(hidden)]
    pub fn with_test_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    fn resolved_program(&self) -> Result<PathBuf, ProviderError> {
        if self.program.is_relative()
            && self
                .program
                .parent()
                .is_some_and(|parent| !parent.as_os_str().is_empty())
        {
            return std::env::current_dir()
                .map(|directory| directory.join(&self.program))
                .map_err(|error| {
                    ProviderError::new(format!(
                        "failed to resolve Claude Code executable path: {error}"
                    ))
                });
        }
        Ok(self.program.clone())
    }
    async fn run(
        &self,
        request: CompletionRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a CompletionDelta) + Send),
    ) -> Result<Completion, ProviderError> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        if !request.tools.is_empty()
            || request
                .messages
                .iter()
                .any(|m| !m.tool_calls.is_empty() || m.role == Role::Tool)
        {
            return Err(ProviderError::new(
                "Claude subscription profiles do not accept Rynna tool calls",
            ));
        }
        let prompt = request
            .messages
            .into_iter()
            .map(|m| {
                format!(
                    "{}: {}",
                    match m.role {
                        Role::System => "System",
                        Role::User => "User",
                        Role::Assistant => "Assistant",
                        Role::Tool => "Tool",
                    },
                    m.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        if prompt.len() > MAX_CLAUDE_PROMPT_BYTES {
            return Err(ProviderError::new(
                "Claude Code prompt exceeded the size limit",
            ));
        }
        let program = self.resolved_program()?;
        let mut version_command = Command::new(&program);
        version_command
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        isolate_claude_subscription_environment(&mut version_command);
        for (k, v) in &self.environment {
            version_command.env(k, v);
        }
        for (k, v) in &self.secret_environment {
            if CLAUDE_SUBSCRIPTION_ALLOWED_ENV_VARS.contains(&k.as_str()) {
                version_command.env(k, v);
            }
        }
        for name in CLAUDE_SUBSCRIPTION_CONFLICTING_ENV_VARS {
            version_command.env_remove(name);
        }
        let (mut version_child, _version_group) =
            rynna_core::process::ProcessGroup::spawn(&mut version_command).map_err(|error| {
                ProviderError::new(format!("failed to check Claude Code version: {error}"))
            })?;
        let version_stdout = match version_child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                terminate_child(&mut version_child).await;
                return Err(ProviderError::new("Claude Code version stdout unavailable"));
            }
        };
        let version_stderr = match version_child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                terminate_child(&mut version_child).await;
                return Err(ProviderError::new("Claude Code version stderr unavailable"));
            }
        };
        let version_deadline = std::cmp::min(
            deadline,
            tokio::time::Instant::now() + Duration::from_secs(30),
        );
        let version = tokio::time::timeout_at(version_deadline, async {
            tokio::try_join!(
                read_version_output(version_stdout),
                read_version_output(version_stderr),
                version_child.wait()
            )
        })
        .await;
        let (version_stdout, version_stderr, version_status) = match version {
            Ok(Ok(version)) => version,
            Ok(Err(error)) => {
                terminate_child(&mut version_child).await;
                return Err(ProviderError::new(format!(
                    "failed to check Claude Code version: {error}"
                )));
            }
            Err(_) => {
                terminate_child(&mut version_child).await;
                return Err(ProviderError::new("Claude Code version check timed out"));
            }
        };
        drop(_version_group);
        if !version_status.success() {
            return Err(ProviderError::new(
                "Claude Code version check did not complete successfully",
            ));
        }
        if version_stdout.len() > 1024 || version_stderr.len() > 1024 {
            return Err(ProviderError::new(
                "Claude Code version output exceeded the size limit",
            ));
        }
        let reported_version = std::str::from_utf8(&version_stdout)
            .ok()
            .map(|version| version.trim_end_matches(['\r', '\n']));
        if reported_version != Some(SUPPORTED_CLAUDE_CODE_VERSION_OUTPUT) {
            return Err(ProviderError::new(format!(
                "unsupported Claude Code version; Rynna requires {SUPPORTED_CLAUDE_CODE_VERSION}"
            )));
        }
        let working_directory = tempfile::tempdir()
            .map_err(|e| ProviderError::new(format!("failed to isolate Claude Code: {e}")))?;
        let mut command = Command::new(&program);
        isolate_claude_subscription_environment(&mut command);
        command
            .args([
                "--print",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--safe-mode",
                "--no-chrome",
                "--tools",
                "",
                "--disallowedTools",
                "mcp__*",
                "--disable-slash-commands",
                "--no-session-persistence",
                "--model",
                &self.model,
            ])
            .current_dir(working_directory.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if self.thinking != rynna_core::ThinkingLevel::Default {
            command.arg("--effort").arg(self.thinking.as_str());
        }
        for (k, v) in &self.environment {
            command.env(k, v);
        }
        for (k, v) in &self.secret_environment {
            if CLAUDE_SUBSCRIPTION_ALLOWED_ENV_VARS.contains(&k.as_str()) {
                command.env(k, v);
            }
        }
        for name in CLAUDE_SUBSCRIPTION_CONFLICTING_ENV_VARS {
            command.env_remove(name);
        }
        let (mut child, _process_group) = rynna_core::process::ProcessGroup::spawn(&mut command)
            .map_err(|e| ProviderError::new(format!("failed to start Claude Code: {e}")))?;
        let mut stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                terminate_child(&mut child).await;
                return Err(ProviderError::new("Claude Code stdin unavailable"));
            }
        };
        match tokio::time::timeout_at(deadline, stdin.write_all(prompt.as_bytes())).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                terminate_child(&mut child).await;
                return Err(ProviderError::new(format!(
                    "failed to write Claude prompt: {error}"
                )));
            }
            Err(_) => {
                terminate_child(&mut child).await;
                return Err(ProviderError::new("Claude Code timed out"));
            }
        }
        drop(stdin);
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                terminate_child(&mut child).await;
                return Err(ProviderError::new("Claude Code stdout unavailable"));
            }
        };
        let mut stdout = BufReader::new(stdout);
        let mut content = String::new();
        let mut output_bytes = 0usize;
        let mut message_count = 0usize;
        let mut successful_result = false;
        loop {
            let line = match read_claude_message(&mut stdout, deadline).await {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    terminate_child(&mut child).await;
                    return Err(error);
                }
            };
            output_bytes = output_bytes.saturating_add(line.len());
            message_count += 1;
            if output_bytes > MAX_RESPONSE_BYTES || message_count > MAX_CLAUDE_MESSAGES {
                terminate_child(&mut child).await;
                return Err(ProviderError::new(
                    "Claude Code output exceeded the size limit",
                ));
            }
            let value: Value = match serde_json::from_slice(&line) {
                Ok(value) => value,
                Err(error) => {
                    terminate_child(&mut child).await;
                    return Err(ProviderError::new(format!(
                        "Claude Code returned invalid stream JSON: {error}"
                    )));
                }
            };
            if value["type"] == "assistant"
                && content.is_empty()
                && let Some(blocks) = value.pointer("/message/content").and_then(Value::as_array)
            {
                for block in blocks {
                    if block["type"] == "text"
                        && let Some(text) = block["text"].as_str()
                    {
                        content.push_str(text);
                        on_delta(&CompletionDelta::Content(text.to_owned()));
                    }
                }
            }
            if value["type"] == "stream_event"
                && value.pointer("/event/delta/type")
                    == Some(&Value::String("text_delta".to_owned()))
                && let Some(text) = value.pointer("/event/delta/text").and_then(Value::as_str)
            {
                content.push_str(text);
                on_delta(&CompletionDelta::Content(text.to_owned()));
            }
            if value["type"] == "result" && value["subtype"] != "success" {
                terminate_child(&mut child).await;
                return Err(ProviderError::new(format!(
                    "Claude Code failed: {}",
                    value["result"].as_str().unwrap_or("unknown error")
                )));
            }
            if value["type"] == "result" && value["subtype"] == "success" {
                successful_result = true;
            }
        }
        let status = match tokio::time::timeout_at(deadline, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => {
                terminate_child(&mut child).await;
                return Err(ProviderError::new(format!(
                    "failed to wait for Claude Code: {error}"
                )));
            }
            Err(_) => {
                terminate_child(&mut child).await;
                return Err(ProviderError::new("Claude Code timed out"));
            }
        };
        if !status.success() {
            return Err(ProviderError::new(
                "Claude Code did not complete successfully",
            ));
        }
        if !successful_result {
            return Err(ProviderError::new(
                "Claude Code stream ended without a successful result event",
            ));
        }
        if content.is_empty() {
            return Err(ProviderError::new("Claude Code returned an empty response"));
        }
        Ok(Completion::new(Message::assistant(content)))
    }
}

async fn read_claude_message<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    deadline: tokio::time::Instant,
) -> Result<Option<Vec<u8>>, ProviderError> {
    let mut line = Vec::new();
    loop {
        let available = tokio::time::timeout_at(deadline, reader.fill_buf())
            .await
            .map_err(|_| ProviderError::new("Claude Code timed out"))?
            .map_err(|error| {
                ProviderError::new(format!("failed to read Claude Code output: {error}"))
            })?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |position| position + 1);
        if line.len().saturating_add(take) > MAX_CLAUDE_MESSAGE_BYTES {
            return Err(ProviderError::new(
                "Claude Code message exceeded the size limit",
            ));
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
    }
}
#[async_trait]
impl ModelProvider for ClaudeCodeProvider {
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
        on_delta: &mut (dyn for<'a> FnMut(&'a CompletionDelta) + Send),
    ) -> Result<Completion, ProviderError> {
        self.run(request, on_delta).await
    }
}
