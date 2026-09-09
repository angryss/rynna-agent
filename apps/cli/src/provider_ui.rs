use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use rynna_config::{
    AnthropicAuthentication, ConfiguredProvider, OpenAiAuthentication, ProviderSettingsStore,
    secure_private_directory,
};
use rynna_provider_anthropic::{
    CLAUDE_SUBSCRIPTION_CONFLICTING_ENV_VARS, claude_subscription_environment,
};

#[derive(Clone, Copy, Eq, PartialEq)]
enum ProviderChoice {
    Ollama,
    Mlx,
    OpenRouter,
    OpenAi,
    Anthropic,
}

impl ProviderChoice {
    fn id(self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::Mlx => "mlx",
            Self::OpenRouter => "openrouter",
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Ollama => "Ollama",
            Self::Mlx => "MLX",
            Self::OpenRouter => "OpenRouter",
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
        }
    }

    fn toggle(self) -> Self {
        match self {
            Self::Ollama => Self::Mlx,
            Self::Mlx => Self::OpenRouter,
            Self::OpenRouter => Self::OpenAi,
            Self::OpenAi => Self::Anthropic,
            Self::Anthropic => Self::Ollama,
        }
    }
}

struct ProviderUi {
    providers: Vec<ConfiguredProvider>,
    selected: usize,
    editing: bool,
    existing: bool,
    choice: ProviderChoice,
    authentication: OpenAiAuthentication,
    input: String,
    error: Option<String>,
}

impl ProviderUi {
    fn new(providers: Vec<ConfiguredProvider>) -> Self {
        Self {
            providers,
            selected: 0,
            editing: false,
            existing: false,
            choice: ProviderChoice::Ollama,
            authentication: OpenAiAuthentication::Chatgpt,
            input: String::new(),
            error: None,
        }
    }

    fn begin_add(&mut self) {
        let Some(choice) = [
            ProviderChoice::Ollama,
            ProviderChoice::Mlx,
            ProviderChoice::OpenRouter,
            ProviderChoice::OpenAi,
            ProviderChoice::Anthropic,
        ]
        .into_iter()
        .find(|choice| !self.has(choice.id())) else {
            return;
        };
        self.existing = false;
        self.choice = choice;
        self.authentication = OpenAiAuthentication::Chatgpt;
        self.input = if self.choice == ProviderChoice::Ollama {
            "http://127.0.0.1:11434/v1".to_owned()
        } else if self.choice == ProviderChoice::Mlx {
            rynna_config::DEFAULT_MLX_API_BASE.to_owned()
        } else {
            String::new()
        };
        self.error = None;
        self.editing = true;
    }

    fn begin_edit(&mut self) {
        let Some(provider) = self.providers.get(self.selected) else {
            return;
        };
        self.existing = true;
        self.error = None;
        match provider {
            ConfiguredProvider::Ollama { api_base } => {
                self.choice = ProviderChoice::Ollama;
                self.input = api_base.clone();
            }
            ConfiguredProvider::Mlx { api_base } => {
                self.choice = ProviderChoice::Mlx;
                self.input = api_base.clone();
            }
            ConfiguredProvider::OpenRouter => {
                self.choice = ProviderChoice::OpenRouter;
                self.input.clear();
            }
            ConfiguredProvider::OpenAi { authentication, .. } => {
                self.choice = ProviderChoice::OpenAi;
                self.authentication = *authentication;
                self.input.clear();
            }
            ConfiguredProvider::Anthropic { authentication } => {
                self.choice = ProviderChoice::Anthropic;
                self.authentication = match authentication {
                    AnthropicAuthentication::ApiKey => OpenAiAuthentication::ApiKey,
                    AnthropicAuthentication::Subscription => OpenAiAuthentication::Chatgpt,
                };
                self.input.clear();
            }
        }
        self.editing = true;
    }

    fn has(&self, id: &str) -> bool {
        self.providers.iter().any(|provider| provider.id() == id)
    }

    fn refresh(&mut self, store: &ProviderSettingsStore, profile: &str) {
        self.providers = store.list(profile);
        self.selected = self.selected.min(self.providers.len().saturating_sub(1));
    }
}

pub fn run(path: PathBuf, profile: &str) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("provider configuration requires an interactive terminal");
    }
    let mut store =
        ProviderSettingsStore::load(path).context("failed to load Rynna provider settings")?;
    let mut ui = ProviderUi::new(store.list(profile));
    enable_raw_mode().context("failed to enable terminal raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
    let result = run_loop(&mut ui, &mut store, profile, &mut stdout);
    let _ = disable_raw_mode();
    let _ = execute!(stdout, LeaveAlternateScreen);
    result
}

fn run_loop(
    ui: &mut ProviderUi,
    store: &mut ProviderSettingsStore,
    profile: &str,
    stdout: &mut io::Stdout,
) -> Result<()> {
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to initialize provider TUI")?;
    loop {
        if !ui.editing {
            store
                .refresh()
                .context("failed to refresh provider settings")?;
            ui.refresh(store, profile);
        }
        terminal.draw(|frame| draw(frame, ui))?;
        let Event::Key(key) = event::read().context("failed to read terminal input")? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if ui.editing {
            match key.code {
                KeyCode::Esc => {
                    ui.editing = false;
                    ui.input.clear();
                    ui.error = None;
                }
                KeyCode::Left | KeyCode::Right if !ui.existing => {
                    let mut next = ui.choice.toggle();
                    for _ in 0..5 {
                        if !ui.has(next.id()) {
                            ui.choice = next;
                            ui.input = if next == ProviderChoice::Ollama {
                                "http://127.0.0.1:11434/v1".to_owned()
                            } else if next == ProviderChoice::Mlx {
                                rynna_config::DEFAULT_MLX_API_BASE.to_owned()
                            } else {
                                String::new()
                            };
                            break;
                        }
                        next = next.toggle();
                    }
                }
                KeyCode::Tab
                    if !matches!(
                        ui.choice,
                        ProviderChoice::Ollama | ProviderChoice::Mlx | ProviderChoice::OpenRouter
                    ) =>
                {
                    ui.authentication = match ui.authentication {
                        OpenAiAuthentication::ApiKey => OpenAiAuthentication::Chatgpt,
                        OpenAiAuthentication::Chatgpt => OpenAiAuthentication::ApiKey,
                    };
                    ui.input.clear();
                }
                KeyCode::Backspace => {
                    ui.input.pop();
                }
                KeyCode::Char(character) => ui.input.push(character),
                KeyCode::Enter => {
                    save_provider_with_terminal_suspended(&mut terminal, ui, store, profile)?
                }
                _ => {}
            }
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Char('a') => ui.begin_add(),
            KeyCode::Char('e') | KeyCode::Enter => ui.begin_edit(),
            KeyCode::Char('d') => {
                if let Some(provider) = ui.providers.get(ui.selected) {
                    if let Err(error) = store.delete(profile, provider.id()) {
                        ui.error = Some(error.to_string());
                    } else {
                        ui.refresh(store, profile);
                    }
                }
            }
            KeyCode::Up => ui.selected = ui.selected.saturating_sub(1),
            KeyCode::Down => {
                ui.selected = (ui.selected + 1).min(ui.providers.len().saturating_sub(1));
            }
            _ => {}
        }
    }
    Ok(())
}

fn save_provider_with_terminal_suspended(
    terminal: &mut Terminal<CrosstermBackend<&mut io::Stdout>>,
    ui: &mut ProviderUi,
    store: &mut ProviderSettingsStore,
    profile: &str,
) -> Result<()> {
    disable_raw_mode().context("failed to suspend terminal raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .context("failed to suspend alternate screen")?;

    save_provider(ui, store, profile);

    execute!(terminal.backend_mut(), EnterAlternateScreen)
        .context("failed to restore alternate screen")?;
    enable_raw_mode().context("failed to restore terminal raw mode")?;
    terminal
        .clear()
        .context("failed to redraw provider settings")?;
    Ok(())
}

fn save_provider(ui: &mut ProviderUi, store: &mut ProviderSettingsStore, profile: &str) {
    let provider = match ui.choice {
        ProviderChoice::Ollama => ConfiguredProvider::Ollama {
            api_base: ui.input.trim().to_owned(),
        },
        ProviderChoice::Mlx => ConfiguredProvider::Mlx {
            api_base: ui.input.trim().to_owned(),
        },
        ProviderChoice::OpenRouter => ConfiguredProvider::OpenRouter,
        ProviderChoice::OpenAi => {
            if let Err(error) = authenticate_openai(ui.authentication, &ui.input) {
                ui.error = Some(error.to_string());
                ui.input.clear();
                return;
            }
            ConfiguredProvider::OpenAi {
                authentication: ui.authentication,
                reuse_existing: false,
            }
        }
        ProviderChoice::Anthropic => {
            let authentication = match ui.authentication {
                OpenAiAuthentication::ApiKey => AnthropicAuthentication::ApiKey,
                OpenAiAuthentication::Chatgpt => AnthropicAuthentication::Subscription,
            };
            if authentication == AnthropicAuthentication::Subscription
                && let Err(error) = authenticate_anthropic()
            {
                ui.error = Some(error.to_string());
                return;
            }
            ConfiguredProvider::Anthropic { authentication }
        }
    };
    let result = if ui.existing {
        store.update(profile, provider)
    } else {
        store.add(profile, provider)
    };
    match result {
        Ok(()) => {
            ui.refresh(store, profile);
            ui.editing = false;
            ui.input.clear();
            ui.error = None;
        }
        Err(error) => ui.error = Some(error.to_string()),
    }
}

fn authenticate_openai(authentication: OpenAiAuthentication, api_key: &str) -> Result<()> {
    let codex_home = std::env::var_os("RYNNA_CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::config_dir().map(|path| path.join("rynna").join("codex")))
        .context("Rynna could not determine its configuration directory")?;
    let codex_home = secure_private_directory(codex_home)
        .context("failed to prepare secure OpenAI credentials")?;
    let program = std::env::var_os("RYNNA_CODEX_PATH").unwrap_or_else(|| "codex".into());
    let mut command = Command::new(program);
    command
        .env("CODEX_HOME", codex_home)
        .arg("login")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let (child, timeout) = match authentication {
        OpenAiAuthentication::Chatgpt => (
            command.stdin(Stdio::null()).spawn(),
            Duration::from_secs(300),
        ),
        OpenAiAuthentication::ApiKey => {
            if api_key.trim().is_empty() {
                bail!("OpenAI API key must not be blank");
            }
            if api_key.len() > 16 * 1024 {
                bail!("OpenAI API key is too large");
            }
            let mut child = command
                .arg("--with-api-key")
                .stdin(Stdio::piped())
                .spawn()?;
            let mut stdin = child
                .stdin
                .take()
                .context("Codex login stdin is unavailable")?;
            stdin.write_all(api_key.as_bytes())?;
            stdin.write_all(b"\n")?;
            drop(stdin);
            (Ok(child), Duration::from_secs(120))
        }
    };
    let status = wait_for_child(child.context("failed to start OpenAI sign-in")?, timeout)?;
    if !status.success() {
        bail!("OpenAI sign-in did not complete");
    }
    Ok(())
}

fn authenticate_anthropic() -> Result<()> {
    let program = std::env::var_os("RYNNA_CLAUDE_PATH").unwrap_or_else(|| "claude".into());
    let mut command = Command::new(program);
    command.env_clear().envs(claude_subscription_environment());
    command
        .args(["auth", "login", "--claudeai"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for name in CLAUDE_SUBSCRIPTION_CONFLICTING_ENV_VARS {
        command.env_remove(name);
    }
    let status = wait_for_child(
        command
            .spawn()
            .context("failed to start Claude subscription sign-in")?,
        Duration::from_secs(300),
    )?;
    if !status.success() {
        bail!("Claude subscription sign-in did not complete");
    }
    Ok(())
}

trait ChildProcess {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;
    fn kill(&mut self) -> io::Result<()>;
    fn wait(&mut self) -> io::Result<ExitStatus>;
}

impl ChildProcess for Child {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Child::try_wait(self)
    }

    fn kill(&mut self) -> io::Result<()> {
        Child::kill(self)
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        Child::wait(self)
    }
}

fn wait_for_child(mut child: Child, timeout: Duration) -> Result<ExitStatus> {
    wait_for_child_process(&mut child, timeout)
}

fn wait_for_child_process(child: &mut impl ChildProcess, timeout: Duration) -> Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context("failed to wait for provider sign-in");
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("provider sign-in timed out");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn draw(frame: &mut ratatui::Frame<'_>, ui: &ProviderUi) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(4),
            Constraint::Length(2),
        ])
        .split(frame.area());
    frame.render_widget(
        Paragraph::new(concat!(
            "Rynna Settings — Providers\n",
            "Provider settings record credential readiness only.\n",
            "Runtime provider/profile/model routing remains authoritative in config.yaml and loads at startup."
        ))
            .style(
                Style::default()
                    .fg(Color::LightMagenta)
                    .add_modifier(Modifier::BOLD),
            )
            .block(Block::default().borders(Borders::BOTTOM)),
        chunks[0],
    );
    if ui.editing {
        let authentication = match (ui.choice, ui.authentication) {
            (_, OpenAiAuthentication::ApiKey) => "API key",
            (ProviderChoice::Anthropic, OpenAiAuthentication::Chatgpt) => {
                "Claude subscription / usage bundle"
            }
            (_, OpenAiAuthentication::Chatgpt) => "ChatGPT subscription",
        };
        let displayed = if ui.choice == ProviderChoice::OpenAi
            && ui.authentication == OpenAiAuthentication::ApiKey
        {
            "•".repeat(ui.input.chars().count())
        } else {
            ui.input.clone()
        };
        let text = Text::from(vec![
            Line::from(format!("Provider: {}  (←/→ to change)", ui.choice.title())),
            Line::from(match ui.choice {
                ProviderChoice::Ollama => "Ollama API base URL:".to_owned(),
                ProviderChoice::Mlx => "MLX API base URL:".to_owned(),
                ProviderChoice::OpenRouter => {
                    "Authentication: API key from OPENROUTER_API_KEY".to_owned()
                }
                _ => format!("Authentication: {authentication}  (Tab to change)"),
            }),
            Line::from(displayed),
            Line::from(""),
            Line::from("Enter save • Esc cancel"),
        ]);
        frame.render_widget(
            Paragraph::new(text)
                .block(Block::default().title("Provider").borders(Borders::ALL))
                .wrap(Wrap { trim: false }),
            chunks[1],
        );
    } else if ui.providers.is_empty() {
        frame.render_widget(
            Paragraph::new("No providers configured.\n\nPress a to add a provider.")
                .block(Block::default().borders(Borders::ALL)),
            chunks[1],
        );
    } else {
        let items = ui.providers.iter().map(|provider| {
            let detail = match provider {
                ConfiguredProvider::Ollama { api_base } | ConfiguredProvider::Mlx { api_base } => {
                    api_base.as_str()
                }
                ConfiguredProvider::OpenRouter => "API key from OPENROUTER_API_KEY",
                ConfiguredProvider::OpenAi {
                    authentication: OpenAiAuthentication::ApiKey,
                    ..
                } => "API key",
                ConfiguredProvider::OpenAi {
                    authentication: OpenAiAuthentication::Chatgpt,
                    ..
                } => "ChatGPT subscription",
                ConfiguredProvider::Anthropic { authentication } => match authentication {
                    rynna_config::AnthropicAuthentication::ApiKey => {
                        "API key (configure via environment)"
                    }
                    rynna_config::AnthropicAuthentication::Subscription => {
                        "Claude subscription / usage bundle"
                    }
                },
            };
            ListItem::new(format!("{}  {detail}", provider_title(provider)))
        });
        let mut state = ListState::default().with_selected(Some(ui.selected));
        frame.render_stateful_widget(
            List::new(items)
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
                .block(Block::default().borders(Borders::ALL)),
            chunks[1],
            &mut state,
        );
    }
    let status = ui
        .error
        .as_deref()
        .unwrap_or("a add • e/Enter edit • d delete • ↑/↓ select • q quit");
    frame.render_widget(
        Paragraph::new(status).style(if ui.error.is_some() {
            Style::default().fg(Color::LightRed)
        } else {
            Style::default().fg(Color::DarkGray)
        }),
        chunks[2],
    );
}

fn provider_title(provider: &ConfiguredProvider) -> &'static str {
    match provider {
        ConfiguredProvider::Ollama { .. } => "Ollama",
        ConfiguredProvider::Mlx { .. } => "MLX",
        ConfiguredProvider::OpenRouter => "OpenRouter",
        ConfiguredProvider::OpenAi { .. } => "OpenAI",
        ConfiguredProvider::Anthropic { .. } => "Anthropic",
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::process::ExitStatus;
    #[cfg(unix)]
    use std::process::{Command, Stdio};
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    use super::{ChildProcess, ProviderUi, draw, wait_for_child, wait_for_child_process};
    use ratatui::{Terminal, backend::TestBackend};
    use rynna_config::{ConfiguredProvider, ProviderSettingsStore};

    #[cfg(unix)]
    #[test]
    fn openai_login_timeout_kills_a_hung_child() {
        let child = Command::new("sh")
            .args(["-c", "sleep 5"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let started = Instant::now();

        let error = wait_for_child(child, Duration::from_millis(10)).unwrap_err();

        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    struct FailingChild {
        kill_called: bool,
        wait_called: bool,
    }

    impl ChildProcess for FailingChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            Err(io::Error::other("synthetic wait failure"))
        }

        fn kill(&mut self) -> io::Result<()> {
            self.kill_called = true;
            Ok(())
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            self.wait_called = true;
            Err(io::Error::other("synthetic reap failure"))
        }
    }

    #[test]
    fn provider_child_wait_error_kills_and_reaps_before_returning() {
        let mut child = FailingChild {
            kill_called: false,
            wait_called: false,
        };

        let error = wait_for_child_process(&mut child, Duration::from_secs(1)).unwrap_err();

        assert!(child.kill_called);
        assert!(child.wait_called);
        assert!(error.to_string().contains("provider sign-in"));
        assert!(!error.to_string().contains("OpenAI"));
    }

    #[test]
    fn provider_ui_starts_empty_and_offers_mlx_with_its_local_endpoint() {
        let mut ui = ProviderUi::new(Vec::new());
        assert!(ui.providers.is_empty());
        ui.begin_add();
        assert_eq!(ui.choice.id(), "ollama");

        ui.providers.push(ConfiguredProvider::Ollama {
            api_base: "http://localhost:11434/v1".to_owned(),
        });
        ui.begin_add();
        assert_eq!(ui.choice.id(), "mlx");
        assert_eq!(ui.input, "http://127.0.0.1:8000/v1");
        ui.providers.push(ConfiguredProvider::Mlx {
            api_base: ui.input.clone(),
        });
        ui.begin_add();
        assert_eq!(ui.choice.id(), "openrouter");
    }

    #[test]
    fn provider_ui_can_add_any_fifth_provider_and_stops_when_all_are_configured() {
        let providers = vec![
            ConfiguredProvider::Ollama {
                api_base: "http://localhost:11434/v1".to_owned(),
            },
            ConfiguredProvider::Mlx {
                api_base: "http://localhost:8000/v1".to_owned(),
            },
            ConfiguredProvider::OpenRouter,
            ConfiguredProvider::OpenAi {
                authentication: rynna_config::OpenAiAuthentication::ApiKey,
                reuse_existing: false,
            },
            ConfiguredProvider::Anthropic {
                authentication: rynna_config::AnthropicAuthentication::ApiKey,
            },
        ];
        for missing in &providers {
            let mut ui = ProviderUi::new(
                providers
                    .iter()
                    .filter(|provider| provider.id() != missing.id())
                    .cloned()
                    .collect(),
            );
            ui.begin_add();
            assert!(ui.editing);
            assert_eq!(ui.choice.id(), missing.id());

            ui.providers.push(missing.clone());
            ui.editing = false;
            ui.begin_add();
            assert!(!ui.editing);
        }
    }

    #[test]
    fn provider_ui_refreshes_the_selected_profile_scope() {
        let directory = tempfile::tempdir().unwrap();
        let mut store =
            ProviderSettingsStore::load(directory.path().join("providers.yaml")).unwrap();
        store
            .add(
                "work",
                ConfiguredProvider::Ollama {
                    api_base: "http://localhost:11434/v1".to_owned(),
                },
            )
            .unwrap();
        let mut ui = ProviderUi::new(Vec::new());

        ui.refresh(&store, "work");

        assert_eq!(ui.providers.len(), 1);
    }

    #[test]
    fn provider_ui_discloses_that_runtime_routing_comes_from_rynna_config() {
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        let ui = ProviderUi::new(Vec::new());

        terminal.draw(|frame| draw(frame, &ui)).unwrap();

        let screen = terminal.backend().to_string();
        assert!(screen.contains("credential readiness only"), "{screen}");
        assert!(
            screen.contains(
                "Runtime provider/profile/model routing remains authoritative in config.yaml"
            ),
            "{screen}"
        );
        assert!(screen.contains("loads at startup"), "{screen}");
    }
}
