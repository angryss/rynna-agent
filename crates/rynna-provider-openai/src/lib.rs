use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, Url};
use rynna_core::retry::{Backoff, parse_retry_after};
use rynna_core::{
    CacheOptimizer, Completion, CompletionDelta, CompletionRequest, ContextPlan, Message,
    ModelProvider, PrefixCacheOptimizer, ProviderContext, ProviderError, Role, ServerCompaction,
    ToolCall, ToolDefinition,
};
use rynna_core::{ProviderErrorKind, classify_status, context_overflow_signal};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_ERROR_BODY_CHARS: usize = 512;
// Non-streaming chat completions should fit comfortably in 1 MiB while bounding memory use.
const MAX_RESPONSE_BODY_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct OpenAiCompatibleProvider {
    thinking: rynna_core::ThinkingLevel,
    client: Client,
    completion_url: Url,
    responses_url: Url,
    model: String,
    api_key: Option<String>,
    cache_technology: CacheTechnology,
    cache_optimizer: Arc<dyn CacheOptimizer>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CacheTechnology {
    OpenAi,
    Ollama,
    Generic,
}

impl OpenAiCompatibleProvider {
    pub fn new(
        base_url: impl AsRef<str>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Result<Self, ProviderConfigError> {
        let base_url = base_url.as_ref();
        let cache_technology = cache_technology_for(base_url);
        Self::with_cache_technology(base_url, model, api_key, cache_technology)
    }

    pub fn new_openai(
        base_url: impl AsRef<str>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Result<Self, ProviderConfigError> {
        Self::with_cache_technology(base_url, model, api_key, CacheTechnology::OpenAi)
    }

    pub fn new_ollama(
        base_url: impl AsRef<str>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Result<Self, ProviderConfigError> {
        Self::with_cache_technology(base_url, model, api_key, CacheTechnology::Ollama)
    }

    fn with_cache_technology(
        base_url: impl AsRef<str>,
        model: impl Into<String>,
        api_key: Option<String>,
        cache_technology: CacheTechnology,
    ) -> Result<Self, ProviderConfigError> {
        let model = model.into();
        if model.trim().is_empty() {
            return Err(ProviderConfigError::BlankModel);
        }

        let endpoint = format!(
            "{}/chat/completions",
            base_url.as_ref().trim_end_matches('/')
        );
        let completion_url = Url::parse(&endpoint)
            .map_err(|error| ProviderConfigError::InvalidBaseUrl(error.to_string()))?;
        let responses_url = Url::parse(&format!(
            "{}/responses",
            base_url.as_ref().trim_end_matches('/')
        ))
        .map_err(|error| ProviderConfigError::InvalidBaseUrl(error.to_string()))?;
        let api_key = api_key.filter(|key| !key.trim().is_empty());
        if !completion_url.username().is_empty() || completion_url.password().is_some() {
            return Err(ProviderConfigError::EmbeddedCredentials);
        }
        if !matches!(completion_url.scheme(), "http" | "https") {
            return Err(ProviderConfigError::UnsupportedScheme);
        }
        if api_key.is_some()
            && completion_url.scheme() == "http"
            && !matches!(
                completion_url.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]")
            )
        {
            return Err(ProviderConfigError::InsecureCredentials);
        }
        let client = build_http_client(DEFAULT_TIMEOUT)?;

        Ok(Self {
            client,
            completion_url,
            responses_url,
            model,
            api_key,
            cache_technology,
            thinking: rynna_core::ThinkingLevel::Default,
            cache_optimizer: Arc::new(PrefixCacheOptimizer),
        })
    }

    pub fn with_cache_optimizer(mut self, optimizer: Arc<dyn CacheOptimizer>) -> Self {
        self.cache_optimizer = optimizer;
        self
    }
}

fn cache_technology_for(base_url: &str) -> CacheTechnology {
    let Ok(url) = Url::parse(base_url) else {
        return CacheTechnology::Generic;
    };
    if url.host_str() == Some("api.openai.com") {
        CacheTechnology::OpenAi
    } else if url.port_or_known_default() == Some(11434) {
        CacheTechnology::Ollama
    } else {
        CacheTechnology::Generic
    }
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

fn request_error(error: reqwest::Error) -> ProviderError {
    // Connect and timeout faults usually clear on their own; decode faults do not.
    let kind = if error.is_timeout() || error.is_connect() {
        ProviderErrorKind::Transient
    } else {
        ProviderErrorKind::Permanent
    };
    ProviderError::new(format!("request failed: {error}")).with_kind(kind)
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

fn build_http_client(io_timeout: Duration) -> Result<Client, reqwest::Error> {
    Client::builder()
        .connect_timeout(io_timeout)
        .read_timeout(io_timeout)
        .build()
}

#[derive(Debug, Error)]
pub enum ProviderConfigError {
    #[error("provider base URL is invalid: {0}")]
    InvalidBaseUrl(String),
    #[error("provider base URL must not contain embedded credentials")]
    EmbeddedCredentials,
    #[error("provider base URL must use HTTP or HTTPS")]
    UnsupportedScheme,
    #[error("provider credentials require HTTPS except for loopback HTTP endpoints")]
    InsecureCredentials,
    #[error("provider model must not be blank")]
    BlankModel,
    #[error("failed to build HTTP client: {0}")]
    Client(#[from] reqwest::Error),
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAiRequestMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAiTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    stream: bool,
}

#[derive(Serialize)]
struct OpenAiRequestMessage {
    role: Role,
    content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<OpenAiToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reasoning_details: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct OpenAiToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: OpenAiToolCallFunction,
}

#[derive(Serialize)]
struct OpenAiToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct OpenAiTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: OpenAiToolFunction,
}

#[derive(Serialize)]
struct OpenAiToolFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: OpenAiResponseMessage,
}

#[derive(Deserialize)]
struct OpenAiResponseMessage {
    role: Role,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OpenAiResponseToolCall>,
    #[serde(default)]
    reasoning_details: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize)]
struct OpenAiResponseToolCall {
    id: String,
    function: OpenAiResponseToolCallFunction,
}

#[derive(Deserialize)]
struct OpenAiResponseToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct ChatCompletionChunk {
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Deserialize)]
struct StreamDelta {
    content: Option<String>,
    reasoning_content: Option<String>,
    reasoning: Option<String>,
    #[serde(default)]
    reasoning_details: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    tool_calls: Vec<OpenAiStreamToolCall>,
}

#[derive(Deserialize)]
struct OpenAiStreamToolCall {
    index: usize,
    id: Option<String>,
    function: Option<OpenAiStreamToolCallFunction>,
}

#[derive(Deserialize)]
struct OpenAiStreamToolCallFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Default)]
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[async_trait]
impl ModelProvider for OpenAiCompatibleProvider {
    fn workflow_policy(&self) -> String {
        format!(
            "{}:{}:{}",
            self.completion_url,
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
        (self.cache_technology == CacheTechnology::OpenAi).then_some(ServerCompaction::OpenAi)
    }

    async fn complete_managed(&self, plan: ContextPlan) -> Result<Completion, ProviderError> {
        let has_continuation = has_openai_context(&plan.request);
        if self.server_compaction().is_none()
            || (plan.server_compaction_threshold.is_none() && !has_continuation)
        {
            return self.complete(plan.request).await;
        }
        let mut payload =
            responses_request(&self.model, plan.request, plan.server_compaction_threshold)?;
        if self.thinking != rynna_core::ThinkingLevel::Default {
            payload["reasoning"] = serde_json::json!({"effort": self.thinking.as_str()});
        }
        let mut request_builder = self.client.post(self.responses_url.clone()).json(&payload);
        if let Some(api_key) = &self.api_key {
            request_builder = request_builder.bearer_auth(api_key);
        }
        let response = send_with_retry(request_builder).await?;
        let status = response.status();
        let retry_after = retry_after_of(&response);
        let body = read_response_body(response).await?;
        if !status.is_success() {
            let body = String::from_utf8_lossy(&body);
            return Err(classify_body(
                ProviderError::new(format!(
                    "provider returned {status}: {}",
                    truncate(&body, MAX_ERROR_BODY_CHARS)
                ))
                .with_status(status.as_u16())
                .with_retry_after(retry_after),
                &body,
            ));
        }
        response_completion(&body)
    }

    async fn complete_stream_managed(
        &self,
        plan: ContextPlan,
        on_delta: &mut (dyn for<'delta> FnMut(&'delta CompletionDelta) + Send),
    ) -> Result<Completion, ProviderError> {
        if plan.server_compaction_threshold.is_some() || has_openai_context(&plan.request) {
            let completion = self.complete_managed(plan).await?;
            if !completion.message.content.is_empty() {
                on_delta(&CompletionDelta::Content(
                    completion.message.content.clone(),
                ));
            }
            Ok(completion)
        } else {
            self.complete_stream(plan.request, on_delta).await
        }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        let cache = self.cache_optimizer.optimize(&request);
        let cache_key = (self.cache_technology == CacheTechnology::OpenAi
            && cache.use_server_cache)
            .then(|| cache.server_cache_key());
        let mut payload = serde_json::to_value(chat_completion_request(
            &self.model,
            request,
            false,
            cache_key,
        ))
        .map_err(|error| ProviderError::new(error.to_string()))?;
        if self.thinking != rynna_core::ThinkingLevel::Default {
            payload["reasoning_effort"] = serde_json::json!(self.thinking.as_str());
        }
        let mut request_builder = self.client.post(self.completion_url.clone()).json(&payload);
        if let Some(api_key) = &self.api_key {
            request_builder = request_builder.bearer_auth(api_key);
        }

        let response = send_with_retry(request_builder).await?;
        let status = response.status();
        let retry_after = retry_after_of(&response);
        if !status.is_success() {
            let body = read_response_body(response).await?;
            let body = String::from_utf8_lossy(&body);
            return Err(classify_body(
                ProviderError::new(format!(
                    "provider returned {status}: {}",
                    truncate(&body, MAX_ERROR_BODY_CHARS)
                ))
                .with_status(status.as_u16())
                .with_retry_after(retry_after),
                &body,
            ));
        }

        let body = read_response_body(response).await?;
        let response: ChatCompletionResponse = serde_json::from_slice(&body)
            .map_err(|error| ProviderError::new(format!("invalid provider response: {error}")))?;
        let message = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::new("provider returned no choices"))?
            .message;

        if message.role != Role::Assistant {
            return Err(ProviderError::new(
                "provider response message must have the assistant role",
            ));
        }

        Ok(Completion::new(response_message(message)?))
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
        on_delta: &mut (dyn for<'delta> FnMut(&'delta CompletionDelta) + Send),
    ) -> Result<Completion, ProviderError> {
        let cache = self.cache_optimizer.optimize(&request);
        let cache_key = (self.cache_technology == CacheTechnology::OpenAi
            && cache.use_server_cache)
            .then(|| cache.server_cache_key());
        let mut payload = serde_json::to_value(chat_completion_request(
            &self.model,
            request,
            true,
            cache_key,
        ))
        .map_err(|error| ProviderError::new(error.to_string()))?;
        if self.thinking != rynna_core::ThinkingLevel::Default {
            payload["reasoning_effort"] = serde_json::json!(self.thinking.as_str());
        }
        let mut request_builder = self.client.post(self.completion_url.clone()).json(&payload);
        if let Some(api_key) = &self.api_key {
            request_builder = request_builder.bearer_auth(api_key);
        }

        let mut response = send_with_retry(request_builder).await?;
        let status = response.status();
        let retry_after = retry_after_of(&response);
        if !status.is_success() {
            let body = read_response_body(response).await?;
            let body = String::from_utf8_lossy(&body);
            return Err(classify_body(
                ProviderError::new(format!(
                    "provider returned {status}: {}",
                    truncate(&body, MAX_ERROR_BODY_CHARS)
                ))
                .with_status(status.as_u16())
                .with_retry_after(retry_after),
                &body,
            ));
        }

        let mut pending = Vec::new();
        let mut content = String::new();
        let mut tool_calls = BTreeMap::new();
        let mut reasoning_details = Vec::new();
        let mut received = 0usize;
        let mut done = false;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| ProviderError::new(format!("failed to read response: {error}")))?
        {
            received = received.saturating_add(chunk.len());
            if received > MAX_RESPONSE_BODY_BYTES {
                return Err(response_body_too_large());
            }
            pending.extend_from_slice(&chunk);
            while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                let mut line = pending.drain(..=newline).collect::<Vec<_>>();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if process_sse_line(
                    &line,
                    &mut content,
                    &mut tool_calls,
                    &mut reasoning_details,
                    on_delta,
                )? {
                    done = true;
                    pending.clear();
                    break;
                }
            }
            if done {
                break;
            }
        }
        if !done && !pending.is_empty() {
            done = process_sse_line(
                &pending,
                &mut content,
                &mut tool_calls,
                &mut reasoning_details,
                on_delta,
            )?;
        }
        if !done {
            return Err(ProviderError::new(
                "provider stream ended before [DONE] terminator",
            ));
        }

        Ok(Completion::new(stream_message(
            content,
            tool_calls,
            reasoning_details,
        )?))
    }
}

fn responses_request(
    model: &str,
    request: CompletionRequest,
    compact_threshold: Option<usize>,
) -> Result<serde_json::Value, ProviderError> {
    let mut input = Vec::new();
    for message in request.messages {
        if let Some(ProviderContext::OpenAi(output)) = message.provider_context {
            validate_openai_output(&output)?;
            if let Some(compaction_index) = output
                .iter()
                .rposition(|item| item["type"].as_str() == Some("compaction"))
            {
                input.clear();
                input.extend(output.into_iter().skip(compaction_index));
            } else {
                input.extend(output);
            }
            continue;
        }
        if message.role != Role::Tool && !message.content.is_empty() {
            input.push(serde_json::json!({
                "role": message.role,
                "content": message.content,
            }));
        }
        for call in message.tool_calls {
            input.push(serde_json::json!({
                "type": "function_call",
                "call_id": call.id,
                "name": call.name,
                "arguments": call.arguments.to_string(),
            }));
        }
        if message.role == Role::Tool {
            let call_id = message
                .tool_call_id
                .ok_or_else(|| ProviderError::new("tool result is missing its tool-call id"))?;
            input.push(serde_json::json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": message.content,
            }));
        }
    }
    let tools = request
        .tools
        .into_iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.input_schema,
            })
        })
        .collect::<Vec<_>>();
    let mut payload = serde_json::json!({
        "model": model,
        "input": input,
        "include": ["reasoning.encrypted_content"],
        "store": false,
    });
    if let Some(compact_threshold) = compact_threshold {
        payload["context_management"] = serde_json::json!([{
            "type": "compaction",
            "compact_threshold": compact_threshold,
        }]);
    }
    if !tools.is_empty() {
        payload["tools"] = serde_json::Value::Array(tools);
    }
    Ok(payload)
}

fn response_completion(body: &[u8]) -> Result<Completion, ProviderError> {
    let response: serde_json::Value = serde_json::from_slice(body)
        .map_err(|error| ProviderError::new(format!("invalid provider response: {error}")))?;
    let output = response["output"]
        .as_array()
        .ok_or_else(|| ProviderError::new("provider response is missing output"))?;
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    for item in output {
        match item["type"].as_str() {
            Some("message") if item["role"].as_str() == Some("assistant") => {
                if let Some(parts) = item["content"].as_array() {
                    for part in parts {
                        if part["type"].as_str() == Some("output_text") {
                            content.push_str(part["text"].as_str().unwrap_or_default());
                        }
                    }
                }
            }
            Some("function_call") => {
                let id = item["call_id"]
                    .as_str()
                    .ok_or_else(|| ProviderError::new("function call is missing call_id"))?;
                let name = item["name"]
                    .as_str()
                    .ok_or_else(|| ProviderError::new("function call is missing name"))?;
                let arguments = item["arguments"]
                    .as_str()
                    .ok_or_else(|| ProviderError::new("function call is missing arguments"))?;
                let arguments = serde_json::from_str(arguments).map_err(|error| {
                    ProviderError::new(format!("invalid tool-call arguments: {error}"))
                })?;
                tool_calls.push(ToolCall::new(id, name, arguments));
            }
            _ => {}
        }
    }
    Ok(Completion::new(Message {
        role: Role::Assistant,
        content,
        tool_calls,
        tool_call_id: None,
        provider_context: Some(ProviderContext::OpenAi(output.clone())),
    }))
}

fn has_openai_context(request: &CompletionRequest) -> bool {
    request
        .messages
        .iter()
        .any(|message| matches!(message.provider_context, Some(ProviderContext::OpenAi(_))))
}

fn validate_openai_output(output: &[serde_json::Value]) -> Result<(), ProviderError> {
    for item in output {
        match item["type"].as_str() {
            Some("compaction" | "reasoning" | "function_call") => {}
            Some("message") if item["role"].as_str() == Some("assistant") => {}
            _ => {
                return Err(ProviderError::new(
                    "stored OpenAI context contains an unsupported output item",
                ));
            }
        }
    }
    Ok(())
}

fn chat_completion_request(
    model: &str,
    request: CompletionRequest,
    stream: bool,
    prompt_cache_key: Option<String>,
) -> ChatCompletionRequest<'_> {
    ChatCompletionRequest {
        model,
        messages: request
            .messages
            .into_iter()
            .map(|message| {
                let Message {
                    role,
                    content,
                    tool_calls,
                    tool_call_id,
                    provider_context,
                } = message;
                let content =
                    if role == Role::Assistant && !tool_calls.is_empty() && content.is_empty() {
                        None
                    } else {
                        Some(content)
                    };
                let reasoning_details = match provider_context {
                    Some(ProviderContext::OpenAiChatReasoningDetails(details))
                        if role == Role::Assistant =>
                    {
                        details
                    }
                    _ => Vec::new(),
                };
                OpenAiRequestMessage {
                    role,
                    content,
                    tool_calls: tool_calls
                        .into_iter()
                        .map(|call| OpenAiToolCall {
                            id: call.id,
                            kind: "function",
                            function: OpenAiToolCallFunction {
                                name: call.name,
                                arguments: call.arguments.to_string(),
                            },
                        })
                        .collect(),
                    tool_call_id,
                    reasoning_details,
                }
            })
            .collect(),
        tools: request.tools.into_iter().map(openai_tool).collect(),
        prompt_cache_key,
        stream,
    }
}

fn openai_tool(definition: ToolDefinition) -> OpenAiTool {
    OpenAiTool {
        kind: "function",
        function: OpenAiToolFunction {
            name: definition.name,
            description: definition.description,
            parameters: definition.input_schema,
        },
    }
}

fn response_message(message: OpenAiResponseMessage) -> Result<Message, ProviderError> {
    let OpenAiResponseMessage {
        role,
        content,
        tool_calls,
        reasoning_details,
    } = message;
    let tool_calls = tool_calls
        .into_iter()
        .map(|call| {
            let arguments = serde_json::from_str(&call.function.arguments).map_err(|error| {
                ProviderError::new(format!("invalid tool-call arguments: {error}"))
            })?;
            Ok(ToolCall::new(call.id, call.function.name, arguments))
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;
    Ok(Message {
        role,
        content: content.unwrap_or_default(),
        tool_calls,
        tool_call_id: None,
        provider_context: reasoning_details
            .filter(|details| !details.is_empty())
            .map(ProviderContext::OpenAiChatReasoningDetails),
    })
}

fn stream_message(
    content: String,
    pending: BTreeMap<usize, PendingToolCall>,
    reasoning_details: Vec<serde_json::Value>,
) -> Result<Message, ProviderError> {
    let tool_calls = pending
        .into_values()
        .map(|call| {
            if call.id.is_empty() || call.name.is_empty() {
                return Err(ProviderError::new("incomplete streamed tool call"));
            }
            let arguments = serde_json::from_str(&call.arguments).map_err(|error| {
                ProviderError::new(format!("invalid streamed tool-call arguments: {error}"))
            })?;
            Ok(ToolCall::new(call.id, call.name, arguments))
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;
    Ok(Message {
        role: Role::Assistant,
        content,
        tool_calls,
        tool_call_id: None,
        provider_context: (!reasoning_details.is_empty()).then_some(
            ProviderContext::OpenAiChatReasoningDetails(reasoning_details),
        ),
    })
}

fn process_sse_line(
    line: &[u8],
    content: &mut String,
    tool_calls: &mut BTreeMap<usize, PendingToolCall>,
    reasoning_details: &mut Vec<serde_json::Value>,
    on_delta: &mut (dyn for<'delta> FnMut(&'delta CompletionDelta) + Send),
) -> Result<bool, ProviderError> {
    let Some(data) = line.strip_prefix(b"data:") else {
        return Ok(false);
    };
    let data = data.strip_prefix(b" ").unwrap_or(data);
    if data == b"[DONE]" {
        return Ok(true);
    }
    if data.is_empty() {
        return Ok(false);
    }
    let chunk: ChatCompletionChunk = serde_json::from_slice(data)
        .map_err(|error| ProviderError::new(format!("invalid provider stream: {error}")))?;
    for choice in chunk.choices {
        reasoning_details.extend(choice.delta.reasoning_details.unwrap_or_default());
        for delta in choice.delta.tool_calls {
            let pending = tool_calls.entry(delta.index).or_default();
            if let Some(id) = delta.id {
                pending.id.push_str(&id);
            }
            if let Some(function) = delta.function {
                if let Some(name) = function.name {
                    pending.name.push_str(&name);
                }
                if let Some(arguments) = function.arguments {
                    pending.arguments.push_str(&arguments);
                }
            }
        }
        if let Some(reasoning) = choice
            .delta
            .reasoning_content
            .or(choice.delta.reasoning)
            .filter(|reasoning| !reasoning.is_empty())
        {
            on_delta(&CompletionDelta::Thinking(reasoning));
        }
        if let Some(delta) = choice.delta.content {
            on_delta(&CompletionDelta::Content(delta.clone()));
            content.push_str(&delta);
        }
    }
    Ok(false)
}

async fn read_response_body(mut response: reqwest::Response) -> Result<Vec<u8>, ProviderError> {
    let content_length = response.content_length().unwrap_or(0);
    if content_length > MAX_RESPONSE_BODY_BYTES as u64 {
        return Err(response_body_too_large());
    }

    let mut body = Vec::with_capacity(content_length as usize);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ProviderError::new(format!("failed to read response: {error}")))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BODY_BYTES {
            return Err(response_body_too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn response_body_too_large() -> ProviderError {
    ProviderError::new(format!(
        "provider response exceeded {MAX_RESPONSE_BODY_BYTES}-byte limit"
    ))
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use reqwest::Url;
    use rynna_core::{CompletionDelta, CompletionRequest, Message, ModelProvider};

    use super::{OpenAiCompatibleProvider, build_http_client};

    #[test]
    fn constructor_selects_openai_server_caching_for_the_official_api() {
        let provider = OpenAiCompatibleProvider::new(
            "https://api.openai.com/v1",
            "gpt-5",
            Some("test-key".to_owned()),
        )
        .unwrap();

        assert_eq!(provider.cache_technology, super::CacheTechnology::OpenAi);
    }

    #[test]
    fn constructor_selects_ollama_prefix_caching_for_the_default_local_api() {
        let provider =
            OpenAiCompatibleProvider::new("http://127.0.0.1:11434/v1", "qwen3:8b", None).unwrap();

        assert_eq!(provider.cache_technology, super::CacheTechnology::Ollama);
    }

    #[tokio::test]
    async fn active_stream_can_outlive_the_per_read_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let request_bytes = connection.read(&mut request).unwrap();
            assert!(request_bytes > 0);
            connection
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .unwrap();

            for body in [
                "data: {\"choices\":[{\"delta\":{\"content\":\"still\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\" stream\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"ing\"}}]}\n\n",
                "data: [DONE]\n\n",
            ] {
                write!(connection, "{:x}\r\n{body}\r\n", body.len()).unwrap();
                connection.flush().unwrap();
                thread::sleep(Duration::from_millis(40));
            }
            connection.write_all(b"0\r\n\r\n").unwrap();
        });
        let provider = OpenAiCompatibleProvider {
            client: build_http_client(Duration::from_millis(70)).unwrap(),
            completion_url: Url::parse(&format!("http://{address}/v1/chat/completions")).unwrap(),
            responses_url: Url::parse(&format!("http://{address}/v1/responses")).unwrap(),
            model: "test-model".to_owned(),
            api_key: None,
            cache_technology: super::CacheTechnology::Generic,
            thinking: rynna_core::ThinkingLevel::Default,
            cache_optimizer: Arc::new(super::PrefixCacheOptimizer),
        };
        let mut deltas = Vec::new();

        let completion = provider
            .complete_stream(
                CompletionRequest {
                    messages: vec![Message::user("Hello")],
                    tools: Vec::new(),
                },
                &mut |delta| deltas.push(delta.clone()),
            )
            .await
            .unwrap();

        server.join().unwrap();
        assert_eq!(
            deltas,
            [
                CompletionDelta::Content("still".to_owned()),
                CompletionDelta::Content(" stream".to_owned()),
                CompletionDelta::Content("ing".to_owned()),
            ]
        );
        assert_eq!(completion.message, Message::assistant("still streaming"));
    }
}
