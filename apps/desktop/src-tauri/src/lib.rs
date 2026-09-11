use std::env;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use rynna_config::mcp::{McpSettings, McpSettingsStore};
use rynna_config::memory::{MemorySettings, MemorySettingsResponse, MemorySettingsStore};
use rynna_config::{
    AnthropicAuthentication, ConfiguredProvider, OPENAI_ACCOUNT_PROFILE, OpenAiAuthentication,
    ProfileCatalog, ProviderKind, ProviderSettingsStore, ResolvedCapability, ResolvedProfile,
    ResolvedProvider,
};
use rynna_core::{
    Agent, AgentProfiles, CompletionDelta, FallbackProvider, Message, ModelProvider, Profile,
    ProfileProvider, Tool,
};
use rynna_mcp::McpToolSource;
use rynna_memory_hindsight::configured_memory;
use rynna_provider_anthropic::{
    AnthropicMessagesProvider, CLAUDE_SUBSCRIPTION_CONFLICTING_ENV_VARS, ClaudeCodeProvider,
    isolate_claude_subscription_environment, terminate_child,
};
use rynna_provider_openai::OpenAiCompatibleProvider;
use rynna_tools_command::{CommandConfig, CommandTool};
use rynna_tools_filesystem::{FileSystemConfig, FileSystemToolset};
use serde::{Deserialize, Serialize};
use tauri::{State, ipc::Channel};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant, timeout};

mod responses;
mod workflows;
pub use rynna_provider_openai::CodexAppServerProvider;
#[cfg(test)]
use rynna_provider_openai::codex_protocol::{MAX_CODEX_MESSAGE_BYTES, read_codex_message};
use rynna_provider_openai::codex_protocol::{
    OpenAiCredentialSelection, read_codex_response, secure_codex_home, write_codex_message,
};

const MAX_API_KEY_BYTES: usize = 16 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum OpenAiConnectRequest {
    Chatgpt,
    ApiKey { api_key: String },
}

#[derive(Clone, Debug, Serialize)]
pub struct OpenAiAccountResponse {
    pub connected: bool,
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ProviderInput {
    Ollama {
        api_base: String,
    },
    Mlx {
        api_base: String,
    },
    #[serde(rename = "openrouter")]
    OpenRouter,
    #[serde(rename = "openai")]
    OpenAi {
        authentication: OpenAiAuthentication,
        #[serde(default)]
        api_key: Option<String>,
        #[serde(default)]
        reuse_existing: bool,
    },
    Anthropic {
        authentication: AnthropicAuthentication,
    },
}

impl ProviderInput {
    fn kind(&self) -> &'static str {
        match self {
            Self::Ollama { .. } => "ollama",
            Self::Mlx { .. } => "mlx",
            Self::OpenRouter => "openrouter",
            Self::OpenAi { .. } => "openai",
            Self::Anthropic { .. } => "anthropic",
        }
    }
}

#[derive(Default)]
struct OpenAiAuthenticationLock(Mutex<()>);

impl OpenAiAuthenticationLock {
    async fn acquire(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.0.lock().await
    }
}

pub async fn connect_openai_with_program(
    program: impl AsRef<OsStr>,
    request: OpenAiConnectRequest,
) -> Result<OpenAiAccountResponse, String> {
    connect_openai_with_program_and_optional_home(program.as_ref(), None, request).await
}

pub async fn connect_openai_with_program_and_home(
    program: impl AsRef<OsStr>,
    codex_home: impl AsRef<OsStr>,
    request: OpenAiConnectRequest,
) -> Result<OpenAiAccountResponse, String> {
    connect_openai_with_program_and_optional_home(
        program.as_ref(),
        Some(codex_home.as_ref()),
        request,
    )
    .await
}

async fn connect_openai_with_program_and_optional_home(
    program: &OsStr,
    codex_home: Option<&OsStr>,
    request: OpenAiConnectRequest,
) -> Result<OpenAiAccountResponse, String> {
    let mut command = Command::new(program);
    if let Some(codex_home) = codex_home {
        command.env("CODEX_HOME", codex_home);
    }
    command
        .arg("login")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match request {
        OpenAiConnectRequest::Chatgpt => {
            command.stdin(Stdio::null());
        }
        OpenAiConnectRequest::ApiKey { api_key } => {
            if api_key.trim().is_empty() {
                return Err("OpenAI API key must not be blank".to_owned());
            }
            if api_key.len() > MAX_API_KEY_BYTES {
                return Err("OpenAI API key is too large".to_owned());
            }
            command.arg("--with-api-key").stdin(Stdio::piped());
            let mut child = command
                .kill_on_drop(true)
                .spawn()
                .map_err(|error| format!("failed to start Codex login: {error}"))?;
            let mut stdin = child
                .stdin
                .take()
                .ok_or("Codex login stdin is unavailable")?;
            stdin
                .write_all(api_key.as_bytes())
                .await
                .map_err(|_| "failed to send the API key to Codex".to_owned())?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|_| "failed to send the API key to Codex".to_owned())?;
            drop(stdin);
            let status = timeout(Duration::from_secs(120), child.wait())
                .await
                .map_err(|_| "Codex API-key login timed out".to_owned())?
                .map_err(|error| format!("failed to wait for Codex login: {error}"))?;
            if !status.success() {
                return Err("Codex rejected the OpenAI API key".to_owned());
            }
            return match codex_home {
                Some(codex_home) => openai_account_with_program_and_home(program, codex_home).await,
                None => openai_account_with_program(program).await,
            };
        }
    }

    let mut child = command
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("failed to start ChatGPT login: {error}"))?;
    let status = timeout(Duration::from_secs(300), child.wait())
        .await
        .map_err(|_| "ChatGPT login timed out".to_owned())?
        .map_err(|error| format!("failed to wait for ChatGPT login: {error}"))?;
    if !status.success() {
        return Err("ChatGPT login did not complete".to_owned());
    }
    match codex_home {
        Some(codex_home) => openai_account_with_program_and_home(program, codex_home).await,
        None => openai_account_with_program(program).await,
    }
}

pub async fn openai_account_with_program(
    program: impl AsRef<OsStr>,
) -> Result<OpenAiAccountResponse, String> {
    let mut command = Command::new(program);
    command
        .env_remove("CODEX_HOME")
        .env_remove("RYNNA_CODEX_HOME");
    openai_account_with_command(command).await
}

pub async fn openai_account_with_program_and_home(
    program: impl AsRef<OsStr>,
    codex_home: impl AsRef<OsStr>,
) -> Result<OpenAiAccountResponse, String> {
    let mut command = Command::new(program);
    command.env("CODEX_HOME", codex_home);
    openai_account_with_command(command).await
}

async fn openai_account_with_command(
    mut command: Command,
) -> Result<OpenAiAccountResponse, String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut child = command
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("failed to start Codex app-server: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or("Codex app-server stdin is unavailable")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Codex app-server stdout is unavailable")?;
    let mut stdout = BufReader::new(stdout);

    write_codex_message(
        &mut stdin,
        &serde_json::json!({
            "method": "initialize",
            "id": 1,
            "params": {"clientInfo": {"name": "rynna", "title": "Rynna", "version": env!("CARGO_PKG_VERSION")}}
        }),
        deadline,
    )
    .await?;
    read_codex_response(&mut stdout, 1, deadline).await?;
    write_codex_message(
        &mut stdin,
        &serde_json::json!({"method": "initialized", "params": {}}),
        deadline,
    )
    .await?;
    write_codex_message(
        &mut stdin,
        &serde_json::json!({"method": "account/read", "id": 2, "params": {"refreshToken": false}}),
        deadline,
    )
    .await?;
    let response = read_codex_response(&mut stdout, 2, deadline).await?;
    let account = response.pointer("/result/account");
    let Some(kind) = account
        .and_then(|account| account.get("type"))
        .and_then(|kind| kind.as_str())
    else {
        return Ok(OpenAiAccountResponse {
            connected: false,
            method: None,
            plan: None,
        });
    };
    let (method, plan) = match kind {
        "apiKey" => ("api_key", None),
        "chatgpt" => (
            "chatgpt",
            account
                .and_then(|account| account.get("planType"))
                .and_then(|plan| plan.as_str())
                .map(str::to_owned),
        ),
        _ => {
            return Ok(OpenAiAccountResponse {
                connected: false,
                method: None,
                plan: None,
            });
        }
    };
    Ok(OpenAiAccountResponse {
        connected: true,
        method: Some(method.to_owned()),
        plan,
    })
}

fn codex_program() -> PathBuf {
    env::var_os("RYNNA_CODEX_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("codex"))
}

fn claude_program() -> PathBuf {
    env::var_os("RYNNA_CLAUDE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("claude"))
}

pub fn prepare_codex_home(
    config_directory: impl AsRef<std::path::Path>,
) -> Result<PathBuf, String> {
    secure_codex_home(config_directory.as_ref().join("rynna").join("codex"))
}

fn configured_codex_home() -> Result<PathBuf, String> {
    match env::var_os("RYNNA_CODEX_HOME") {
        Some(home) => secure_codex_home(PathBuf::from(home)),
        None => prepare_codex_home(
            dirs::config_dir().ok_or("Rynna could not determine its configuration directory")?,
        ),
    }
}

fn selected_openai_codex_home(
    credential_selection: &OpenAiCredentialSelection,
    private_home: &std::path::Path,
) -> Option<PathBuf> {
    (!credential_selection.reuses_existing()).then(|| private_home.to_path_buf())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RespondRequest {
    #[serde(default)]
    pub selection: Option<rynna_core::ModelSelection>,
    #[serde(default)]
    pub session_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    pub prompt: String,
    #[serde(default)]
    pub history: Vec<Message>,
}

#[derive(Debug, Serialize)]
pub struct RespondResponse {
    pub message: Message,
}

#[derive(Debug, Serialize)]
pub struct ProfilesResponse {
    pub default_profile: String,
    pub provider_ids: Vec<String>,
    pub profiles: Vec<Profile>,
    pub configured_profiles: Vec<Profile>,
}

pub async fn context_with_profiles(
    profiles: &AgentProfiles,
    request: rynna_core::ContextRequest,
) -> Result<rynna_core::ContextResponse, String> {
    profiles
        .clone()
        .with_project(request.profile.as_deref(), request.project.as_deref())
        .map_err(|e| e.to_string())?
        .with_model_selection(request.profile.as_deref(), request.selection.as_ref())
        .map_err(|e| e.to_string())?
        .conversation_context(&request)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn session_title(
    profiles: State<'_, Arc<Mutex<AgentProfiles>>>,
    request: rynna_core::SessionTitleRequest,
) -> Result<String, String> {
    let profiles = profiles.lock().await.clone();
    profiles
        .session_title(&request)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn conversation_context(
    host: State<'_, Arc<rynna_workflows::host::Host>>,
    profiles: State<'_, Arc<Mutex<AgentProfiles>>>,
    request: rynna_core::ContextRequest,
) -> Result<rynna_core::ContextResponse, String> {
    let _lease = host.chat_lease(request.session_id).await?;
    let profiles = profiles.lock().await.clone();
    context_with_profiles(&profiles, request).await
}

pub async fn respond_with_agent(
    agent: &Agent,
    request: RespondRequest,
) -> Result<RespondResponse, String> {
    if request.selection.is_some() || request.project.is_some() {
        return Err("model and project selection require a profile".to_owned());
    }
    let message = agent
        .clone()
        .with_memory_session(request.session_id)
        .respond(&request.history, &request.prompt)
        .await
        .map_err(|error| error.to_string())?;
    Ok(RespondResponse { message })
}

pub async fn respond_with_profiles(
    profiles: &AgentProfiles,
    request: RespondRequest,
) -> Result<RespondResponse, String> {
    let message = profiles
        .clone()
        .with_memory_session(request.session_id)
        .with_project(request.profile.as_deref(), request.project.as_deref())
        .map_err(|error| error.to_string())?
        .with_model_selection(request.profile.as_deref(), request.selection.as_ref())
        .map_err(|error| error.to_string())?
        .respond(
            request.profile.as_deref(),
            &request.history,
            &request.prompt,
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(RespondResponse { message })
}

#[doc(hidden)]
pub async fn respond_with_locked_profiles(
    profiles: &Mutex<AgentProfiles>,
    request: RespondRequest,
) -> Result<RespondResponse, String> {
    let profiles = profiles.lock().await.clone();
    respond_with_profiles(&profiles, request).await
}

pub async fn respond_stream_with_profiles(
    profiles: &AgentProfiles,
    request: RespondRequest,
    on_delta: &mut (dyn for<'delta> FnMut(&'delta CompletionDelta) + Send),
) -> Result<RespondResponse, String> {
    let message = profiles
        .clone()
        .with_memory_session(request.session_id)
        .with_project(request.profile.as_deref(), request.project.as_deref())
        .map_err(|error| error.to_string())?
        .with_model_selection(request.profile.as_deref(), request.selection.as_ref())
        .map_err(|error| error.to_string())?
        .respond_stream(
            request.profile.as_deref(),
            &request.history,
            &request.prompt,
            on_delta,
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(RespondResponse { message })
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompletionDeltaEvent {
    Started,
    Thinking { content: String },
    Content { content: String },
    ToolStarted { call: rynna_core::ToolCall },
    ToolFinished { id: String },
}

impl From<&CompletionDelta> for CompletionDeltaEvent {
    fn from(delta: &CompletionDelta) -> Self {
        match delta {
            CompletionDelta::ToolStarted(call) => Self::ToolStarted { call: call.clone() },
            CompletionDelta::ToolFinished(id) => Self::ToolFinished { id: id.clone() },
            CompletionDelta::Thinking(content) => Self::Thinking {
                content: content.clone(),
            },
            CompletionDelta::Content(content) => Self::Content {
                content: content.clone(),
            },
        }
    }
}

pub fn list_profiles(
    runtime: &AgentProfiles,
    catalog: Option<&ProfileCatalog>,
) -> Result<ProfilesResponse, String> {
    let (provider_ids, catalog_profiles) = match catalog {
        Some(catalog) => (
            catalog.provider_ids(),
            catalog
                .resolve_all()
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|resolved| resolved.profile)
                .collect(),
        ),
        None => (Vec::new(), Vec::new()),
    };
    let configured_profiles = if catalog.is_some() {
        catalog_profiles.clone()
    } else {
        runtime.profiles()
    };
    let mut profiles = runtime.profiles();
    for profile in &mut profiles {
        profile.providers.retain(|provider| provider.enabled);
    }
    for mut profile in catalog_profiles {
        profile.providers.retain(|provider| provider.enabled);
        if !profiles
            .iter()
            .any(|candidate: &Profile| candidate.name == profile.name)
        {
            profiles.push(profile);
        }
    }
    profiles.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(ProfilesResponse {
        default_profile: runtime.default_profile().to_owned(),
        provider_ids,
        profiles,
        configured_profiles,
    })
}

#[tauri::command]
async fn respond(
    host: State<'_, Arc<rynna_workflows::host::Host>>,
    profiles: State<'_, Arc<Mutex<AgentProfiles>>>,
    request: RespondRequest,
) -> Result<RespondResponse, String> {
    let _lease = host.chat_lease(request.session_id).await?;
    respond_with_locked_profiles(&profiles, request).await
}

#[tauri::command]
async fn respond_stream(
    host: State<'_, Arc<rynna_workflows::host::Host>>,
    profiles: State<'_, Arc<Mutex<AgentProfiles>>>,
    request: RespondRequest,
    on_event: Channel<CompletionDeltaEvent>,
    responses: State<'_, responses::Responses>,
    response_id: Option<uuid::Uuid>,
) -> Result<RespondResponse, String> {
    let execute = async {
        let _lease = host.chat_lease(request.session_id).await?;
        let profiles = profiles.lock().await.clone();
        let mut on_delta = |delta: &CompletionDelta| {
            let _ = on_event.send(CompletionDeltaEvent::from(delta));
        };
        respond_stream_with_profiles(&profiles, request, &mut on_delta).await
    };
    if let Some(id) = response_id {
        let (_guard, cancelled) = responses.register(id)?;
        on_event
            .send(CompletionDeltaEvent::Started)
            .map_err(|e| e.to_string())?;
        tokio::select! {
            biased;
            _ = cancelled => Err("Response stopped".into()),
            result = execute => result,
        }
    } else {
        execute.await
    }
}

#[tauri::command]
fn cancel_response(responses: State<'_, responses::Responses>, response_id: uuid::Uuid) {
    responses.cancel(response_id);
}

#[tauri::command]
async fn profiles(
    catalog: State<'_, Arc<Mutex<ProfileCatalog>>>,
    profiles: State<'_, Arc<Mutex<AgentProfiles>>>,
) -> Result<ProfilesResponse, String> {
    let catalog = catalog.lock().await;
    let profiles = profiles.lock().await;
    list_profiles(&profiles, Some(&catalog))
}

#[tauri::command]
async fn create_profile(
    catalog: State<'_, Arc<Mutex<ProfileCatalog>>>,
    profiles: State<'_, Arc<Mutex<AgentProfiles>>>,
    profile: Profile,
) -> Result<Profile, String> {
    let mut catalog = catalog.lock().await;
    let mut runtime = profiles.lock().await;
    create_saved_profile(&mut catalog, &mut runtime, profile)
}

#[tauri::command]
async fn update_profile(
    host: State<'_, Arc<rynna_workflows::host::Host>>,
    catalog: State<'_, Arc<Mutex<ProfileCatalog>>>,
    profiles: State<'_, Arc<Mutex<AgentProfiles>>>,
    provider_settings: State<'_, Mutex<ProviderSettingsStore>>,
    name: String,
    profile: Profile,
) -> Result<Profile, String> {
    let _admission = host.admission.lock().await;
    let mut catalog = catalog.lock().await;
    let original = catalog.resolve(&name).map_err(|e| e.to_string())?.profile;
    if profile.name != name
        || profile.projects != original.projects
        || profile.default_project_directory != original.default_project_directory
    {
        host.ensure_profile_idle(&name).await?;
    }
    let mut runtime = profiles.lock().await;
    let mut provider_settings = provider_settings.lock().await;
    update_saved_profile(
        &mut catalog,
        &mut runtime,
        Some(&mut provider_settings),
        &name,
        profile,
    )
}

#[tauri::command]
async fn delete_profile(
    host: State<'_, Arc<rynna_workflows::host::Host>>,
    catalog: State<'_, Arc<Mutex<ProfileCatalog>>>,
    profiles: State<'_, Arc<Mutex<AgentProfiles>>>,
    provider_settings: State<'_, Mutex<ProviderSettingsStore>>,
    name: String,
) -> Result<(), String> {
    let _admission = host.admission.lock().await;
    host.ensure_profile_idle(&name).await?;
    let mut catalog = catalog.lock().await;
    let mut runtime = profiles.lock().await;
    let mut provider_settings = provider_settings.lock().await;
    delete_saved_profile(
        &mut catalog,
        &mut runtime,
        Some(&mut provider_settings),
        &name,
    )
}

#[doc(hidden)]
pub fn create_saved_profile(
    catalog: &mut ProfileCatalog,
    _runtime: &mut AgentProfiles,
    profile: Profile,
) -> Result<Profile, String> {
    catalog
        .add_profile(profile)
        .map_err(|error| error.to_string())
}

#[doc(hidden)]
pub fn update_saved_profile(
    catalog: &mut ProfileCatalog,
    runtime: &mut AgentProfiles,
    provider_settings: Option<&mut ProviderSettingsStore>,
    original_name: &str,
    profile: Profile,
) -> Result<Profile, String> {
    let saved = match provider_settings {
        Some(provider_settings) => rynna_config::profile_update::update_profile_with_settings(
            catalog,
            provider_settings,
            original_name,
            profile,
        )
        .map_err(|error| error.to_string()),
        None => catalog
            .update_profile(original_name, profile)
            .map_err(|error| error.to_string()),
    }?;
    if saved.name == original_name && runtime.contains(original_name) {
        runtime
            .set_project_configuration(
                original_name,
                saved.default_project_directory.clone(),
                saved.projects.clone(),
            )
            .map_err(|error| error.to_string())?;
        runtime
            .set_subagents(original_name, saved.subagents.clone())
            .map_err(|error| error.to_string())?;
    }
    Ok(saved)
}

#[doc(hidden)]
pub fn delete_saved_profile(
    catalog: &mut ProfileCatalog,
    runtime: &mut AgentProfiles,
    provider_settings: Option<&mut ProviderSettingsStore>,
    name: &str,
) -> Result<(), String> {
    if runtime.contains(name) && runtime.len() <= 1 {
        return Err("the last runtime profile cannot be deleted before restart".to_owned());
    }
    catalog
        .delete_profile(name)
        .map_err(|error| error.to_string())?;
    if let Some(provider_settings) = provider_settings {
        McpSettingsStore::new(provider_settings.mcp_settings_path())
            .delete_profile(name)
            .map_err(|error| error.to_string())?;
        MemorySettingsStore::new(provider_settings.memory_settings_path())
            .delete_profile(name)
            .map_err(|error| error.to_string())?;
        provider_settings
            .delete_profile(name)
            .map_err(|error| error.to_string())?;
    }
    if runtime.contains(name) {
        runtime.remove(name).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn openai_account(
    credential_selection: State<'_, OpenAiCredentialSelection>,
) -> Result<OpenAiAccountResponse, String> {
    let private_home = configured_codex_home()?;
    match selected_openai_codex_home(&credential_selection, &private_home) {
        Some(home) => openai_account_with_program_and_home(codex_program(), home).await,
        None => openai_account_with_program(codex_program()).await,
    }
}

#[tauri::command]
async fn existing_openai_account() -> Result<OpenAiAccountResponse, String> {
    openai_account_with_program(codex_program()).await
}

#[tauri::command]
async fn connect_openai(
    authentication_lock: State<'_, OpenAiAuthenticationLock>,
    request: OpenAiConnectRequest,
) -> Result<OpenAiAccountResponse, String> {
    let _authentication = authentication_lock.acquire().await;
    connect_openai_with_program_and_home(codex_program(), configured_codex_home()?, request).await
}

#[tauri::command]
async fn list_providers(
    provider_settings: State<'_, Mutex<ProviderSettingsStore>>,
    profile: String,
) -> Result<Vec<ConfiguredProvider>, String> {
    let mut store = provider_settings.lock().await;
    store.refresh().map_err(|error| error.to_string())?;
    Ok(store.list(&profile))
}

async fn configured_provider_from_input(
    input: ProviderInput,
) -> Result<ConfiguredProvider, String> {
    match input {
        ProviderInput::Ollama { api_base } => Ok(ConfiguredProvider::Ollama { api_base }),
        ProviderInput::Mlx { api_base } => Ok(ConfiguredProvider::Mlx { api_base }),
        ProviderInput::OpenRouter => Ok(ConfiguredProvider::OpenRouter),
        ProviderInput::OpenAi {
            authentication,
            api_key,
            reuse_existing,
        } => {
            if reuse_existing {
                if authentication != OpenAiAuthentication::Chatgpt {
                    return Err(
                        "only ChatGPT subscriptions can reuse existing credentials".to_owned()
                    );
                }
                verify_existing_openai_credentials_with_program(codex_program()).await?;
                return Ok(ConfiguredProvider::OpenAi {
                    authentication,
                    reuse_existing: true,
                });
            }
            let request = match authentication {
                OpenAiAuthentication::Chatgpt => OpenAiConnectRequest::Chatgpt,
                OpenAiAuthentication::ApiKey => OpenAiConnectRequest::ApiKey {
                    api_key: api_key.ok_or("OpenAI API key is required")?,
                },
            };
            connect_openai_with_program_and_home(
                codex_program(),
                configured_codex_home()?,
                request,
            )
            .await?;
            Ok(ConfiguredProvider::OpenAi {
                authentication,
                reuse_existing: false,
            })
        }
        ProviderInput::Anthropic { authentication } => {
            if authentication == AnthropicAuthentication::Subscription {
                authenticate_anthropic_subscription_with_program(
                    claude_program(),
                    Duration::from_secs(300),
                )
                .await?;
            }
            Ok(ConfiguredProvider::Anthropic { authentication })
        }
    }
}

async fn authenticate_anthropic_subscription_with_program(
    program: PathBuf,
    login_timeout: Duration,
) -> Result<(), String> {
    let mut command = Command::new(program);
    isolate_claude_subscription_environment(&mut command);
    command
        .args(["auth", "login", "--claudeai"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for name in CLAUDE_SUBSCRIPTION_CONFLICTING_ENV_VARS {
        command.env_remove(name);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start Claude subscription sign-in: {error}"))?;
    let status = match timeout(login_timeout, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            terminate_child(&mut child).await;
            return Err(format!(
                "failed to wait for Claude subscription sign-in: {error}"
            ));
        }
        Err(_) => {
            terminate_child(&mut child).await;
            return Err("Claude subscription sign-in timed out".to_owned());
        }
    };
    if !status.success() {
        return Err("Claude subscription sign-in did not complete".to_owned());
    }
    Ok(())
}

async fn verify_existing_openai_credentials_with_program(
    program: impl AsRef<OsStr>,
) -> Result<(), String> {
    let output = timeout(
        Duration::from_secs(30),
        Command::new(program)
            .args(["login", "status"])
            .env_remove("CODEX_HOME")
            .env_remove("RYNNA_CODEX_HOME")
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "OpenAI credential check timed out".to_owned())?
    .map_err(|error| format!("failed to check existing ChatGPT credentials: {error}"))?;
    if !output.status.success()
        || !(String::from_utf8_lossy(&output.stdout).contains("Logged in using ChatGPT")
            || String::from_utf8_lossy(&output.stderr).contains("Logged in using ChatGPT"))
    {
        return Err("existing ChatGPT credentials are unavailable".to_owned());
    }
    Ok(())
}

#[tauri::command]
async fn list_provider_models(
    catalog: State<'_, Arc<Mutex<ProfileCatalog>>>,
    provider_settings: State<'_, Mutex<ProviderSettingsStore>>,
    profile: String,
    provider: String,
) -> Result<Vec<String>, String> {
    let provider = catalog
        .lock()
        .await
        .model_provider(&profile, &provider)
        .map_err(|error| error.to_string())?;
    let settings = provider_settings.lock().await.path().to_owned();
    rynna_provider_openai::models::list_models(provider, settings, &profile).await
}

#[tauri::command]
async fn create_provider(
    catalog: State<'_, Arc<Mutex<ProfileCatalog>>>,
    authentication_lock: State<'_, OpenAiAuthenticationLock>,
    provider_settings: State<'_, Mutex<ProviderSettingsStore>>,
    provider: ProviderInput,
    profile: String,
) -> Result<ConfiguredProvider, String> {
    let _authentication = authentication_lock.acquire().await;
    catalog
        .lock()
        .await
        .resolve(&profile)
        .map_err(|error| error.to_string())?;
    let mut store = provider_settings.lock().await;
    store.refresh().map_err(|error| error.to_string())?;
    if store.get(&profile, provider.kind()).is_some() {
        return Err(format!(
            "provider `{}` is already configured",
            provider.kind()
        ));
    }
    drop(store);
    let provider = configured_provider_from_input(provider).await?;
    let mut catalog = catalog.lock().await;
    let mut store = provider_settings.lock().await;
    rynna_config::profile_update::save_provider_with_catalog(
        &mut catalog,
        &mut store,
        &profile,
        provider.clone(),
        false,
    )
    .map_err(|error| error.to_string())?;
    Ok(provider)
}

#[tauri::command]
async fn update_provider(
    catalog: State<'_, Arc<Mutex<ProfileCatalog>>>,
    authentication_lock: State<'_, OpenAiAuthenticationLock>,
    provider_settings: State<'_, Mutex<ProviderSettingsStore>>,
    provider: ProviderInput,
    profile: String,
) -> Result<ConfiguredProvider, String> {
    let _authentication = authentication_lock.acquire().await;
    catalog
        .lock()
        .await
        .resolve(&profile)
        .map_err(|error| error.to_string())?;
    let mut store = provider_settings.lock().await;
    store.refresh().map_err(|error| error.to_string())?;
    if store.get(&profile, provider.kind()).is_none() {
        return Err(format!("provider `{}` is not configured", provider.kind()));
    }
    drop(store);
    let provider = configured_provider_from_input(provider).await?;
    let mut catalog = catalog.lock().await;
    let mut store = provider_settings.lock().await;
    rynna_config::profile_update::save_provider_with_catalog(
        &mut catalog,
        &mut store,
        &profile,
        provider.clone(),
        true,
    )
    .map_err(|error| error.to_string())?;
    Ok(provider)
}

#[tauri::command]
async fn delete_provider(
    provider_settings: State<'_, Mutex<ProviderSettingsStore>>,
    kind: String,
    profile: String,
) -> Result<(), String> {
    provider_settings
        .lock()
        .await
        .delete(&profile, &kind)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn ensure_memory_profile(
    catalog: &ProfileCatalog,
    runtime: &AgentProfiles,
    profile: &str,
) -> Result<(), String> {
    if catalog.resolve(profile).is_ok()
        || (profile == OPENAI_ACCOUNT_PROFILE && runtime.contains(profile))
    {
        Ok(())
    } else {
        Err("memory profile is not defined".to_owned())
    }
}

/// Runs one blocking settings-store operation off the async runtime.
///
/// These stores read and write YAML under an exclusive `flock`. Tauri commands
/// are async, so doing that inline would stall a runtime worker for the whole
/// I/O. Callers keep holding their tokio guards across this await, so lock
/// ordering is unchanged.
async fn offload<S, T, F>(store: S, operation: F) -> Result<T, String>
where
    S: Send + 'static,
    T: Send + 'static,
    F: FnOnce(S) -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(move || operation(store))
        .await
        .map_err(|_| "settings task failed".to_owned())?
}

#[tauri::command]
async fn get_mcp_settings(
    catalog: State<'_, Arc<Mutex<ProfileCatalog>>>,
    profiles: State<'_, Arc<Mutex<AgentProfiles>>>,
    provider_settings: State<'_, Mutex<ProviderSettingsStore>>,
    profile: String,
) -> Result<McpSettings, String> {
    let catalog = catalog.lock().await;
    let runtime = profiles.lock().await;
    ensure_memory_profile(&catalog, &runtime, &profile)?;
    let store = provider_settings.lock().await;
    offload(
        McpSettingsStore::new(store.mcp_settings_path()),
        move |store| store.load(&profile).map_err(|error| error.to_string()),
    )
    .await
}

#[tauri::command]
async fn save_mcp_settings(
    catalog: State<'_, Arc<Mutex<ProfileCatalog>>>,
    provider_settings: State<'_, Mutex<ProviderSettingsStore>>,
    profiles: State<'_, Arc<Mutex<AgentProfiles>>>,
    profile: String,
    settings: McpSettings,
) -> Result<McpSettings, String> {
    let catalog = catalog.lock().await;
    let mut profiles = profiles.lock().await;
    ensure_memory_profile(&catalog, &profiles, &profile)?;
    let store = provider_settings.lock().await;
    // Kept inline: the runtime update below must not be separated from this write by
    // an await, or a dropped command future could persist settings while the running
    // profile keeps the old tool source. Tauri's borrowed State makes the server's
    // spawn-the-whole-section approach impractical here.
    let settings = McpSettingsStore::new(store.mcp_settings_path())
        .save(&profile, settings)
        .map_err(|error| error.to_string())?;
    let source = Some(Arc::new(McpToolSource(settings.clone())) as Arc<dyn rynna_core::ToolSource>);
    if profiles.contains(&profile) {
        profiles
            .set_tool_source(&profile, source)
            .map_err(|error| error.to_string())?;
    }
    Ok(settings)
}

#[tauri::command]
async fn get_memory_settings(
    catalog: State<'_, Arc<Mutex<ProfileCatalog>>>,
    profiles: State<'_, Arc<Mutex<AgentProfiles>>>,
    provider_settings: State<'_, Mutex<ProviderSettingsStore>>,
    profile: String,
) -> Result<MemorySettingsResponse, String> {
    let catalog = catalog.lock().await;
    let runtime = profiles.lock().await;
    ensure_memory_profile(&catalog, &runtime, &profile)?;
    let store = provider_settings.lock().await;
    offload(
        MemorySettingsStore::new(store.memory_settings_path()),
        move |store| {
            store
                .load(&profile)
                .map(|settings| settings.response())
                .map_err(|error| error.to_string())
        },
    )
    .await
}

#[tauri::command]
async fn save_memory_settings(
    catalog: State<'_, Arc<Mutex<ProfileCatalog>>>,
    provider_settings: State<'_, Mutex<ProviderSettingsStore>>,
    profiles: State<'_, Arc<Mutex<AgentProfiles>>>,
    profile: String,
    settings: MemorySettings,
) -> Result<MemorySettingsResponse, String> {
    let catalog = catalog.lock().await;
    let mut profiles = profiles.lock().await;
    ensure_memory_profile(&catalog, &profiles, &profile)?;
    let store = provider_settings.lock().await;
    // Kept inline for the same reason as save_mcp_settings.
    let settings = MemorySettingsStore::new(store.memory_settings_path())
        .save(&profile, settings)
        .map_err(|error| error.to_string())?;
    let memory = configured_memory(&settings).map_err(|error| error.to_string())?;
    if profiles.contains(&profile) {
        profiles
            .set_memory_provider(&profile, memory)
            .map_err(|error| error.to_string())?;
    }
    Ok(settings.response())
}

pub fn run() {
    let mut catalog = configured_catalog()
        .unwrap_or_else(|error| panic!("failed to load Rynna configuration: {error}"));
    let provider_settings = configured_provider_settings()
        .unwrap_or_else(|error| panic!("failed to load Rynna provider settings: {error}"));
    catalog
        .register_openai_accounts(&provider_settings)
        .unwrap_or_else(|error| panic!("failed to register OpenAI account models: {error}"));
    let credential_profile = optional_env("RYNNA_PROFILE")
        .unwrap_or_else(|error| panic!("failed to select Rynna profile: {error}"))
        .unwrap_or_else(|| catalog.default_profile().to_owned());
    let credential_selection = OpenAiCredentialSelection::new(
        openai_account_reuses_existing_credentials(&provider_settings, &credential_profile),
    );
    let mut configured = configured_profiles(credential_selection.clone(), &catalog)
        .unwrap_or_else(|error| panic!("failed to configure Rynna model provider: {error}"));

    let mcp_store = McpSettingsStore::new(provider_settings.mcp_settings_path());
    for profile in configured.profiles() {
        let settings = mcp_store
            .load(&profile.name)
            .unwrap_or_else(|error| panic!("failed to load Rynna MCP settings: {error}"));
        configured
            .set_tool_source(&profile.name, Some(Arc::new(McpToolSource(settings))))
            .expect("existing profile");
    }
    let memory_store = MemorySettingsStore::new(provider_settings.memory_settings_path());
    for profile in configured.profiles() {
        let settings = memory_store
            .load(&profile.name)
            .unwrap_or_else(|error| panic!("failed to load Rynna memory settings: {error}"));
        let memory = configured_memory(&settings)
            .unwrap_or_else(|error| panic!("failed to configure Rynna memory provider: {error}"));
        configured
            .set_memory_provider(&profile.name, memory)
            .expect("existing profile");
    }

    let configured = Arc::new(Mutex::new(configured));
    let catalog = Arc::new(Mutex::new(catalog));
    let path = env::var_os("RYNNA_WORKFLOW_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            provider_settings
                .memory_settings_path()
                .with_file_name("workflow-runs")
        });
    let workflow_host = Arc::new(rynna_workflows::host::Host::new(
        configured.clone(),
        Some(catalog.clone()),
        path,
    ));
    let shutdown_host = workflow_host.clone();
    tauri::Builder::default()
        .setup(|app| {
            // WKWebView requires a download handler even for local transcript blobs.
            tauri::WebviewWindowBuilder::from_config(app, &app.config().app.windows[0])?
                .on_download(|_, event| match event {
                    tauri::webview::DownloadEvent::Requested { url, .. } => url.scheme() == "blob",
                    _ => true,
                })
                .build()?;
            Ok(())
        })
        .manage(responses::Responses::default())
        .manage(configured)
        .manage(catalog)
        .manage(workflow_host)
        .manage(credential_selection)
        .manage(Mutex::new(provider_settings))
        .manage(OpenAiAuthenticationLock::default())
        .invoke_handler(tauri::generate_handler![
            workflows::list_workflows,
            workflows::read_workflow,
            workflows::save_workflow,
            workflows::delete_workflow,
            workflows::start_workflow,
            workflows::list_workflow_runs,
            workflows::read_workflow_run,
            workflows::control_workflow,
            conversation_context,
            session_title,
            respond,
            respond_stream,
            cancel_response,
            profiles,
            create_profile,
            update_profile,
            delete_profile,
            openai_account,
            existing_openai_account,
            connect_openai,
            get_mcp_settings,
            save_mcp_settings,
            get_memory_settings,
            save_memory_settings,
            list_providers,
            list_provider_models,
            create_provider,
            update_provider,
            delete_provider
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Rynna desktop application")
        .run(move |_, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                tauri::async_runtime::block_on(shutdown_host.shutdown());
                tauri::async_runtime::block_on(rynna_core::workflow_runs::shutdown_workflows());
                tauri::async_runtime::block_on(rynna_core::flush_memory_writes());
            }
        });
}

fn configured_provider_settings() -> Result<ProviderSettingsStore, String> {
    match env::var_os("RYNNA_PROVIDER_CONFIG") {
        Some(path) => ProviderSettingsStore::load(PathBuf::from(path)),
        None => ProviderSettingsStore::load_default(),
    }
    .map_err(|error| error.to_string())
}

fn configured_catalog() -> Result<ProfileCatalog, String> {
    match optional_env("RYNNA_CONFIG")? {
        Some(path) => ProfileCatalog::load(path),
        None => ProfileCatalog::load_default(),
    }
    .map_err(|error| error.to_string())
}

fn configured_profiles(
    credential_selection: OpenAiCredentialSelection,
    catalog: &ProfileCatalog,
) -> Result<AgentProfiles, String> {
    let default_profile =
        optional_env("RYNNA_PROFILE")?.unwrap_or_else(|| catalog.default_profile().to_owned());
    catalog
        .resolve(&default_profile)
        .map_err(|error| error.to_string())?;

    let mut configured = Vec::new();
    for mut profile in catalog.resolve_all().map_err(|error| error.to_string())? {
        let api_key_override = if profile.profile.name == default_profile {
            if let Some(api_base) = optional_env("RYNNA_API_BASE")?
                && let Some(provider) = profile.providers.first_mut()
            {
                provider.api_base = api_base;
            }
            if let Some(model) = optional_env("RYNNA_MODEL")? {
                profile.override_default_model(&model);
            }
            if let Some(system_prompt) = optional_env("RYNNA_SYSTEM_PROMPT")? {
                profile.system_prompt = system_prompt;
            }
            optional_env("RYNNA_API_KEY")?
        } else {
            None
        };
        let agent = configured_agent(&profile, api_key_override)?;
        configured.push((profile.profile, agent));
    }

    if configured
        .iter()
        .any(|(profile, _)| profile.name == OPENAI_ACCOUNT_PROFILE)
    {
        return Err(format!(
            "profile name `{OPENAI_ACCOUNT_PROFILE}` is reserved for the desktop OpenAI account"
        ));
    }
    let openai_profile = Profile {
        name: OPENAI_ACCOUNT_PROFILE.to_owned(),
        providers: vec![ProfileProvider {
            provider: "openai".to_owned(),
            model: "Codex default".to_owned(),
            enabled: true,
            is_default: true,
            context_window: None,
        }],
        active_skills: Vec::new(),
        mcp_servers: Vec::new(),
        capabilities: Vec::new(),
        default_project_directory: ".".into(),
        projects: Vec::new(),
        subagents: Vec::new(),
    };
    let openai_provider: Arc<dyn ModelProvider> =
        Arc::new(CodexAppServerProvider::with_selectable_home(
            codex_program(),
            configured_codex_home()?,
            credential_selection,
            None,
        ));
    let openai_agent = Agent::new(
        openai_provider,
        "You are Rynna, a careful and capable AI software agent.",
    );
    configured.push((openai_profile, openai_agent));

    AgentProfiles::new(default_profile, configured).map_err(|error| error.to_string())
}

fn openai_account_reuses_existing_credentials(
    provider_settings: &ProviderSettingsStore,
    profile: &str,
) -> bool {
    provider_settings
        .get(profile, "openai")
        .is_some_and(configured_provider_reuses_existing_credentials)
}

fn configured_provider_reuses_existing_credentials(provider: &ConfiguredProvider) -> bool {
    matches!(
        provider,
        ConfiguredProvider::OpenAi {
            authentication: OpenAiAuthentication::Chatgpt,
            reuse_existing: true,
        }
    )
}

fn optional_env(name: &str) -> Result<Option<String>, String> {
    decode_optional_env(name, env::var(name))
}

fn decode_optional_env(
    name: &str,
    value: Result<String, env::VarError>,
) -> Result<Option<String>, String> {
    match value {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(format!(
            "environment variable `{name}` is not valid Unicode"
        )),
    }
}

fn configured_agent(
    profile: &ResolvedProfile,
    api_key_override: Option<String>,
) -> Result<Agent, String> {
    let mut providers = profile
        .providers
        .iter()
        .enumerate()
        .map(|(index, provider)| {
            configured_model_provider(
                &profile.profile.name,
                provider,
                if index == 0 {
                    api_key_override.clone()
                } else {
                    None
                },
            )
        })
        .collect::<Result<Vec<_>, String>>()?;
    let model_options = profile
        .providers
        .iter()
        .map(|p| rynna_core::ProfileProvider {
            provider: p.name.clone(),
            model: p.model.clone(),
            enabled: true,
            is_default: false,
            context_window: None,
        })
        .zip(providers.iter().cloned())
        .collect();
    let provider: Arc<dyn ModelProvider> = if providers.len() == 1 {
        providers.remove(0)
    } else {
        Arc::new(FallbackProvider::new(providers).map_err(|error| error.to_string())?)
    };

    compose_agent(profile, provider).map(|agent| agent.with_model_options(model_options))
}

fn configured_model_provider(
    profile_name: &str,
    provider: &ResolvedProvider,
    api_key_override: Option<String>,
) -> Result<Arc<dyn ModelProvider>, String> {
    let api_key = match api_key_override {
        Some(api_key) => Some(api_key),
        None => provider
            .api_key_env
            .as_deref()
            .map(|name| {
                env::var(name).map_err(|_| {
                    format!(
                        "profile `{}` requires provider API key environment variable `{name}`",
                        profile_name
                    )
                })
            })
            .transpose()?,
    };
    let configured: Arc<dyn ModelProvider> = match provider.provider_kind {
        ProviderKind::OpenAiAccount => Arc::new(CodexAppServerProvider::for_profile(
            configured_provider_settings()?.path().to_owned(),
            profile_name,
            &provider.model,
        )?),
        ProviderKind::OpenAiCompatible | ProviderKind::Mlx => Arc::new(
            OpenAiCompatibleProvider::new(&provider.api_base, &provider.model, api_key)
                .map_err(|error| error.to_string())?,
        ),
        ProviderKind::AnthropicMessages => Arc::new(
            AnthropicMessagesProvider::with_base_url(
                &provider.api_base,
                &provider.model,
                api_key.ok_or_else(|| "Anthropic API key is required".to_owned())?,
            )
            .map_err(|error| error.to_string())?,
        ),
        ProviderKind::ClaudeSubscription => Arc::new(ClaudeCodeProvider::new(
            &provider.claude_program,
            &provider.model,
        )),
    };
    Ok(configured)
}

#[doc(hidden)]
pub fn compose_agent(
    profile: &ResolvedProfile,
    provider: Arc<dyn ModelProvider>,
) -> Result<Agent, String> {
    let tools = configured_tools(profile)?;
    if tools.is_empty() {
        Ok(Agent::new(provider, profile.system_prompt.clone()))
    } else {
        Agent::with_tools(provider, profile.system_prompt.clone(), tools)
            .map_err(|error| error.to_string())
    }
}

fn configured_tools(profile: &ResolvedProfile) -> Result<Vec<Arc<dyn Tool>>, String> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    if let Some(skills) =
        rynna_skills::SkillsTool::load(&profile.profile.active_skills, &profile.skills_directory)
            .map_err(|error| error.to_string())?
    {
        tools.push(Arc::new(skills));
    }
    for capability in &profile.capabilities {
        match capability {
            ResolvedCapability::Command(capability) => {
                tools.push(Arc::new(
                    CommandTool::new(CommandConfig {
                        working_directory: capability.working_directory.clone(),
                        programs: capability.programs.clone(),
                        timeout_seconds: capability.timeout_seconds,
                        max_output_bytes: capability.max_output_bytes,
                    })
                    .map_err(|error| error.to_string())?,
                ));
            }
            ResolvedCapability::FileSystem(capability) => {
                let mut config = FileSystemConfig::new(&capability.root);
                config.read_only = capability.read_only;
                config.allowed_patterns = capability.allowed_patterns.clone();
                if let Some(patterns) = &capability.denied_patterns {
                    config.denied_patterns.clone_from(patterns);
                }
                if let Some(patterns) = &capability.protected_patterns {
                    config.protected_patterns.clone_from(patterns);
                }
                if let Some(limit) = capability.max_read_bytes {
                    config.max_read_bytes = limit;
                }
                if let Some(limit) = capability.max_results {
                    config.max_results = limit;
                }
                if let Some(limit) = capability.max_traversal_files {
                    config.max_traversal_files = limit;
                }
                if let Some(limit) = capability.max_traversal_depth {
                    config.max_traversal_depth = limit;
                }
                if let Some(limit) = capability.max_search_bytes {
                    config.max_search_bytes = limit;
                }
                tools.extend(
                    FileSystemToolset::new(config)
                        .map_err(|error| error.to_string())?
                        .tools(),
                );
            }
        }
    }
    Ok(tools)
}

#[cfg(test)]
mod tests {
    use std::env::VarError;
    use std::ffi::OsString;
    use std::path::PathBuf;

    use tokio::io::BufReader;
    use tokio::time::{Duration, Instant};

    use rynna_config::{ConfiguredProvider, OpenAiAuthentication, ProviderSettingsStore};

    use super::{
        MAX_CODEX_MESSAGE_BYTES, OpenAiAuthenticationLock, OpenAiCredentialSelection,
        authenticate_anthropic_subscription_with_program, decode_optional_env,
        openai_account_reuses_existing_credentials, read_codex_message, selected_openai_codex_home,
        verify_existing_openai_credentials_with_program, write_codex_message,
    };

    #[test]
    fn provider_commands_resolve_the_managed_catalog_and_persist_changes() {
        use std::sync::Arc;
        use tauri::test::{mock_builder, mock_context, noop_assets};
        use tokio::sync::Mutex;

        let directory = tempfile::tempdir().unwrap();
        let settings_path = directory.path().join("providers.yaml");
        let app = mock_builder()
            .manage(Arc::new(Mutex::new(
                rynna_config::ProfileCatalog::built_in(),
            )))
            .manage(Mutex::new(
                ProviderSettingsStore::load(&settings_path).unwrap(),
            ))
            .manage(OpenAiAuthenticationLock::default())
            .invoke_handler(tauri::generate_handler![
                super::create_provider,
                super::update_provider
            ])
            .build(mock_context(noop_assets()))
            .unwrap();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        for (command, api_base) in [
            ("create_provider", "http://localhost:11434/v1"),
            ("update_provider", "http://localhost:11435/v1"),
        ] {
            let provider = serde_json::json!({"kind": "ollama", "api_base": api_base});
            let response = tauri::test::get_ipc_response(
                &webview,
                tauri::webview::InvokeRequest {
                    cmd: command.into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: if cfg!(any(windows, target_os = "android")) {
                        "http://tauri.localhost"
                    } else {
                        "tauri://localhost"
                    }
                    .parse()
                    .unwrap(),
                    body: tauri::ipc::InvokeBody::Json(
                        serde_json::json!({"profile": "default", "provider": provider}),
                    ),
                    headers: Default::default(),
                    invoke_key: tauri::test::INVOKE_KEY.to_owned(),
                },
            )
            .unwrap_or_else(|error| panic!("{command} failed: {error}"));
            assert_eq!(
                response.deserialize::<serde_json::Value>().unwrap(),
                provider
            );
            assert_eq!(
                ProviderSettingsStore::load(&settings_path)
                    .unwrap()
                    .get("default", "ollama"),
                Some(&ConfiguredProvider::Ollama {
                    api_base: api_base.into()
                })
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn claude_login_timeout_kills_and_reaps_the_child() {
        let program = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/hanging_claude_login.sh");
        let pid_file = PathBuf::from(format!("{}.pid", program.display()));
        let _ = std::fs::remove_file(&pid_file);

        let error =
            authenticate_anthropic_subscription_with_program(program, Duration::from_millis(250))
                .await
                .unwrap_err();
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        let running = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        if running {
            let _ = std::process::Command::new("kill")
                .args(["-9", pid.trim()])
                .status();
        }
        let _ = std::fs::remove_file(pid_file);

        assert!(error.contains("timed out"));
        assert!(!running, "timed-out Claude login child {pid} remained");
    }

    #[tokio::test]
    async fn openai_authentication_lock_serializes_credential_mutations() {
        let lock = OpenAiAuthenticationLock::default();
        let first = lock.acquire().await;

        assert!(
            tokio::time::timeout(Duration::from_millis(10), lock.acquire())
                .await
                .is_err()
        );

        drop(first);
        let _released = tokio::time::timeout(Duration::from_millis(50), lock.acquire())
            .await
            .expect("the credential lock should be released");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn existing_chatgpt_reuse_rejects_an_api_key_login() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("codex");
        std::fs::write(
            &program,
            "#!/bin/sh\nprintf '%s\\n' 'Logged in using an API key'\n",
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();

        let error = verify_existing_openai_credentials_with_program(&program)
            .await
            .unwrap_err();

        assert_eq!(error, "existing ChatGPT credentials are unavailable");

        std::fs::write(
            &program,
            "#!/bin/sh\nprintf '%s\\n' 'Logged in using ChatGPT'\n",
        )
        .unwrap();
        verify_existing_openai_credentials_with_program(&program)
            .await
            .unwrap();
    }

    #[test]
    fn configured_openai_reuse_selects_the_normal_codex_account() {
        let directory = tempfile::tempdir().unwrap();
        let mut settings =
            ProviderSettingsStore::load(directory.path().join("providers.yaml")).unwrap();
        settings
            .add(
                "work",
                ConfiguredProvider::OpenAi {
                    authentication: OpenAiAuthentication::Chatgpt,
                    reuse_existing: true,
                },
            )
            .unwrap();

        assert!(openai_account_reuses_existing_credentials(
            &settings, "work"
        ));
        assert!(!openai_account_reuses_existing_credentials(
            &settings, "personal"
        ));
    }

    #[test]
    fn openai_account_status_uses_the_selected_credential_home() {
        let selection = OpenAiCredentialSelection::new(false);
        let private_home = PathBuf::from("/private/rynna/codex");

        assert_eq!(
            selected_openai_codex_home(&selection, &private_home),
            Some(private_home.clone())
        );

        selection.set_reuse_existing(true);
        assert_eq!(selected_openai_codex_home(&selection, &private_home), None);
    }

    #[test]
    fn configured_environment_values_must_be_valid_unicode() {
        let error = decode_optional_env(
            "RYNNA_CONFIG",
            Err(VarError::NotUnicode(OsString::from("invalid"))),
        )
        .unwrap_err();

        assert_eq!(
            error,
            "environment variable `RYNNA_CONFIG` is not valid Unicode"
        );
    }

    #[tokio::test]
    async fn codex_message_reader_rejects_an_oversized_unterminated_line() {
        let input = vec![b'x'; MAX_CODEX_MESSAGE_BYTES + 1];
        let mut reader = BufReader::new(input.as_slice());

        let error = read_codex_message(&mut reader, Instant::now() + Duration::from_secs(1))
            .await
            .unwrap_err();

        assert_eq!(error, "Codex app-server message exceeded the size limit");
    }

    #[tokio::test]
    async fn codex_message_reader_observes_the_operation_deadline() {
        let (_writer, reader) = tokio::io::duplex(16);
        let mut reader = BufReader::new(reader);

        let error = read_codex_message(&mut reader, Instant::now() + Duration::from_millis(1))
            .await
            .unwrap_err();

        assert_eq!(error, "Codex app-server response timed out");
    }

    #[tokio::test]
    async fn codex_message_writer_rejects_an_oversized_request() {
        let (mut writer, _reader) = tokio::io::duplex(16);
        let message = serde_json::json!({"value": "x".repeat(MAX_CODEX_MESSAGE_BYTES)});

        let error = write_codex_message(
            &mut writer,
            &message,
            Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap_err();

        assert_eq!(error, "Codex app-server request exceeded the size limit");
    }

    #[tokio::test]
    async fn codex_message_writer_observes_the_operation_deadline() {
        let (mut writer, _reader) = tokio::io::duplex(1);
        let message = serde_json::json!({"value": "blocked"});

        let error = write_codex_message(
            &mut writer,
            &message,
            Instant::now() + Duration::from_millis(1),
        )
        .await
        .unwrap_err();

        assert_eq!(error, "Codex app-server request timed out");
    }
}
