use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::{io, io::BufRead, io::IsTerminal, io::Read, io::Write};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use rynna_config::{
    ProfileCatalog, ProviderKind, ProviderSettingsStore, ResolvedCapability, ResolvedProfile,
    ResolvedProvider,
};
use rynna_core::{Agent, AgentProfiles, FallbackProvider, ModelProvider, Project, Tool};
use rynna_provider_anthropic::{AnthropicMessagesProvider, ClaudeCodeProvider};
use rynna_provider_openai::OpenAiCompatibleProvider;
use rynna_tools_command::{CommandConfig, CommandTool};
use rynna_tools_filesystem::{FileSystemConfig, FileSystemToolset};
use tracing_subscriber::EnvFilter;

mod chat_ui;
mod doctor;
mod model_picker;
mod model_selection;
mod provider_ui;

#[derive(Parser)]
#[command(name = "rynna", version, about = "An AI software agent")]
struct Cli {
    /// YAML configuration file. Uses the platform default when omitted.
    #[arg(long, env = "RYNNA_CONFIG", global = true)]
    config: Option<PathBuf>,
    /// Provider settings file. Uses the platform default when omitted.
    #[arg(long, env = "RYNNA_PROVIDER_CONFIG", global = true)]
    provider_config: Option<PathBuf>,
    /// Open the interactive provider settings interface.
    #[arg(long, global = true)]
    configure_providers: bool,
    /// Profile to use as the process default.
    #[arg(long, env = "RYNNA_PROFILE", global = true)]
    profile: Option<String>,
    /// Named project to use for a chat or one-shot session. Omit for the profile's default project.
    #[arg(long, env = "RYNNA_PROJECT", global = true)]
    project: Option<String>,
    /// Base URL for an OpenAI-compatible API, including any `/v1` prefix.
    #[arg(long, env = "RYNNA_API_BASE", global = true)]
    api_base: Option<String>,
    /// Model identifier understood by the provider.
    #[arg(long, env = "RYNNA_MODEL", global = true)]
    model: Option<String>,
    /// Optional API key. Prefer the environment variable to shell history.
    #[arg(long, env = "RYNNA_API_KEY", global = true, hide_env_values = true)]
    api_key: Option<String>,
    /// Trusted system instruction prepended to every request.
    #[arg(long, env = "RYNNA_SYSTEM_PROMPT", global = true)]
    system_prompt: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start an interactive terminal conversation.
    Chat,
    /// Run one prompt and exit for scripts, cron, and automation.
    Run {
        /// Prompt text. Reads stdin when omitted.
        #[arg(long)]
        prompt: Option<String>,
        /// Response encoding written to stdout.
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Run the HTTP API and optional web application.
    Serve {
        /// Address to listen on. Loopback is the secure default.
        #[arg(long, default_value = "127.0.0.1:3000")]
        bind: SocketAddr,
        /// Directory containing a built web application.
        #[arg(long)]
        web_dir: Option<PathBuf>,
    },
    /// List configured profiles without contacting model providers.
    Profiles {
        /// Profile-list encoding written to stdout.
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Create, update, list, and delete projects for the selected profile.
    Projects {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Check configuration without contacting model providers.
    Doctor {
        /// Report encoding written to stdout.
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// List the selected profile's projects.
    List {
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Create a named project.
    Create {
        name: String,
        #[arg(long = "directory", required = true)]
        directories: Vec<PathBuf>,
        #[arg(long)]
        default_directory: Option<PathBuf>,
    },
    /// Update a named project. Omitted fields keep their current values.
    Update {
        name: String,
        #[arg(long)]
        new_name: Option<String>,
        #[arg(long = "directory")]
        directories: Vec<PathBuf>,
        #[arg(long)]
        default_directory: Option<PathBuf>,
    },
    /// Delete a named project.
    Delete { name: String },
    /// Set the starting directory used by the implicit default project.
    SetDefault { directory: PathBuf },
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Default)]
struct ProfileOverrides {
    api_base: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    system_prompt: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let provider_config = match cli.provider_config.clone() {
        Some(path) => path,
        None => ProviderSettingsStore::default_path()
            .context("failed to locate Rynna provider settings")?,
    };
    let mut catalog = match &cli.config {
        Some(path) => ProfileCatalog::load(path)
            .with_context(|| format!("failed to load configuration from {}", path.display()))?,
        None => ProfileCatalog::load_default().context("failed to load Rynna configuration")?,
    };
    let default_profile = cli
        .profile
        .clone()
        .unwrap_or_else(|| catalog.default_profile().to_owned());
    if cli.configure_providers {
        catalog
            .resolve(&default_profile)
            .with_context(|| format!("failed to resolve profile `{default_profile}`"))?;
        return provider_ui::run(provider_config, &default_profile);
    }
    let command = cli.command.unwrap_or(Command::Chat);
    if let Command::Profiles { output } = command {
        return list_profiles(&catalog, &default_profile, cli.model.as_deref(), output);
    }
    if let Command::Projects { command } = command {
        return manage_projects(&mut catalog, &default_profile, command);
    }
    if let Command::Doctor { output } = command {
        return doctor::run(&catalog, &default_profile, output);
    }
    let include_all_profiles = matches!(&command, Command::Serve { .. });
    let mut profiles = configured_profiles(
        &catalog,
        &default_profile,
        ProfileOverrides {
            api_base: cli.api_base,
            model: cli.model,
            api_key: cli.api_key,
            system_prompt: cli.system_prompt,
        },
        include_all_profiles,
    )?;

    let mcp_store =
        rynna_config::mcp::McpSettingsStore::new(provider_config.with_file_name("mcp.yaml"));
    for profile in profiles.profiles() {
        let settings = mcp_store.load(&profile.name)?;
        profiles.set_tool_source(
            &profile.name,
            Some(Arc::new(rynna_mcp::McpToolSource(settings))),
        )?;
    }
    let memory_store = rynna_config::memory::MemorySettingsStore::new(
        provider_config.with_file_name("memory.yaml"),
    );
    for profile in profiles.profiles() {
        let memory = rynna_memory_hindsight::configured_memory(&memory_store.load(&profile.name)?)?;
        profiles.set_memory_provider(&profile.name, memory)?;
    }

    let result = match command {
        Command::Run { prompt, output } => {
            run_once(
                &profiles,
                &default_profile,
                cli.project.as_deref(),
                prompt,
                output,
            )
            .await
        }
        Command::Chat => chat(&profiles, &default_profile, cli.project.as_deref()).await,
        Command::Serve { bind, web_dir } => {
            serve(profiles, bind, web_dir, provider_config, catalog).await
        }
        Command::Profiles { .. } => unreachable!("profiles returned before provider configuration"),
        Command::Projects { .. } => {
            unreachable!("projects returned before provider configuration")
        }
        Command::Doctor { .. } => unreachable!("doctor returned before provider configuration"),
    };
    rynna_core::flush_memory_writes().await;
    result
}

fn manage_projects(
    catalog: &mut ProfileCatalog,
    profile_name: &str,
    command: ProjectCommand,
) -> Result<()> {
    let mut profile = catalog
        .resolve(profile_name)
        .with_context(|| format!("failed to resolve profile `{profile_name}`"))?
        .profile;
    match command {
        ProjectCommand::List { output } => match output {
            OutputFormat::Json => println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "profile": profile.name,
                    "default_project_directory": profile.default_project_directory,
                    "projects": profile.projects,
                }))?
            ),
            OutputFormat::Text => {
                println!(
                    "Default project\t{}",
                    profile.default_project_directory.display()
                );
                for project in &profile.projects {
                    println!(
                        "{}\t{}\t{}",
                        project.name,
                        project.default_directory.display(),
                        project
                            .directories
                            .iter()
                            .map(|directory| directory.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
            }
        },
        ProjectCommand::Create {
            name,
            directories,
            default_directory,
        } => {
            ensure!(
                !profile.projects.iter().any(|project| project.name == name),
                "project `{name}` already exists for profile `{profile_name}`"
            );
            let default_directory = default_directory
                .or_else(|| directories.first().cloned())
                .context("a project must contain at least one directory")?;
            profile.projects.push(Project {
                name,
                directories,
                default_directory,
            });
            catalog.update_profile(profile_name, profile)?;
        }
        ProjectCommand::Update {
            name,
            new_name,
            directories,
            default_directory,
        } => {
            let project = profile
                .projects
                .iter_mut()
                .find(|project| project.name == name)
                .with_context(|| {
                    format!("project `{name}` is not defined for profile `{profile_name}`")
                })?;
            if let Some(new_name) = new_name {
                project.name = new_name;
            }
            if !directories.is_empty() {
                project.directories = directories;
            }
            if let Some(default_directory) = default_directory {
                project.default_directory = default_directory;
            }
            catalog.update_profile(profile_name, profile)?;
        }
        ProjectCommand::Delete { name } => {
            let original_len = profile.projects.len();
            profile.projects.retain(|project| project.name != name);
            ensure!(
                profile.projects.len() != original_len,
                "project `{name}` is not defined for profile `{profile_name}`"
            );
            catalog.update_profile(profile_name, profile)?;
        }
        ProjectCommand::SetDefault { directory } => {
            profile.default_project_directory = directory;
            catalog.update_profile(profile_name, profile)?;
        }
    }
    Ok(())
}

fn list_profiles(
    catalog: &ProfileCatalog,
    default_profile: &str,
    model_override: Option<&str>,
    output: OutputFormat,
) -> Result<()> {
    if let Some(model) = model_override {
        ensure!(!model.trim().is_empty(), "provider model must not be blank");
    }
    catalog
        .resolve(default_profile)
        .with_context(|| format!("failed to select profile `{default_profile}`"))?;
    let profiles = catalog
        .resolve_all()?
        .into_iter()
        .map(|profile| {
            let mut profile = profile.profile;
            if profile.name == default_profile
                && let Some(model) = model_override
                && let Some(provider) = profile
                    .providers
                    .iter_mut()
                    .find(|p| p.enabled && p.is_default)
            {
                provider.model = model.to_owned();
            }
            profile
        })
        .collect::<Vec<_>>();

    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "default_profile": default_profile,
                "profiles": profiles,
            }))?
        ),
        OutputFormat::Text => {
            for profile in profiles {
                let marker = if profile.name == default_profile {
                    "*"
                } else {
                    " "
                };
                let providers = profile
                    .providers
                    .iter()
                    .map(|provider| format!("{}:{}", provider.provider, provider.model))
                    .collect::<Vec<_>>()
                    .join(",");
                println!(
                    "{marker} {}\tproviders={}\tskills={}\tmcp_servers={}\tcapabilities={}",
                    profile.name,
                    providers,
                    profile.active_skills.join(","),
                    profile.mcp_servers.join(","),
                    profile.capabilities.join(",")
                );
            }
        }
    }
    Ok(())
}

fn configured_profiles(
    catalog: &ProfileCatalog,
    default_profile: &str,
    overrides: ProfileOverrides,
    include_all_profiles: bool,
) -> Result<AgentProfiles> {
    let selected = catalog
        .resolve(default_profile)
        .with_context(|| format!("failed to select profile `{default_profile}`"))?;
    let resolved = if include_all_profiles {
        catalog.resolve_all()?
    } else {
        vec![selected]
    };
    let mut configured = Vec::new();
    for mut profile in resolved {
        let api_key_override = if profile.profile.name == default_profile {
            if let Some(api_base) = &overrides.api_base
                && let Some(provider) = profile.providers.first_mut()
            {
                provider.api_base.clone_from(api_base);
            }
            if let Some(model) = &overrides.model {
                profile.override_default_model(model);
            }
            if let Some(system_prompt) = &overrides.system_prompt {
                profile.system_prompt.clone_from(system_prompt);
            }
            overrides.api_key.clone()
        } else {
            None
        };
        let agent = configured_agent(&profile, api_key_override)?;
        configured.push((profile.profile, agent));
    }

    AgentProfiles::new(default_profile, configured).context("invalid profile catalog")
}

fn configured_agent(profile: &ResolvedProfile, api_key_override: Option<String>) -> Result<Agent> {
    let mut providers = profile
        .providers
        .iter()
        .enumerate()
        .map(|(index, provider)| {
            configured_provider(
                &profile.profile.name,
                provider,
                if index == 0 {
                    api_key_override.clone()
                } else {
                    None
                },
            )
        })
        .collect::<Result<Vec<_>>>()?;
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
        Arc::new(FallbackProvider::new(providers).map_err(anyhow::Error::msg)?)
    };

    let tools = configured_tools(profile)?;
    if tools.is_empty() {
        Ok(Agent::new(provider, profile.system_prompt.clone()).with_model_options(model_options))
    } else {
        Agent::with_tools(provider, profile.system_prompt.clone(), tools)
            .map(|agent| agent.with_model_options(model_options))
            .context("invalid profile tool configuration")
    }
}

fn configured_provider(
    profile_name: &str,
    provider: &ResolvedProvider,
    api_key_override: Option<String>,
) -> Result<Arc<dyn ModelProvider>> {
    let api_key = match api_key_override {
        Some(api_key) => Some(api_key),
        None => provider
            .api_key_env
            .as_deref()
            .map(|name| {
                env::var(name).with_context(|| {
                    format!(
                        "profile `{}` requires provider API key environment variable `{name}`",
                        profile_name
                    )
                })
            })
            .transpose()?,
    };
    let configured: Arc<dyn ModelProvider> = match provider.provider_kind {
        ProviderKind::OpenAiCompatible | ProviderKind::Mlx => Arc::new(
            OpenAiCompatibleProvider::new(&provider.api_base, &provider.model, api_key)
                .with_context(|| {
                    format!(
                        "invalid model provider configuration for profile `{}`",
                        profile_name
                    )
                })?,
        ),
        ProviderKind::AnthropicMessages => Arc::new(
            AnthropicMessagesProvider::with_base_url(
                &provider.api_base,
                &provider.model,
                api_key.ok_or_else(|| {
                    anyhow::anyhow!("profile `{}` requires an Anthropic API key", profile_name)
                })?,
            )
            .with_context(|| {
                format!(
                    "invalid Anthropic provider configuration for profile `{}`",
                    profile_name
                )
            })?,
        ),
        ProviderKind::ClaudeSubscription => Arc::new(ClaudeCodeProvider::new(
            &provider.claude_program,
            &provider.model,
        )),
    };
    Ok(configured)
}

fn configured_tools(profile: &ResolvedProfile) -> Result<Vec<Arc<dyn Tool>>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    if let Some(skills) =
        rynna_skills::SkillsTool::load(&profile.profile.active_skills, &profile.skills_directory)?
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
                    .context("invalid command capability")?,
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
                        .context("invalid filesystem capability")?
                        .tools(),
                );
            }
        }
    }
    Ok(tools)
}

async fn chat(profiles: &AgentProfiles, profile: &str, project: Option<&str>) -> Result<()> {
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        let model = profiles
            .profiles()
            .into_iter()
            .find(|candidate| candidate.name == profile)
            .and_then(|candidate| {
                candidate
                    .providers
                    .first()
                    .map(|provider| provider.model.clone())
            })
            .with_context(|| format!("profile `{profile}` is not configured"))?;
        return chat_ui::run(profiles, profile, &model, project).await;
    }

    let profiles = profiles
        .clone()
        .with_memory_session(Some(uuid::Uuid::new_v4()));
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut history = Vec::new();
    let mut line = String::new();

    let mut selection = None;
    println!(
        "Rynna interactive mode. /compact summarizes context; /model selects a model; /thinking sets effort; /quit exits."
    );
    loop {
        print!("you> ");
        io::stdout().flush().context("failed to flush stdout")?;
        line.clear();
        if input.read_line(&mut line).context("failed to read stdin")? == 0 {
            break;
        }

        let prompt = line.trim_end();
        if matches!(prompt, "/quit" | "/exit") {
            break;
        }
        if prompt.trim().is_empty() {
            continue;
        }

        if model_selection::is_command(prompt) {
            let result = model_selection::apply(&profiles, profile, &mut selection, prompt);
            println!(
                "{}",
                sanitize_terminal_text(&result.unwrap_or_else(|error| error))
            );
            continue;
        }
        let selected = profiles
            .clone()
            .with_model_selection(Some(profile), selection.as_ref())?
            .with_project(Some(profile), project)?;
        if prompt.split_whitespace().next() == Some("/compact") {
            if prompt != "/compact" {
                println!("/compact does not take arguments.");
                continue;
            }
            let request = rynna_core::ContextRequest {
                profile: Some(profile.to_owned()),
                history: history.clone(),
                compact: true,
                selection: selection.clone(),
                ..Default::default()
            };
            match selected.conversation_context(&request).await {
                Ok(result) => {
                    history = result.history;
                    println!(
                        "{}: ~{}% used ({} / {} estimated tokens).",
                        if result.compacted {
                            "Context compacted"
                        } else {
                            "No context to compact"
                        },
                        result.size.current_tokens * 100 / result.size.max_tokens,
                        result.size.current_tokens,
                        result.size.max_tokens
                    );
                }
                Err(error) => println!("{}", sanitize_terminal_text(&error.to_string())),
            }
            continue;
        }
        let message = selected
            .respond(Some(profile), &history, prompt)
            .await
            .map_err(sanitize_agent_error)?;
        println!("rynna> {}", sanitize_terminal_text(&message.content));
        history.push(rynna_core::Message::user(prompt));
        history.push(message);
        if let Some(agent) = selected.clone_agent(profile) {
            let size = agent.conversation_size(&history, "")?;
            println!(
                "Context: ~{}% used ({} / {} estimated tokens).",
                size.current_tokens * 100 / size.max_tokens,
                size.current_tokens,
                size.max_tokens
            );
        }
    }

    Ok(())
}

async fn serve(
    profiles: AgentProfiles,
    bind: SocketAddr,
    web_dir: Option<PathBuf>,
    provider_config: PathBuf,
    catalog: ProfileCatalog,
) -> Result<()> {
    let provider_settings = ProviderSettingsStore::load(provider_config)
        .context("failed to load Rynna provider settings")?;
    let app = match web_dir {
        Some(web_dir) => {
            ensure!(
                web_dir.join("index.html").is_file(),
                "web directory does not contain index.html: {}",
                web_dir.display()
            );
            rynna_server::router_with_profiles_provider_settings_catalog_and_web(
                profiles,
                provider_settings,
                Some(catalog),
                web_dir,
            )
        }
        None => rynna_server::router_with_profiles_provider_settings_and_catalog(
            profiles,
            provider_settings,
            catalog,
        ),
    };
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("failed to bind Rynna server to {bind}"))?;
    tracing::info!(address = %bind, "Rynna server started");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        shutdown_signal().await;
        rynna_core::workflow_runs::shutdown_workflows().await;
    })
    .await
    .context("Rynna server failed")
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(terminate) => terminate,
            Err(error) => {
                tracing::error!(%error, "failed to install SIGTERM handler");
                wait_for_ctrl_c().await;
                return;
            }
        };

        tokio::select! {
            () = wait_for_ctrl_c() => {}
            signal = terminate.recv() => {
                if signal.is_none() {
                    tracing::error!("SIGTERM handler closed before receiving a signal");
                }
            }
        }
    }

    #[cfg(not(unix))]
    wait_for_ctrl_c().await;
}

async fn wait_for_ctrl_c() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "failed to install shutdown signal handler");
    }
}

async fn run_once(
    profiles: &AgentProfiles,
    profile: &str,
    project: Option<&str>,
    prompt: Option<String>,
    output: OutputFormat,
) -> Result<()> {
    let prompt = match prompt {
        Some(prompt) => prompt,
        None => {
            let mut prompt = String::new();
            io::stdin()
                .read_to_string(&mut prompt)
                .context("failed to read prompt from stdin")?;
            prompt.trim_end().to_owned()
        }
    };
    let message = profiles
        .clone()
        .with_project(Some(profile), project)?
        .respond(Some(profile), &[], &prompt)
        .await
        .map_err(sanitize_agent_error)?;

    match output {
        OutputFormat::Text => println!("{}", sanitize_terminal_text(&message.content)),
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string(&rynna_server::RespondResponse { message })?
        ),
    }
    Ok(())
}

fn sanitize_agent_error(error: impl std::fmt::Display) -> anyhow::Error {
    anyhow::Error::msg(sanitize_terminal_text(&error.to_string()))
}

fn sanitize_terminal_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
        .collect()
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::sanitize_terminal_text;

    #[test]
    fn terminal_sanitizer_removes_controls_but_preserves_newlines_and_tabs() {
        let malicious = "safe\u{1b}[2J\u{1b}]0;owned\u{7}\r\u{8}\u{9b}31m\n\ttext";

        assert_eq!(
            sanitize_terminal_text(malicious),
            "safe[2J]0;owned31m\n\ttext"
        );
    }
}
