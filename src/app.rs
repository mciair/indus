use std::{ops::Range, path::PathBuf, time::Instant};

use ratatui::layout::Rect;

use crate::{
    slash::{COMMANDS, CompletionPhase, SlashMenu},
    theme::ThemeKind,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuAction {
    Changelog,
    Resume,
    Worktree,
    Quit,
}

#[derive(Clone, Copy, Debug)]
pub struct MenuItem {
    pub label: &'static str,
    pub key: &'static str,
    pub action: MenuAction,
}

pub const HOME_MENU: &[MenuItem] = &[
    MenuItem {
        label: "Changelog",
        key: "Enter",
        action: MenuAction::Changelog,
    },
    MenuItem {
        label: "Resume Session",
        key: "Enter",
        action: MenuAction::Resume,
    },
    MenuItem {
        label: "New Worktree",
        key: "Enter",
        action: MenuAction::Worktree,
    },
    MenuItem {
        label: "Quit",
        key: "Ctrl+Q",
        action: MenuAction::Quit,
    },
];

#[derive(Debug, Default)]
pub struct HitZones {
    pub menu: Vec<(Rect, MenuAction)>,
    pub alpha: Option<Rect>,
    pub slash_rows: Vec<(Rect, usize)>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Composer {
    text: String,
    cursor: usize,
}

impl Composer {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn set(&mut self, value: impl Into<String>) {
        self.text = value.into();
        self.cursor = self.text.len();
    }

    pub fn clear(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub fn insert_char(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub fn backspace(&mut self) {
        let Some(previous) = self.text[..self.cursor].char_indices().next_back() else {
            return;
        };
        self.text.replace_range(previous.0..self.cursor, "");
        self.cursor = previous.0;
    }

    pub fn delete(&mut self) {
        let Some(ch) = self.text[self.cursor..].chars().next() else {
            return;
        };
        self.text
            .replace_range(self.cursor..self.cursor + ch.len_utf8(), "");
    }

    pub fn move_left(&mut self) {
        if let Some((index, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.cursor = index;
        }
    }

    pub fn move_right(&mut self) {
        if let Some(ch) = self.text[self.cursor..].chars().next() {
            self.cursor += ch.len_utf8();
        }
    }

    pub fn move_home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
    }

    pub fn move_end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |offset| self.cursor + offset);
    }

    pub fn replace_range(&mut self, range: Range<usize>, replacement: &str) -> bool {
        if range.start > range.end
            || range.end > self.text.len()
            || !self.text.is_char_boundary(range.start)
            || !self.text.is_char_boundary(range.end)
        {
            return false;
        }
        self.text.replace_range(range.clone(), replacement);
        self.cursor = range.start + replacement.len();
        true
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranscriptEntry {
    User {
        text: String,
        slash_tokens: Vec<Range<usize>>,
    },
    Thinking {
        text: String,
        running: bool,
        elapsed_ms: Option<u128>,
    },
    Assistant {
        text: String,
        streaming: bool,
    },
    Event(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TurnActivity {
    WaitingForResponse,
    Thinking,
    Responding,
    RunningTool(String),
    Retrying(u16),
    Cancelling,
}

impl TurnActivity {
    pub fn label(&self) -> String {
        match self {
            Self::WaitingForResponse => "Waiting for response…".to_string(),
            Self::Thinking => "Thinking…".to_string(),
            Self::Responding => "Responding…".to_string(),
            Self::RunningTool(tool) => format!("Run {tool}"),
            Self::Retrying(attempt) => format!("Retrying (attempt {attempt})…"),
            Self::Cancelling => "Cancelling…".to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ActiveTurn {
    pub activity: TurnActivity,
    pub started_at: Instant,
    pub activity_started_at: Instant,
    pub thinking_entry: Option<usize>,
    pub assistant_entry: Option<usize>,
}

impl ActiveTurn {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            activity: TurnActivity::WaitingForResponse,
            started_at: now,
            activity_started_at: now,
            thinking_entry: None,
            assistant_entry: None,
        }
    }

    fn set_activity(&mut self, activity: TurnActivity) {
        self.activity = activity;
        self.activity_started_at = Instant::now();
    }
}

pub struct App {
    pub cwd: PathBuf,
    pub composer: Composer,
    pub transcript: Vec<TranscriptEntry>,
    pub selected_menu: usize,
    pub slash: SlashMenu,
    pub slash_scroll: usize,
    pub theme_kind: ThemeKind,
    pub preview_theme: Option<ThemeKind>,
    pub turn: Option<ActiveTurn>,
    pub animation_tick: u64,
    pub running: bool,
    pub hit_zones: HitZones,
}

impl App {
    pub fn new() -> Self {
        let theme_kind = std::env::var("INDUS_THEME")
            .ok()
            .and_then(|name| ThemeKind::from_name(&name))
            .unwrap_or_default();
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            composer: Composer::default(),
            transcript: Vec::new(),
            selected_menu: 0,
            slash: SlashMenu::default(),
            slash_scroll: 0,
            theme_kind,
            preview_theme: None,
            turn: None,
            animation_tick: 0,
            running: true,
            hit_zones: HitZones::default(),
        }
    }

    pub fn effective_theme_kind(&self) -> ThemeKind {
        self.preview_theme.unwrap_or(self.theme_kind)
    }

    pub fn refresh_slash(&mut self) {
        self.slash.refresh(
            self.composer.text(),
            self.composer.cursor(),
            self.theme_kind,
        );
        if !self.slash.open {
            self.preview_theme = None;
        }
        self.clamp_slash_scroll();
    }

    pub fn edit_composer(&mut self, edit: impl FnOnce(&mut Composer)) {
        edit(&mut self.composer);
        self.refresh_slash();
    }

    pub fn move_slash_selection(&mut self, delta: isize) {
        self.slash.move_selection(delta);
        self.preview_selected_argument();
        self.clamp_slash_scroll();
    }

    pub fn select_slash_index(&mut self, index: usize) {
        if index < self.slash.suggestions.len() {
            self.slash.selected = index;
            self.preview_selected_argument();
        }
    }

    pub fn accept_slash_completion(&mut self) -> bool {
        let Some(row) = self.slash.selection().cloned() else {
            return false;
        };
        let range = match self.slash.phase {
            CompletionPhase::Command => self.slash.command_range.clone(),
            CompletionPhase::Arguments { .. } => self
                .slash
                .argument_range
                .clone()
                .unwrap_or(self.composer.cursor()..self.composer.cursor()),
        };
        if !self.composer.replace_range(range, &row.insert_text) {
            return false;
        }
        self.refresh_slash();
        true
    }

    pub fn close_slash(&mut self) {
        self.preview_theme = None;
        self.slash = SlashMenu::default();
    }

    pub fn submit(&mut self) {
        let text = self.composer.text().trim().to_string();
        if text.is_empty() {
            self.run_menu_action(HOME_MENU[self.selected_menu].action);
            return;
        }

        if text.starts_with('/') && self.run_slash_command(&text) {
            self.composer.clear();
            self.close_slash();
            return;
        }

        let slash_tokens = recognized_slash_tokens(&text);
        self.transcript
            .push(TranscriptEntry::User { text, slash_tokens });
        self.composer.clear();
        self.close_slash();
        self.turn = Some(ActiveTurn::new());
    }

    pub fn run_menu_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::Changelog => self
                .transcript
                .push(TranscriptEntry::Event("Changelog".to_string())),
            MenuAction::Resume => self
                .transcript
                .push(TranscriptEntry::Event("Resume Session".to_string())),
            MenuAction::Worktree => self
                .transcript
                .push(TranscriptEntry::Event("New Worktree".to_string())),
            MenuAction::Quit => self.running = false,
        }
    }

    pub fn begin_thinking(&mut self) {
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        turn.set_activity(TurnActivity::Thinking);
        let entry = self.transcript.len();
        self.transcript.push(TranscriptEntry::Thinking {
            text: String::new(),
            running: true,
            elapsed_ms: None,
        });
        turn.thinking_entry = Some(entry);
    }

    pub fn push_thinking(&mut self, chunk: &str) {
        if self.turn.is_none() {
            return;
        }
        let Some(index) = self.turn.as_ref().and_then(|turn| turn.thinking_entry) else {
            self.begin_thinking();
            return self.push_thinking(chunk);
        };
        if let Some(TranscriptEntry::Thinking { text, .. }) = self.transcript.get_mut(index) {
            text.push_str(chunk);
        }
    }

    pub fn begin_responding(&mut self) {
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        if let Some(index) = turn.thinking_entry
            && let Some(TranscriptEntry::Thinking {
                running,
                elapsed_ms,
                ..
            }) = self.transcript.get_mut(index)
        {
            *running = false;
            *elapsed_ms = Some(turn.activity_started_at.elapsed().as_millis());
        }
        turn.set_activity(TurnActivity::Responding);
        let entry = self.transcript.len();
        self.transcript.push(TranscriptEntry::Assistant {
            text: String::new(),
            streaming: true,
        });
        turn.assistant_entry = Some(entry);
    }

    pub fn push_response(&mut self, chunk: &str) {
        if self.turn.is_none() {
            return;
        }
        let Some(index) = self.turn.as_ref().and_then(|turn| turn.assistant_entry) else {
            self.begin_responding();
            return self.push_response(chunk);
        };
        if let Some(TranscriptEntry::Assistant { text, .. }) = self.transcript.get_mut(index) {
            text.push_str(chunk);
        }
    }

    pub fn finish_turn(&mut self) {
        let Some(turn) = self.turn.take() else {
            return;
        };
        if let Some(index) = turn.assistant_entry
            && let Some(TranscriptEntry::Assistant { streaming, .. }) =
                self.transcript.get_mut(index)
        {
            *streaming = false;
        }
        self.transcript.push(TranscriptEntry::Event(format!(
            "Worked for {}",
            format_elapsed(turn.started_at.elapsed().as_millis())
        )));
    }

    pub fn cancel_turn(&mut self) {
        if self.turn.take().is_some() {
            self.transcript.push(TranscriptEntry::Event(
                "Turn cancelled by user.".to_string(),
            ));
        }
    }

    pub fn on_tick(&mut self) {
        self.animation_tick = self.animation_tick.wrapping_add(1);
    }

    fn preview_selected_argument(&mut self) {
        self.preview_theme = match self.slash.phase {
            CompletionPhase::Arguments { command_index }
                if COMMANDS[command_index].name == "theme" =>
            {
                self.slash
                    .selection()
                    .and_then(|row| ThemeKind::from_name(&row.insert_text))
            }
            _ => None,
        };
    }

    fn clamp_slash_scroll(&mut self) {
        const VISIBLE: usize = 6;
        if self.slash.selected < self.slash_scroll {
            self.slash_scroll = self.slash.selected;
        } else if self.slash.selected >= self.slash_scroll + VISIBLE {
            self.slash_scroll = self.slash.selected + 1 - VISIBLE;
        }
        self.slash_scroll = self
            .slash_scroll
            .min(self.slash.suggestions.len().saturating_sub(1));
    }

    fn run_slash_command(&mut self, text: &str) -> bool {
        let body = text.trim_start_matches('/');
        let mut parts = body.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or_default();
        let args = parts.next().unwrap_or_default().trim();
        let Some(command) = COMMANDS
            .iter()
            .copied()
            .find(|command| command.matches_name(name))
        else {
            return false;
        };

        if command.arguments_required && args.is_empty() {
            return false;
        }

        match command.name {
            "quit" => self.running = false,
            "home" => {
                self.transcript.clear();
                self.turn = None;
            }
            "theme" => {
                let next = if args.is_empty() {
                    let current = ThemeKind::ALL
                        .iter()
                        .position(|kind| *kind == self.theme_kind)
                        .unwrap_or(0);
                    ThemeKind::ALL[(current + 1) % ThemeKind::ALL.len()]
                } else if let Some(kind) = ThemeKind::from_name(args) {
                    kind
                } else {
                    return false;
                };
                self.theme_kind = next;
                self.preview_theme = None;
                self.transcript.push(TranscriptEntry::Event(format!(
                    "Theme changed to {}.",
                    next.name()
                )));
            }
            "help" => self.transcript.push(TranscriptEntry::Event(
                "Type / to browse commands. Use Tab to complete and Enter to run.".to_string(),
            )),
            _ => self
                .transcript
                .push(TranscriptEntry::Event(command.usage.to_string())),
        }
        true
    }
}

fn recognized_slash_tokens(text: &str) -> Vec<Range<usize>> {
    text.char_indices()
        .filter(|(index, ch)| {
            *ch == '/'
                && (*index == 0
                    || text[..*index]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace))
        })
        .filter_map(|(start, _)| {
            let end = text[start..]
                .find(char::is_whitespace)
                .map_or(text.len(), |offset| start + offset);
            let name = &text[start + 1..end];
            COMMANDS
                .iter()
                .any(|command| command.matches_name(name))
                .then_some(start..end)
        })
        .collect()
}

fn format_elapsed(milliseconds: u128) -> String {
    if milliseconds < 60_000 {
        format!("{:.1}s", milliseconds as f64 / 1_000.0)
    } else {
        let minutes = milliseconds / 60_000;
        let seconds = (milliseconds % 60_000) / 1_000;
        format!("{minutes}m{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_not_interpreted_as_a_shortcut() {
        let mut app = App::new();
        app.composer.set("q and u are ordinary input");
        app.submit();
        assert!(app.running);
        assert!(matches!(app.transcript[0], TranscriptEntry::User { .. }));
    }

    #[test]
    fn slash_acceptance_chains_into_theme_arguments() {
        let mut app = App::new();
        app.composer.set("/th");
        app.refresh_slash();
        assert!(app.accept_slash_completion());
        assert_eq!(app.composer.text(), "/theme ");
        assert!(matches!(app.slash.phase, CompletionPhase::Arguments { .. }));
    }

    #[test]
    fn turn_completion_creates_worked_for_event() {
        let mut app = App::new();
        app.composer.set("hello");
        app.submit();
        app.begin_thinking();
        app.push_thinking("Considering the request");
        app.begin_responding();
        app.push_response("Done");
        app.finish_turn();
        assert!(matches!(
            app.transcript.last(),
            Some(TranscriptEntry::Event(text)) if text.starts_with("Worked for ")
        ));
    }
}
