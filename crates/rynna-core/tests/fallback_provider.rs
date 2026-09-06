use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rynna_core::{
    Completion, CompletionDelta, CompletionRequest, ContextPlan, ContextSize, FallbackProvider,
    Message, ModelProvider, ProviderError,
};

struct RecordingProvider {
    name: &'static str,
    result: Result<&'static str, &'static str>,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl ModelProvider for RecordingProvider {
    async fn complete(&self, _request: CompletionRequest) -> Result<Completion, ProviderError> {
        self.calls.lock().unwrap().push(self.name);
        match self.result {
            Ok(reply) => Ok(Completion::new(Message::assistant(reply))),
            Err(message) => Err(ProviderError::new(message)),
        }
    }
}

struct StreamingProvider {
    delta: &'static str,
    result: Result<&'static str, &'static str>,
}

#[async_trait]
impl ModelProvider for StreamingProvider {
    async fn complete(&self, _request: CompletionRequest) -> Result<Completion, ProviderError> {
        match self.result {
            Ok(reply) => Ok(Completion::new(Message::assistant(reply))),
            Err(message) => Err(ProviderError::new(message)),
        }
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
        on_delta: &mut (dyn for<'delta> FnMut(&'delta CompletionDelta) + Send),
    ) -> Result<Completion, ProviderError> {
        on_delta(&CompletionDelta::Content(self.delta.to_owned()));
        match self.result {
            Ok(reply) => Ok(Completion::new(Message::assistant(reply))),
            Err(message) => Err(ProviderError::new(message)),
        }
    }
}

struct ManagedOnlyProvider;

#[async_trait]
impl ModelProvider for ManagedOnlyProvider {
    async fn complete(&self, _request: CompletionRequest) -> Result<Completion, ProviderError> {
        Err(ProviderError::new("plain completion path used"))
    }

    async fn complete_managed(&self, _plan: ContextPlan) -> Result<Completion, ProviderError> {
        Ok(Completion::new(Message::assistant("managed reply")))
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
        _on_delta: &mut (dyn for<'delta> FnMut(&'delta CompletionDelta) + Send),
    ) -> Result<Completion, ProviderError> {
        Err(ProviderError::new("plain streaming path used"))
    }

    async fn complete_stream_managed(
        &self,
        _plan: ContextPlan,
        on_delta: &mut (dyn for<'delta> FnMut(&'delta CompletionDelta) + Send),
    ) -> Result<Completion, ProviderError> {
        on_delta(&CompletionDelta::Content("managed stream".to_owned()));
        Ok(Completion::new(Message::assistant("managed stream")))
    }
}

fn request() -> CompletionRequest {
    CompletionRequest {
        messages: vec![Message::user("hello")],
        tools: Vec::new(),
    }
}

fn plan() -> ContextPlan {
    ContextPlan {
        request: request(),
        size: ContextSize {
            current_tokens: 1,
            max_tokens: 10,
        },
        server_compaction_threshold: None,
        compacted: false,
    }
}

#[tokio::test]
async fn fallback_provider_tries_configured_providers_in_order() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let provider = FallbackProvider::new(vec![
        Arc::new(RecordingProvider {
            name: "primary",
            result: Err("primary unavailable"),
            calls: calls.clone(),
        }),
        Arc::new(RecordingProvider {
            name: "secondary",
            result: Ok("secondary reply"),
            calls: calls.clone(),
        }),
        Arc::new(RecordingProvider {
            name: "unused",
            result: Ok("unused reply"),
            calls: calls.clone(),
        }),
    ])
    .unwrap();

    let completion = provider.complete(request()).await.unwrap();

    assert_eq!(completion.message, Message::assistant("secondary reply"));
    assert_eq!(*calls.lock().unwrap(), vec!["primary", "secondary"]);
}

#[tokio::test]
async fn fallback_provider_retries_when_the_stream_fails_before_output() {
    for managed in [false, true] {
        let provider = FallbackProvider::new(vec![
            Arc::new(StreamingProvider {
                delta: "",
                result: Err("stream failed"),
            }),
            Arc::new(StreamingProvider {
                delta: "kept",
                result: Ok("kept"),
            }),
        ])
        .unwrap();
        let mut deltas = Vec::new();

        let mut on_delta = |delta: &CompletionDelta| deltas.push(delta.clone());
        let completion = if managed {
            provider
                .complete_stream_managed(plan(), &mut on_delta)
                .await
        } else {
            provider.complete_stream(request(), &mut on_delta).await
        }
        .unwrap();

        assert_eq!(completion.message, Message::assistant("kept"));
        assert_eq!(deltas, vec![CompletionDelta::Content("kept".to_owned())]);
    }
}

#[tokio::test]
async fn fallback_provider_preserves_managed_completion_paths() {
    let provider = FallbackProvider::new(vec![Arc::new(ManagedOnlyProvider)]).unwrap();
    let mut deltas = Vec::new();

    let completion = provider.complete_managed(plan()).await.unwrap();
    let streamed = provider
        .complete_stream_managed(plan(), &mut |delta| deltas.push(delta.clone()))
        .await
        .unwrap();

    assert_eq!(completion.message, Message::assistant("managed reply"));
    assert_eq!(streamed.message, Message::assistant("managed stream"));
    assert_eq!(
        deltas,
        vec![CompletionDelta::Content("managed stream".to_owned())]
    );
}

#[test]
fn fallback_provider_requires_at_least_one_provider() {
    assert!(FallbackProvider::new(Vec::new()).is_err());
}

struct GatedStreamingProvider {
    emitted: tokio::sync::Notify,
    result: Result<&'static str, &'static str>,
    thinking: bool,
}

#[async_trait]
impl ModelProvider for GatedStreamingProvider {
    async fn complete(&self, _request: CompletionRequest) -> Result<Completion, ProviderError> {
        unreachable!("streaming test provider")
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
        on_delta: &mut (dyn for<'delta> FnMut(&'delta CompletionDelta) + Send),
    ) -> Result<Completion, ProviderError> {
        let delta = if self.thinking {
            CompletionDelta::Thinking("Thinking now".to_owned())
        } else {
            CompletionDelta::Content("Answer now".to_owned())
        };
        on_delta(&delta);
        // Completion can only finish once the caller actually sees the delta.
        self.emitted.notified().await;
        self.result
            .map(|text| Completion::new(Message::assistant(text)))
            .map_err(ProviderError::new)
    }
}

#[tokio::test]
async fn fallback_streams_before_completion_and_never_switches_after_output() {
    for managed in [false, true] {
        for thinking in [false, true] {
            for result in [Ok("Done"), Err("interrupted")] {
                let primary = Arc::new(GatedStreamingProvider {
                    emitted: tokio::sync::Notify::new(),
                    result,
                    thinking,
                });
                let fallback_calls = Arc::new(Mutex::new(Vec::new()));
                let provider = FallbackProvider::new(vec![
                    primary.clone(),
                    Arc::new(RecordingProvider {
                        name: "fallback",
                        result: Ok("Must not run"),
                        calls: fallback_calls.clone(),
                    }),
                ])
                .unwrap();
                let mut deltas = Vec::new();
                let mut on_delta = |delta: &CompletionDelta| {
                    deltas.push(delta.clone());
                    primary.emitted.notify_one();
                };
                let completion = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                    if managed {
                        provider
                            .complete_stream_managed(plan(), &mut on_delta)
                            .await
                    } else {
                        provider.complete_stream(request(), &mut on_delta).await
                    }
                })
                .await
                .expect("delta must reach the caller before completion");
                assert_eq!(completion.is_ok(), result.is_ok());
                if let Err(error) = completion {
                    assert_eq!(error.to_string(), "model provider failed: interrupted");
                }
                assert_eq!(
                    deltas,
                    vec![if thinking {
                        CompletionDelta::Thinking("Thinking now".to_owned())
                    } else {
                        CompletionDelta::Content("Answer now".to_owned())
                    }]
                );
                assert!(fallback_calls.lock().unwrap().is_empty());
            }
        }
    }
}

#[tokio::test]
async fn tool_enabled_fallback_discards_failed_content_before_retrying() {
    for managed in [false, true] {
        let provider = FallbackProvider::new(vec![
            Arc::new(StreamingProvider {
                delta: "discarded",
                result: Err("truncated"),
            }),
            Arc::new(StreamingProvider {
                delta: "kept",
                result: Ok("kept"),
            }),
        ])
        .unwrap();
        let mut plan = plan();
        plan.request.tools.push(rynna_core::ToolDefinition::new(
            "inspect",
            "Inspect the host",
            serde_json::json!({"type": "object"}),
        ));
        let mut deltas = Vec::new();
        let mut on_delta = |delta: &CompletionDelta| deltas.push(delta.clone());
        let completion = if managed {
            provider.complete_stream_managed(plan, &mut on_delta).await
        } else {
            provider.complete_stream(plan.request, &mut on_delta).await
        }
        .unwrap();
        assert_eq!(completion.message, Message::assistant("kept"));
        assert_eq!(deltas, vec![CompletionDelta::Content("kept".to_owned())]);
    }
}

#[tokio::test]
async fn tool_enabled_thinking_streams_live_and_prevents_retry_after_failure() {
    for managed in [false, true] {
        let primary = Arc::new(GatedStreamingProvider {
            emitted: tokio::sync::Notify::new(),
            result: Err("interrupted"),
            thinking: true,
        });
        let fallback_calls = Arc::new(Mutex::new(Vec::new()));
        let provider = FallbackProvider::new(vec![
            primary.clone(),
            Arc::new(RecordingProvider {
                name: "fallback",
                result: Ok("Must not run"),
                calls: fallback_calls.clone(),
            }),
        ])
        .unwrap();
        let mut plan = plan();
        plan.request.tools.push(rynna_core::ToolDefinition::new(
            "inspect",
            "Inspect the host",
            serde_json::json!({"type": "object"}),
        ));
        let mut deltas = Vec::new();
        let mut on_delta = |delta: &CompletionDelta| {
            deltas.push(delta.clone());
            primary.emitted.notify_one();
        };
        let completion = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            if managed {
                provider.complete_stream_managed(plan, &mut on_delta).await
            } else {
                provider.complete_stream(plan.request, &mut on_delta).await
            }
        })
        .await
        .expect("thinking must stream before completion");
        assert!(completion.is_err());
        assert_eq!(
            deltas,
            vec![CompletionDelta::Thinking("Thinking now".to_owned())]
        );
        assert!(fallback_calls.lock().unwrap().is_empty());
    }
}
