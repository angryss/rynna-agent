use std::{process::Stdio, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures_util::future::try_join_all;
use rmcp::{
    RoleClient, ServiceExt,
    model::CallToolRequestParams,
    service::RunningService,
    transport::{
        StreamableHttpClientTransport, TokioChildProcess,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use rynna_config::mcp::{McpSettings, McpTransport};
use rynna_core::{Tool, ToolDefinition, ToolError, ToolSource};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::{process::Command, time::timeout};

struct Session {
    client: RunningService<RoleClient, ()>,
    _process_group: ProcessGroupCleanup,
}

// The SDK gracefully waits for the parent, which can exit before its descendants.
// Retain the group ID so all descendants are stopped even in that case.
struct ProcessGroupCleanup(Option<u32>);
impl Drop for ProcessGroupCleanup {
    fn drop(&mut self) {
        if let Some(_id) = self.0 {
            // This group was created exclusively for this MCP child process.
            #[cfg(unix)]
            unsafe {
                libc::killpg(_id as i32, libc::SIGKILL);
            }
        }
    }
}

/// Immutable profile snapshot. Each response owns its sessions, released on completion/cancellation.
pub struct McpToolSource(pub McpSettings);

#[async_trait]
impl ToolSource for McpToolSource {
    fn replaces_code_search(&self) -> bool {
        self.0
            .servers
            .values()
            .any(|server| server.enabled && server.code_search_tool.is_some())
    }
    fn workflow_policy(&self) -> String {
        let mut settings = self.0.clone();
        for server in settings.servers.values_mut() {
            if let rynna_config::mcp::McpTransport::Stdio { env, .. } = &mut server.transport {
                for (name, value) in env.iter_mut() {
                    let name = name.to_ascii_uppercase();
                    if ["KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL", "AUTH"]
                        .iter()
                        .any(|part| name.contains(part))
                    {
                        value.clear();
                    }
                }
            }
        }
        serde_json::to_string(&settings).expect("MCP settings")
    }

    async fn discover(&self) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        self.0
            .validate()
            .map_err(|_| ToolError::new("invalid MCP settings"))?;
        let servers = self.0.servers.iter().filter(|(_, server)| server.enabled);
        let results = try_join_all(servers.map(|(name, server)| async move {
            timeout(
                Duration::from_secs(10),
                connect(name, &server.transport, server.code_search_tool.as_deref()),
            )
            .await
            .map_err(|_| {
                ToolError::new(format!(
                    "MCP server `{name}` connection or discovery timed out"
                ))
            })?
        }))
        .await?;
        Ok(results.into_iter().flatten().collect())
    }
}

async fn connect(
    name: &str,
    transport: &McpTransport,
    code_search_tool: Option<&str>,
) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
    // Transport errors may contain URLs, arguments, or credentials. Keep those out of responses.
    let failed = || {
        ToolError::new(format!(
            "could not connect to or discover tools from MCP server `{name}`"
        ))
    };
    let (client, process_group) = match transport {
        McpTransport::Stdio { command, args, env } => {
            let mut cmd = Command::new(command);
            cmd.args(args).env_clear().envs(env);
            // Permit ordinary executable/package resolution without inheriting provider credentials.
            for key in [
                "PATH",
                "HOME",
                "USERPROFILE",
                "SYSTEMROOT",
                "TEMP",
                "TMP",
                "TMPDIR",
            ] {
                if !env.contains_key(key)
                    && let Some(value) = std::env::var_os(key)
                {
                    cmd.env(key, value);
                }
            }
            let mut command = process_wrap::tokio::CommandWrap::from(cmd);
            #[cfg(unix)]
            command.wrap(process_wrap::tokio::ProcessGroup::leader());
            #[cfg(windows)]
            command.wrap(process_wrap::tokio::JobObject);
            command.wrap(process_wrap::tokio::KillOnDrop);
            let (transport, _) = TokioChildProcess::builder(command)
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| failed())?;
            let cleanup = ProcessGroupCleanup(transport.id());
            let client = ().serve(transport).await.map_err(|_| failed())?;
            (client, cleanup)
        }
        McpTransport::StreamableHttp {
            url,
            bearer_token_env,
        } => {
            let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone());
            if let Some(key) = bearer_token_env {
                // rmcp’s reqwest transport calls bearer_auth(), which adds the scheme.
                config.auth_header = Some(std::env::var(key).map_err(|_| {
                    ToolError::new(format!(
                        "MCP server `{name}` bearer token environment variable is unavailable"
                    ))
                })?);
            }
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .build()
                .map_err(|_| failed())?;
            let client =
                ().serve(StreamableHttpClientTransport::with_client(client, config))
                    .await
                    .map_err(|_| failed())?;
            (client, ProcessGroupCleanup(None))
        }
    };
    let session = Arc::new(Session {
        client,
        _process_group: process_group,
    });
    let mut definitions = Vec::new();
    let mut cursor = None;
    loop {
        let page = session
            .client
            .list_tools(cursor.map(|cursor| {
                let mut params = rmcp::model::PaginatedRequestParams::default();
                params.cursor = Some(cursor);
                params
            }))
            .await
            .map_err(|_| failed())?;
        definitions.extend(page.tools);
        if definitions.len() > 256 {
            return Err(ToolError::new(format!(
                "MCP server `{name}` exceeds the 256-tool limit"
            )));
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    if let Some(selected) = code_search_tool
        && !definitions.iter().any(|tool| tool.name == selected)
    {
        return Err(ToolError::new(format!(
            "MCP server `{name}` did not supply its configured code search tool"
        )));
    }
    Ok(definitions
        .into_iter()
        .map(|tool| {
            // Stable, provider-safe names (under 64 bytes), including a digest to avoid normalization collisions.
            let digest = Sha256::digest(format!("{name}\0{}", tool.name).as_bytes());
            let suffix = digest[..9]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            let definition = ToolDefinition::new(
                if code_search_tool == Some(tool.name.as_ref()) {
                    "code_search".to_owned()
                } else {
                    format!("mcp_{name}_{suffix}")
                },
                format!(
                    "MCP {name} / {}: {}",
                    tool.name,
                    tool.description.unwrap_or_default()
                ),
                Value::Object(tool.input_schema.as_ref().clone()),
            );
            Arc::new(McpTool {
                session: session.clone(),
                remote_name: tool.name.into_owned(),
                definition,
            }) as Arc<dyn Tool>
        })
        .collect())
}

struct McpTool {
    session: Arc<Session>,
    remote_name: String,
    definition: ToolDefinition,
}

#[async_trait]
impl Tool for McpTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    async fn execute(&self, arguments: Value) -> Result<Value, ToolError> {
        let arguments = arguments
            .as_object()
            .cloned()
            .ok_or_else(|| ToolError::new("MCP tool arguments must be an object"))?;
        let result = timeout(
            Duration::from_secs(60),
            self.session.client.call_tool(
                CallToolRequestParams::new(self.remote_name.clone()).with_arguments(arguments),
            ),
        )
        .await
        .map_err(|_| ToolError::new("MCP tool timed out"))?
        .map_err(|_| ToolError::new("MCP tool request failed"))?;
        let result =
            serde_json::to_value(result).map_err(|_| ToolError::new("invalid MCP tool result"))?;
        if serde_json::to_vec(&result)
            .map_err(|_| ToolError::new("invalid MCP tool result"))?
            .len()
            > 1024 * 1024
        {
            return Err(ToolError::new("MCP tool result exceeds 1 MiB"));
        }
        // Preserve content, structuredContent and isError as untrusted tool output.
        Ok(result)
    }
}
