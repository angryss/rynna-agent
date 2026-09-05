use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
};
use rynna_core::{ModelSelection, ProfileProvider};

const LEVELS: [&str; 4] = ["default", "low", "medium", "high"];

#[derive(Default)]
pub struct ModelPicker {
    pub pairs: Vec<ProfileProvider>,
    pub selection: Option<ModelSelection>,
    pub open: bool,
    query: String,
    highlighted: usize,
}

impl ModelPicker {
    pub fn label(&self) -> String {
        let model = self
            .selection
            .as_ref()
            .map(|s| s.model.as_str())
            .or_else(|| {
                self.pairs
                    .iter()
                    .find(|p| p.is_default)
                    .or(self.pairs.first())
                    .map(|p| p.model.as_str())
            })
            .unwrap_or("No models");
        format!(
            " {} · {} ▾ F2 ",
            super::sanitize_terminal_text(model),
            self.level()
        )
    }

    fn level(&self) -> &str {
        self.selection
            .as_ref()
            .map_or("default", |s| s.thinking.as_str())
    }

    pub fn show(&mut self) {
        if !self.pairs.is_empty() {
            self.open = true;
            self.query.clear();
            self.highlighted = 0;
        }
    }

    // None identifies Profile default; indices retain the runtime command ordering.
    fn options(&self) -> Vec<Option<usize>> {
        let query = self.query.trim().to_lowercase();
        let mut options = Vec::new();
        if query.is_empty() {
            options.push(None);
        }
        let mut providers = Vec::new();
        for pair in &self.pairs {
            if !providers.contains(&pair.provider) {
                providers.push(pair.provider.clone());
            }
        }
        for provider in providers {
            for (index, pair) in self.pairs.iter().enumerate() {
                if pair.provider == provider
                    && format!("{} {}", pair.provider, pair.model)
                        .to_lowercase()
                        .contains(&query)
                {
                    options.push(Some(index));
                }
            }
        }
        options
    }

    fn choose(&mut self) -> Option<String> {
        let option = *self.options().get(self.highlighted)?;
        self.open = false;
        Some(option.map_or_else(
            || "/model default".to_owned(),
            |index| {
                let pair = &self.pairs[index];
                if self
                    .selection
                    .as_ref()
                    .is_some_and(|s| s.provider == pair.provider && s.model == pair.model)
                {
                    format!("/thinking {}", self.level())
                } else {
                    format!("/model {}", index + 1)
                }
            },
        ))
    }

    pub fn key(&mut self, key: KeyEvent) -> Option<String> {
        let count = self.options().len();
        match key.code {
            KeyCode::Esc | KeyCode::F(2) => self.open = false,
            KeyCode::Enter => return self.choose(),
            KeyCode::Up => {
                self.highlighted = self
                    .highlighted
                    .checked_sub(1)
                    .unwrap_or(count.saturating_sub(1))
            }
            KeyCode::Down => self.highlighted = (self.highlighted + 1) % count.max(1),
            KeyCode::Tab | KeyCode::Right | KeyCode::Left | KeyCode::BackTab => {
                let index = LEVELS
                    .iter()
                    .position(|level| *level == self.level())
                    .unwrap_or(0);
                let step = if matches!(key.code, KeyCode::Left | KeyCode::BackTab) {
                    3
                } else {
                    1
                };
                return Some(format!("/thinking {}", LEVELS[(index + step) % 4]));
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.highlighted = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.query.push(c);
                self.highlighted = 0;
            }
            _ => {}
        }
        None
    }

    fn rows(&self) -> Vec<(String, Option<usize>)> {
        let mut rows = Vec::new();
        let mut previous = None;
        for (option_index, option) in self.options().into_iter().enumerate() {
            let (label, selected) = match option {
                None => ("Profile default".to_owned(), self.selection.is_none()),
                Some(index) => {
                    let pair = &self.pairs[index];
                    if previous != Some(&pair.provider) {
                        rows.push((super::sanitize_terminal_text(&pair.provider), None));
                        previous = Some(&pair.provider);
                    }
                    (
                        super::sanitize_terminal_text(&pair.model),
                        self.selection
                            .as_ref()
                            .is_some_and(|s| s.provider == pair.provider && s.model == pair.model),
                    )
                }
            };
            rows.push((
                format!("{} {label}", if selected { "✓" } else { " " }),
                Some(option_index),
            ));
        }
        if rows.is_empty() {
            rows.push(("No matching models".to_owned(), None));
        }
        rows
    }

    fn offset(&self, height: u16) -> usize {
        let selected = self
            .rows()
            .iter()
            .position(|(_, i)| *i == Some(self.highlighted))
            .unwrap_or(0);
        selected.saturating_sub(usize::from(height).saturating_sub(1))
    }

    pub fn mouse(&mut self, event: MouseEvent, area: Rect) -> Option<String> {
        if matches!(
            event.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            let key = if event.kind == MouseEventKind::ScrollUp {
                KeyCode::Up
            } else {
                KeyCode::Down
            };
            return self.key(KeyEvent::new(key, KeyModifiers::NONE));
        }
        if !matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
            return None;
        }
        if !area.contains((event.column, event.row).into()) {
            self.open = false;
            return None;
        }
        let inner = area.inner(ratatui::layout::Margin::new(1, 1));
        if inner.height < 4 || !inner.contains((event.column, event.row).into()) {
            return None;
        }
        if event.row == inner.bottom() - 2 && inner.width > 0 {
            let index = usize::from(
                ((u32::from(event.column - inner.x + 1) * 4 - 1) / u32::from(inner.width)).min(3)
                    as u16,
            );
            return Some(format!("/thinking {}", LEVELS[index]));
        }
        let list_height = inner.height.saturating_sub(3);
        if event.row > inner.y && event.row < inner.y + 1 + list_height {
            let row = usize::from(event.row - inner.y - 1) + self.offset(list_height);
            if let Some((_, Some(index))) = self.rows().get(row) {
                self.highlighted = *index;
                return self.choose();
            }
        }
        None
    }

    pub fn draw(&self, frame: &mut Frame<'_>, area: Rect) {
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Models ")
            .border_style(Style::default().fg(Color::Cyan));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height < 4 {
            return;
        }
        frame.render_widget(
            Paragraph::new(format!("Search: {}", self.query)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        let height = inner.height - 3;
        let rows = self.rows();
        let lines: Vec<_> = rows
            .iter()
            .skip(self.offset(height))
            .take(usize::from(height))
            .map(|(label, index)| {
                let style = if *index == Some(self.highlighted) {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else if index.is_none() {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default()
                };
                Line::styled(label.clone(), style)
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines),
            Rect::new(inner.x, inner.y + 1, inner.width, height),
        );
        for (i, level) in LEVELS.iter().enumerate() {
            let start = inner.width * i as u16 / 4;
            let end = inner.width * (i + 1) as u16 / 4;
            let style = if *level == self.level() {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::Gray)
            };
            frame.render_widget(
                Paragraph::new(*level).style(style),
                Rect::new(inner.x + start, inner.bottom() - 2, end - start, 1),
            );
        }
        frame.render_widget(
            Paragraph::new("↑↓ model · ←→ effort · Enter · Esc")
                .style(Style::default().fg(Color::DarkGray)),
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        );
    }
}

pub fn popup_area(screen: Rect, composer: Rect) -> Rect {
    let width = screen.width.min(52);
    let height = composer
        .y
        .saturating_sub(screen.y)
        .clamp(7, 18)
        .min(screen.height);
    Rect::new(
        screen.right().saturating_sub(width),
        composer.y.saturating_sub(height).max(screen.y),
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use rynna_core::ThinkingLevel;

    fn picker() -> ModelPicker {
        ModelPicker {
            pairs: [("local", "small"), ("cloud", "deep"), ("local", "large")]
                .into_iter()
                .enumerate()
                .map(|(i, (provider, model))| ProfileProvider {
                    provider: provider.into(),
                    model: model.into(),
                    enabled: true,
                    is_default: i == 0,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn search_groups_providers_but_preserves_runtime_command_indices() {
        let mut picker = picker();
        picker.show();
        assert_eq!(picker.options(), vec![None, Some(0), Some(2), Some(1)]);
        for c in "CLOUD".chars() {
            picker.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(
            picker.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some("/model 2".into())
        );
        assert!(!picker.open);
        picker.show();
        picker.key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(
            picker.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            None
        );
        assert!(picker.open);
        picker.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!picker.open);
    }

    #[test]
    fn thinking_and_reselection_preserve_effort_and_default_restores_routing() {
        let mut picker = picker();
        picker.selection = Some(ModelSelection {
            provider: "cloud".into(),
            model: "deep".into(),
            thinking: ThinkingLevel::High,
        });
        picker.show();
        assert_eq!(
            picker.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
            Some("/thinking medium".into())
        );
        assert!(picker.open);
        picker.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(
            picker.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some("/thinking high".into())
        );
        picker.show();
        assert_eq!(
            picker.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some("/model default".into())
        );
    }

    #[test]
    fn mouse_selects_visible_rows_effort_and_dismisses_outside() {
        let mut picker = picker();
        picker.show();
        let area = Rect::new(0, 0, 52, 18);
        let click = |column, row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            picker.mouse(click(13, 15), area),
            Some("/thinking low".into())
        );
        assert_eq!(picker.mouse(click(2, 4), area), Some("/model 1".into()));
        picker.show();
        picker.mouse(click(60, 20), area);
        assert!(!picker.open);
    }

    #[test]
    fn renders_groups_selection_and_scrolls_to_keyboard_choice_on_small_screens() {
        let mut picker = picker();
        picker.show();
        for (width, height) in [(80, 24), (32, 10), (12, 5)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    picker.draw(
                        frame,
                        popup_area(
                            frame.area(),
                            Rect::new(0, height.saturating_sub(4), width, 3),
                        ),
                    )
                })
                .unwrap();
        }
        let mut terminal = Terminal::new(TestBackend::new(52, 18)).unwrap();
        terminal
            .draw(|frame| picker.draw(frame, frame.area()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        for expected in [
            "Search:",
            "Profile default",
            "local",
            "cloud",
            "deep",
            "medium",
        ] {
            assert!(text.contains(expected), "missing {expected}");
        }
        picker.highlighted = 3;
        let offset = picker.offset(2);
        assert!(offset > 0);
        assert_eq!(picker.rows()[offset + 1].1, Some(3));
    }
}
