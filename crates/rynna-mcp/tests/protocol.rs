use async_trait::async_trait;
use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use rynna_config::mcp::{McpServer, McpSettings, McpTransport};
use rynna_core::{
    Agent, AgentProfiles, Completion, CompletionRequest, Message, ModelProvider, Profile,
    ProviderError, ToolCall, ToolSource,
};
use rynna_mcp::McpToolSource;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

fn settings(transport: McpTransport) -> McpSettings {
    McpSettings {
        servers: BTreeMap::from([(
            "tools".into(),
            McpServer {
                enabled: true,
                code_search_tool: None,
                transport,
            },
        )]),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_discovers_calls_and_isolates_same_named_servers() {
    async fn tools(marker: &str) -> Vec<Arc<dyn rynna_core::Tool>> {
        McpToolSource(settings(McpTransport::Stdio {
            command: "python3".into(),
            args: vec![format!(
                "{}/tests/fixtures/server.py",
                env!("CARGO_MANIFEST_DIR")
            )],
            env: BTreeMap::from([("PROFILE_MARKER".into(), marker.into())]),
        }))
        .discover()
        .await
        .unwrap()
    }
    let work = tools("work").await;
    let personal = tools("personal").await;
    assert_eq!(work[0].definition().name, personal[0].definition().name);
    assert!(work[0].definition().name.len() <= 64);
    let result = work[0].execute(json!({"text":"hello"})).await.unwrap();
    assert_eq!(result["content"][0]["text"], "hello");
    assert_eq!(result["structuredContent"]["profile"], "work");
    assert_eq!(
        personal[0].execute(json!({"text":"hello"})).await.unwrap()["structuredContent"]["profile"],
        "personal"
    );
    assert!(work[0].execute(json!("invalid")).await.is_err());
}

async fn mcp(
    State(calls): State<Arc<Mutex<Vec<Value>>>>,
    Json(request): Json<Value>,
) -> axum::response::Response {
    calls.lock().unwrap().push(request.clone());
    let Some(id) = request.get("id") else {
        return StatusCode::ACCEPTED.into_response();
    };
    let result = match request["method"].as_str().unwrap() {
        "initialize" => {
            json!({"protocolVersion":request["params"]["protocolVersion"],"capabilities":{"tools":{}},"serverInfo":{"name":"test","version":"1"}})
        }
        "tools/list" if request["params"]["cursor"] == "page2" => {
            json!({"tools":[{"name":"two","description":"Second tool","inputSchema":{"type":"object"}}]})
        }
        "tools/list" => {
            json!({"tools":[{"name":"one","description":"First tool","inputSchema":{"type":"object"}}],"nextCursor":"page2"})
        }
        "tools/call" => {
            json!({"content":[{"type":"text","text":"remote result"}],"isError":true,"structuredContent":{"value":42}})
        }
        _ => panic!("unexpected method"),
    };
    Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
}

struct Model {
    requests: Mutex<Vec<CompletionRequest>>,
    supported: bool,
}
#[async_trait]
impl ModelProvider for Model {
    fn supports_external_tools(&self) -> bool {
        self.supported
    }
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        if !request.tools.is_empty() && !request.messages.iter().any(|m| m.tool_call_id.is_some()) {
            Ok(Completion::with_tool_calls(vec![ToolCall {
                id: "call".into(),
                name: request.tools[0].name.clone(),
                arguments: json!({}),
            }]))
        } else {
            Ok(Completion::new(Message::assistant("done")))
        }
    }
}
fn profile(name: &str) -> Profile {
    Profile {
        name: name.into(),
        providers: vec![],
        active_skills: vec![],
        mcp_servers: vec![],
        capabilities: vec![],
        default_project_directory: ".".into(),
        projects: vec![],
        subagents: Vec::new(),
    }
}

#[tokio::test]
async fn http_pagination_tool_loop_profile_isolation_and_live_disable() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/mcp", post(mcp))
        .with_state(calls.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let model = Arc::new(Model {
        requests: Mutex::new(vec![]),
        supported: true,
    });
    let mut profiles = AgentProfiles::new(
        "work",
        [
            (profile("work"), Agent::new(model.clone(), "policy")),
            (profile("personal"), Agent::new(model.clone(), "policy")),
        ],
    )
    .unwrap();
    profiles
        .set_tool_source(
            "work",
            Some(Arc::new(McpToolSource(settings(
                McpTransport::StreamableHttp {
                    url,
                    bearer_token_env: None,
                },
            )))),
        )
        .unwrap();
    profiles
        .respond(Some("work"), &[], "use tools")
        .await
        .unwrap();
    {
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests[0].tools.len(), 2);
        let output = requests[1]
            .messages
            .iter()
            .find(|m| m.tool_call_id.is_some())
            .unwrap();
        let value: Value = serde_json::from_str(&output.content).unwrap();
        assert_eq!(value["isError"], true);
        assert_eq!(value["structuredContent"]["value"], 42);
    }
    profiles
        .respond(Some("personal"), &[], "no tools")
        .await
        .unwrap();
    assert!(
        model
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .tools
            .is_empty()
    );
    let count = calls.lock().unwrap().len();
    profiles
        .set_tool_source(
            "work",
            Some(Arc::new(McpToolSource(McpSettings::default()))),
        )
        .unwrap();
    profiles
        .respond_stream(Some("work"), &[], "disabled", &mut |_| {})
        .await
        .unwrap();
    assert!(
        model
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .tools
            .is_empty()
    );
    assert_eq!(calls.lock().unwrap().len(), count);
    server.abort();
}

#[tokio::test]
async fn disabled_servers_and_subscription_models_never_connect() {
    let invalid = McpTransport::Stdio {
        command: "rynna-missing-command".into(),
        args: vec![],
        env: BTreeMap::new(),
    };
    let mut disabled = settings(invalid.clone());
    disabled.servers.get_mut("tools").unwrap().enabled = false;
    assert!(McpToolSource(disabled).discover().await.unwrap().is_empty());
    assert!(
        McpToolSource(settings(invalid.clone()))
            .discover()
            .await
            .is_err()
    );
    let model = Arc::new(Model {
        requests: Mutex::new(vec![]),
        supported: false,
    });
    let mut profiles = AgentProfiles::new(
        "subscription",
        [(profile("subscription"), Agent::new(model.clone(), "policy"))],
    )
    .unwrap();
    profiles
        .set_tool_source(
            "subscription",
            Some(Arc::new(McpToolSource(settings(invalid)))),
        )
        .unwrap();
    profiles
        .respond(Some("subscription"), &[], "hello")
        .await
        .unwrap();
    assert!(model.requests.lock().unwrap()[0].tools.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_response_tools_cleans_up_the_process_tree() {
    let tools = McpToolSource(settings(McpTransport::Stdio {
        command: "python3".into(),
        args: vec![format!(
            "{}/tests/fixtures/server.py",
            env!("CARGO_MANIFEST_DIR")
        )],
        env: BTreeMap::from([("SPAWN_CHILD".into(), "true".into())]),
    }))
    .discover()
    .await
    .unwrap();
    let output = tools[0].execute(json!({"text":"test"})).await.unwrap();
    assert_eq!(output["structuredContent"]["isolated"], true);
    let parent = output["structuredContent"]["pid"].as_i64().unwrap() as i32;
    let child = output["structuredContent"]["child_pid"].as_i64().unwrap() as i32;
    drop(tools);
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            // Signal 0 only checks process existence; it does not send a signal.
            if unsafe { libc::kill(parent, 0) } == -1 && unsafe { libc::kill(child, 0) } == -1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("MCP subprocesses should be reaped after the response ends");
}

#[tokio::test]
async fn stalled_initialization_has_a_deadline() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = Router::new().route(
        "/mcp",
        post(|| async { std::future::pending::<StatusCode>().await }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let result = McpToolSource(settings(McpTransport::StreamableHttp {
        url,
        bearer_token_env: None,
    }))
    .discover()
    .await;
    assert!(result.err().unwrap().to_string().contains("timed out"));
    server.abort();
}

#[tokio::test]
async fn raw_environment_token_is_sent_as_a_bearer_header() {
    const TOKEN_ENV: &str = "RYNNA_MCP_PROTOCOL_TEST_TOKEN";
    // Set the environment on a subprocess, avoiding process-global mutation in parallel tests.
    if std::env::var(TOKEN_ENV).as_deref() != Ok("raw-test-token") {
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "raw_environment_token_is_sent_as_a_bearer_header",
                "--nocapture",
            ])
            .env(TOKEN_ENV, "raw-test-token")
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        return;
    }
    async fn authenticated_mcp(
        state: State<Arc<Mutex<Vec<Value>>>>,
        headers: axum::http::HeaderMap,
        request: Json<Value>,
    ) -> axum::response::Response {
        if headers
            .get("authorization")
            .and_then(|header| header.to_str().ok())
            != Some("Bearer raw-test-token")
        {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        mcp(state, request).await
    }
    let calls = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/mcp", post(authenticated_mcp))
        .with_state(calls.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let tools = McpToolSource(settings(McpTransport::StreamableHttp {
        url,
        bearer_token_env: Some(TOKEN_ENV.into()),
    }))
    .discover()
    .await
    .unwrap();
    assert_eq!(tools.len(), 2);
    tools[0].execute(json!({})).await.unwrap();
    let methods = calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| call["method"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    for method in [
        "initialize",
        "notifications/initialized",
        "tools/list",
        "tools/call",
    ] {
        assert!(
            methods.iter().any(|actual| actual == method),
            "missing authenticated {method}"
        );
    }
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn code_search_plugin_alias_executes_remote_tool_and_missing_selection_fails() {
    let mut config = settings(McpTransport::Stdio {
        command: "python3".into(),
        args: vec![format!(
            "{}/tests/fixtures/server.py",
            env!("CARGO_MANIFEST_DIR")
        )],
        env: BTreeMap::new(),
    });
    config.servers.get_mut("tools").unwrap().code_search_tool = Some("echo/path".into());
    let source = McpToolSource(config.clone());
    assert!(source.replaces_code_search());
    let tools = source.discover().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].definition().name, "code_search");
    let result = tools[0]
        .execute(json!({"text":"plugin-result"}))
        .await
        .unwrap();
    assert!(result.to_string().contains("plugin-result"));
    config.servers.get_mut("tools").unwrap().code_search_tool = Some("missing".into());
    assert!(McpToolSource(config.clone()).discover().await.is_err());
    config.servers.get_mut("tools").unwrap().enabled = false;
    let source = McpToolSource(config);
    assert!(!source.replaces_code_search());
    assert!(source.discover().await.unwrap().is_empty());
}
