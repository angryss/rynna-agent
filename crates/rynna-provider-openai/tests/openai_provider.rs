use std::sync::Arc;

use rynna_core::{
    CacheOptimization, CacheOptimizer, CompletionDelta, CompletionRequest, ContextPlan,
    ContextSize, Message, ModelProvider, ProviderContext, ServerCompaction, ToolCall,
    ToolDefinition,
};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use rynna_provider_openai::OpenAiCompatibleProvider;

struct DisabledCacheOptimizer;

impl CacheOptimizer for DisabledCacheOptimizer {
    fn optimize(&self, _request: &CompletionRequest) -> CacheOptimization {
        CacheOptimization {
            use_server_cache: false,
            scope_key: "unused".to_owned(),
        }
    }
}

struct ArbitraryScopeCacheOptimizer;

impl CacheOptimizer for ArbitraryScopeCacheOptimizer {
    fn optimize(&self, _request: &CompletionRequest) -> CacheOptimization {
        CacheOptimization {
            use_server_cache: true,
            scope_key: "x".repeat(200),
        }
    }
}

#[test]
fn remote_http_endpoint_rejects_api_key() {
    let secret = "super-secret";
    let error = OpenAiCompatibleProvider::new(
        "http://api.example.com/v1",
        "test-model",
        Some(secret.to_owned()),
    )
    .err()
    .expect("remote HTTP endpoint with credentials must be rejected");

    assert!(error.to_string().contains("HTTPS"));
    assert!(!error.to_string().contains(secret));
}

#[test]
fn unsupported_url_schemes_are_rejected() {
    let error = OpenAiCompatibleProvider::new("ftp://api.example.com/v1", "test-model", None)
        .err()
        .expect("non-HTTP schemes must be rejected");

    assert!(error.to_string().contains("HTTP or HTTPS"));
}

#[test]
fn provider_urls_with_embedded_credentials_are_rejected() {
    let error = OpenAiCompatibleProvider::new(
        "http://user:password@api.example.com/v1",
        "test-model",
        None,
    )
    .err()
    .expect("URL-embedded credentials must be rejected");

    assert!(error.to_string().contains("embedded credentials"));
    assert!(!error.to_string().contains("password"));
}

#[test]
fn loopback_http_endpoints_accept_api_keys() {
    for base_url in [
        "http://localhost:11434/v1",
        "http://127.0.0.1:11434/v1",
        "http://[::1]:11434/v1",
    ] {
        assert!(
            OpenAiCompatibleProvider::new(base_url, "test-model", Some("test-key".to_owned()))
                .is_ok(),
            "{base_url} should be allowed"
        );
    }
}

#[tokio::test]
async fn ollama_requests_use_the_cache_friendly_openai_compatible_shape() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .and(body_json(json!({
            "model": "test-model",
            "messages": [
                {"role": "system", "content": "You are Rynna."},
                {"role": "user", "content": "Hello"}
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "Hello from the model"}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new_ollama(
        format!("{}/v1/", server.uri()),
        "test-model",
        Some("test-key".to_owned()),
    )
    .unwrap();

    let completion = provider
        .complete(CompletionRequest {
            messages: vec![Message::system("You are Rynna."), Message::user("Hello")],
            tools: Vec::new(),
        })
        .await
        .unwrap();

    assert_eq!(
        completion.message,
        Message::assistant("Hello from the model")
    );
}

#[tokio::test]
async fn openai_uses_responses_server_side_compaction_for_managed_context() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(body_json(json!({
            "model": "test-model",
            "input": [
                {"role": "system", "content": "You are Rynna."},
                {"role": "user", "content": "Continue"}
            ],
            "context_management": [{"type": "compaction", "compact_threshold": 80}],
            "include": ["reasoning.encrypted_content"],
            "store": false
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Compacted answer"}]
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let provider = OpenAiCompatibleProvider::new_openai(
        format!("{}/v1", server.uri()),
        "test-model",
        Some("test-key".to_owned()),
    )
    .unwrap();

    assert_eq!(provider.server_compaction(), Some(ServerCompaction::OpenAi));
    let completion = provider
        .complete_managed(ContextPlan {
            request: CompletionRequest {
                messages: vec![Message::system("You are Rynna."), Message::user("Continue")],
                tools: vec![],
            },
            size: ContextSize {
                current_tokens: 85,
                max_tokens: 100,
            },
            server_compaction_threshold: Some(80),
            compacted: false,
        })
        .await
        .unwrap();

    assert_eq!(completion.message.content, "Compacted answer");
    assert!(matches!(
        completion.message.provider_context,
        Some(ProviderContext::OpenAi(_))
    ));
}

#[tokio::test]
async fn openai_round_trips_server_compaction_output_on_the_next_response() {
    let server = MockServer::start().await;
    let compacted_output = json!([
        {"type": "compaction", "encrypted_content": "opaque-summary"},
        {"type": "reasoning", "encrypted_content": "opaque-reasoning"},
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "Compacted answer"}]
        }
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"output": compacted_output})))
        .expect(1)
        .mount(&server)
        .await;
    let provider = OpenAiCompatibleProvider::new_openai(
        format!("{}/v1", server.uri()),
        "test-model",
        Some("test-key".to_owned()),
    )
    .unwrap();

    let compacted = provider
        .complete_managed(ContextPlan {
            request: CompletionRequest {
                messages: vec![Message::user("Start")],
                tools: vec![],
            },
            size: ContextSize {
                current_tokens: 85,
                max_tokens: 100,
            },
            server_compaction_threshold: Some(80),
            compacted: false,
        })
        .await
        .unwrap();
    assert_eq!(
        compacted.message.provider_context,
        Some(ProviderContext::OpenAi(
            compacted_output.as_array().unwrap().clone()
        ))
    );

    server.reset().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(body_json(json!({
            "model": "test-model",
            "input": [
                {"type": "compaction", "encrypted_content": "opaque-summary"},
                {"type": "reasoning", "encrypted_content": "opaque-reasoning"},
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "Compacted answer"}]
                },
                {"role": "user", "content": "Continue"}
            ],
            "include": ["reasoning.encrypted_content"],
            "store": false
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Final answer"}]
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let completion = provider
        .complete_managed(ContextPlan {
            request: CompletionRequest {
                messages: vec![
                    Message::user("Old prompt that was compacted"),
                    Message::assistant("Old answer that was compacted"),
                    compacted.message,
                    Message::user("Continue"),
                ],
                tools: vec![],
            },
            size: ContextSize {
                current_tokens: 20,
                max_tokens: 100,
            },
            server_compaction_threshold: None,
            compacted: false,
        })
        .await
        .unwrap();

    assert_eq!(completion.message.content, "Final answer");
}

#[tokio::test]
async fn openai_streaming_preserves_a_server_compaction_continuation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Continued answer"}]
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let provider = OpenAiCompatibleProvider::new_openai(
        format!("{}/v1", server.uri()),
        "test-model",
        Some("test-key".to_owned()),
    )
    .unwrap();
    let mut prior = Message::assistant("Compacted answer");
    prior.provider_context = Some(ProviderContext::OpenAi(vec![json!({
        "type": "compaction",
        "encrypted_content": "opaque-summary"
    })]));
    let mut deltas = Vec::new();

    let completion = provider
        .complete_stream_managed(
            ContextPlan {
                request: CompletionRequest {
                    messages: vec![prior, Message::user("Continue")],
                    tools: vec![],
                },
                size: ContextSize {
                    current_tokens: 20,
                    max_tokens: 100,
                },
                server_compaction_threshold: None,
                compacted: false,
            },
            &mut |delta| deltas.push(delta.clone()),
        )
        .await
        .unwrap();

    assert_eq!(completion.message.content, "Continued answer");
    assert_eq!(
        deltas,
        vec![CompletionDelta::Content("Continued answer".to_owned())]
    );
}

#[test]
fn ollama_reports_that_server_side_compaction_is_unavailable() {
    let provider =
        OpenAiCompatibleProvider::new_ollama("http://127.0.0.1:11434/v1", "test-model", None)
            .unwrap();

    assert_eq!(provider.server_compaction(), None);
}

#[tokio::test]
async fn openai_requests_include_a_stable_prompt_cache_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "Hello"}
            }]
        })))
        .expect(2)
        .mount(&server)
        .await;
    let provider = OpenAiCompatibleProvider::new_openai(
        format!("{}/v1", server.uri()),
        "test-model",
        Some("test-key".to_owned()),
    )
    .unwrap();

    for messages in [
        vec![Message::system("Stable policy"), Message::user("First")],
        vec![
            Message::system("Stable policy"),
            Message::user("First"),
            Message::assistant("Answer"),
            Message::user("Second"),
        ],
    ] {
        provider
            .complete(CompletionRequest {
                messages,
                tools: Vec::new(),
            })
            .await
            .unwrap();
    }

    let requests = server.received_requests().await.unwrap();
    let bodies = requests
        .iter()
        .map(|request| serde_json::from_slice::<serde_json::Value>(&request.body).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(bodies[0]["prompt_cache_key"], bodies[1]["prompt_cache_key"]);
    assert_eq!(bodies[0]["prompt_cache_key"].as_str().unwrap().len(), 64);
}

#[tokio::test]
async fn openai_cache_optimizer_can_be_substituted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_json(json!({
            "model": "test-model",
            "messages": [
                {"role": "system", "content": "Stable policy"},
                {"role": "user", "content": "Hello"}
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "Done"}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let provider =
        OpenAiCompatibleProvider::new_openai(format!("{}/v1", server.uri()), "test-model", None)
            .unwrap()
            .with_cache_optimizer(Arc::new(DisabledCacheOptimizer));

    provider
        .complete(CompletionRequest {
            messages: vec![Message::system("Stable policy"), Message::user("Hello")],
            tools: Vec::new(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn openai_normalizes_substituted_cache_scopes_for_the_server() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "Done"}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let provider =
        OpenAiCompatibleProvider::new_openai(format!("{}/v1", server.uri()), "test-model", None)
            .unwrap()
            .with_cache_optimizer(Arc::new(ArbitraryScopeCacheOptimizer));

    provider
        .complete(CompletionRequest {
            messages: vec![Message::user("Hello")],
            tools: Vec::new(),
        })
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let cache_key = body["prompt_cache_key"].as_str().unwrap();
    assert_eq!(cache_key.len(), 64);
    assert_ne!(cache_key, "x".repeat(200));
}

#[tokio::test]
async fn complete_sends_tool_definitions_and_parses_tool_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_json(json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "Read README.md"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "Read a workspace file",
                    "parameters": {
                        "type": "object",
                        "properties": {"path": {"type": "string"}},
                        "required": ["path"]
                    }
                }
            }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"README.md\"}"
                        }
                    }]
                }
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let provider =
        OpenAiCompatibleProvider::new(format!("{}/v1", server.uri()), "test-model", None).unwrap();

    let completion = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("Read README.md")],
            tools: vec![ToolDefinition::new(
                "read_file",
                "Read a workspace file",
                json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }),
            )],
        })
        .await
        .unwrap();

    assert_eq!(completion.message.tool_calls.len(), 1);
    assert_eq!(completion.message.tool_calls[0].name, "read_file");
    assert_eq!(
        completion.message.tool_calls[0].arguments,
        json!({"path": "README.md"})
    );
}

#[tokio::test]
async fn complete_serializes_assistant_tool_calls_and_tool_results() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_json(json!({
            "model": "test-model",
            "messages": [
                {"role": "user", "content": "Read it"},
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"README.md\"}"
                        }
                    }]
                },
                {"role": "tool", "content": "{\"content\":\"# Rynna\"}", "tool_call_id": "call-1"}
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "Done"}}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let provider =
        OpenAiCompatibleProvider::new(format!("{}/v1", server.uri()), "test-model", None).unwrap();

    provider
        .complete(CompletionRequest {
            messages: vec![
                Message::user("Read it"),
                Message::assistant_with_tool_calls(vec![ToolCall::new(
                    "call-1",
                    "read_file",
                    json!({"path": "README.md"}),
                )]),
                Message::tool("call-1", "{\"content\":\"# Rynna\"}"),
            ],
            tools: Vec::new(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn complete_preserves_reasoning_details_across_tool_turns() {
    let server = MockServer::start().await;
    let reasoning_details = vec![json!({
        "type": "reasoning.encrypted",
        "data": "opaque-reasoning",
        "id": "reasoning-1",
        "format": "openai-responses-v1",
        "index": 0
    })];
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_json(json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "Read it"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "reasoning_details": reasoning_details,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"README.md\"}"
                        }
                    }]
                }
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_json(json!({
            "model": "test-model",
            "messages": [
                {"role": "user", "content": "Read it"},
                {
                    "role": "assistant",
                    "content": null,
                    "reasoning_details": reasoning_details,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"README.md\"}"
                        }
                    }]
                },
                {"role": "tool", "content": "# Rynna", "tool_call_id": "call-1"}
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "Done"}}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let provider =
        OpenAiCompatibleProvider::new(format!("{}/v1", server.uri()), "test-model", None).unwrap();

    let first = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("Read it")],
            tools: Vec::new(),
        })
        .await
        .unwrap();

    assert_eq!(
        first.message.provider_context,
        Some(ProviderContext::OpenAiChatReasoningDetails(
            reasoning_details.clone()
        ))
    );
    provider
        .complete(CompletionRequest {
            messages: vec![
                Message::user("Read it"),
                first.message,
                Message::tool("call-1", "# Rynna"),
            ],
            tools: Vec::new(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn complete_stream_distinguishes_reasoning_from_user_facing_content() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_json(json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"Check\"}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"reasoning\":\" facts\"}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n",
                    "data: [DONE]\n\n"
                )),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider =
        OpenAiCompatibleProvider::new(format!("{}/v1", server.uri()), "test-model", None).unwrap();
    let mut deltas = Vec::new();
    let mut on_delta = |delta: &CompletionDelta| deltas.push(delta.clone());

    let completion = provider
        .complete_stream(
            CompletionRequest {
                messages: vec![Message::user("Hello")],
                tools: Vec::new(),
            },
            &mut on_delta,
        )
        .await
        .unwrap();

    assert_eq!(
        deltas,
        [
            CompletionDelta::Thinking("Check".to_owned()),
            CompletionDelta::Thinking(" facts".to_owned()),
            CompletionDelta::Content("Hello".to_owned()),
            CompletionDelta::Content(" world".to_owned()),
        ]
    );
    assert_eq!(completion.message, Message::assistant("Hello world"));
}

#[tokio::test]
async fn complete_stream_accumulates_fragmented_tool_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"README.md\\\"}\"}}]}}]}\n\n",
                    "data: [DONE]\n\n"
                )),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider =
        OpenAiCompatibleProvider::new(format!("{}/v1", server.uri()), "test-model", None).unwrap();
    let mut deltas = Vec::new();

    let completion = provider
        .complete_stream(
            CompletionRequest {
                messages: vec![Message::user("Read README.md")],
                tools: vec![ToolDefinition::new(
                    "read_file",
                    "Read a workspace file",
                    json!({"type": "object"}),
                )],
            },
            &mut |delta| deltas.push(delta.clone()),
        )
        .await
        .unwrap();

    assert!(deltas.is_empty());
    assert_eq!(completion.message.tool_calls.len(), 1);
    assert_eq!(completion.message.tool_calls[0].id, "call-1");
    assert_eq!(
        completion.message.tool_calls[0].arguments,
        json!({"path": "README.md"})
    );
}

#[tokio::test]
async fn complete_stream_preserves_reasoning_detail_chunks() {
    let server = MockServer::start().await;
    let reasoning_details = vec![
        json!({
            "type": "reasoning.summary",
            "summary": "Inspect the project",
            "id": "reasoning-1",
            "format": "anthropic-claude-v1",
            "index": 0
        }),
        json!({
            "type": "reasoning.encrypted",
            "data": "opaque-reasoning",
            "id": "reasoning-2",
            "format": "anthropic-claude-v1",
            "index": 1
        }),
    ];
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "data: {\"choices\":[{\"delta\":{\"reasoning_details\":[{\"type\":\"reasoning.summary\",\"summary\":\"Inspect the project\",\"id\":\"reasoning-1\",\"format\":\"anthropic-claude-v1\",\"index\":0}]}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"reasoning_details\":[{\"type\":\"reasoning.encrypted\",\"data\":\"opaque-reasoning\",\"id\":\"reasoning-2\",\"format\":\"anthropic-claude-v1\",\"index\":1}],\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{}\"}}]}}]}\n\n",
                    "data: [DONE]\n\n"
                )),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider =
        OpenAiCompatibleProvider::new(format!("{}/v1", server.uri()), "test-model", None).unwrap();

    let completion = provider
        .complete_stream(
            CompletionRequest {
                messages: vec![Message::user("Inspect it")],
                tools: Vec::new(),
            },
            &mut |_| {},
        )
        .await
        .unwrap();

    assert_eq!(
        completion.message.provider_context,
        Some(ProviderContext::OpenAiChatReasoningDetails(
            reasoning_details
        ))
    );
}

async fn incomplete_stream_error(body: &str) -> String {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = OpenAiCompatibleProvider::new(server.uri(), "test-model", None).unwrap();

    provider
        .complete_stream(
            CompletionRequest {
                messages: vec![Message::user("Hello")],
                tools: Vec::new(),
            },
            &mut |_| {},
        )
        .await
        .expect_err("an SSE response without a protocol terminator must fail")
        .to_string()
}

#[tokio::test]
async fn complete_stream_rejects_abrupt_empty_success_response() {
    let error = incomplete_stream_error("").await;
    assert!(error.contains("before [DONE]"), "unexpected error: {error}");
}

#[tokio::test]
async fn complete_stream_rejects_truncated_answer() {
    let error = incomplete_stream_error(
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial answer\"}}]}\n\n",
    )
    .await;
    assert!(error.contains("before [DONE]"), "unexpected error: {error}");
}

#[tokio::test]
async fn complete_stream_rejects_complete_looking_tool_call_without_terminator() {
    let error = incomplete_stream_error(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{}\"}}]}}]}\n\n",
    )
    .await;
    assert!(error.contains("before [DONE]"), "unexpected error: {error}");
}

#[tokio::test]
async fn oversized_success_response_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 1024 * 1024 + 1]))
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(server.uri(), "test-model", None).unwrap();
    let error = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("Hello")],
            tools: Vec::new(),
        })
        .await
        .expect_err("oversized success body must be rejected");

    assert!(
        error.to_string().contains("1048576-byte limit"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn oversized_error_response_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_bytes(vec![b'x'; 1024 * 1024 + 1]))
        // A 500 is transient, so it is now attempted three times before giving up.
        .expect(3)
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(server.uri(), "test-model", None).unwrap();
    let error = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("Hello")],
            tools: Vec::new(),
        })
        .await
        .expect_err("oversized error body must be rejected");

    assert!(
        error.to_string().contains("1048576-byte limit"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn selected_thinking_is_sent_in_chat_and_managed_responses() {
    use rynna_core::ThinkingLevel;
    let server = MockServer::start().await;
    Mock::given(path("/v1/chat/completions"))
        .and(wiremock::matchers::body_partial_json(
            json!({"model":"test-model","reasoning_effort":"high"}),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"choices":[{"message":{"role":"assistant","content":"done"}}]}),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider =
        OpenAiCompatibleProvider::new(format!("{}/v1", server.uri()), "test-model", None)
            .unwrap()
            .with_thinking(ThinkingLevel::High)
            .unwrap();
    provider
        .complete(CompletionRequest {
            messages: vec![Message::user("hello")],
            tools: vec![],
        })
        .await
        .unwrap();
    Mock::given(path("/v1/responses"))
        .and(wiremock::matchers::body_partial_json(json!({"reasoning":{"effort":"low"}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}]})))
        .expect(1).mount(&server).await;
    let provider =
        OpenAiCompatibleProvider::new_openai(format!("{}/v1", server.uri()), "test-model", None)
            .unwrap()
            .with_thinking(ThinkingLevel::Low)
            .unwrap();
    provider
        .complete_managed(ContextPlan {
            request: CompletionRequest {
                messages: vec![Message::user("hello")],
                tools: vec![],
            },
            size: ContextSize::default(),
            server_compaction_threshold: Some(1000),
            compacted: false,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn local_custom_model_names_are_sent_unchanged_without_authentication() {
    for model in [
        "qwen3:14b",
        "mlx-community/Qwen3.8-27B-8bit",
        "/models/my-model",
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(wiremock::matchers::body_partial_json(
                json!({"model": model}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"role": "assistant", "content": "Hello"}}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let provider =
            OpenAiCompatibleProvider::new(format!("{}/v1", server.uri()), model, None).unwrap();
        let result = provider
            .complete(CompletionRequest {
                messages: vec![Message::user("Hello")],
                tools: vec![],
            })
            .await
            .unwrap();
        assert_eq!(result.message, Message::assistant("Hello"));
        assert!(
            !server.received_requests().await.unwrap()[0]
                .headers
                .contains_key("authorization")
        );
    }
}

#[test]
fn workflow_policy_tracks_endpoint_and_model_but_not_credentials() {
    let first = OpenAiCompatibleProvider::new(
        "http://localhost:8000/v1",
        "model",
        Some("old-secret".into()),
    )
    .unwrap();
    let rotated = OpenAiCompatibleProvider::new(
        "http://localhost:8000/v1",
        "model",
        Some("new-secret".into()),
    )
    .unwrap();
    let moved = OpenAiCompatibleProvider::new("http://localhost:9000/v1", "model", None).unwrap();
    assert_eq!(first.workflow_policy(), rotated.workflow_policy());
    assert_ne!(first.workflow_policy(), moved.workflow_policy());
    assert!(!first.workflow_policy().contains("secret"));
}

fn chat_provider(server: &MockServer) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        format!("{}/v1", server.uri()),
        "test-model",
        Some("test-key".to_owned()),
    )
    .unwrap()
}

fn hello() -> CompletionRequest {
    CompletionRequest {
        messages: vec![Message::user("Hello")],
        tools: Vec::new(),
    }
}

#[tokio::test]
async fn rate_limited_requests_are_retried_and_then_succeed() {
    let server = MockServer::start().await;
    // First attempt is rate limited; the retry must reissue the identical request.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "recovered"}}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let completion = chat_provider(&server).complete(hello()).await.unwrap();
    assert_eq!(completion.message, Message::assistant("recovered"));
}

#[tokio::test]
async fn overloaded_providers_are_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "ok"}}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    assert!(chat_provider(&server).complete(hello()).await.is_ok());
}

#[tokio::test]
async fn client_errors_are_not_retried() {
    let server = MockServer::start().await;
    // A 400 is the model's fault, not the network's: exactly one attempt.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
        .expect(1)
        .mount(&server)
        .await;

    let error = chat_provider(&server).complete(hello()).await.unwrap_err();
    assert!(!error.is_transient(), "400 must not be transient");
    assert_eq!(error.status(), Some(400));
    // The response body still reaches the caller.
    assert!(error.message().contains("bad request"), "{error}");
}

#[tokio::test]
async fn invalid_credentials_are_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
        .expect(1)
        .mount(&server)
        .await;

    let error = chat_provider(&server).complete(hello()).await.unwrap_err();
    assert_eq!(error.kind(), rynna_core::ProviderErrorKind::Auth);
}

#[tokio::test]
async fn exhausted_retries_surface_the_final_response_body() {
    let server = MockServer::start().await;
    // Persistent rate limiting: the initial attempt plus two retries, then give up
    // with a classified error that still carries the provider's own message.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_string("still limited"))
        .expect(3)
        .mount(&server)
        .await;

    let error = chat_provider(&server).complete(hello()).await.unwrap_err();
    assert!(error.is_transient(), "429 stays transient when exhausted");
    assert_eq!(error.status(), Some(429));
    assert!(error.message().contains("still limited"), "{error}");
}

#[tokio::test]
async fn a_long_retry_after_stops_retrying_rather_than_sleeping() {
    let server = MockServer::start().await;
    // Sleeping 120s would burn the caller's whole allowance, so we surface the
    // transient error immediately and let the caller decide to resume later.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "120")
                .set_body_string("come back later"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let error = chat_provider(&server).complete(hello()).await.unwrap_err();
    assert!(error.is_transient());
    assert_eq!(
        error.retry_after(),
        Some(std::time::Duration::from_secs(120))
    );
}
