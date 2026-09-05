use std::io::{self, Stdout};

use anyhow::{Context, Result};
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
        KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use rynna_core::{AgentProfiles, CompletionDelta, Message};
use tokio::sync::mpsc;

const USER_BACKGROUND: Color = Color::Rgb(52, 52, 52);
const COMMAND_COLUMN_WIDTH: usize = 18;
const MAX_COMPOSER_CONTENT_HEIGHT: u16 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SlashCommand {
    name: &'static str,
    description: &'static str,
    action: SlashAction,
    aliases: &'static [SlashCommandAlias],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SlashCommandAlias {
    name: &'static str,
    description: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SlashCommandMatch {
    name: &'static str,
    description: &'static str,
    action: SlashAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SlashAction {
    Local(CommandAction),
    Selection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandAction {
    Model,
    Clear,
    Help,
    Quit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum InputAction {
    Prompt(String),
    Command(CommandAction),
    Selection(String),
}

const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/clear",
        description: "Clear the conversation",
        action: SlashAction::Local(CommandAction::Clear),
        aliases: &[],
    },
    SlashCommand {
        name: "/help",
        description: "Show available commands",
        action: SlashAction::Local(CommandAction::Help),
        aliases: &[],
    },
    SlashCommand {
        name: "/model",
        description: "Open the model selector",
        action: SlashAction::Local(CommandAction::Model),
        aliases: &[],
    },
    SlashCommand {
        name: "/provider",
        description: "List providers or set <provider>",
        action: SlashAction::Selection,
        aliases: &[],
    },
    SlashCommand {
        name: "/thinking",
        description: "Set default|low|medium|high effort",
        action: SlashAction::Selection,
        aliases: &[],
    },
    SlashCommand {
        name: "/quit",
        description: "Exit Rynna",
        action: SlashAction::Local(CommandAction::Quit),
        aliases: &[SlashCommandAlias {
            name: "/exit",
            description: "Alias for /quit",
        }],
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MessageKind {
    User,
    Thinking,
    Assistant,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DisplayMessage {
    kind: MessageKind,
    content: String,
    expanded: bool,
}

struct ChatUi {
    messages: Vec<DisplayMessage>,
    input: String,
    cursor: usize,
    profile: String,
    model: String,
    busy: bool,
    scroll_from_bottom: u16,
    selected_command: usize,
    picker: super::model_picker::ModelPicker,
}

impl ChatUi {
    fn new(profile: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            messages: Vec::new(),
            input: String::new(),
            cursor: 0,
            profile: profile.into(),
            model: model.into(),
            busy: false,
            scroll_from_bottom: 0,
            selected_command: 0,
            picker: Default::default(),
        }
    }

    #[cfg(test)]
    fn messages(&self) -> &[DisplayMessage] {
        &self.messages
    }

    fn push_message(&mut self, kind: MessageKind, content: impl Into<String>) {
        self.messages.push(DisplayMessage {
            kind,
            content: content.into(),
            expanded: false,
        });
        self.scroll_from_bottom = 0;
    }

    fn command_matches(&self) -> Vec<SlashCommandMatch> {
        if !self.input.starts_with('/') || self.input.contains(char::is_whitespace) {
            return Vec::new();
        }
        let mut matches = Vec::new();
        for command in SLASH_COMMANDS {
            if command.name.starts_with(&self.input) {
                matches.push(SlashCommandMatch {
                    name: command.name,
                    description: command.description,
                    action: command.action,
                });
            }
            matches.extend(
                command
                    .aliases
                    .iter()
                    .filter(|alias| alias.name.starts_with(&self.input))
                    .map(|alias| SlashCommandMatch {
                        name: alias.name,
                        description: alias.description,
                        action: command.action,
                    }),
            );
        }
        matches
    }

    fn take_submission(&mut self) -> Option<String> {
        let prompt = self.input.trim().to_owned();
        if prompt.is_empty() {
            return None;
        }
        self.input.clear();
        self.cursor = 0;
        self.push_message(MessageKind::User, prompt.clone());
        Some(prompt)
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<InputAction> {
        if self.picker.open {
            return self.picker.key(key).map(InputAction::Selection);
        }
        if key.code == KeyCode::F(2) {
            if !self.busy {
                self.picker.show();
            }
            return None;
        }
        if key.code == KeyCode::Enter
            && key.modifiers.is_empty()
            && self.input.trim() == "/model"
            && !self.busy
        {
            self.input.clear();
            self.cursor = 0;
            self.selected_command = 0;
            return Some(InputAction::Command(CommandAction::Model));
        }
        if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if let Some(message) = self
                .messages
                .iter_mut()
                .rev()
                .find(|message| message.kind == MessageKind::Thinking)
            {
                message.expanded = !message.expanded;
                self.scroll_from_bottom = 0;
            }
            return None;
        }
        if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::ALT) {
            self.input.insert(self.cursor, '\n');
            self.cursor += 1;
            self.selected_command = 0;
            return None;
        }
        let commands = self.command_matches();
        if !commands.is_empty() {
            match key.code {
                KeyCode::Up => {
                    self.selected_command = self
                        .selected_command
                        .checked_sub(1)
                        .unwrap_or(commands.len() - 1);
                    return None;
                }
                KeyCode::Down => {
                    self.selected_command = (self.selected_command + 1) % commands.len();
                    return None;
                }
                KeyCode::Tab => {
                    let selected = commands[self.selected_command.min(commands.len() - 1)];
                    self.input = selected.name.to_owned();
                    self.cursor = self.input.len();
                    self.selected_command = 0;
                    return None;
                }
                _ => {}
            }
        }
        if key.code == KeyCode::Enter {
            if self.busy {
                return None;
            }
            if super::model_selection::is_command(&self.input) {
                let command = std::mem::take(&mut self.input);
                self.cursor = 0;
                self.selected_command = 0;
                return Some(InputAction::Selection(command));
            }
            if !commands.is_empty() {
                let selected = commands[self.selected_command.min(commands.len() - 1)];
                self.input.clear();
                self.cursor = 0;
                self.selected_command = 0;
                return Some(match selected.action {
                    SlashAction::Local(action) => InputAction::Command(action),
                    SlashAction::Selection => InputAction::Selection(selected.name.to_owned()),
                });
            }
            if self.input.starts_with('/') {
                let command = self.input.split_whitespace().next().unwrap_or_default();
                self.push_message(MessageKind::Error, format!("Unknown command: {command}"));
                self.input.clear();
                self.cursor = 0;
                return None;
            }
            return self.take_submission().map(InputAction::Prompt);
        }
        self.handle_edit_key(key);
        self.selected_command = 0;
        None
    }

    fn start_assistant_response(&mut self) {
        self.push_message(MessageKind::Assistant, String::new());
    }

    fn append_assistant_delta(&mut self, delta: &str) {
        if let Some(message) = self.messages.last_mut()
            && message.kind == MessageKind::Assistant
        {
            message.content.push_str(delta);
            self.scroll_from_bottom = 0;
        }
    }

    fn append_completion_delta(&mut self, delta: &CompletionDelta) {
        match delta {
            CompletionDelta::Thinking(delta) => {
                if delta.is_empty() {
                    return;
                }
                if let Some(message) = self.messages.last_mut()
                    && message.kind == MessageKind::Thinking
                {
                    message.content.push_str(delta);
                    message.expanded = true;
                } else {
                    self.messages.push(DisplayMessage {
                        kind: MessageKind::Thinking,
                        content: delta.clone(),
                        expanded: true,
                    });
                }
                self.scroll_from_bottom = 0;
            }
            CompletionDelta::Content(delta) => {
                if delta.is_empty() {
                    return;
                }
                if let Some(thinking) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .find(|message| message.kind == MessageKind::Thinking)
                {
                    thinking.expanded = false;
                }
                if self
                    .messages
                    .last()
                    .is_none_or(|message| message.kind != MessageKind::Assistant)
                {
                    self.start_assistant_response();
                }
                self.append_assistant_delta(delta);
            }
        }
    }

    fn handle_edit_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.input.insert(self.cursor, character);
                self.cursor += character.len_utf8();
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let previous = self.input[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map(|(index, _)| index)
                    .unwrap_or(0);
                self.input.drain(previous..self.cursor);
                self.cursor = previous;
            }
            KeyCode::Delete if self.cursor < self.input.len() => {
                let next = self.input[self.cursor..]
                    .char_indices()
                    .nth(1)
                    .map(|(index, _)| self.cursor + index)
                    .unwrap_or(self.input.len());
                self.input.drain(self.cursor..next);
            }
            KeyCode::Left => {
                self.cursor = self.input[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map(|(index, _)| index)
                    .unwrap_or(0);
            }
            KeyCode::Right => {
                self.cursor = self.input[self.cursor..]
                    .char_indices()
                    .nth(1)
                    .map(|(index, _)| self.cursor + index)
                    .unwrap_or(self.input.len());
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.len(),
            _ => {}
        }
    }
}

struct ChatLayout {
    transcript: Rect,
    suggestions: Option<Rect>,
    composer: Rect,
    status: Rect,
}

fn chat_layout(area: Rect, composer_height: u16) -> ChatLayout {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .split(area);
    ChatLayout {
        transcript: chunks[0],
        suggestions: None,
        composer: chunks[1],
        status: chunks[2],
    }
}

fn chat_layout_with_suggestions(
    area: Rect,
    suggestion_count: usize,
    composer_height: u16,
) -> ChatLayout {
    if suggestion_count == 0 {
        return chat_layout(area, composer_height);
    }
    let suggestion_height = u16::try_from(suggestion_count).unwrap_or(u16::MAX);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(suggestion_height),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .split(area);
    ChatLayout {
        transcript: chunks[0],
        suggestions: Some(chunks[1]),
        composer: chunks[2],
        status: chunks[3],
    }
}

fn composer_text(input: &str) -> Text<'_> {
    let mut input_lines = input.split('\n');
    let first = input_lines.next().unwrap_or_default();
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "› ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(first),
    ])];
    lines.extend(input_lines.map(|line| {
        if line.is_empty() {
            Line::default()
        } else {
            Line::from(vec![Span::raw("  "), Span::raw(line)])
        }
    }));
    Text::from(lines)
}

fn composer_line_count(input: &str, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    u16::try_from(
        Paragraph::new(composer_text(input))
            .wrap(Wrap { trim: false })
            .line_count(width),
    )
    .unwrap_or(u16::MAX)
}

fn composer_height(ui: &ChatUi, width: u16) -> u16 {
    composer_line_count(&ui.input, width)
        .clamp(1, MAX_COMPOSER_CONTENT_HEIGHT)
        .saturating_add(2)
}

fn composer_cursor(ui: &ChatUi, width: u16) -> (u16, u16) {
    if width == 0 {
        return (0, 0);
    }
    let before_cursor = &ui.input[..ui.cursor];
    let row = composer_line_count(before_cursor, width).saturating_sub(1);
    let current_line = before_cursor
        .rsplit_once('\n')
        .map_or(before_cursor, |(_, line)| line);
    let column =
        u16::try_from(Line::from(format!("  {current_line}")).width()).unwrap_or(u16::MAX) % width;
    (row, column)
}

fn command_suggestions(commands: &[SlashCommandMatch], selected_command: usize) -> Text<'static> {
    Text::from(
        commands
            .iter()
            .enumerate()
            .map(|(index, command)| {
                let command_style = if index == selected_command {
                    Style::default()
                        .fg(Color::LightMagenta)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                Line::from(vec![
                    Span::styled(
                        format!("  {:<COMMAND_COLUMN_WIDTH$}", command.name),
                        command_style,
                    ),
                    Span::styled(command.description, Style::default().fg(Color::DarkGray)),
                ])
            })
            .collect::<Vec<_>>(),
    )
}

fn apply_command(ui: &mut ChatUi, history: &mut Vec<Message>, command: CommandAction) -> bool {
    match command {
        CommandAction::Model => {
            if !ui.busy {
                ui.picker.show();
            }
            false
        }
        CommandAction::Clear => {
            ui.messages.clear();
            ui.scroll_from_bottom = 0;
            history.clear();
            false
        }
        CommandAction::Help => {
            let mut help = Vec::new();
            for command in SLASH_COMMANDS {
                help.push(format!("{} — {}", command.name, command.description));
                help.extend(
                    command
                        .aliases
                        .iter()
                        .map(|alias| format!("{} — {}", alias.name, alias.description)),
                );
            }
            let help = help.join("\n");
            ui.push_message(MessageKind::Assistant, help);
            false
        }
        CommandAction::Quit => true,
    }
}

fn highlighted_user_line(mut line: Line<'static>, width: u16) -> Line<'static> {
    let width = usize::from(width);
    if width > 0 {
        let remainder = line.width() % width;
        if remainder > 0 {
            line.push_span(Span::raw(" ".repeat(width - remainder)));
        }
    }
    line.style(Style::default().fg(Color::White).bg(USER_BACKGROUND))
}

fn transcript_text(ui: &ChatUi, width: u16) -> Text<'static> {
    let mut lines = vec![Line::styled(
        "Rynna",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    if ui.messages.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            "Ask a question or describe a task to begin.",
            Style::default().fg(Color::DarkGray),
        ));
    }
    for message in &ui.messages {
        lines.push(Line::from(""));
        let mut content = message.content.split('\n');
        let first = content.next().unwrap_or_default().to_owned();
        match message.kind {
            MessageKind::User => {
                lines.push(highlighted_user_line(
                    Line::from(vec![
                        Span::styled("› ", Style::default().fg(Color::Gray)),
                        Span::raw(first),
                    ]),
                    width,
                ));
                lines.extend(
                    content
                        .map(|line| highlighted_user_line(Line::from(format!("  {line}")), width)),
                );
            }
            MessageKind::Thinking => {
                let marker = if message.expanded { "▼" } else { "▶" };
                let label = if message.expanded {
                    "Thinking".to_owned()
                } else {
                    format!(
                        "Thinking ({} lines)",
                        message.content.lines().count().max(1)
                    )
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{marker} "), Style::default().fg(Color::DarkGray)),
                    Span::styled(label, Style::default().fg(Color::DarkGray)),
                ]));
                if message.expanded {
                    lines.push(Line::styled(
                        format!("  {first}"),
                        Style::default().fg(Color::DarkGray),
                    ));
                    lines.extend(content.map(|line| {
                        Line::styled(format!("  {line}"), Style::default().fg(Color::DarkGray))
                    }));
                }
            }
            MessageKind::Assistant => {
                lines.push(Line::from(vec![
                    Span::styled("● ", Style::default().fg(Color::Cyan)),
                    Span::styled(first, Style::default().fg(Color::White)),
                ]));
                lines.extend(content.map(|line| {
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(line.to_owned(), Style::default().fg(Color::White)),
                    ])
                }));
            }
            MessageKind::Error => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "! ",
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(first, Style::default().fg(Color::Red)),
                ]));
                lines.extend(content.map(|line| {
                    Line::styled(format!("  {line}"), Style::default().fg(Color::Red))
                }));
            }
        }
    }
    Text::from(lines)
}

fn render(frame: &mut Frame<'_>, ui: &ChatUi) {
    let commands = ui.command_matches();
    let composer_height = composer_height(ui, frame.area().width);
    let layout = chat_layout_with_suggestions(frame.area(), commands.len(), composer_height);
    let text = transcript_text(ui, layout.transcript.width);
    let transcript = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(Color::White));
    let total_lines = transcript.line_count(layout.transcript.width) as u16;
    let maximum_scroll = total_lines.saturating_sub(layout.transcript.height);
    let scroll = maximum_scroll.saturating_sub(ui.scroll_from_bottom);
    frame.render_widget(transcript.scroll((scroll, 0)), layout.transcript);

    if let Some(area) = layout.suggestions {
        let selected_command = ui.selected_command.min(commands.len().saturating_sub(1));
        frame.render_widget(
            Paragraph::new(command_suggestions(&commands, selected_command)),
            area,
        );
    }

    let visible_composer_rows = layout.composer.height.saturating_sub(2).max(1);
    let (cursor_row, cursor_column) = composer_cursor(ui, layout.composer.width);
    let composer_scroll = cursor_row.saturating_sub(visible_composer_rows.saturating_sub(1));
    let composer = Paragraph::new(composer_text(&ui.input))
        .wrap(Wrap { trim: false })
        .scroll((composer_scroll, 0))
        .block(
            Block::default()
                .borders(Borders::TOP | Borders::BOTTOM)
                .border_style(Style::default().fg(Color::DarkGray))
                .title_bottom(Line::from(ui.picker.label()).right_aligned()),
        );
    frame.render_widget(composer, layout.composer);

    let state = if ui.busy { "Thinking…" } else { "Ready" };
    let controls = if commands.is_empty() {
        "  Enter send · F2 models · Alt-Enter newline · Ctrl-T thinking · PgUp/PgDn scroll · Ctrl-C exit"
    } else {
        "  ↑/↓ select · Tab complete · Enter run · Ctrl-C exit"
    };
    let status = Line::from(vec![
        Span::styled(
            format!(" {state} "),
            Style::default().fg(if ui.busy { Color::Yellow } else { Color::Green }),
        ),
        Span::styled(
            format!("{} · {}", ui.profile, ui.model),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(controls, Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(status), layout.status);

    if ui.picker.open {
        ui.picker.draw(
            frame,
            super::model_picker::popup_area(frame.area(), layout.composer),
        );
    }

    if !ui.picker.open && layout.composer.width > 4 {
        let x = layout
            .composer
            .x
            .saturating_add(cursor_column)
            .min(layout.composer.right().saturating_sub(1));
        let y = layout
            .composer
            .y
            .saturating_add(1)
            .saturating_add(cursor_row.saturating_sub(composer_scroll))
            .min(layout.composer.bottom().saturating_sub(2));
        frame.set_cursor_position((x, y));
    }
}

enum ResponseEvent {
    Delta(CompletionDelta),
    Finished {
        prompt: String,
        result: Result<Message, String>,
    },
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("failed to enable terminal raw mode")?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
            return Err(error).context("failed to enter terminal screen");
        }
        let terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = disable_raw_mode();
                let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
                return Err(error).context("failed to initialize terminal UI");
            }
        };
        Ok(Self { terminal })
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}

pub async fn run(
    profiles: &AgentProfiles,
    profile: &str,
    model: &str,
    workspace: Option<&str>,
) -> Result<()> {
    let mut session = TerminalSession::enter()?;
    let mut ui = ChatUi::new(profile, model);
    ui.picker.pairs = profiles
        .profiles()
        .into_iter()
        .find(|p| p.name == profile)
        .map(|p| p.providers.into_iter().filter(|p| p.enabled).collect())
        .unwrap_or_default();
    let mut history = Vec::<Message>::new();
    let mut selection = None;
    ui.push_message(
        MessageKind::Assistant,
        "Click the model label or press F2 to choose a model and thinking level.",
    );
    let mut memory_session = uuid::Uuid::new_v4();
    let mut events = EventStream::new();
    let (response_tx, mut response_rx) = mpsc::unbounded_channel::<ResponseEvent>();

    loop {
        session
            .terminal
            .draw(|frame| render(frame, &ui))
            .context("failed to draw terminal UI")?;
        tokio::select! {
            maybe_event = events.next() => {
                let event = maybe_event
                    .context("terminal event stream ended unexpectedly")?
                    .context("failed to read terminal input")?;
                let action = match event {
                    Event::Mouse(mouse) if !ui.busy => {
                        let size = session.terminal.size()?;
                        let screen = Rect::new(0, 0, size.width, size.height);
                        let layout = chat_layout_with_suggestions(screen, ui.command_matches().len(), composer_height(&ui, screen.width));
                        if ui.picker.open {
                            ui.picker.mouse(mouse, super::model_picker::popup_area(screen, layout.composer)).map(InputAction::Selection)
                        } else {
                            match mouse.kind {
                                MouseEventKind::ScrollUp => ui.scroll_from_bottom = ui.scroll_from_bottom.saturating_add(5),
                                MouseEventKind::ScrollDown => ui.scroll_from_bottom = ui.scroll_from_bottom.saturating_sub(5),
                                _ => {}
                            }
                            let label_width = Line::from(ui.picker.label()).width().min(usize::from(screen.width)) as u16;
                            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                                && mouse.row == layout.composer.bottom().saturating_sub(1)
                                && mouse.column >= screen.right().saturating_sub(label_width) {
                                ui.picker.show();
                            }
                            None
                        }
                    }
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) { break; }
                        match key.code {
                            KeyCode::PageUp if !ui.picker.open => { ui.scroll_from_bottom = ui.scroll_from_bottom.saturating_add(5); None }
                            KeyCode::PageDown if !ui.picker.open => { ui.scroll_from_bottom = ui.scroll_from_bottom.saturating_sub(5); None }
                            _ => ui.handle_key(key),
                        }
                    }
                    _ => None,
                };
                if let Some(action) = action {
                        let prompt = match action {
                            InputAction::Selection(command) => {
                                match super::model_selection::apply(profiles, profile, &mut selection, &command) {
                                    Ok(feedback) => {
                                        ui.picker.selection = selection.clone();
                                        ui.model = super::sanitize_terminal_text(&super::model_selection::summary(selection.as_ref()));
                                        ui.push_message(MessageKind::Assistant, super::sanitize_terminal_text(&feedback));
                                    }
                                    Err(error) => ui.push_message(MessageKind::Error, super::sanitize_terminal_text(&error)),
                                }
                                continue;
                            }
                            InputAction::Command(command) => {
                                if matches!(command, CommandAction::Clear) {
                                    memory_session = uuid::Uuid::new_v4();
                                }
                                if apply_command(&mut ui, &mut history, command) {
                                    break;
                                }
                                continue;
                            }
                            InputAction::Prompt(prompt) => prompt,
                        };

                        ui.busy = true;
                        let profiles = profiles.clone().with_memory_session(Some(memory_session))
                            .with_workspace(Some(profile), workspace)?
                            .with_model_selection(Some(profile), selection.as_ref())?;
                        let profile = profile.to_owned();
                        let request_history = history.clone();
                        let sender = response_tx.clone();
                        tokio::spawn(async move {
                            let result = {
                                let delta_sender = sender.clone();
                                let mut on_delta = move |delta: &CompletionDelta| {
                                    let delta = match delta {
                                        CompletionDelta::Thinking(content) => {
                                            CompletionDelta::Thinking(
                                                super::sanitize_terminal_text(content),
                                            )
                                        }
                                        CompletionDelta::Content(content) => {
                                            CompletionDelta::Content(
                                                super::sanitize_terminal_text(content),
                                            )
                                        }
                                    };
                                    let _ = delta_sender.send(ResponseEvent::Delta(delta));
                                };
                                profiles
                                    .respond_stream(
                                        Some(&profile),
                                        &request_history,
                                        &prompt,
                                        &mut on_delta,
                                    )
                                    .await
                                    .map_err(|error| {
                                        super::sanitize_terminal_text(&error.to_string())
                                    })
                            };
                            let _ = sender.send(ResponseEvent::Finished { prompt, result });
                        });
                }
            }
            Some(response) = response_rx.recv() => {
                match response {
                    ResponseEvent::Delta(delta) => ui.append_completion_delta(&delta),
                    ResponseEvent::Finished { prompt, result } => {
                        ui.busy = false;
                        match result {
                            Ok(message) => {
                                history.push(Message::user(prompt));
                                history.push(message);
                            }
                            Err(error) => ui.push_message(MessageKind::Error, error),
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect, style::Color};

    use super::{
        ChatUi, CommandAction, CompletionDelta, InputAction, MAX_COMPOSER_CONTENT_HEIGHT, Message,
        MessageKind, apply_command, chat_layout, composer_cursor, composer_height, render,
    };

    #[test]
    fn selection_commands_complete_and_dispatch_without_becoming_prompts() {
        for (prefix, command) in [("/p", "/provider"), ("/t", "/thinking")] {
            for complete in [false, true] {
                let mut ui = ChatUi::new("local", "small");
                ui.input = prefix.into();
                ui.cursor = ui.input.len();
                if complete {
                    ui.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
                    assert_eq!(ui.input, command);
                }
                assert_eq!(
                    ui.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                    Some(InputAction::Selection(command.into()))
                );
                assert!(ui.input.is_empty());
                assert!(ui.messages.is_empty());
            }
        }
        for command in ["/provider cloud", "/thinking high", "/model 2"] {
            let mut ui = ChatUi::new("local", "small");
            ui.input = command.into();
            ui.cursor = ui.input.len();
            assert_eq!(
                ui.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                Some(InputAction::Selection(command.into()))
            );
            assert!(ui.messages.is_empty());
        }
    }

    #[test]
    fn alt_enter_in_model_command_inserts_newline_without_opening_picker() {
        let mut ui = ChatUi::new("local", "small");
        ui.picker.pairs = vec![rynna_core::ProfileProvider {
            provider: "local".into(),
            model: "small".into(),
            enabled: true,
            is_default: true,
        }];
        ui.input = "/model".into();
        ui.cursor = ui.input.len();
        let action = ui.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(action, None);
        assert_eq!(ui.input, "/model\n");
        assert_eq!(ui.cursor, ui.input.len());
        assert!(!ui.picker.open);
        assert!(ui.messages.is_empty());
    }

    #[test]
    fn model_command_opens_picker_from_full_command_or_autocomplete() {
        for input in ["/model", "/model ", "/m", "/mo"] {
            let mut ui = ChatUi::new("local", "small");
            ui.picker.pairs = vec![rynna_core::ProfileProvider {
                provider: "local".into(),
                model: "small".into(),
                enabled: true,
                is_default: true,
            }];
            ui.input = input.into();
            ui.cursor = ui.input.len();
            if input == "/m" {
                ui.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
                assert_eq!(ui.input, "/model");
            }
            let action = ui.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            assert_eq!(action, Some(InputAction::Command(CommandAction::Model)));
            let mut history = vec![Message::user("Previous question")];
            assert!(!apply_command(&mut ui, &mut history, CommandAction::Model));
            assert!(ui.picker.open);
            assert!(ui.input.is_empty());
            assert!(ui.messages.is_empty());
            assert_eq!(history.len(), 1);
        }
    }

    #[test]
    fn picker_preserves_draft_and_blocks_opening_during_response() {
        let mut ui = ChatUi::new("local", "small");
        ui.picker.pairs = vec![rynna_core::ProfileProvider {
            provider: "local".into(),
            model: "small".into(),
            enabled: true,
            is_default: true,
        }];
        ui.input = "Unfinished prompt".into();
        ui.cursor = 5;
        ui.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        assert!(ui.picker.open);
        ui.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert_eq!(
            ui.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(InputAction::Selection("/model 1".into()))
        );
        assert_eq!(ui.input, "Unfinished prompt");
        assert_eq!(ui.cursor, 5);
        assert!(ui.messages.is_empty());
        ui.busy = true;
        ui.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        assert!(!ui.picker.open);
    }

    #[test]
    fn layout_keeps_the_composer_and_status_at_the_bottom() {
        let area = Rect::new(0, 0, 100, 30);

        let layout = chat_layout(area, 3);

        assert_eq!(layout.transcript, Rect::new(0, 0, 100, 26));
        assert_eq!(layout.composer, Rect::new(0, 26, 100, 3));
        assert_eq!(layout.status, Rect::new(0, 29, 100, 1));
    }

    #[test]
    fn alt_enter_inserts_a_newline_at_the_cursor_without_submitting() {
        let mut ui = ChatUi::new("local", "test-model");
        ui.input = "firstsecond".to_owned();
        ui.cursor = 5;

        let action = ui.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));

        assert_eq!(action, None);
        assert_eq!(ui.input, "first\nsecond");
        assert_eq!(ui.cursor, 6);
        assert!(ui.messages().is_empty());
    }

    #[test]
    fn trailing_newline_places_the_cursor_on_the_immediate_next_row() {
        let mut ui = ChatUi::new("local", "test-model");
        ui.input = "first\nsecond\n".to_owned();
        ui.cursor = ui.input.len();

        assert_eq!(composer_cursor(&ui, 40), (2, 2));
        assert_eq!(composer_height(&ui, 40), 5);
    }

    #[test]
    fn an_explicit_blank_line_occupies_exactly_one_visual_row() {
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut ui = ChatUi::new("local", "test-model");
        ui.input = "line 1\nline 2\n\nline 4".to_owned();
        ui.cursor = ui.input.len();

        terminal.draw(|frame| render(frame, &ui)).unwrap();

        let screen = terminal.backend().to_string();
        let rows = screen.lines().collect::<Vec<_>>();
        let line_2_row = rows.iter().position(|row| row.contains("line 2")).unwrap();
        let line_4_row = rows.iter().position(|row| row.contains("line 4")).unwrap();
        assert_eq!(line_4_row - line_2_row, 2, "{screen}");
        assert_eq!(composer_height(&ui, 40), 6);
    }

    #[test]
    fn composer_grows_with_input_until_its_content_height_limit() {
        let mut ui = ChatUi::new("local", "test-model");
        let width = 40;

        ui.input = "one\ntwo\nthree".to_owned();
        assert_eq!(composer_height(&ui, width), 5);

        ui.input = (0..MAX_COMPOSER_CONTENT_HEIGHT + 3)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(composer_height(&ui, width), MAX_COMPOSER_CONTENT_HEIGHT + 2);
    }

    #[test]
    fn composer_scrolls_to_keep_the_cursor_visible_after_reaching_its_limit() {
        let backend = TestBackend::new(40, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut ui = ChatUi::new("local", "test-model");
        ui.input = (0..MAX_COMPOSER_CONTENT_HEIGHT + 2)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        ui.cursor = ui.input.len();

        terminal.draw(|frame| render(frame, &ui)).unwrap();

        let screen = terminal.backend().to_string();
        assert!(!screen.contains("line 0"), "{screen}");
        assert!(!screen.contains("line 1"), "{screen}");
        assert!(
            screen.contains(&format!("line {}", MAX_COMPOSER_CONTENT_HEIGHT + 1)),
            "{screen}"
        );
    }

    #[test]
    fn render_distinguishes_user_and_assistant_rows_with_backgrounds() {
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut ui = ChatUi::new("local", "test-model");
        ui.push_message(MessageKind::User, "Question");
        ui.push_message(MessageKind::Assistant, "Answer");

        terminal.draw(|frame| render(frame, &ui)).unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((0, 2)).unwrap().symbol(), "›");
        assert_eq!(buffer.cell((0, 2)).unwrap().bg, Color::Rgb(52, 52, 52));
        assert_eq!(buffer.cell((39, 2)).unwrap().bg, Color::Rgb(52, 52, 52));
        assert_eq!(buffer.cell((0, 4)).unwrap().symbol(), "●");
        assert_eq!(buffer.cell((0, 4)).unwrap().bg, Color::Reset);
        assert_eq!(buffer.cell((39, 4)).unwrap().bg, Color::Reset);
    }

    #[test]
    fn render_keeps_the_latest_chat_and_input_visible() {
        let backend = TestBackend::new(48, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut ui = ChatUi::new("local", "test-model");
        for index in 0..12 {
            ui.push_message(MessageKind::Assistant, format!("old response {index}"));
        }
        ui.push_message(MessageKind::User, "latest question");
        ui.input = "next prompt".to_owned();

        terminal.draw(|frame| render(frame, &ui)).unwrap();

        let screen = terminal.backend().to_string();
        assert!(screen.contains("latest question"), "{screen}");
        assert!(screen.contains("› next prompt"), "{screen}");
        assert!(screen.contains("local · test-model"), "{screen}");
        assert!(!screen.contains("old response 0"), "{screen}");
    }

    #[test]
    fn typing_a_slash_shows_commands_with_descriptions() {
        let backend = TestBackend::new(72, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut ui = ChatUi::new("local", "test-model");
        ui.handle_edit_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));

        terminal.draw(|frame| render(frame, &ui)).unwrap();

        let screen = terminal.backend().to_string();
        assert!(screen.contains("/clear"), "{screen}");
        assert!(screen.contains("Clear the conversation"), "{screen}");
        assert!(screen.contains("/provider"), "{screen}");
        assert!(
            screen.contains("List providers or set <provider>"),
            "{screen}"
        );
        assert!(screen.contains("/thinking"), "{screen}");
        assert!(
            screen.contains("Set default|low|medium|high effort"),
            "{screen}"
        );
        assert!(screen.contains("/model"), "{screen}");
        assert!(screen.contains("Open the model selector"), "{screen}");
        assert!(screen.contains("/help"), "{screen}");
        assert!(screen.contains("Show available commands"), "{screen}");
        assert!(screen.contains("/quit"), "{screen}");
        assert!(screen.contains("Exit Rynna"), "{screen}");
        assert!(screen.contains("/exit"), "{screen}");
        assert!(screen.contains("Alias for /quit"), "{screen}");
    }

    #[test]
    fn slash_commands_filter_as_the_user_types() {
        let backend = TestBackend::new(72, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut ui = ChatUi::new("local", "test-model");
        for character in "/q".chars() {
            ui.handle_edit_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }

        terminal.draw(|frame| render(frame, &ui)).unwrap();

        let screen = terminal.backend().to_string();
        assert!(screen.contains("/quit"), "{screen}");
        assert!(!screen.contains("/clear"), "{screen}");
        assert!(!screen.contains("/help"), "{screen}");
    }

    #[test]
    fn arrows_select_a_command_and_tab_completes_it() {
        let mut ui = ChatUi::new("local", "test-model");
        ui.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));

        ui.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        ui.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

        assert_eq!(ui.input, "/help");
        assert_eq!(ui.cursor, ui.input.len());
    }

    #[test]
    fn enter_runs_the_selected_command_instead_of_sending_a_prompt() {
        let mut ui = ChatUi::new("local", "test-model");
        for character in "/h".chars() {
            ui.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }

        let action = ui.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(action, Some(InputAction::Command(CommandAction::Help)));
        assert!(ui.input.is_empty());
        assert!(ui.messages().is_empty());
    }

    #[test]
    fn exit_alias_executes_the_quit_command() {
        let mut ui = ChatUi::new("local", "test-model");
        for character in "/exit".chars() {
            ui.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }

        let action = ui.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(action, Some(InputAction::Command(CommandAction::Quit)));
        assert!(ui.input.is_empty());
        assert!(ui.messages().is_empty());
    }

    #[test]
    fn clear_command_resets_displayed_and_model_history() {
        let mut ui = ChatUi::new("local", "test-model");
        ui.push_message(MessageKind::User, "old question");
        let mut history = vec![Message::user("old question")];

        let should_quit = apply_command(&mut ui, &mut history, CommandAction::Clear);

        assert!(!should_quit);
        assert!(ui.messages().is_empty());
        assert!(history.is_empty());
    }

    #[test]
    fn help_command_displays_command_descriptions() {
        let mut ui = ChatUi::new("local", "test-model");
        let mut history = Vec::new();

        let should_quit = apply_command(&mut ui, &mut history, CommandAction::Help);

        assert!(!should_quit);
        let help = &ui.messages().last().unwrap().content;
        assert!(help.contains("/clear — Clear the conversation"), "{help}");
        assert!(help.contains("/help — Show available commands"), "{help}");
        assert!(help.contains("/quit — Exit Rynna"), "{help}");
        assert!(help.contains("/exit — Alias for /quit"), "{help}");
    }

    #[test]
    fn unknown_slash_command_is_not_submitted_to_the_model() {
        let mut ui = ChatUi::new("local", "test-model");
        ui.input = "/missing".to_owned();
        ui.cursor = ui.input.len();

        let action = ui.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(action, None);
        assert_eq!(ui.messages().last().unwrap().kind, MessageKind::Error);
        assert_eq!(
            ui.messages().last().unwrap().content,
            "Unknown command: /missing"
        );
    }

    #[test]
    fn editor_supports_cursor_movement_and_insertion() {
        let mut ui = ChatUi::new("local", "test-model");
        ui.input = "helo".to_owned();
        ui.cursor = 2;

        ui.handle_edit_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        ui.handle_edit_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        ui.handle_edit_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));

        assert_eq!(ui.input, "hello!");
        assert_eq!(ui.cursor, ui.input.len());
    }

    #[test]
    fn submitting_moves_the_prompt_into_the_transcript() {
        let mut ui = ChatUi::new("local", "test-model");
        ui.input = "Explain this code".to_owned();
        ui.cursor = ui.input.len();

        let prompt = ui.take_submission().unwrap();

        assert_eq!(prompt, "Explain this code");
        assert!(ui.input.is_empty());
        assert_eq!(ui.cursor, 0);
        assert_eq!(ui.messages().last().unwrap().content, "Explain this code");
        assert_eq!(ui.messages().last().unwrap().kind, MessageKind::User);
    }

    #[test]
    fn editor_accepts_input_while_a_response_is_in_flight() {
        let mut ui = ChatUi::new("local", "test-model");
        ui.busy = true;

        let submission = ui.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));

        assert_eq!(submission, None);
        assert_eq!(ui.input, "n");
        assert_eq!(ui.cursor, 1);
    }

    #[test]
    fn enter_does_not_submit_the_next_prompt_while_busy() {
        let mut ui = ChatUi::new("local", "test-model");
        ui.busy = true;
        ui.input = "queue this next".to_owned();
        ui.cursor = ui.input.len();

        let submission = ui.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(submission, None);
        assert_eq!(ui.input, "queue this next");
        assert_eq!(ui.messages().len(), 0);
    }

    #[test]
    fn assistant_response_is_rendered_incrementally() {
        let mut ui = ChatUi::new("local", "test-model");

        ui.start_assistant_response();
        ui.append_assistant_delta("Hello");
        ui.append_assistant_delta(" world");

        assert_eq!(ui.messages().len(), 1);
        assert_eq!(ui.messages()[0].kind, MessageKind::Assistant);
        assert_eq!(ui.messages()[0].content, "Hello world");
    }

    #[test]
    fn reasoning_streams_separately_then_collapses_when_user_facing_content_begins() {
        let mut ui = ChatUi::new("local", "test-model");

        ui.append_completion_delta(&CompletionDelta::Thinking("Check".to_owned()));
        ui.append_completion_delta(&CompletionDelta::Thinking(" facts".to_owned()));
        assert_eq!(ui.messages().len(), 1);
        assert_eq!(ui.messages()[0].kind, MessageKind::Thinking);
        assert_eq!(ui.messages()[0].content, "Check facts");
        assert!(ui.messages()[0].expanded);

        ui.append_completion_delta(&CompletionDelta::Content("Answer".to_owned()));

        assert_eq!(ui.messages().len(), 2);
        assert!(!ui.messages()[0].expanded);
        assert_eq!(ui.messages()[1].kind, MessageKind::Assistant);
        assert_eq!(ui.messages()[1].content, "Answer");
    }

    #[test]
    fn collapsed_reasoning_can_be_expanded_with_ctrl_t() {
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut ui = ChatUi::new("local", "test-model");
        ui.append_completion_delta(&CompletionDelta::Thinking(
            "Inspect the request\nCompare the fields".to_owned(),
        ));
        ui.append_completion_delta(&CompletionDelta::Content("Final answer".to_owned()));

        terminal.draw(|frame| render(frame, &ui)).unwrap();
        let collapsed = terminal.backend().to_string();
        assert!(collapsed.contains("Thinking (2 lines)"), "{collapsed}");
        assert!(!collapsed.contains("Inspect the request"), "{collapsed}");

        ui.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        terminal.draw(|frame| render(frame, &ui)).unwrap();
        let expanded = terminal.backend().to_string();
        assert!(expanded.contains("Inspect the request"), "{expanded}");
        assert!(expanded.contains("Compare the fields"), "{expanded}");
        assert!(expanded.contains("Final answer"), "{expanded}");
    }
}
