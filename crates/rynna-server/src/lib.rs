use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{ConnectInfo, FromRequestParts, Path as AxumPath, State};
use axum::http::request::Parts;
use axum::http::{Request, StatusCode};
use axum::middleware::{Next, from_fn};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use futures_util::stream::{self, Stream};
use rynna_config::mcp::{McpSettings, McpSettingsError, McpSettingsStore};
use rynna_config::memory::{
    MemorySettings, MemorySettingsError, MemorySettingsResponse, MemorySettingsStore,
};
use rynna_config::{
    AnthropicAuthentication, ConfigError, ConfiguredProvider, OpenAiAuthentication, ProfileCatalog,
    ProviderSettingsError, ProviderSettingsStore, secure_private_directory,
};
use rynna_core::{
    Agent, AgentError, AgentProfiles, CompletionDelta, Message, Profile, ProfileAgentError,
    ProfileError, ProfileProvider,
};
use rynna_mcp::McpToolSource;
use rynna_memory_hindsight::configured_memory;
use rynna_provider_anthropic::{
    CLAUDE_SUBSCRIPTION_CONFLICTING_ENV_VARS, isolate_claude_subscription_environment,
    terminate_child,
};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::{Mutex, mpsc};
use tokio::time::{Duration, timeout};
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
struct AppState {
    profiles: Arc<Mutex<AgentProfiles>>,
    catalog: Option<Arc<Mutex<ProfileCatalog>>>,
    provider_settings: Option<Arc<Mutex<ProviderSettingsStore>>>,
    codex_program: PathBuf,
    codex_home: PathBuf,
    claude_program: PathBuf,
}

pub fn router(agent: Agent) -> Router {
    let profile = Profile {
        name: "default".to_owned(),
        providers: vec![ProfileProvider {
            provider: "configured".to_owned(),
            model: "configured".to_owned(),
            enabled: true,
            is_default: true,
        }],
        active_skills: Vec::new(),
        mcp_servers: Vec::new(),
        capabilities: Vec::new(),
        default_project_directory: ".".into(),
        projects: Vec::new(),
    };
    let profiles = AgentProfiles::new("default", [(profile, agent)])
        .expect("the built-in server profile must be valid");
    router_with_profiles(profiles)
}

pub fn router_with_profiles(profiles: AgentProfiles) -> Router {
    router_with_state(profiles, None)
}

pub fn router_with_profiles_and_provider_settings(
    profiles: AgentProfiles,
    provider_settings: ProviderSettingsStore,
) -> Router {
    router_with_state(profiles, Some(provider_settings))
}

fn router_with_state(
    profiles: AgentProfiles,
    provider_settings: Option<ProviderSettingsStore>,
) -> Router {
    let codex_program = std::env::var_os("RYNNA_CODEX_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("codex"));
    let codex_home = std::env::var_os("RYNNA_CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::config_dir().map(|path| path.join("rynna").join("codex")))
        .unwrap_or_else(|| PathBuf::from(".rynna-codex"));
    router_with_runtime(profiles, provider_settings, None, codex_program, codex_home)
}

pub fn router_with_profiles_and_provider_runtime(
    profiles: AgentProfiles,
    provider_settings: ProviderSettingsStore,
    codex_program: impl Into<PathBuf>,
    codex_home: impl Into<PathBuf>,
) -> Router {
    router_with_runtime(
        profiles,
        Some(provider_settings),
        None,
        codex_program.into(),
        codex_home.into(),
    )
}

pub fn router_with_profiles_provider_settings_and_catalog(
    profiles: AgentProfiles,
    provider_settings: ProviderSettingsStore,
    catalog: ProfileCatalog,
) -> Router {
    let codex_program = std::env::var_os("RYNNA_CODEX_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("codex"));
    let codex_home = std::env::var_os("RYNNA_CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::config_dir().map(|path| path.join("rynna").join("codex")))
        .unwrap_or_else(|| PathBuf::from(".rynna-codex"));
    router_with_runtime(
        profiles,
        Some(provider_settings),
        Some(catalog),
        codex_program,
        codex_home,
    )
}

fn router_with_runtime(
    profiles: AgentProfiles,
    provider_settings: Option<ProviderSettingsStore>,
    catalog: Option<ProfileCatalog>,
    codex_program: PathBuf,
    codex_home: PathBuf,
) -> Router {
    let claude_program = std::env::var_os("RYNNA_CLAUDE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("claude"));
    let mut profiles = profiles;
    if let Some(store) = &provider_settings {
        let store = MemorySettingsStore::new(store.memory_settings_path());
        for profile in profiles.profiles() {
            match store.load(&profile.name) {
                Ok(settings) => match configured_memory(&settings) {
                    Ok(memory) => {
                        profiles
                            .set_memory_provider(&profile.name, memory)
                            .expect("existing profile");
                    }
                    Err(_) => tracing::warn!("could not configure memory provider"),
                },
                Err(_) => tracing::warn!("could not load memory settings"),
            }
        }
    }
    if let Some(store) = &provider_settings {
        let store = McpSettingsStore::new(store.mcp_settings_path());
        for profile in profiles.profiles() {
            match store.load(&profile.name) {
                Ok(settings) => {
                    profiles
                        .set_tool_source(&profile.name, Some(Arc::new(McpToolSource(settings))))
                        .expect("existing profile");
                }
                Err(_) => tracing::warn!("could not load MCP settings"),
            }
        }
    }
    let provider_routes = Router::new()
        .route(
            "/v1/profiles/{profile}/mcp",
            get(get_mcp_settings)
                .put(save_mcp_settings)
                .fallback(api_method_not_allowed),
        )
        .route(
            "/v1/profiles/{profile}/memory",
            get(get_memory_settings)
                .put(save_memory_settings)
                .fallback(api_method_not_allowed),
        )
        .route(
            "/v1/providers/openai/existing-account",
            get(existing_openai_account).fallback(api_method_not_allowed),
        )
        .route(
            "/v1/profiles/{profile}/providers",
            get(list_provider_settings)
                .post(create_provider)
                .fallback(api_method_not_allowed),
        )
        .route(
            "/v1/profiles/{profile}/providers/{kind}",
            axum::routing::put(update_provider)
                .delete(delete_provider)
                .fallback(api_method_not_allowed),
        )
        .route(
            "/v1/profiles/{name}",
            axum::routing::put(update_saved_profile)
                .delete(delete_saved_profile)
                .fallback(api_method_not_allowed),
        )
        .layer(from_fn(require_loopback_provider_admin));

    Router::new()
        .route("/healthz", get(health))
        .route(
            "/v1/profiles",
            get(list_profiles)
                .post(create_profile)
                .fallback(api_method_not_allowed),
        )
        .route(
            "/v1/respond",
            post(respond).fallback(api_method_not_allowed),
        )
        .route(
            "/v1/respond/stream",
            post(respond_stream).fallback(api_method_not_allowed),
        )
        .merge(provider_routes)
        .route("/v1", any(api_not_found))
        .route("/v1/{*path}", any(api_not_found))
        .with_state(AppState {
            profiles: Arc::new(Mutex::new(profiles)),
            catalog: catalog.map(|catalog| Arc::new(Mutex::new(catalog))),
            provider_settings: provider_settings.map(|store| Arc::new(Mutex::new(store))),
            codex_program,
            codex_home,
            claude_program,
        })
}

async fn require_loopback_provider_admin(
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let is_loopback = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|ConnectInfo(address)| address.ip().is_loopback());
    if is_loopback {
        next.run(request).await
    } else {
        ApiError {
            status: StatusCode::FORBIDDEN,
            code: "provider_admin_forbidden",
            message: "provider administration is restricted to loopback clients".to_owned(),
        }
        .into_response()
    }
}

pub fn router_with_web(agent: Agent, web_dir: impl AsRef<Path>) -> Router {
    let profile = Profile {
        name: "default".to_owned(),
        providers: vec![ProfileProvider {
            provider: "configured".to_owned(),
            model: "configured".to_owned(),
            enabled: true,
            is_default: true,
        }],
        active_skills: Vec::new(),
        mcp_servers: Vec::new(),
        capabilities: Vec::new(),
        default_project_directory: ".".into(),
        projects: Vec::new(),
    };
    let profiles = AgentProfiles::new("default", [(profile, agent)])
        .expect("the built-in server profile must be valid");
    router_with_profiles_and_web(profiles, web_dir)
}

pub fn router_with_profiles_and_web(profiles: AgentProfiles, web_dir: impl AsRef<Path>) -> Router {
    let web_dir = web_dir.as_ref();
    let index = web_dir.join("index.html");
    let assets = ServeDir::new(web_dir).fallback(ServeFile::new(index));
    router_with_profiles(profiles).fallback_service(assets)
}

pub fn router_with_profiles_provider_settings_and_web(
    profiles: AgentProfiles,
    provider_settings: ProviderSettingsStore,
    web_dir: impl AsRef<Path>,
) -> Router {
    router_with_profiles_provider_settings_catalog_and_web(
        profiles,
        provider_settings,
        None,
        web_dir,
    )
}

pub fn router_with_profiles_provider_settings_catalog_and_web(
    profiles: AgentProfiles,
    provider_settings: ProviderSettingsStore,
    catalog: Option<ProfileCatalog>,
    web_dir: impl AsRef<Path>,
) -> Router {
    let web_dir = web_dir.as_ref();
    let index = web_dir.join("index.html");
    let assets = ServeDir::new(web_dir).fallback(ServeFile::new(index));
    match catalog {
        Some(catalog) => {
            router_with_profiles_provider_settings_and_catalog(profiles, provider_settings, catalog)
                .fallback_service(assets)
        }
        None => router_with_profiles_and_provider_settings(profiles, provider_settings)
            .fallback_service(assets),
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

#[derive(Serialize)]
struct ProfilesResponse {
    default_profile: String,
    provider_ids: Vec<String>,
    profiles: Vec<Profile>,
    configured_profiles: Vec<Profile>,
}

async fn list_profiles(State(state): State<AppState>) -> Result<Json<ProfilesResponse>, ApiError> {
    let catalog_metadata = if let Some(catalog) = &state.catalog {
        let catalog = catalog.lock().await;
        Some((
            catalog.provider_ids(),
            catalog
                .resolve_all()
                .map_err(catalog_error)?
                .into_iter()
                .map(|resolved| resolved.profile)
                .collect::<Vec<_>>(),
        ))
    } else {
        None
    };
    let runtime = state.profiles.lock().await;
    let (provider_ids, catalog_profiles) = catalog_metadata.unwrap_or_default();
    let configured_profiles = catalog_profiles.clone();
    let mut profiles = runtime.profiles();
    for profile in &mut profiles {
        profile.providers.retain(|provider| provider.enabled);
    }
    for mut profile in catalog_profiles {
        profile.providers.retain(|provider| provider.enabled);
        if !profiles
            .iter()
            .any(|candidate| candidate.name == profile.name)
        {
            profiles.push(profile);
        }
    }
    profiles.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(Json(ProfilesResponse {
        default_profile: runtime.default_profile().to_owned(),
        provider_ids,
        profiles,
        configured_profiles,
    }))
}

fn catalog_store(state: &AppState) -> Result<&Arc<Mutex<ProfileCatalog>>, ApiError> {
    state.catalog.as_ref().ok_or_else(|| ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "profile_catalog_unavailable",
        message: "profile catalog is unavailable".to_owned(),
    })
}

fn catalog_error(error: ConfigError) -> ApiError {
    match error {
        ConfigError::Read { .. }
        | ConfigError::Inspect { .. }
        | ConfigError::Write { .. }
        | ConfigError::Encode(_)
        | ConfigError::Parse(_)
        | ConfigError::UnsupportedVersion(_)
        | ConfigError::ConfigDirectoryUnavailable => ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "profile_catalog_persistence_failed",
            message: "profile catalog could not be persisted".to_owned(),
        },
        _ => ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_profile",
            message: error.to_string(),
        },
    }
}

fn ensure_loopback_admin(LoopbackClient(is_loopback): LoopbackClient) -> Result<(), ApiError> {
    if is_loopback {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "provider_admin_forbidden",
            message: "provider administration is restricted to loopback clients".to_owned(),
        })
    }
}

struct LoopbackClient(bool);

impl<S> FromRequestParts<S> for LoopbackClient
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .is_some_and(|ConnectInfo(address)| address.ip().is_loopback()),
        ))
    }
}

async fn create_profile(
    peer: LoopbackClient,
    State(state): State<AppState>,
    request: Result<Json<Profile>, JsonRejection>,
) -> Result<Json<Profile>, ApiError> {
    ensure_loopback_admin(peer)?;
    let Json(profile) = request.map_err(ApiError::from)?;
    let mut catalog = catalog_store(&state)?.lock().await;
    let saved = catalog.add_profile(profile).map_err(catalog_error)?;
    Ok(Json(saved))
}

async fn update_saved_profile(
    State(state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    request: Result<Json<Profile>, JsonRejection>,
) -> Result<Json<Profile>, ApiError> {
    let Json(profile) = request.map_err(ApiError::from)?;
    let mut catalog = catalog_store(&state)?.lock().await;
    let saved = if let Some(provider_settings) = &state.provider_settings {
        let mut provider_settings = provider_settings.lock().await;
        rynna_config::profile_update::update_profile_with_settings(
            &mut catalog,
            &mut provider_settings,
            &name,
            profile,
        )
        .map_err(profile_update_error)?
    } else {
        catalog
            .update_profile(&name, profile)
            .map_err(catalog_error)?
    };
    if saved.name == name {
        let mut runtime = state.profiles.lock().await;
        if runtime.contains(&name) {
            runtime
                .set_project_configuration(
                    &name,
                    saved.default_project_directory.clone(),
                    saved.projects.clone(),
                )
                .map_err(runtime_profile_error)?;
        }
    }
    Ok(Json(saved))
}

async fn delete_saved_profile(
    State(state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Result<StatusCode, ApiError> {
    let mut catalog = catalog_store(&state)?.lock().await;
    {
        let runtime = state.profiles.lock().await;
        if runtime.contains(&name) && runtime.len() <= 1 {
            return Err(ApiError {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_profile",
                message: "the last runtime profile cannot be deleted before restart".to_owned(),
            });
        }
    }
    catalog.delete_profile(&name).map_err(catalog_error)?;
    if let Some(provider_settings) = &state.provider_settings {
        let mut provider_settings = provider_settings.lock().await;
        McpSettingsStore::new(provider_settings.mcp_settings_path())
            .delete_profile(&name)
            .map_err(mcp_settings_error)?;
        MemorySettingsStore::new(provider_settings.memory_settings_path())
            .delete_profile(&name)
            .map_err(memory_settings_error)?;
        provider_settings
            .delete_profile(&name)
            .map_err(provider_store_error)?;
    }
    let mut runtime = state.profiles.lock().await;
    if runtime.contains(&name) {
        runtime.remove(&name).map_err(runtime_profile_error)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

fn runtime_profile_error(error: ProfileError) -> ApiError {
    ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "invalid_profile",
        message: error.to_string(),
    }
}

fn project_error(error: ProfileError) -> ApiError {
    match error {
        ProfileError::UnknownProfile(profile) => ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "unknown_profile",
            message: format!("profile `{profile}` is not defined"),
        },
        error => ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message: error.to_string(),
        },
    }
}

#[derive(Serialize)]
struct OpenAiAccountResponse {
    connected: bool,
    method: Option<&'static str>,
}

async fn existing_openai_account(
    State(state): State<AppState>,
) -> Result<Json<OpenAiAccountResponse>, ApiError> {
    let connected = existing_chatgpt_credentials_available(&state).await?;
    Ok(Json(OpenAiAccountResponse {
        connected,
        method: connected.then_some("chatgpt"),
    }))
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

fn provider_store(state: &AppState) -> Result<&Arc<Mutex<ProviderSettingsStore>>, ApiError> {
    state.provider_settings.as_ref().ok_or_else(|| ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "provider_settings_unavailable",
        message: "provider settings are unavailable".to_owned(),
    })
}

fn profile_update_error(error: rynna_config::profile_update::ProfileUpdateError) -> ApiError {
    use rynna_config::profile_update::ProfileUpdateError;
    match error {
        ProfileUpdateError::Catalog(error) => catalog_error(error),
        ProfileUpdateError::Providers(error) => provider_store_error(error),
        ProfileUpdateError::Mcp(error) => mcp_settings_error(error),
        ProfileUpdateError::Memory(error) => memory_settings_error(error),
        ProfileUpdateError::DestinationExists => provider_settings_error(error.to_string()),
        ProfileUpdateError::Persistence | ProfileUpdateError::Rollback => ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "profile_update_error",
            message: error.to_string(),
        },
    }
}

fn mcp_settings_error(error: McpSettingsError) -> ApiError {
    ApiError {
        status: if matches!(error, McpSettingsError::Invalid(_)) {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        },
        code: "mcp_settings_error",
        message: error.to_string(),
    }
}

async fn get_mcp_settings(
    State(state): State<AppState>,
    AxumPath(profile): AxumPath<String>,
) -> Result<Json<McpSettings>, ApiError> {
    ensure_known_profile(&state, &profile).await?;
    let store = provider_store(&state)?.lock().await;
    let settings = McpSettingsStore::new(store.mcp_settings_path())
        .load(&profile)
        .map_err(mcp_settings_error)?;
    Ok(Json(settings))
}

async fn save_mcp_settings(
    State(state): State<AppState>,
    AxumPath(profile): AxumPath<String>,
    request: Result<Json<McpSettings>, JsonRejection>,
) -> Result<Json<McpSettings>, ApiError> {
    let Json(input) = request.map_err(|_| {
        mcp_settings_error(McpSettingsError::Invalid("invalid mcp settings request"))
    })?;
    // Serialize against catalog rename/delete, then lock runtime and credentials in that order.
    let catalog = match &state.catalog {
        Some(catalog) => Some(catalog.lock().await),
        None => None,
    };
    let mut profiles = state.profiles.lock().await;
    let exists = catalog.as_ref().map_or_else(
        || profiles.contains(&profile),
        |catalog| catalog.resolve(&profile).is_ok(),
    );
    if !exists {
        return Err(provider_settings_error("mcp profile is not defined"));
    }
    let store = provider_store(&state)?.lock().await;
    let settings = McpSettingsStore::new(store.mcp_settings_path())
        .save(&profile, input)
        .map_err(mcp_settings_error)?;
    let source = Some(Arc::new(McpToolSource(settings.clone())) as Arc<dyn rynna_core::ToolSource>);
    // Newly created or renamed catalog profiles become runnable on restart.
    if profiles.contains(&profile) {
        profiles
            .set_tool_source(&profile, source)
            .map_err(runtime_profile_error)?;
    }
    Ok(Json(settings))
}

fn memory_settings_error(error: MemorySettingsError) -> ApiError {
    ApiError {
        status: if matches!(error, MemorySettingsError::Invalid(_)) {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        },
        code: "memory_settings_error",
        message: error.to_string(),
    }
}

async fn get_memory_settings(
    State(state): State<AppState>,
    AxumPath(profile): AxumPath<String>,
) -> Result<Json<MemorySettingsResponse>, ApiError> {
    ensure_known_profile(&state, &profile).await?;
    let store = provider_store(&state)?.lock().await;
    let settings = MemorySettingsStore::new(store.memory_settings_path())
        .load(&profile)
        .map_err(memory_settings_error)?;
    Ok(Json(settings.response()))
}

async fn save_memory_settings(
    State(state): State<AppState>,
    AxumPath(profile): AxumPath<String>,
    request: Result<Json<MemorySettings>, JsonRejection>,
) -> Result<Json<MemorySettingsResponse>, ApiError> {
    let Json(input) = request.map_err(|_| {
        memory_settings_error(MemorySettingsError::Invalid(
            "invalid memory settings request",
        ))
    })?;
    // Serialize against catalog rename/delete, then lock runtime and credentials in that order.
    let catalog = match &state.catalog {
        Some(catalog) => Some(catalog.lock().await),
        None => None,
    };
    let mut profiles = state.profiles.lock().await;
    let exists = catalog.as_ref().map_or_else(
        || profiles.contains(&profile),
        |catalog| catalog.resolve(&profile).is_ok(),
    );
    if !exists {
        return Err(provider_settings_error("memory profile is not defined"));
    }
    let store = provider_store(&state)?.lock().await;
    let settings = MemorySettingsStore::new(store.memory_settings_path())
        .save(&profile, input)
        .map_err(memory_settings_error)?;
    let memory = configured_memory(&settings).map_err(|_| {
        memory_settings_error(MemorySettingsError::Invalid(
            "could not configure memory provider",
        ))
    })?;
    // Newly created or renamed catalog profiles become runnable on restart.
    if profiles.contains(&profile) {
        profiles
            .set_memory_provider(&profile, memory)
            .map_err(runtime_profile_error)?;
    }
    Ok(Json(settings.response()))
}

async fn list_provider_settings(
    State(state): State<AppState>,
    AxumPath(profile): AxumPath<String>,
) -> Result<Json<Vec<ConfiguredProvider>>, ApiError> {
    ensure_known_profile(&state, &profile).await?;
    let mut store = provider_store(&state)?.lock().await;
    store.refresh().map_err(provider_store_error)?;
    Ok(Json(store.list(&profile)))
}

async fn create_provider(
    State(state): State<AppState>,
    AxumPath(profile): AxumPath<String>,
    request: Result<Json<ProviderInput>, JsonRejection>,
) -> Result<Json<ConfiguredProvider>, ApiError> {
    ensure_known_profile(&state, &profile).await?;
    let Json(input) = request.map_err(ApiError::from)?;
    let mut store = provider_store(&state)?.lock().await;
    store.refresh().map_err(provider_store_error)?;
    if store.get(&profile, input.kind()).is_some() {
        return Err(provider_settings_error(format!(
            "provider `{}` is already configured",
            input.kind()
        )));
    }
    let provider = configured_provider(&state, input).await?;
    store
        .add(&profile, provider.clone())
        .map_err(provider_store_error)?;
    Ok(Json(provider))
}

async fn update_provider(
    State(state): State<AppState>,
    AxumPath((profile, kind)): AxumPath<(String, String)>,
    request: Result<Json<ProviderInput>, JsonRejection>,
) -> Result<Json<ConfiguredProvider>, ApiError> {
    ensure_known_profile(&state, &profile).await?;
    let Json(input) = request.map_err(ApiError::from)?;
    if kind != input.kind() {
        return Err(provider_settings_error(
            "provider path and request kind must match",
        ));
    }
    let mut store = provider_store(&state)?.lock().await;
    store.refresh().map_err(provider_store_error)?;
    if store.get(&profile, &kind).is_none() {
        return Err(provider_settings_error(format!(
            "provider `{kind}` is not configured"
        )));
    }
    let provider = configured_provider(&state, input).await?;
    store
        .update(&profile, provider.clone())
        .map_err(provider_store_error)?;
    Ok(Json(provider))
}

async fn delete_provider(
    State(state): State<AppState>,
    AxumPath((profile, kind)): AxumPath<(String, String)>,
) -> Result<StatusCode, ApiError> {
    ensure_known_profile(&state, &profile).await?;
    provider_store(&state)?
        .lock()
        .await
        .delete(&profile, &kind)
        .map_err(provider_store_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn ensure_known_profile(state: &AppState, profile: &str) -> Result<(), ApiError> {
    let exists = if let Some(catalog) = &state.catalog {
        catalog.lock().await.resolve(profile).is_ok()
    } else {
        state.profiles.lock().await.contains(profile)
    };
    if exists {
        Ok(())
    } else {
        Err(provider_settings_error(format!(
            "profile `{profile}` is not defined"
        )))
    }
}

async fn configured_provider(
    state: &AppState,
    input: ProviderInput,
) -> Result<ConfiguredProvider, ApiError> {
    match input {
        ProviderInput::Ollama { api_base } => Ok(ConfiguredProvider::Ollama { api_base }),
        ProviderInput::Mlx { api_base } => Ok(ConfiguredProvider::Mlx { api_base }),
        ProviderInput::OpenRouter => Ok(ConfiguredProvider::OpenRouter),
        ProviderInput::OpenAi {
            authentication,
            api_key,
            reuse_existing,
        } => {
            authenticate_openai(state, authentication, api_key, reuse_existing).await?;
            Ok(ConfiguredProvider::OpenAi {
                authentication,
                reuse_existing,
            })
        }
        ProviderInput::Anthropic { authentication } => {
            if authentication == AnthropicAuthentication::Subscription {
                authenticate_anthropic_subscription(state).await?;
            }
            Ok(ConfiguredProvider::Anthropic { authentication })
        }
    }
}

async fn authenticate_anthropic_subscription(state: &AppState) -> Result<(), ApiError> {
    authenticate_anthropic_subscription_with_program(
        &state.claude_program,
        Duration::from_secs(300),
    )
    .await
}

async fn authenticate_anthropic_subscription_with_program(
    program: &Path,
    login_timeout: Duration,
) -> Result<(), ApiError> {
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
        .map_err(|_| provider_settings_error("failed to start Claude subscription sign-in"))?;
    let status = match timeout(login_timeout, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(_)) => {
            terminate_child(&mut child).await;
            return Err(provider_settings_error(
                "failed to wait for Claude subscription sign-in",
            ));
        }
        Err(_) => {
            terminate_child(&mut child).await;
            return Err(provider_settings_error(
                "Claude subscription sign-in timed out",
            ));
        }
    };
    if !status.success() {
        return Err(provider_settings_error(
            "Claude subscription sign-in did not complete",
        ));
    }
    Ok(())
}

async fn authenticate_openai(
    state: &AppState,
    authentication: OpenAiAuthentication,
    api_key: Option<String>,
    reuse_existing: bool,
) -> Result<(), ApiError> {
    if reuse_existing {
        if authentication != OpenAiAuthentication::Chatgpt {
            return Err(provider_settings_error(
                "only ChatGPT subscriptions can reuse existing credentials",
            ));
        }
        if !existing_chatgpt_credentials_available(state).await? {
            return Err(provider_settings_error(
                "existing ChatGPT credentials are unavailable",
            ));
        }
        return Ok(());
    }
    secure_private_directory(state.codex_home.clone())
        .map_err(|_| provider_settings_error("failed to prepare secure OpenAI credentials"))?;
    let mut command = Command::new(&state.codex_program);
    command
        .env("CODEX_HOME", &state.codex_home)
        .arg("login")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = match authentication {
        OpenAiAuthentication::Chatgpt => command
            .stdin(Stdio::null())
            .spawn()
            .map_err(|_| provider_settings_error("failed to start ChatGPT sign-in"))?,
        OpenAiAuthentication::ApiKey => {
            let api_key = api_key
                .filter(|key| !key.trim().is_empty())
                .ok_or_else(|| provider_settings_error("OpenAI API key must not be blank"))?;
            if api_key.len() > 16 * 1024 {
                return Err(provider_settings_error("OpenAI API key is too large"));
            }
            let mut child = command
                .arg("--with-api-key")
                .stdin(Stdio::piped())
                .spawn()
                .map_err(|_| provider_settings_error("failed to start OpenAI API-key sign-in"))?;
            let mut stdin = child.stdin.take().ok_or_else(|| {
                provider_settings_error("OpenAI API-key sign-in input is unavailable")
            })?;
            stdin
                .write_all(api_key.as_bytes())
                .await
                .map_err(|_| provider_settings_error("failed to send the OpenAI API key"))?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|_| provider_settings_error("failed to send the OpenAI API key"))?;
            drop(stdin);
            child
        }
    };
    let wait = match authentication {
        OpenAiAuthentication::Chatgpt => Duration::from_secs(300),
        OpenAiAuthentication::ApiKey => Duration::from_secs(120),
    };
    let status = timeout(wait, child.wait())
        .await
        .map_err(|_| provider_settings_error("OpenAI sign-in timed out"))?
        .map_err(|_| provider_settings_error("failed to wait for OpenAI sign-in"))?;
    if !status.success() {
        return Err(provider_settings_error("OpenAI sign-in did not complete"));
    }
    Ok(())
}

async fn existing_chatgpt_credentials_available(state: &AppState) -> Result<bool, ApiError> {
    let output = timeout(
        Duration::from_secs(30),
        Command::new(&state.codex_program)
            .args(["login", "status"])
            .env_remove("CODEX_HOME")
            .env_remove("RYNNA_CODEX_HOME")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| provider_settings_error("OpenAI credential check timed out"))?
    .map_err(|_| provider_settings_error("failed to check existing ChatGPT credentials"))?;
    Ok(output.status.success()
        && String::from_utf8_lossy(&output.stdout).contains("Logged in using ChatGPT"))
}

fn provider_settings_error(message: impl Into<String>) -> ApiError {
    ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "invalid_provider_settings",
        message: message.into(),
    }
}

fn provider_store_error(error: ProviderSettingsError) -> ApiError {
    match error {
        ProviderSettingsError::Duplicate(_)
        | ProviderSettingsError::NotConfigured(_)
        | ProviderSettingsError::InvalidOllamaUrl(_)
        | ProviderSettingsError::UnsafeOllamaUrl => provider_settings_error(error.to_string()),
        _ => ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "provider_settings_persistence_failed",
            message: "provider settings could not be persisted".to_owned(),
        },
    }
}

async fn api_not_found() -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        code: "not_found",
        message: "API route not found".to_owned(),
    }
}

async fn api_method_not_allowed() -> ApiError {
    ApiError {
        status: StatusCode::METHOD_NOT_ALLOWED,
        code: "method_not_allowed",
        message: "HTTP method is not allowed for this API route".to_owned(),
    }
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

#[derive(Serialize)]
pub struct RespondResponse {
    pub message: Message,
}

async fn respond(
    State(state): State<AppState>,
    request: Result<Json<RespondRequest>, JsonRejection>,
) -> Result<Json<RespondResponse>, ApiError> {
    let Json(request) = request.map_err(ApiError::from)?;
    let profiles = state
        .profiles
        .lock()
        .await
        .clone()
        .with_memory_session(request.session_id)
        .with_project(request.profile.as_deref(), request.project.as_deref())
        .map_err(project_error)?
        .with_model_selection(request.profile.as_deref(), request.selection.as_ref())
        .map_err(|error| ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message: error.to_string(),
        })?;
    let message = profiles
        .respond(
            request.profile.as_deref(),
            &request.history,
            &request.prompt,
        )
        .await
        .map_err(ApiError::from)?;
    Ok(Json(RespondResponse { message }))
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StreamResponseEvent {
    Thinking { content: String },
    Content { content: String },
    Done { message: Message },
    Error { message: String },
}

impl From<&CompletionDelta> for StreamResponseEvent {
    fn from(delta: &CompletionDelta) -> Self {
        match delta {
            CompletionDelta::Thinking(content) => Self::Thinking {
                content: content.clone(),
            },
            CompletionDelta::Content(content) => Self::Content {
                content: content.clone(),
            },
        }
    }
}

async fn respond_stream(
    State(state): State<AppState>,
    request: Result<Json<RespondRequest>, JsonRejection>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let Json(request) = request.map_err(ApiError::from)?;
    let profiles = state
        .profiles
        .lock()
        .await
        .clone()
        .with_memory_session(request.session_id)
        .with_project(request.profile.as_deref(), request.project.as_deref())
        .map_err(project_error)?
        .with_model_selection(request.profile.as_deref(), request.selection.as_ref())
        .map_err(|error| ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message: error.to_string(),
        })?;
    let (sender, receiver) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        let delta_sender = sender.clone();
        let mut on_delta = move |delta: &CompletionDelta| {
            let _ = delta_sender.send(StreamResponseEvent::from(delta));
        };
        let result = profiles
            .respond_stream(
                request.profile.as_deref(),
                &request.history,
                &request.prompt,
                &mut on_delta,
            )
            .await;
        let event = match result {
            Ok(message) => StreamResponseEvent::Done { message },
            Err(error) => StreamResponseEvent::Error {
                message: ApiError::from(error).message,
            },
        };
        let _ = sender.send(event);
    });

    let stream = stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|event| {
            let event = Event::default()
                .json_data(event)
                .expect("stream response events must serialize");
            (Ok(event), receiver)
        })
    });
    Ok(Sse::new(stream))
}

struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl From<AgentError> for ApiError {
    fn from(error: AgentError) -> Self {
        match error {
            AgentError::BlankInput | AgentError::InvalidHistory => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_request",
                message: error.to_string(),
            },
            AgentError::Provider(_)
            | AgentError::InvalidProviderResponse
            | AgentError::EmptyProviderResponse
            | AgentError::UnexpectedToolCallAfterFinalAnswer => Self {
                status: StatusCode::BAD_GATEWAY,
                code: "provider_error",
                message: "model provider request failed".to_owned(),
            },
            AgentError::ToolDiscovery(_)
            | AgentError::ToolLoopLimit(_)
            | AgentError::ToolCallLimit(_)
            | AgentError::ToolResultByteLimit(_)
            | AgentError::ToolExecutionDeadline(_)
            | AgentError::ToolLoopDeadline(_) => Self {
                status: StatusCode::BAD_GATEWAY,
                code: "tool_loop_error",
                message: error.to_string(),
            },
            AgentError::BlankToolName | AgentError::DuplicateTool(_) => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "agent_configuration_error",
                message: "agent tool configuration is invalid".to_owned(),
            },
        }
    }
}

impl From<ProfileAgentError> for ApiError {
    fn from(error: ProfileAgentError) -> Self {
        match error {
            ProfileAgentError::UnknownProfile(profile) => Self {
                status: StatusCode::BAD_REQUEST,
                code: "unknown_profile",
                message: format!("profile `{profile}` is not defined"),
            },
            ProfileAgentError::Agent(error) => Self::from(error),
        }
    }
}

impl From<JsonRejection> for ApiError {
    fn from(error: JsonRejection) -> Self {
        if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            return Self {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "payload_too_large",
                message: "request body is too large".to_owned(),
            };
        }

        if error.status() == StatusCode::UNSUPPORTED_MEDIA_TYPE {
            return Self {
                status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
                code: "unsupported_media_type",
                message: "content type must be application/json".to_owned(),
            };
        }

        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message: "request body must be valid JSON".to_owned(),
        }
    }
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::authenticate_anthropic_subscription_with_program;
    use std::path::PathBuf;
    use tokio::time::Duration;

    #[tokio::test]
    async fn claude_login_timeout_kills_and_reaps_the_child() {
        let program = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/hanging_claude_login.sh");
        let pid_file = PathBuf::from(format!("{}.pid", program.display()));
        let _ = std::fs::remove_file(&pid_file);

        let error =
            authenticate_anthropic_subscription_with_program(&program, Duration::from_millis(250))
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

        assert!(error.message.contains("timed out"));
        assert!(!running, "timed-out Claude login child {pid} remained");
    }
}
