use async_trait::async_trait;
use rynna_core::{
    Agent, AgentProfiles, Completion, CompletionDelta, CompletionRequest, Message, ModelProvider,
    Profile, ProviderError, Subagent, Tool, ToolCall, ToolDefinition, ToolError, ToolSource,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct Provider {
    requests: Mutex<Vec<CompletionRequest>>,
    arguments: Value,
    tools_supported: bool,
    fail_child: bool,
    stall_child: bool,
}

#[async_trait]
impl ModelProvider for Provider {
    fn supports_external_tools(&self) -> bool {
        self.tools_supported
    }
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        let child = request.messages[0].content.contains("Subagent role:");
        if child {
            if self.stall_child {
                std::future::pending::<()>().await;
            }
            if self.fail_child {
                return Err(ProviderError::new("child failed"));
            }
            if request.messages.len() == 2 {
                return Ok(Completion::with_tool_calls(vec![ToolCall::new(
                    "read",
                    "read_file",
                    json!({}),
                )]));
            }
            return Ok(Completion::new(Message::assistant("child findings")));
        }
        if request.messages.last().unwrap().role == rynna_core::Role::Tool {
            return Ok(Completion::new(Message::assistant(
                request.messages.last().unwrap().content.clone(),
            )));
        }
        if request
            .tools
            .iter()
            .any(|tool| tool.name == "delegate_task")
        {
            return Ok(Completion::with_tool_calls(vec![ToolCall::new(
                "delegate",
                "delegate_task",
                self.arguments.clone(),
            )]));
        }
        Ok(Completion::new(Message::assistant("no delegation")))
    }
}

struct ReadFile;
#[async_trait]
impl Tool for ReadFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new("read_file", "Permitted file read", json!({"type":"object"}))
    }
    async fn execute(&self, _: Value) -> Result<Value, ToolError> {
        Ok(json!({"file":"permitted contents"}))
    }
}

struct Source;
#[async_trait]
impl ToolSource for Source {
    async fn discover(&self) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        Ok(vec![Arc::new(ReadFile)])
    }
}

fn helper(name: &str) -> Subagent {
    Subagent {
        name: name.into(),
        description: "Review requested code".into(),
        instructions: "Report actionable issues".into(),
    }
}
fn profile(name: &str, subagents: Vec<Subagent>) -> Profile {
    serde_json::from_value(json!({"name":name,"providers":[],"subagents":subagents})).unwrap()
}
fn provider(arguments: Value) -> Provider {
    Provider {
        requests: Mutex::new(vec![]),
        arguments,
        tools_supported: true,
        fail_child: false,
        stall_child: false,
    }
}
fn profiles(provider: Arc<Provider>) -> AgentProfiles {
    let mut result = AgentProfiles::new(
        "work",
        [
            (
                profile("work", vec![helper("reviewer")]),
                Agent::new(provider.clone(), "Parent policy"),
            ),
            (
                profile("personal", vec![helper("writer")]),
                Agent::new(provider, "Personal policy"),
            ),
        ],
    )
    .unwrap();
    result
        .set_tool_source("work", Some(Arc::new(Source)))
        .unwrap();
    result
        .set_project_configuration("work", "/work/project".into(), vec![])
        .unwrap();
    result
}

#[tokio::test]
async fn delegation_inherits_project_and_tools_but_not_history_or_recursive_delegation() {
    for stream in [false, true] {
        let provider = Arc::new(provider(
            json!({"subagent":"reviewer","task":"Review the change"}),
        ));
        let profiles = profiles(provider.clone())
            .with_project(Some("work"), None)
            .unwrap();
        let agent = profiles.clone_agent("work").unwrap();
        let history = [Message::user("private earlier context")];
        let reply = if stream {
            agent
                .respond_stream(&history, "delegate", &mut |_: &CompletionDelta| {})
                .await
        } else {
            agent.respond(&history, "delegate").await
        }
        .unwrap();
        assert!(reply.content.contains("child findings"));
        let requests = provider.requests.lock().unwrap();
        let parent = &requests[0];
        assert_eq!(
            parent
                .tools
                .iter()
                .find(|t| t.name == "delegate_task")
                .unwrap()
                .input_schema["properties"]["subagent"]["enum"],
            json!(["reviewer"])
        );
        let child = &requests[1];
        assert_eq!(child.messages.len(), 2);
        assert!(child.messages[0].content.contains("Parent policy"));
        assert!(child.messages[0].content.contains("/work/project"));
        assert!(
            child.messages[0]
                .content
                .contains("Report actionable issues")
        );
        assert_eq!(child.messages[1].content, "Review the change");
        assert_eq!(
            child
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["read_file"]
        );
        assert!(
            requests[2]
                .messages
                .last()
                .unwrap()
                .content
                .contains("permitted contents")
        );
    }
}

#[tokio::test]
async fn rejects_other_profiles_helpers_and_invalid_arguments_without_starting_a_child() {
    for args in [
        json!({"subagent":"writer","task":"work"}),
        json!({"subagent":"reviewer","task":" "}),
        json!({"subagent":"reviewer","task":"work","profile":"personal"}),
    ] {
        let provider = Arc::new(provider(args));
        let reply = profiles(provider.clone())
            .respond(None, &[], "delegate")
            .await
            .unwrap();
        assert!(reply.content.contains("error"));
        assert_eq!(provider.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn updates_are_profile_scoped_and_inflight_snapshots_keep_their_helpers() {
    let provider = Arc::new(provider(json!({"subagent":"reviewer","task":"work"})));
    let mut profiles = profiles(provider.clone());
    let before = profiles.clone();
    profiles.set_subagents("work", vec![]).unwrap();
    assert_eq!(
        profiles
            .respond(Some("work"), &[], "delegate")
            .await
            .unwrap()
            .content,
        "no delegation"
    );
    assert!(
        before
            .respond(Some("work"), &[], "delegate")
            .await
            .unwrap()
            .content
            .contains("child findings")
    );
    assert!(
        profiles
            .profiles()
            .iter()
            .find(|p| p.name == "personal")
            .unwrap()
            .subagents
            .iter()
            .any(|s| s.name == "writer")
    );
    assert!(profiles.set_subagents("missing", vec![]).is_err());
    assert!(
        profiles
            .set_subagents("work", vec![helper("same"), helper("same")])
            .is_err()
    );
}

#[tokio::test]
async fn providers_without_tool_support_do_not_receive_subagents() {
    let mut provider = provider(json!({"subagent":"reviewer","task":"work"}));
    provider.tools_supported = false;
    let provider = Arc::new(provider);
    assert_eq!(
        profiles(provider.clone())
            .respond(None, &[], "hello")
            .await
            .unwrap()
            .content,
        "no delegation"
    );
    assert!(provider.requests.lock().unwrap()[0].tools.is_empty());
}

#[tokio::test]
async fn child_errors_return_to_the_parent_as_tool_errors() {
    let mut provider = provider(json!({"subagent":"reviewer","task":"work"}));
    provider.fail_child = true;
    let reply = profiles(Arc::new(provider))
        .respond(None, &[], "hello")
        .await
        .unwrap();
    assert!(reply.content.contains("child failed"));
}

#[tokio::test(start_paused = true)]
async fn parent_deadline_bounds_child_execution() {
    let mut provider = provider(json!({"subagent":"reviewer","task":"work"}));
    provider.stall_child = true;
    let start = tokio::time::Instant::now();
    let result = profiles(Arc::new(provider))
        .respond(None, &[], "hello")
        .await;
    let text = match result {
        Ok(reply) => reply.content,
        Err(error) => error.to_string(),
    };
    assert!(text.contains("deadline"));
    assert!(start.elapsed() <= std::time::Duration::from_secs(300));
}

#[tokio::test]
async fn helpers_use_the_request_selected_model_without_memory_hooks() {
    use rynna_core::{
        MemoryConversation, MemoryError, MemoryProvider, ModelSelection, ProfileProvider,
        ThinkingLevel,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[derive(Default)]
    struct Memory {
        recalls: AtomicUsize,
        retains: AtomicUsize,
    }
    #[async_trait]
    impl MemoryProvider for Memory {
        async fn recall(&self, _: &str) -> Result<Vec<String>, MemoryError> {
            self.recalls.fetch_add(1, Ordering::SeqCst);
            Ok(vec!["private recalled fact".into()])
        }
        async fn retain(&self, _: &MemoryConversation) -> Result<(), MemoryError> {
            self.retains.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    let default = Arc::new(provider(json!({})));
    let selected = Arc::new(provider(json!({"subagent":"reviewer","task":"Review"})));
    let model = ProfileProvider {
        provider: "local".into(),
        model: "chosen".into(),
        enabled: true,
        is_default: true,
    };
    let mut metadata = profile("work", vec![helper("reviewer")]);
    metadata.providers = vec![model.clone()];
    let memory = Arc::new(Memory::default());
    let agent = Agent::new(default.clone(), "policy")
        .with_model_options(vec![(model, selected.clone())])
        .with_memory_provider(Some(memory.clone()));
    let profiles = AgentProfiles::new("work", [(metadata, agent)])
        .unwrap()
        .with_model_selection(
            None,
            Some(&ModelSelection {
                provider: "local".into(),
                model: "chosen".into(),
                thinking: ThinkingLevel::Default,
            }),
        )
        .unwrap();
    profiles.respond(None, &[], "Delegate").await.unwrap();
    rynna_core::flush_memory_writes().await;
    assert!(default.requests.lock().unwrap().is_empty());
    let requests = selected.requests.lock().unwrap();
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|message| message.content.contains("private recalled fact"))
    );
    assert!(
        requests
            .iter()
            .filter(|request| request.messages[0].content.contains("Subagent role:"))
            .all(|request| !request
                .messages
                .iter()
                .any(|message| message.content.contains("private recalled fact")))
    );
    assert_eq!(memory.recalls.load(Ordering::SeqCst), 1);
    assert_eq!(memory.retains.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn parent_and_helpers_share_one_tool_call_budget_per_response() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct SideEffect(Arc<AtomicUsize>);
    #[async_trait]
    impl Tool for SideEffect {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "side_effect",
                "Count an operation",
                json!({"type":"object"}),
            )
        }
        async fn execute(&self, _: Value) -> Result<Value, ToolError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"done":true}))
        }
    }
    struct ManyCalls(usize);
    #[async_trait]
    impl ModelProvider for ManyCalls {
        async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
            let child = request.messages[0].content.contains("Subagent role:");
            if request.messages.last().unwrap().role == rynna_core::Role::Tool {
                return Ok(Completion::new(Message::assistant(
                    request.messages.last().unwrap().content.clone(),
                )));
            }
            let calls = if child {
                (0..self.0)
                    .map(|i| ToolCall::new(format!("effect-{i}"), "side_effect", json!({})))
                    .collect()
            } else {
                (0..2)
                    .map(|i| {
                        ToolCall::new(
                            format!("delegate-{i}"),
                            "delegate_task",
                            json!({"subagent":"reviewer","task":"Perform operations"}),
                        )
                    })
                    .collect()
            };
            Ok(Completion::with_tool_calls(calls))
        }
    }
    for (calls_per_child, expected_effects) in [(31, 62), (32, 32)] {
        let effects = Arc::new(AtomicUsize::new(0));
        let agent = Agent::with_tools(
            Arc::new(ManyCalls(calls_per_child)),
            "policy",
            vec![Arc::new(SideEffect(effects.clone()))],
        )
        .unwrap();
        let profiles =
            AgentProfiles::new("work", [(profile("work", vec![helper("reviewer")]), agent)])
                .unwrap();
        // Reusing the runtime must give the next independent response a fresh budget.
        for response in 1..=2 {
            let reply = profiles.respond(None, &[], "Delegate twice").await.unwrap();
            assert_eq!(effects.load(Ordering::SeqCst), expected_effects * response);
            assert_eq!(
                reply.content.contains("maximum of 64 tool calls"),
                calls_per_child == 32
            );
        }
    }
}

#[tokio::test]
async fn helpers_share_the_aggregate_result_byte_budget_across_short_summaries() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct LargeResult;
    #[async_trait]
    impl Tool for LargeResult {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new(
                "large_result",
                "Read a large result",
                json!({"type":"object"}),
            )
        }
        async fn execute(&self, _: Value) -> Result<Value, ToolError> {
            Ok(json!("x".repeat(5 * 1024 * 1024)))
        }
    }
    struct Summarizer(Arc<AtomicUsize>);
    #[async_trait]
    impl ModelProvider for Summarizer {
        async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
            let child = request.messages[0].content.contains("Subagent role:");
            if request.messages.last().unwrap().role == rynna_core::Role::Tool {
                if child {
                    self.0.fetch_add(1, Ordering::SeqCst);
                    return Ok(Completion::new(Message::assistant("short summary")));
                }
                return Ok(Completion::new(Message::assistant(
                    request.messages.last().unwrap().content.clone(),
                )));
            }
            let calls = if child {
                vec![ToolCall::new("read", "large_result", json!({}))]
            } else {
                (0..2)
                    .map(|i| {
                        ToolCall::new(
                            format!("delegate-{i}"),
                            "delegate_task",
                            json!({"subagent":"reviewer","task":"Summarize"}),
                        )
                    })
                    .collect()
            };
            Ok(Completion::with_tool_calls(calls))
        }
    }
    let summaries = Arc::new(AtomicUsize::new(0));
    let agent = Agent::with_tools(
        Arc::new(Summarizer(summaries.clone())),
        "policy",
        vec![Arc::new(LargeResult)],
    )
    .unwrap();
    let profiles =
        AgentProfiles::new("work", [(profile("work", vec![helper("reviewer")]), agent)]).unwrap();
    for response in 1..=2 {
        let reply = profiles.respond(None, &[], "Delegate twice").await.unwrap();
        assert!(reply.content.contains("aggregate tool result byte limit"));
        // The second large payload must never reach a model, even though the first was summarized.
        assert_eq!(summaries.load(Ordering::SeqCst), response);
    }
}

#[tokio::test]
async fn dropping_parent_response_drops_a_subagents_active_tool() {
    struct PendingTool {
        entered: tokio::sync::Notify,
        dropped: Arc<tokio::sync::Notify>,
    }
    struct Dropped(Arc<tokio::sync::Notify>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }
    #[async_trait]
    impl Tool for PendingTool {
        fn definition(&self) -> ToolDefinition {
            ReadFile.definition()
        }
        async fn execute(&self, _: Value) -> Result<Value, ToolError> {
            let _guard = Dropped(self.dropped.clone());
            self.entered.notify_one();
            std::future::pending().await
        }
    }
    let tool = Arc::new(PendingTool {
        entered: tokio::sync::Notify::new(),
        dropped: Arc::new(tokio::sync::Notify::new()),
    });
    let provider = Arc::new(provider(json!({"subagent":"reviewer","task":"Review"})));
    let agent = Agent::with_tools(provider, "Parent", vec![tool.clone() as Arc<dyn Tool>]).unwrap();
    let agents =
        AgentProfiles::new("work", [(profile("work", vec![helper("reviewer")]), agent)]).unwrap();
    let task = tokio::spawn(async move { agents.respond(Some("work"), &[], "Delegate").await });
    tokio::time::timeout(std::time::Duration::from_secs(2), tool.entered.notified())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(2), tool.dropped.notified())
        .await
        .expect("subagent tool outlived parent");
}
