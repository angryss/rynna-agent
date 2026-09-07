use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rynna_config::{ConfiguredProvider, ProfileCatalog, ProviderSettingsStore};
use rynna_core::{
    Agent, AgentProfiles, Completion, CompletionDelta, CompletionRequest, Message, ModelProvider,
    Profile, ProfileProvider, ProviderError, ToolCall,
};
use rynna_desktop::{
    CodexAppServerProvider, OpenAiConnectRequest, RespondRequest, compose_agent,
    connect_openai_with_program, connect_openai_with_program_and_home, create_saved_profile,
    delete_saved_profile, list_profiles, openai_account_with_program,
    openai_account_with_program_and_home, prepare_codex_home, respond_stream_with_profiles,
    respond_with_agent, respond_with_locked_profiles, respond_with_profiles, update_saved_profile,
};

#[derive(Default)]
struct RecordingProvider {
    requests: Mutex<Vec<CompletionRequest>>,
}

#[async_trait]
impl ModelProvider for RecordingProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
        self.requests.lock().unwrap().push(request);
        Ok(Completion::new(Message::assistant("Desktop reply")))
    }
}

#[tokio::test]
async fn desktop_command_delegates_to_the_shared_agent_core() {
    let provider = Arc::new(RecordingProvider::default());
    let agent = Agent::new(
        Arc::clone(&provider) as Arc<dyn ModelProvider>,
        "Desktop policy",
    );

    let response = respond_with_agent(
        &agent,
        RespondRequest {
            selection: None,
            session_id: None,
            profile: None,
            project: None,
            prompt: "Continue".to_owned(),
            history: vec![Message::user("Start")],
        },
    )
    .await
    .unwrap();

    assert_eq!(response.message, Message::assistant("Desktop reply"));
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
}

fn profile(name: &str, reply: &'static str) -> (Profile, Agent) {
    struct FixedProvider(&'static str);

    #[async_trait]
    impl ModelProvider for FixedProvider {
        async fn complete(&self, _request: CompletionRequest) -> Result<Completion, ProviderError> {
            Ok(Completion::new(Message::assistant(self.0)))
        }
    }

    (
        Profile {
            name: name.to_owned(),
            providers: vec![ProfileProvider {
                provider: format!("{name}-provider"),
                model: format!("{name}-model"),
                enabled: true,
                is_default: true,
                context_window: None,
            }],
            active_skills: vec![format!("{name}-skill")],
            mcp_servers: vec![format!("{name}-mcp")],
            capabilities: Vec::new(),
            default_project_directory: ".".into(),
            projects: Vec::new(),
            subagents: Vec::new(),
        },
        Agent::new(Arc::new(FixedProvider(reply)), "Desktop policy"),
    )
}

#[tokio::test]
async fn desktop_profile_commands_list_and_dispatch_profiles() {
    let profiles = AgentProfiles::new(
        "local",
        vec![
            profile("local", "Local reply"),
            profile("work", "Work reply"),
        ],
    )
    .unwrap();

    let catalog = list_profiles(&profiles, None).unwrap();
    let response = respond_with_profiles(
        &profiles,
        RespondRequest {
            selection: None,
            session_id: None,
            profile: Some("work".to_owned()),
            project: None,
            prompt: "Continue".to_owned(),
            history: Vec::new(),
        },
    )
    .await
    .unwrap();

    assert_eq!(catalog.default_profile, "local");
    assert_eq!(catalog.profiles[1].name, "work");
    assert_eq!(catalog.configured_profiles[1].name, "work");
    assert_eq!(response.message, Message::assistant("Work reply"));
}

#[test]
fn desktop_profile_mutations_do_not_attach_existing_agents_and_preserve_runtime_default() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
version = 1
default_profile = "alpha"
[providers.ollama]
kind = "openai-compatible"
api_base = "http://127.0.0.1:11434/v1"
[profiles.alpha]
provider = "ollama"
model = "qwen3:8b"
[profiles.work]
provider = "ollama"
model = "qwen3:14b"
"#,
    )
    .unwrap();
    let mut catalog = ProfileCatalog::load(&path).unwrap();
    let mut provider_settings =
        ProviderSettingsStore::load(directory.path().join("providers.toml")).unwrap();
    for profile in ["new", "work"] {
        provider_settings
            .add(
                profile,
                ConfiguredProvider::Ollama {
                    api_base: "http://127.0.0.1:11434/v1".to_owned(),
                },
            )
            .unwrap();
    }
    let mcp_store = rynna_config::mcp::McpSettingsStore::new(provider_settings.mcp_settings_path());
    for name in ["new", "work"] {
        let settings = serde_json::from_value(serde_json::json!({"mcpServers":{"tools":{"transport":"stdio","command":format!("{name}-command"),"enabled":false}}})).unwrap();
        mcp_store.save(name, settings).unwrap();
    }
    let memory_store =
        rynna_config::memory::MemorySettingsStore::new(provider_settings.memory_settings_path());
    for name in ["new", "work"] {
        memory_store
            .save(
                name,
                rynna_config::memory::MemorySettings::Hindsight {
                    deployment: rynna_config::memory::HindsightDeployment::Cloud,
                    api_base: rynna_config::memory::HINDSIGHT_CLOUD_URL.to_owned(),
                    bank_id: name.to_owned(),
                    api_key: Some(format!("{name}-secret")),
                },
            )
            .unwrap();
    }
    let mut runtime = AgentProfiles::new(
        "work",
        vec![
            profile("alpha", "Alpha reply"),
            profile("work", "Sensitive work reply"),
            profile("openai-account", "Runtime fallback"),
        ],
    )
    .unwrap();
    let new_profile = Profile {
        name: "new".to_owned(),
        providers: vec![ProfileProvider {
            provider: "ollama".to_owned(),
            model: "other".to_owned(),
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

    create_saved_profile(&mut catalog, &mut runtime, new_profile.clone()).unwrap();
    assert_eq!(runtime.default_profile(), "work");
    assert!(runtime.clone_agent("new").is_none());
    update_saved_profile(
        &mut catalog,
        &mut runtime,
        Some(&mut provider_settings),
        "new",
        Profile {
            name: "renamed".to_owned(),
            ..new_profile
        },
    )
    .unwrap();
    assert!(runtime.clone_agent("renamed").is_none());
    assert_eq!(runtime.default_profile(), "work");
    assert!(provider_settings.list("new").is_empty());
    assert_eq!(provider_settings.list("renamed").len(), 1);
    assert!(mcp_store.load("new").unwrap().servers.is_empty());
    assert!(
        serde_json::to_string(&mcp_store.load("renamed").unwrap())
            .unwrap()
            .contains("new-command")
    );
    assert!(matches!(
        memory_store.load("new").unwrap(),
        rynna_config::memory::MemorySettings::None
    ));
    assert!(
        matches!(memory_store.load("renamed").unwrap(), rynna_config::memory::MemorySettings::Hindsight { api_key: Some(key), .. } if key == "new-secret")
    );

    delete_saved_profile(
        &mut catalog,
        &mut runtime,
        Some(&mut provider_settings),
        "work",
    )
    .unwrap();
    assert_eq!(runtime.default_profile(), "alpha");
    assert!(runtime.clone_agent("work").is_none());
    assert!(provider_settings.list("work").is_empty());
    assert!(mcp_store.load("work").unwrap().servers.is_empty());
    assert_eq!(mcp_store.load("renamed").unwrap().servers.len(), 1);
    assert!(matches!(
        memory_store.load("work").unwrap(),
        rynna_config::memory::MemorySettings::None
    ));
    assert!(matches!(
        memory_store.load("renamed").unwrap(),
        rynna_config::memory::MemorySettings::Hindsight { .. }
    ));
}

#[test]
fn desktop_profile_catalog_lists_unused_custom_provider_ids_and_runtime_only_profiles() {
    let catalog = ProfileCatalog::from_toml(
        r#"
version = 1
default_profile = "alpha"
[providers.ollama]
kind = "openai-compatible"
api_base = "http://127.0.0.1:11434/v1"
[providers.unused-custom]
kind = "openai-compatible"
api_base = "https://custom.example/v1"
[profiles.alpha]
providers = [
  { provider = "ollama", model = "qwen3:8b", enabled = true, default = true },
  { provider = "ollama", model = "qwen3:14b", enabled = false },
]
"#,
    )
    .unwrap();
    let mut alpha = profile("alpha", "Alpha");
    alpha.0.providers.push(ProfileProvider {
        provider: "ollama".to_owned(),
        model: "qwen3:14b".to_owned(),
        enabled: false,
        is_default: false,
        context_window: None,
    });
    let runtime =
        AgentProfiles::new("alpha", vec![alpha, profile("openai-account", "OpenAI")]).unwrap();

    let response = list_profiles(&runtime, Some(&catalog)).unwrap();

    assert_eq!(response.provider_ids, ["ollama", "unused-custom"]);
    assert!(
        response
            .profiles
            .iter()
            .any(|profile| profile.name == "openai-account")
    );
    let runtime_alpha = response
        .profiles
        .iter()
        .find(|profile| profile.name == "alpha")
        .unwrap();
    assert_eq!(runtime_alpha.providers.len(), 1);
    let configured_alpha = response
        .configured_profiles
        .iter()
        .find(|profile| profile.name == "alpha")
        .unwrap();
    assert_eq!(configured_alpha.providers.len(), 2);
}

#[tokio::test]
async fn desktop_non_streaming_response_releases_profiles_lock_while_provider_is_pending() {
    use tokio::sync::{Mutex as AsyncMutex, Notify};
    use tokio::time::{Duration, timeout};

    struct BlockingProvider {
        started: Notify,
        release: Notify,
    }

    #[async_trait]
    impl ModelProvider for BlockingProvider {
        async fn complete(&self, _request: CompletionRequest) -> Result<Completion, ProviderError> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(Completion::new(Message::assistant("released")))
        }
    }

    let provider = Arc::new(BlockingProvider {
        started: Notify::new(),
        release: Notify::new(),
    });
    let runtime_profile = Profile {
        name: "alpha".to_owned(),
        providers: vec![ProfileProvider {
            provider: "test".to_owned(),
            model: "test".to_owned(),
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
    let profiles = Arc::new(AsyncMutex::new(
        AgentProfiles::new(
            "alpha",
            [(runtime_profile, Agent::new(provider.clone(), "policy"))],
        )
        .unwrap(),
    ));
    let request_profiles = profiles.clone();
    let pending = tokio::spawn(async move {
        respond_with_locked_profiles(
            &request_profiles,
            RespondRequest {
                selection: None,
                session_id: None,
                profile: None,
                project: None,
                prompt: "wait".to_owned(),
                history: Vec::new(),
            },
        )
        .await
    });
    provider.started.notified().await;

    let acquired = timeout(Duration::from_millis(200), profiles.lock()).await;
    provider.release.notify_one();
    let _ = pending.await.unwrap().unwrap();

    assert!(
        acquired.is_ok(),
        "profiles lock remained held across provider await"
    );
}

#[tokio::test]
async fn desktop_stream_command_forwards_typed_deltas() {
    struct StreamingProvider;

    #[async_trait]
    impl ModelProvider for StreamingProvider {
        async fn complete(&self, _request: CompletionRequest) -> Result<Completion, ProviderError> {
            Ok(Completion::new(Message::assistant("Answer")))
        }

        async fn complete_stream(
            &self,
            _request: CompletionRequest,
            on_delta: &mut (dyn for<'delta> FnMut(&'delta CompletionDelta) + Send),
        ) -> Result<Completion, ProviderError> {
            on_delta(&CompletionDelta::Thinking("Inspect".to_owned()));
            on_delta(&CompletionDelta::Content("Answer".to_owned()));
            Ok(Completion::new(Message::assistant("Answer")))
        }
    }

    let profile = Profile {
        name: "local".to_owned(),
        providers: vec![ProfileProvider {
            provider: "test".to_owned(),
            model: "test".to_owned(),
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
    let profiles = AgentProfiles::new(
        "local",
        [(profile, Agent::new(Arc::new(StreamingProvider), "Policy"))],
    )
    .unwrap();
    let mut deltas = Vec::new();

    let response = respond_stream_with_profiles(
        &profiles,
        RespondRequest {
            selection: None,
            session_id: None,
            profile: None,
            project: None,
            prompt: "Continue".to_owned(),
            history: Vec::new(),
        },
        &mut |delta| deltas.push(delta.clone()),
    )
    .await
    .unwrap();

    assert_eq!(
        deltas,
        [
            CompletionDelta::Thinking("Inspect".to_owned()),
            CompletionDelta::Content("Answer".to_owned()),
        ]
    );
    assert_eq!(response.message, Message::assistant("Answer"));
}

#[cfg(unix)]
#[tokio::test]
async fn desktop_openai_commands_use_codex_managed_credentials_without_echoing_keys() {
    let program =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex_auth.sh");

    let connected = connect_openai_with_program(
        &program,
        OpenAiConnectRequest::ApiKey {
            api_key: "test-credential".to_owned(),
        },
    )
    .await
    .unwrap();
    let status = openai_account_with_program(&program).await.unwrap();

    assert!(connected.connected);
    assert_eq!(connected.method.as_deref(), Some("api_key"));
    assert_eq!(status.method.as_deref(), Some("api_key"));
}

#[cfg(unix)]
#[tokio::test]
async fn codex_app_server_provider_returns_the_subscription_answer() {
    let directory = tempfile::tempdir().unwrap();
    // A checked-in executable avoids write/exec races in parallel Linux tests.
    let program =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex_provider.sh");
    let provider = CodexAppServerProvider::with_home(
        &program,
        directory.path().canonicalize().unwrap().join("rynna-codex"),
        None,
    )
    .with_thinking(rynna_core::ThinkingLevel::High)
    .unwrap();
    let agent = Agent::new(provider, "Desktop policy");

    let response = agent.respond(&[], "Use my subscription").await.unwrap();

    assert_eq!(response, Message::assistant("Subscription answer"));
}

#[cfg(unix)]
#[tokio::test]
async fn codex_app_server_provider_rejects_unreviewed_codex_versions() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("unsupported-codex");
    std::fs::write(
        &program,
        "#!/bin/sh\n[ \"$1\" = \"--version\" ] && { printf '%s\\n' 'codex-cli 0.150.0'; exit 0; }\nexit 9\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let provider = Arc::new(CodexAppServerProvider::new(&program, None));
    let agent = Agent::new(provider, "Desktop policy");

    let error = agent.respond(&[], "Do not run tools").await.unwrap_err();

    assert_eq!(
        error.to_string(),
        "model provider failed: unsupported Codex CLI version; Rynna requires codex-cli 0.149.1"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn codex_app_server_provider_rejects_a_symlink_home_before_launch() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let home = directory.path().canonicalize().unwrap().join("codex-home");
    let marker = outside.path().join("launched");
    symlink(outside.path(), &home).unwrap();
    // A checked-in executable avoids write/exec races in parallel Linux tests.
    let program = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake_codex_launch_marker.sh");
    let provider = Arc::new(CodexAppServerProvider::with_home(&program, home, None));
    let agent = Agent::new(provider, "Desktop policy");

    let error = agent.respond(&[], "Do not run tools").await.unwrap_err();

    assert!(
        error.to_string().contains("must not be a symbolic link"),
        "unexpected provider error: {error}"
    );
    assert!(!marker.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn desktop_openai_account_uses_an_isolated_codex_home() {
    let directory = tempfile::tempdir().unwrap();
    let program =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_codex_home.sh");
    let marker = directory.path().join("codex-home");
    let home = directory.path().join("rynna-codex");

    let account = openai_account_with_program_and_home(&program, &home)
        .await
        .unwrap();

    assert!(account.connected);
    assert_eq!(
        std::fs::read_to_string(marker).unwrap(),
        home.display().to_string()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn desktop_openai_login_uses_the_same_isolated_codex_home() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("fake-codex-login-home");
    let marker = directory.path().join("codex-homes");
    let home = directory.path().join("rynna-codex");
    std::fs::write(
        &program,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$CODEX_HOME" >> '{}'
if [ "$1" = "login" ]; then
  IFS= read -r key
  [ "$key" = "«redacted:sk-…»" ] || exit 3
  exit 0
fi
[ "$1" = "app-server" ] || exit 2
IFS= read -r initialize
printf '%s\n' '{{"id":1,"result":{{"userAgent":"fake"}}}}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":{{"type":"apiKey"}},"requiresOpenaiAuth":true}}}}'
"#,
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();

    let account = connect_openai_with_program_and_home(
        &program,
        &home,
        OpenAiConnectRequest::ApiKey {
            api_key: "«redacted:sk-…»".to_owned(),
        },
    )
    .await
    .unwrap();

    assert!(account.connected);
    assert_eq!(
        std::fs::read_to_string(marker).unwrap(),
        format!("{0}\n{0}\n", home.display())
    );
}

#[cfg(unix)]
#[test]
fn desktop_codex_home_is_private_and_scoped_to_rynna() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let config_directory = directory.path().canonicalize().unwrap();

    let home = prepare_codex_home(&config_directory).unwrap();

    assert_eq!(home, config_directory.join("rynna").join("codex"));
    assert_eq!(
        std::fs::metadata(home).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[cfg(unix)]
#[test]
fn desktop_codex_home_rejects_a_symlink_target() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let directory = tempfile::tempdir().unwrap();
    let ordinary_codex_home = directory.path().join("ordinary-codex");
    std::fs::create_dir(&ordinary_codex_home).unwrap();
    std::fs::set_permissions(&ordinary_codex_home, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir(directory.path().join("rynna")).unwrap();
    symlink(
        &ordinary_codex_home,
        directory.path().join("rynna").join("codex"),
    )
    .unwrap();

    let error = prepare_codex_home(directory.path()).unwrap_err();

    assert!(error.contains("must not be a symbolic link"));
    assert_eq!(
        std::fs::metadata(ordinary_codex_home)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[cfg(unix)]
#[test]
fn desktop_codex_home_rejects_a_symlink_in_an_ancestor() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let redirected = directory.path().join("redirected");
    symlink(outside.path(), &redirected).unwrap();

    let error = prepare_codex_home(&redirected).unwrap_err();

    assert_eq!(
        error,
        "Rynna's Codex directory must not be a symbolic link or contain symbolic links"
    );
    assert!(!outside.path().join("rynna/codex").exists());
}

#[tokio::test]
#[ignore = "requires an installed, authenticated Codex CLI and consumes provider quota"]
async fn live_codex_account_provider_returns_an_answer() {
    let program = std::env::var_os("RYNNA_CODEX_PATH").unwrap_or_else(|| "codex".into());
    let provider = Arc::new(CodexAppServerProvider::new(PathBuf::from(program), None));
    let agent = Agent::new(
        provider,
        "Return only the exact text requested. Do not use tools.",
    );

    let response = agent
        .respond(
            &[
                Message::user("The verification token is RYNNA_LIVE_CODEX_OK."),
                Message::assistant("I will remember the verification token."),
            ],
            "Reply with the verification token only.",
        )
        .await
        .unwrap();

    assert_eq!(response, Message::assistant("RYNNA_LIVE_CODEX_OK"));

    let no_tools = agent
        .respond(
            &[],
            "Use a shell command to run `pwd`. If no shell tool is available, reply with RYNNA_TOOLS_DISABLED only.",
        )
        .await
        .unwrap();

    assert_eq!(no_tools, Message::assistant("RYNNA_TOOLS_DISABLED"));
}

#[cfg(unix)]
#[tokio::test]
async fn desktop_composition_executes_command_capabilities_without_leaking_paths() {
    use std::os::unix::fs::PermissionsExt;

    use rynna_config::ProfileCatalog;
    use serde_json::json;

    struct CommandProvider {
        requests: Mutex<Vec<CompletionRequest>>,
    }

    #[async_trait]
    impl ModelProvider for CommandProvider {
        async fn complete(&self, request: CompletionRequest) -> Result<Completion, ProviderError> {
            let has_result = request
                .messages
                .iter()
                .any(|message| message.role == rynna_core::Role::Tool);
            self.requests.lock().unwrap().push(request);
            if has_result {
                Ok(Completion::new(Message::assistant("command complete")))
            } else {
                Ok(Completion::with_tool_calls(vec![ToolCall::new(
                    "desktop-command",
                    "run_command",
                    json!({"program": "inspect"}),
                )]))
            }
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("inspect");
    std::fs::write(&program, "#!/bin/sh\nprintf 'desktop-command-result'\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let catalog = ProfileCatalog::from_toml(&format!(
        r#"
version = 1
default_profile = "desktop"

[providers.local]
kind = "openai-compatible"
api_base = "http://127.0.0.1:11434/v1"

[profiles.desktop]
provider = "local"
model = "test"
capabilities = ["host"]

[capabilities.host]
kind = "command"
working_directory = "{}"
programs = {{ inspect = "{}" }}
timeout_seconds = 5
max_output_bytes = 8192
"#,
        directory.path().display(),
        program.display()
    ))
    .unwrap();
    let profile = catalog.resolve("desktop").unwrap();
    let provider = Arc::new(CommandProvider {
        requests: Mutex::new(Vec::new()),
    });
    let agent = compose_agent(&profile, Arc::clone(&provider) as Arc<dyn ModelProvider>).unwrap();

    let response = respond_with_agent(
        &agent,
        RespondRequest {
            selection: None,
            session_id: None,
            profile: None,
            project: None,
            prompt: "Inspect the host".to_owned(),
            history: Vec::new(),
        },
    )
    .await
    .unwrap();

    assert_eq!(response.message, Message::assistant("command complete"));
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests[0].tools[0].name, "run_command");
    assert!(
        !requests[0].tools[0]
            .description
            .contains(&program.display().to_string())
    );
    assert!(
        requests[1]
            .messages
            .iter()
            .any(|message| message.content.contains("desktop-command-result"))
    );
}

#[test]
fn desktop_failed_mcp_rename_keeps_all_settings_and_allows_retry() {
    use rynna_config::{
        mcp::McpSettingsStore,
        memory::{MemorySettings, MemorySettingsStore},
    };
    for failure in ["malformed", "directory", "destination"] {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "version = 1\ndefault_profile = 'work'\n[providers.ollama]\nkind = 'openai-compatible'\napi_base = 'http://127.0.0.1:11434/v1'\n[profiles.work]\nprovider = 'ollama'\nmodel = 'model'\n").unwrap();
        let mut catalog = ProfileCatalog::load(&config_path).unwrap();
        let mut runtime = AgentProfiles::new("work", [profile("work", "original reply")]).unwrap();
        let mut providers = ProviderSettingsStore::load(dir.path().join("providers.toml")).unwrap();
        providers
            .add(
                "work",
                ConfiguredProvider::Ollama {
                    api_base: "http://127.0.0.1:11434/v1".into(),
                },
            )
            .unwrap();
        MemorySettingsStore::new(providers.memory_settings_path())
            .save("work", MemorySettings::None)
            .unwrap();
        let mcp_path = providers.mcp_settings_path();
        let mcp = McpSettingsStore::new(&mcp_path);
        mcp.save("work", serde_json::from_value(serde_json::json!({"mcpServers":{"tools":{"transport":"stdio","command":"original","enabled":false}}})).unwrap()).unwrap();
        let original_mcp = std::fs::read(&mcp_path).unwrap();
        match failure {
            "malformed" => std::fs::write(&mcp_path, "malformed = [").unwrap(),
            "directory" => {
                std::fs::remove_file(&mcp_path).unwrap();
                std::fs::create_dir(&mcp_path).unwrap();
            }
            "destination" => {
                mcp.save("renamed", Default::default()).unwrap();
            }
            _ => unreachable!(),
        }
        let paths = [
            config_path.clone(),
            providers.memory_settings_path(),
            dir.path().join("providers.toml"),
        ];
        let before = paths
            .iter()
            .map(|path| std::fs::read(path).unwrap())
            .collect::<Vec<_>>();
        let broken_mcp = std::fs::read(&mcp_path).ok();
        let renamed = Profile {
            name: "renamed".into(),
            ..catalog.resolve("work").unwrap().profile
        };
        assert!(
            update_saved_profile(
                &mut catalog,
                &mut runtime,
                Some(&mut providers),
                "work",
                renamed.clone()
            )
            .is_err()
        );
        assert_eq!(catalog.default_profile(), "work");
        assert!(catalog.resolve("work").is_ok());
        assert!(catalog.resolve("renamed").is_err());
        assert_eq!(runtime.default_profile(), "work");
        assert!(runtime.clone_agent("work").is_some());
        assert!(runtime.clone_agent("renamed").is_none());
        assert_eq!(providers.list("work").len(), 1);
        assert!(providers.list("renamed").is_empty());
        for (path, bytes) in paths.iter().zip(&before) {
            assert_eq!(&std::fs::read(path).unwrap(), bytes, "{failure}");
        }
        assert_eq!(std::fs::read(&mcp_path).ok(), broken_mcp);
        if failure == "directory" {
            std::fs::remove_dir(&mcp_path).unwrap();
        }
        std::fs::write(&mcp_path, &original_mcp).unwrap();
        update_saved_profile(
            &mut catalog,
            &mut runtime,
            Some(&mut providers),
            "work",
            renamed,
        )
        .unwrap();
        assert_eq!(catalog.default_profile(), "renamed");
        assert_eq!(providers.list("renamed").len(), 1);
        assert_eq!(mcp.load("renamed").unwrap().servers.len(), 1);
        assert!(mcp.load("work").unwrap().servers.is_empty());
    }
}

#[tokio::test]
async fn desktop_response_modes_forward_memory_session_ids() {
    #[derive(Default)]
    struct Memory(Mutex<Vec<uuid::Uuid>>);
    #[async_trait]
    impl rynna_core::MemoryProvider for Memory {
        async fn recall(&self, _: &str) -> Result<Vec<String>, rynna_core::MemoryError> {
            Ok(vec![])
        }
        async fn retain(
            &self,
            conversation: &rynna_core::MemoryConversation,
        ) -> Result<(), rynna_core::MemoryError> {
            self.0.lock().unwrap().push(conversation.session_id);
            Ok(())
        }
    }
    let memory = Arc::new(Memory::default());
    let (metadata, agent) = profile("test", "answer");
    let agent = agent.with_memory_provider(Some(memory.clone()));
    let profiles = AgentProfiles::new("test", [(metadata, agent.clone())]).unwrap();
    let session_id = uuid::Uuid::new_v4();
    let request = || RespondRequest {
        selection: None,
        session_id: Some(session_id),
        profile: None,
        project: None,
        prompt: "hello".into(),
        history: vec![],
    };
    respond_with_agent(&agent, request()).await.unwrap();
    respond_with_profiles(&profiles, request()).await.unwrap();
    respond_stream_with_profiles(&profiles, request(), &mut |_| {})
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while memory.0.lock().unwrap().len() < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(*memory.0.lock().unwrap(), [session_id; 3]);
}

#[tokio::test]
async fn desktop_composition_loads_only_the_selected_profiles_skills() {
    let directory = tempfile::tempdir().unwrap();
    let skill = directory.path().join("skills/review");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: review\ndescription: Review code\n---\nPrivate review instructions",
    )
    .unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(
        &config,
        r#"
version = 1
default_profile = "work"
[providers.local]
kind = "openai-compatible"
api_base = "http://127.0.0.1:11434/v1"
[profiles.work]
provider = "local"
model = "test"
active_skills = ["review"]
[profiles.personal]
provider = "local"
model = "test"
"#,
    )
    .unwrap();
    let catalog = ProfileCatalog::load(config).unwrap();
    for (name, count) in [("work", 1), ("personal", 0)] {
        let provider = Arc::new(RecordingProvider::default());
        let agent = compose_agent(&catalog.resolve(name).unwrap(), provider.clone()).unwrap();
        respond_with_agent(
            &agent,
            RespondRequest {
                selection: None,
                session_id: None,
                profile: None,
                project: None,
                prompt: "Review".into(),
                history: vec![],
            },
        )
        .await
        .unwrap();
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests[0].tools.len(), count);
        if count == 1 {
            assert_eq!(requests[0].tools[0].name, "read_skill");
            assert!(requests[0].tools[0].description.contains("Review code"));
            assert!(
                !requests[0].tools[0]
                    .description
                    .contains("Private review instructions")
            );
        }
    }
}

#[tokio::test]
async fn desktop_response_modes_apply_and_validate_model_selection() {
    let (mut metadata, agent) = profile("local", "default");
    let option = ProfileProvider {
        provider: "selected".into(),
        model: "selected-model".into(),
        enabled: true,
        is_default: false,
        context_window: None,
    };
    metadata.providers.push(option.clone());
    let agent = agent.with_model_options(vec![(option, Arc::new(RecordingProvider::default()))]);
    let profiles = AgentProfiles::new("local", vec![(metadata, agent)]).unwrap();
    let request = || {
        serde_json::from_value::<RespondRequest>(serde_json::json!({
            "prompt":"hello", "selection":{"provider":"selected","model":"selected-model"}
        }))
        .unwrap()
    };
    assert_eq!(
        respond_with_profiles(&profiles, request())
            .await
            .unwrap()
            .message
            .content,
        "Desktop reply"
    );
    assert_eq!(
        respond_stream_with_profiles(&profiles, request(), &mut |_| {})
            .await
            .unwrap()
            .message
            .content,
        "Desktop reply"
    );
    let mut invalid = request();
    invalid.selection.as_mut().unwrap().model = "missing".into();
    assert!(respond_with_profiles(&profiles, invalid).await.is_err());
    assert_eq!(
        profiles.respond(None, &[], "hello").await.unwrap().content,
        "default"
    );
}

#[tokio::test]
async fn saved_subagents_update_only_their_desktop_runtime_profile() {
    let mut catalog = ProfileCatalog::built_in();
    let mut work = catalog.resolve("default").unwrap().profile;
    work.name = "work".into();
    catalog.add_profile(work.clone()).unwrap();
    let provider = Arc::new(RecordingProvider::default());
    let mut runtime = AgentProfiles::new(
        "default",
        catalog
            .resolve_all()
            .unwrap()
            .into_iter()
            .map(|resolved| (resolved.profile, Agent::new(provider.clone(), "policy"))),
    )
    .unwrap();
    work.subagents = vec![rynna_core::Subagent {
        name: "reviewer".into(),
        description: "Review code".into(),
        instructions: "Find bugs".into(),
    }];
    update_saved_profile(&mut catalog, &mut runtime, None, "work", work.clone()).unwrap();
    runtime.respond(Some("work"), &[], "Hello").await.unwrap();
    runtime
        .respond(Some("default"), &[], "Hello")
        .await
        .unwrap();
    {
        let requests = provider.requests.lock().unwrap();
        assert!(
            requests[0]
                .tools
                .iter()
                .any(|tool| tool.name == "delegate_task")
        );
        assert!(requests[1].tools.is_empty());
    }
    work.subagents.clear();
    update_saved_profile(&mut catalog, &mut runtime, None, "work", work).unwrap();
    runtime.respond(Some("work"), &[], "Hello").await.unwrap();
    assert!(provider.requests.lock().unwrap()[2].tools.is_empty());
}

#[tokio::test]
async fn desktop_context_command_uses_selected_profile_and_preserves_messages() {
    let profiles = AgentProfiles::new(
        "local",
        [
            profile("local", "Local summary"),
            profile("work", "Work summary"),
        ],
    )
    .unwrap();
    let request = rynna_core::ContextRequest {
        profile: Some("work".into()),
        history: vec![
            Message::user("Important goal"),
            Message::assistant("Working"),
        ],
        compact: true,
        ..Default::default()
    };
    let response = rynna_desktop::context_with_profiles(&profiles, request)
        .await
        .unwrap();
    assert_eq!(response.history[1].content, "Working");
    assert_eq!(
        response.history[1].provider_context,
        Some(rynna_core::ProviderContext::ConversationSummary(
            "Work summary".into()
        ))
    );
    assert!(response.compacted);
}
