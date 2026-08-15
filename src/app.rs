use std::{
    collections::{HashMap, VecDeque},
    ops::Range,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ratatui::layout::Rect;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    features::{self, BrowserAction, BrowserItem},
    harness::{
        SessionMode, SessionSummary,
        event::{FileDiff, HarnessEvent, PermissionReply, RunOutcome},
        session::{AssistantPart, Session, SessionMessage, ToolState},
    },
    provider::{
        DiscoveryErrorKind, ModelDiscovery, ModelRecord, ModelSelection, ProviderId, ProviderStore,
    },
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
    pub fold_rows: Vec<(Rect, usize)>,
    pub catalog_rows: Vec<(Rect, usize)>,
    pub browser_rows: Vec<(Rect, usize)>,
    pub resume_rows: Vec<(Rect, usize)>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SelectionPoint {
    row: usize,
    byte: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TextSelection {
    anchor: SelectionPoint,
    head: SelectionPoint,
    dragging: bool,
}

#[derive(Clone, Debug, Default)]
struct TranscriptViewport {
    area: Rect,
    rows: Vec<String>,
    start_row: usize,
    visible_rows: usize,
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
        id: String,
        text: String,
        running: bool,
        elapsed_ms: Option<u128>,
        expanded: bool,
    },
    Assistant {
        id: String,
        text: String,
        streaming: bool,
    },
    Tool {
        call_id: String,
        name: String,
        description: String,
        input: String,
        output: String,
        state: ToolVisualState,
        elapsed_ms: Option<u128>,
        expanded: bool,
        diffs: Vec<FileDiff>,
    },
    Event(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolVisualState {
    Running,
    Succeeded,
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TurnActivity {
    Compacting,
    WaitingForResponse,
    Thinking,
    Responding,
    RunningTool(String),
    Retrying(u16),
    WaitingForPermission,
    RunningJob(String),
    Cancelling,
}

impl TurnActivity {
    pub fn label(&self) -> String {
        match self {
            Self::Compacting => "Compacting context…".to_string(),
            Self::WaitingForResponse => "Waiting for response…".to_string(),
            Self::Thinking => "Thinking…".to_string(),
            Self::Responding => "Responding…".to_string(),
            Self::RunningTool(tool) => format!("Run {tool}"),
            Self::Retrying(attempt) => format!("Retrying (attempt {attempt})…"),
            Self::WaitingForPermission => "Waiting for permission…".to_string(),
            Self::RunningJob(name) => format!("Running Job: {name}"),
            Self::Cancelling => "Cancelling…".to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ActiveTurn {
    pub run_id: Option<u64>,
    pub activity: TurnActivity,
    pub status_visible: bool,
    pub started_at: Instant,
    pub activity_started_at: Instant,
}

impl ActiveTurn {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            run_id: None,
            activity: TurnActivity::WaitingForResponse,
            status_visible: true,
            started_at: now,
            activity_started_at: now,
        }
    }

    fn set_activity(&mut self, activity: TurnActivity) {
        self.activity = activity;
        self.status_visible = true;
        self.activity_started_at = Instant::now();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionPrompt {
    pub request_id: u64,
    pub description: String,
    pub patterns: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelCatalogView {
    pub provider: ProviderId,
    pub models: Vec<ModelRecord>,
    pub selected: usize,
    pub loading: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogModal {
    Providers {
        selected: usize,
    },
    Models(ModelCatalogView),
    ApiKey {
        provider: ProviderId,
        input: Composer,
        error: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct ResumePanel {
    pub sessions: Vec<SessionSummary>,
    pub query: Composer,
    pub selected: usize,
    pub expanded: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeleteConfirmation {
    pub session_id: String,
    pub title: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserDetail {
    pub title: String,
    pub body: String,
    pub scroll: usize,
    pub action: BrowserAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserPanel {
    pub title: String,
    pub items: Vec<BrowserItem>,
    pub selected: usize,
    pub detail: Option<BrowserDetail>,
}

#[derive(Clone, Debug)]
struct ModeSwitchBanner {
    message: String,
    shown_at: Instant,
}

impl ResumePanel {
    pub fn visible_sessions(&self) -> Vec<&SessionSummary> {
        let query = self.query.text().trim().to_lowercase();
        self.sessions
            .iter()
            .filter(|session| {
                query.is_empty()
                    || session.title.to_lowercase().contains(&query)
                    || session.id.to_lowercase().contains(&query)
                    || session.directory.to_lowercase().contains(&query)
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionCommand {
    OpenResume,
    Resume(String),
    New,
    EditPrompt,
    Copy(String),
    Rename(String),
    Compact,
    SetMode(SessionMode),
    SessionInfo,
    Delete,
    Fork,
    Rewind,
    Export(String),
    Doctor,
    Worktree,
    SelectRelease(String),
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
    pub permission: Option<PermissionPrompt>,
    pub catalog_modal: Option<CatalogModal>,
    pub resume_panel: Option<ResumePanel>,
    pub browser_panel: Option<BrowserPanel>,
    pub delete_confirmation: Option<DeleteConfirmation>,
    pub session_id: Option<String>,
    pub session_title: Option<String>,
    pub animation_tick: u64,
    pub running: bool,
    pub hit_zones: HitZones,
    pub session_mode: SessionMode,
    pub multiline_mode: bool,
    pub timestamps_enabled: bool,
    transcript_viewport: TranscriptViewport,
    transcript_scroll: usize,
    transcript_follow: bool,
    text_selection: Option<TextSelection>,
    selection_mouse: Option<(u16, u16)>,
    selection_autoscroll: i8,
    pending_submission: Option<String>,
    queued_prompts: VecDeque<String>,
    pending_permission_reply: Option<(u64, PermissionReply)>,
    pending_session_command: Option<SessionCommand>,
    transcript_times: Vec<i64>,
    mode_banner: Option<ModeSwitchBanner>,
    thinking_entries: HashMap<String, (usize, Instant)>,
    assistant_entries: HashMap<String, usize>,
    tool_entries: HashMap<String, (usize, Instant)>,
    provider_store: ProviderStore,
    model_discovery: ModelDiscovery,
    pending_provider_key: Option<(ProviderId, String)>,
}

impl App {
    pub fn new() -> Self {
        let theme_kind = std::env::var("INDUS_THEME")
            .ok()
            .and_then(|name| ThemeKind::from_name(&name))
            .or_else(load_theme_preference)
            .unwrap_or_default();
        let provider_store = ProviderStore::load();
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
            permission: None,
            catalog_modal: None,
            resume_panel: None,
            browser_panel: None,
            delete_confirmation: None,
            session_id: None,
            session_title: None,
            animation_tick: 0,
            running: true,
            hit_zones: HitZones::default(),
            session_mode: SessionMode::Normal,
            multiline_mode: false,
            timestamps_enabled: false,
            transcript_viewport: TranscriptViewport::default(),
            transcript_scroll: 0,
            transcript_follow: true,
            text_selection: None,
            selection_mouse: None,
            selection_autoscroll: 0,
            pending_submission: None,
            queued_prompts: VecDeque::new(),
            pending_permission_reply: None,
            pending_session_command: None,
            transcript_times: Vec::new(),
            mode_banner: None,
            thinking_entries: HashMap::new(),
            assistant_entries: HashMap::new(),
            tool_entries: HashMap::new(),
            provider_store,
            model_discovery: ModelDiscovery::new(),
            pending_provider_key: None,
        }
    }

    pub fn active_model(&self) -> Option<&ModelSelection> {
        self.provider_store.active_selection()
    }

    pub fn sync_transcript_viewport(&mut self, area: Rect, rows: Vec<String>) -> usize {
        let visible_rows = area.height as usize;
        let maximum = rows.len().saturating_sub(visible_rows);
        if self.transcript_follow {
            self.transcript_scroll = maximum;
        } else {
            self.transcript_scroll = self.transcript_scroll.min(maximum);
        }
        self.transcript_viewport = TranscriptViewport {
            area,
            rows,
            start_row: self.transcript_scroll,
            visible_rows,
        };
        self.reclamp_selection_head();
        self.transcript_scroll
    }

    pub fn scroll_transcript_up(&mut self, rows: usize) {
        if self.transcript_viewport.rows.is_empty() {
            return;
        }
        self.transcript_follow = false;
        self.transcript_scroll = self.transcript_scroll.saturating_sub(rows);
        self.text_selection = self.text_selection.filter(|selection| selection.dragging);
    }

    pub fn scroll_transcript_down(&mut self, rows: usize) {
        let maximum = self
            .transcript_viewport
            .rows
            .len()
            .saturating_sub(self.transcript_viewport.visible_rows);
        self.transcript_scroll = self.transcript_scroll.saturating_add(rows).min(maximum);
        if self.transcript_scroll == maximum {
            self.transcript_follow = true;
        }
        self.text_selection = self.text_selection.filter(|selection| selection.dragging);
    }

    pub fn page_transcript_up(&mut self) {
        self.scroll_transcript_up(
            self.transcript_viewport
                .visible_rows
                .saturating_sub(1)
                .max(1),
        );
    }

    pub fn page_transcript_down(&mut self) {
        self.scroll_transcript_down(
            self.transcript_viewport
                .visible_rows
                .saturating_sub(1)
                .max(1),
        );
    }

    pub fn scroll_transcript_to_top(&mut self) {
        if !self.transcript_viewport.rows.is_empty() {
            self.transcript_follow = false;
            self.transcript_scroll = 0;
            self.text_selection = None;
        }
    }

    pub fn scroll_transcript_to_bottom(&mut self) {
        self.transcript_follow = true;
        self.transcript_scroll = self
            .transcript_viewport
            .rows
            .len()
            .saturating_sub(self.transcript_viewport.visible_rows);
        self.text_selection = None;
    }

    pub fn transcript_contains(&self, column: u16, row: u16) -> bool {
        self.transcript_viewport.area.contains((column, row).into())
    }

    pub fn begin_text_selection(&mut self, column: u16, row: u16) -> bool {
        let Some(point) = self.selection_point_at(column, row, false) else {
            self.text_selection = None;
            return false;
        };
        self.text_selection = Some(TextSelection {
            anchor: point,
            head: point,
            dragging: true,
        });
        self.selection_mouse = Some((column, row));
        self.selection_autoscroll = 0;
        true
    }

    pub fn is_selecting_text(&self) -> bool {
        self.text_selection
            .is_some_and(|selection| selection.dragging)
    }

    pub fn update_text_selection(&mut self, column: u16, row: u16) -> bool {
        let Some(selection) = self.text_selection.as_ref() else {
            return false;
        };
        if !selection.dragging {
            return false;
        }
        self.selection_mouse = Some((column, row));
        let area = self.transcript_viewport.area;
        self.selection_autoscroll = if row <= area.y {
            -1
        } else if row >= area.bottom().saturating_sub(1) {
            1
        } else {
            0
        };
        let Some(point) = self.selection_point_at(column, row, true) else {
            return true;
        };
        if let Some(selection) = self.text_selection.as_mut() {
            selection.head = point;
        }
        true
    }

    pub fn finish_text_selection(&mut self) -> Option<String> {
        self.selection_autoscroll = 0;
        self.selection_mouse = None;
        let mut selection = self.text_selection?;
        selection.dragging = false;
        if selection.anchor == selection.head {
            self.text_selection = None;
            return None;
        }
        self.text_selection = Some(selection);
        let text = self.selected_text(selection);
        (!text.is_empty()).then_some(text)
    }

    pub fn selection_display_range(&self, row: usize) -> Option<(usize, usize)> {
        let selection = self.text_selection?;
        if selection.dragging && selection.anchor == selection.head {
            return None;
        }
        let line = self.transcript_viewport.rows.get(row)?;
        let range = selected_byte_range(selection, row, line)?;
        Some((line[..range.start].width(), line[..range.end].width()))
    }

    pub fn transcript_scroll_metrics(&self) -> (usize, usize, usize) {
        (
            self.transcript_scroll,
            self.transcript_viewport.visible_rows,
            self.transcript_viewport.rows.len(),
        )
    }

    fn selection_point_at(&self, column: u16, row: u16, nearest: bool) -> Option<SelectionPoint> {
        let area = self.transcript_viewport.area;
        if area.width == 0 || area.height == 0 {
            return None;
        }
        let viewport_row = if nearest {
            row.saturating_sub(area.y)
                .min(area.height.saturating_sub(1)) as usize
        } else {
            if !area.contains((column, row).into()) {
                return None;
            }
            (row - area.y) as usize
        };
        let global_row = self.transcript_viewport.start_row + viewport_row;
        let line = self.transcript_viewport.rows.get(global_row)?;
        if !nearest && (line.is_empty() || column.saturating_sub(area.x) as usize >= line.width()) {
            return None;
        }
        Some(SelectionPoint {
            row: global_row,
            byte: byte_at_display_column(line, column.saturating_sub(area.x) as usize),
        })
    }

    fn selected_text(&self, selection: TextSelection) -> String {
        let (first, last) = ordered_selection(selection);
        let mut output = String::new();
        for row in first.row..=last.row {
            let Some(line) = self.transcript_viewport.rows.get(row) else {
                break;
            };
            if let Some(range) = selected_byte_range(selection, row, line) {
                output.push_str(&line[range]);
            }
            if row != last.row {
                output.push('\n');
            }
        }
        output
    }

    fn reclamp_selection_head(&mut self) {
        let Some((column, row)) = self.selection_mouse else {
            return;
        };
        let Some(point) = self.selection_point_at(column, row, true) else {
            return;
        };
        if let Some(selection) = self.text_selection.as_mut()
            && selection.dragging
        {
            selection.head = point;
        }
    }

    pub fn open_browser_catalog(&mut self, title: impl Into<String>, items: Vec<BrowserItem>) {
        self.close_slash();
        self.catalog_modal = None;
        self.resume_panel = None;
        self.browser_panel = Some(BrowserPanel {
            title: title.into(),
            items,
            selected: 0,
            detail: None,
        });
    }

    pub fn open_document(&mut self, title: impl Into<String>, body: impl Into<String>) {
        let title = title.into();
        self.browser_panel = Some(BrowserPanel {
            title: title.clone(),
            items: Vec::new(),
            selected: 0,
            detail: Some(BrowserDetail {
                title,
                body: body.into(),
                scroll: 0,
                action: BrowserAction::None,
            }),
        });
    }

    pub fn close_browser_level(&mut self) {
        let Some(panel) = self.browser_panel.as_mut() else {
            return;
        };
        if panel.detail.take().is_some() && !panel.items.is_empty() {
            return;
        }
        self.browser_panel = None;
    }

    pub fn move_browser_selection(&mut self, delta: isize) {
        let Some(panel) = self.browser_panel.as_mut() else {
            return;
        };
        if let Some(detail) = panel.detail.as_mut() {
            detail.scroll = if delta.is_negative() {
                detail.scroll.saturating_sub(delta.unsigned_abs())
            } else {
                detail.scroll.saturating_add(delta as usize)
            };
        } else if !panel.items.is_empty() {
            panel.selected =
                (panel.selected as isize + delta).rem_euclid(panel.items.len() as isize) as usize;
        }
    }

    pub fn select_browser_index(&mut self, index: usize) {
        if let Some(panel) = self.browser_panel.as_mut()
            && panel.detail.is_none()
            && index < panel.items.len()
        {
            panel.selected = index;
        }
    }

    pub fn submit_browser_selection(&mut self) {
        let Some(panel) = self.browser_panel.as_mut() else {
            return;
        };
        if let Some(detail) = panel.detail.as_ref() {
            match &detail.action {
                BrowserAction::InsertSkill(name) => {
                    let name = name.clone();
                    self.browser_panel = None;
                    self.composer.set(format!("${name} "));
                    self.refresh_slash();
                }
                BrowserAction::SelectRelease(version) => {
                    self.pending_session_command =
                        Some(SessionCommand::SelectRelease(version.clone()));
                }
                BrowserAction::None => {}
            }
            return;
        }
        let Some(item) = panel.items.get(panel.selected).cloned() else {
            return;
        };
        panel.detail = Some(BrowserDetail {
            title: item.title,
            body: item.body,
            scroll: 0,
            action: item.action,
        });
    }

    pub fn transcript_markdown(&self) -> String {
        let heading = self.session_title.as_deref().unwrap_or("Indus transcript");
        let mut output = vec![format!("# {heading}")];
        if let Some(id) = &self.session_id {
            output.push(format!("\nSession: `{id}`"));
        }
        for entry in &self.transcript {
            let (label, text) = transcript_entry_text(entry);
            if text.trim().is_empty() {
                continue;
            }
            output.push(format!("\n## {label}\n\n{text}"));
        }
        output.join("\n")
    }

    pub fn timeline_text(&mut self) -> String {
        self.sync_transcript_timestamps();
        self.transcript
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let (label, text) = transcript_entry_text(entry);
                let summary = text.lines().next().unwrap_or_default();
                format!(
                    "{:>3}. {}  {:<10} {}",
                    index + 1,
                    format_timestamp(self.transcript_times[index]),
                    label,
                    summary
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn sync_transcript_timestamps(&mut self) {
        let now = unix_seconds();
        self.transcript_times.resize(self.transcript.len(), now);
        self.transcript_times.truncate(self.transcript.len());
    }

    pub fn transcript_timestamp(&self, index: usize) -> Option<String> {
        self.timestamps_enabled
            .then(|| self.transcript_times.get(index).copied())
            .flatten()
            .map(format_timestamp)
    }

    pub fn open_provider_catalog(&mut self) {
        let selected = self
            .active_model()
            .and_then(|active| {
                ProviderId::ALL
                    .iter()
                    .position(|provider| *provider == active.provider)
            })
            .unwrap_or(0);
        self.close_slash();
        self.catalog_modal = Some(CatalogModal::Providers { selected });
    }

    pub fn close_catalog_level(&mut self) {
        self.catalog_modal = match self.catalog_modal.take() {
            Some(CatalogModal::Providers { .. }) | None => None,
            Some(CatalogModal::Models(view)) => Some(CatalogModal::Providers {
                selected: provider_index(view.provider),
            }),
            Some(CatalogModal::ApiKey { provider, .. }) => Some(CatalogModal::Providers {
                selected: provider_index(provider),
            }),
        };
    }

    pub fn move_catalog_selection(&mut self, delta: isize) {
        let (selected, len) = match self.catalog_modal.as_mut() {
            Some(CatalogModal::Providers { selected }) => (selected, ProviderId::ALL.len()),
            Some(CatalogModal::Models(view)) => (&mut view.selected, view.models.len()),
            _ => return,
        };
        if len > 0 {
            *selected = (*selected as isize + delta).rem_euclid(len as isize) as usize;
        }
    }

    pub fn select_catalog_index(&mut self, index: usize) {
        match self.catalog_modal.as_mut() {
            Some(CatalogModal::Providers { selected }) if index < ProviderId::ALL.len() => {
                *selected = index;
            }
            Some(CatalogModal::Models(view)) if index < view.models.len() => {
                view.selected = index;
            }
            _ => {}
        }
    }

    pub fn submit_catalog_selection(&mut self) {
        match self.catalog_modal.take() {
            Some(CatalogModal::Providers { selected }) => {
                self.open_provider(ProviderId::ALL[selected.min(ProviderId::ALL.len() - 1)]);
            }
            Some(CatalogModal::Models(mut view)) => {
                let Some(model) = view.models.get(view.selected).cloned() else {
                    self.catalog_modal = Some(CatalogModal::Models(view));
                    return;
                };
                match self.provider_store.select_model(view.provider, &model) {
                    Ok(()) => {
                        self.transcript.push(TranscriptEntry::Event(format!(
                            "Using {} through {}.",
                            model.name,
                            view.provider.name()
                        )));
                    }
                    Err(error) => {
                        view.error = Some(format!("Could not save model preference: {error}"));
                        self.catalog_modal = Some(CatalogModal::Models(view));
                    }
                }
            }
            Some(CatalogModal::ApiKey {
                provider,
                mut input,
                error,
            }) => {
                let key = input.clear();
                if key.trim().is_empty() {
                    self.catalog_modal = Some(CatalogModal::ApiKey {
                        provider,
                        input,
                        error: Some("Enter an API key.".to_string()),
                    });
                    return;
                }
                self.pending_provider_key = Some((provider, key.clone()));
                self.catalog_modal = Some(
                    ModelCatalogView {
                        provider,
                        models: self.provider_store.cached_models(provider).to_vec(),
                        selected: self.recent_model_index(provider),
                        loading: true,
                        error,
                    }
                    .into(),
                );
                self.model_discovery.fetch(provider, key);
            }
            None => {}
        }
    }

    pub fn edit_api_key(&mut self, edit: impl FnOnce(&mut Composer)) {
        if let Some(CatalogModal::ApiKey { input, error, .. }) = self.catalog_modal.as_mut() {
            edit(input);
            *error = None;
        }
    }

    pub fn refresh_model_catalog(&mut self) {
        let Some(CatalogModal::Models(view)) = self.catalog_modal.as_mut() else {
            return;
        };
        let provider = view.provider;
        let Some(key) = self.provider_store.api_key(provider).map(str::to_string) else {
            self.catalog_modal = Some(CatalogModal::ApiKey {
                provider,
                input: Composer::default(),
                error: None,
            });
            return;
        };
        view.loading = true;
        view.error = None;
        self.model_discovery.fetch(provider, key);
    }

    pub fn replace_provider_key(&mut self) {
        let Some(CatalogModal::Models(view)) = self.catalog_modal.take() else {
            return;
        };
        self.catalog_modal = Some(CatalogModal::ApiKey {
            provider: view.provider,
            input: Composer::default(),
            error: None,
        });
    }

    pub fn process_model_discovery(&mut self) {
        for event in self.model_discovery.drain() {
            let validated_key = self
                .pending_provider_key
                .take_if(|(provider, _)| *provider == event.provider)
                .map(|(_, key)| key);
            match event.result {
                Ok(models) => {
                    let persistence = self.provider_store.accept_models(
                        event.provider,
                        models.clone(),
                        validated_key,
                    );
                    if let Some(CatalogModal::Models(view)) = self.catalog_modal.as_mut()
                        && view.provider == event.provider
                    {
                        view.models = models;
                        view.selected = recent_model_index(
                            self.provider_store.recent_model(event.provider),
                            &view.models,
                        );
                        view.loading = false;
                        view.error = persistence
                            .err()
                            .map(|error| format!("Could not save provider settings: {error}"));
                    }
                }
                Err(error) if error.kind == DiscoveryErrorKind::Authentication => {
                    if self.catalog_modal.as_ref().is_some_and(|modal| {
                        matches!(modal, CatalogModal::Models(view) if view.provider == event.provider)
                    }) {
                        self.catalog_modal = Some(CatalogModal::ApiKey {
                            provider: event.provider,
                            input: Composer::default(),
                            error: Some(error.message),
                        });
                    }
                }
                Err(error) => {
                    if let Some(CatalogModal::Models(view)) = self.catalog_modal.as_mut()
                        && view.provider == event.provider
                    {
                        view.loading = false;
                        view.error = Some(error.message);
                    }
                }
            }
        }
    }

    fn open_provider(&mut self, provider: ProviderId) {
        let Some(key) = self.provider_store.api_key(provider).map(str::to_string) else {
            self.catalog_modal = Some(CatalogModal::ApiKey {
                provider,
                input: Composer::default(),
                error: None,
            });
            return;
        };
        self.catalog_modal = Some(CatalogModal::Models(ModelCatalogView {
            provider,
            models: self.provider_store.cached_models(provider).to_vec(),
            selected: self.recent_model_index(provider),
            loading: true,
            error: None,
        }));
        self.model_discovery.fetch(provider, key);
    }

    fn recent_model_index(&self, provider: ProviderId) -> usize {
        recent_model_index(
            self.provider_store.recent_model(provider),
            self.provider_store.cached_models(provider),
        )
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

        if self.turn.is_some() {
            self.queued_prompts.push_back(text);
            self.composer.clear();
            self.close_slash();
            self.transcript.push(TranscriptEntry::Event(format!(
                "Prompt queued ({}). Press Esc to interrupt and send it now.",
                self.queued_prompts.len()
            )));
            return;
        }

        let slash_tokens = recognized_slash_tokens(&text);
        self.transcript.push(TranscriptEntry::User {
            text: text.clone(),
            slash_tokens,
        });
        self.composer.clear();
        self.close_slash();
        self.turn = Some(ActiveTurn::new());
        self.pending_submission = Some(text);
    }

    pub fn take_submission(&mut self) -> Option<String> {
        self.pending_submission.take()
    }

    pub fn restore_submission(&mut self, prompt: String) {
        self.pending_submission = Some(prompt);
    }

    pub fn has_queued_prompts(&self) -> bool {
        !self.queued_prompts.is_empty()
    }

    pub fn take_queued_prompts(&mut self) -> VecDeque<String> {
        std::mem::take(&mut self.queued_prompts)
    }

    pub fn take_permission_reply(&mut self) -> Option<(u64, PermissionReply)> {
        self.pending_permission_reply.take()
    }

    pub fn take_session_command(&mut self) -> Option<SessionCommand> {
        self.pending_session_command.take()
    }

    pub fn request_next_mode(&mut self) {
        if self.turn.is_none() && self.delete_confirmation.is_none() {
            self.pending_session_command = Some(SessionCommand::SetMode(self.session_mode.next()));
        }
    }

    pub fn confirm_mode(&mut self, mode: SessionMode) {
        self.session_mode = mode;
        self.mode_banner = Some(ModeSwitchBanner {
            message: format!("Switched to mode: {}", mode.label()),
            shown_at: Instant::now(),
        });
    }

    pub fn mode_banner(&self) -> Option<(&str, f32)> {
        let banner = self.mode_banner.as_ref()?;
        let elapsed = banner.shown_at.elapsed();
        let opacity = if elapsed <= Duration::from_secs(2) {
            1.0
        } else {
            1.0 - (elapsed - Duration::from_secs(2)).as_secs_f32() / 0.3
        };
        (opacity > 0.0).then_some((banner.message.as_str(), opacity.clamp(0.0, 1.0)))
    }

    pub fn confirm_delete(&mut self) -> bool {
        if self.delete_confirmation.take().is_none() {
            return false;
        }
        self.pending_session_command = Some(SessionCommand::Delete);
        true
    }

    pub fn cancel_delete(&mut self) {
        self.delete_confirmation = None;
    }

    pub fn open_resume_panel(&mut self, sessions: Vec<SessionSummary>) {
        self.close_slash();
        self.catalog_modal = None;
        self.resume_panel = Some(ResumePanel {
            sessions,
            query: Composer::default(),
            selected: 0,
            expanded: false,
        });
    }

    pub fn close_resume_panel(&mut self) {
        self.resume_panel = None;
        self.delete_confirmation = None;
    }

    pub fn edit_resume_query(&mut self, edit: impl FnOnce(&mut Composer)) {
        let Some(panel) = self.resume_panel.as_mut() else {
            return;
        };
        edit(&mut panel.query);
        panel.selected = 0;
        panel.expanded = false;
    }

    pub fn move_resume_selection(&mut self, delta: isize) {
        let Some(panel) = self.resume_panel.as_mut() else {
            return;
        };
        let query = panel.query.text().trim().to_lowercase();
        let count = panel
            .sessions
            .iter()
            .filter(|session| {
                query.is_empty()
                    || session.title.to_lowercase().contains(&query)
                    || session.id.to_lowercase().contains(&query)
                    || session.directory.to_lowercase().contains(&query)
            })
            .count();
        if count > 0 {
            panel.selected = (panel.selected as isize + delta).rem_euclid(count as isize) as usize;
            panel.expanded = false;
        }
    }

    pub fn select_resume_index(&mut self, index: usize) {
        if let Some(panel) = self.resume_panel.as_mut()
            && index < panel.visible_sessions().len()
        {
            panel.selected = index;
        }
    }

    pub fn toggle_resume_details(&mut self) {
        if let Some(panel) = self.resume_panel.as_mut()
            && !panel.visible_sessions().is_empty()
        {
            panel.expanded = !panel.expanded;
        }
    }

    pub fn submit_resume_selection(&mut self) {
        let Some(panel) = self.resume_panel.as_ref() else {
            return;
        };
        let Some(session_id) = panel
            .visible_sessions()
            .get(panel.selected)
            .map(|session| session.id.clone())
        else {
            return;
        };
        self.resume_panel = None;
        self.pending_session_command = Some(SessionCommand::Resume(session_id));
    }

    pub fn load_session(&mut self, session: &Session) {
        self.transcript.clear();
        self.thinking_entries.clear();
        self.assistant_entries.clear();
        self.tool_entries.clear();
        self.turn = None;
        self.permission = None;
        self.composer.clear();
        self.close_slash();
        self.resume_panel = None;
        self.browser_panel = None;
        self.transcript_times.clear();
        self.session_id = session.is_allocated().then(|| session.id.clone());
        self.session_title = session.title.clone();
        self.queued_prompts.clear();
        for message in &session.messages {
            match message {
                SessionMessage::User(message) => self.transcript.push(TranscriptEntry::User {
                    text: message.text.clone(),
                    slash_tokens: recognized_slash_tokens(&message.text),
                }),
                SessionMessage::Assistant(message) => {
                    for part in &message.parts {
                        match part {
                            AssistantPart::Reasoning(part) if !part.text.is_empty() => {
                                self.transcript.push(TranscriptEntry::Thinking {
                                    id: part.id.clone(),
                                    text: part.text.clone(),
                                    running: !part.completed,
                                    elapsed_ms: None,
                                    expanded: !part.completed,
                                });
                            }
                            AssistantPart::Text(part) if !part.text.is_empty() => {
                                self.transcript.push(TranscriptEntry::Assistant {
                                    id: part.id.clone(),
                                    text: part.text.clone(),
                                    streaming: !part.completed,
                                });
                            }
                            AssistantPart::Tool(part) => {
                                let (description, output, state, diffs, expanded) =
                                    match &part.state {
                                        ToolState::Pending | ToolState::Running => (
                                            part.name.clone(),
                                            String::new(),
                                            ToolVisualState::Running,
                                            Vec::new(),
                                            false,
                                        ),
                                        ToolState::Completed {
                                            title,
                                            output,
                                            diffs,
                                        } => (
                                            title.clone(),
                                            output.clone(),
                                            ToolVisualState::Succeeded,
                                            diffs.clone(),
                                            false,
                                        ),
                                        ToolState::Failed { message } => (
                                            part.name.clone(),
                                            String::new(),
                                            ToolVisualState::Failed(message.clone()),
                                            Vec::new(),
                                            true,
                                        ),
                                    };
                                self.transcript.push(TranscriptEntry::Tool {
                                    call_id: part.call_id.clone(),
                                    name: part.name.clone(),
                                    description,
                                    input: part.input.clone(),
                                    output,
                                    state,
                                    elapsed_ms: None,
                                    expanded,
                                    diffs,
                                });
                            }
                            AssistantPart::Reasoning(_) | AssistantPart::Text(_) => {}
                        }
                    }
                }
            }
        }
        self.transcript_follow = true;
        self.transcript_scroll = 0;
        self.sync_transcript_timestamps();
    }

    pub fn report_session_error(&mut self, message: impl Into<String>) {
        self.transcript.push(TranscriptEntry::Event(message.into()));
    }

    pub fn report_session_info(&mut self, markdown: impl Into<String>) {
        self.transcript.push(TranscriptEntry::Assistant {
            id: format!("session-info-{}", self.animation_tick),
            text: markdown.into(),
            streaming: false,
        });
    }

    pub fn restore_edited_prompt(&mut self, session: &Session, prompt: String) {
        self.load_session(session);
        self.composer.set(prompt);
        self.refresh_slash();
    }

    pub fn set_session_title(&mut self, title: String) {
        self.session_title = Some(title);
    }

    pub fn last_response(&self) -> Option<String> {
        self.transcript.iter().rev().find_map(|entry| match entry {
            TranscriptEntry::Assistant { text, .. } if !text.trim().is_empty() => {
                Some(text.clone())
            }
            _ => None,
        })
    }

    pub fn resolve_permission(&mut self, reply: PermissionReply) -> bool {
        let Some(prompt) = self.permission.take() else {
            return false;
        };
        self.pending_permission_reply = Some((prompt.request_id, reply));
        if let Some(turn) = self.turn.as_mut() {
            turn.set_activity(TurnActivity::WaitingForResponse);
        }
        true
    }

    pub fn run_menu_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::Changelog => {
                self.open_browser_catalog("Release notes", features::release_notes())
            }
            MenuAction::Resume => self.pending_session_command = Some(SessionCommand::OpenResume),
            MenuAction::Worktree => self.pending_session_command = Some(SessionCommand::Worktree),
            MenuAction::Quit => self.running = false,
        }
    }

    pub fn apply_harness_event(&mut self, event: HarnessEvent) {
        match event {
            HarnessEvent::SessionCreated {
                session_id, title, ..
            } => {
                self.session_id = Some(session_id);
                self.session_title = Some(title);
            }
            HarnessEvent::RunStarted { run_id } => {
                if let Some(turn) = self.turn.as_mut() {
                    turn.run_id = Some(run_id);
                    turn.set_activity(TurnActivity::WaitingForResponse);
                }
            }
            HarnessEvent::JobScheduled {
                job_id,
                name,
                schedule,
                ..
            } => self.transcript.push(TranscriptEntry::Event(format!(
                "Job {job_id}: {name} ({schedule})"
            ))),
            HarnessEvent::JobRunStarted {
                run_id,
                job_id,
                name,
            } => {
                let mut turn = ActiveTurn::new();
                turn.run_id = Some(run_id);
                turn.set_activity(TurnActivity::RunningJob(name.clone()));
                self.turn = Some(turn);
                self.transcript.push(TranscriptEntry::Event(format!(
                    "Job {job_id} started: {name}"
                )));
            }
            HarnessEvent::JobRunFinished {
                job_id,
                name,
                succeeded,
                ..
            } => self.transcript.push(TranscriptEntry::Event(format!(
                "Job {job_id} {}: {name}",
                if succeeded { "completed" } else { "failed" }
            ))),
            HarnessEvent::WaitingForResponse { .. } => {
                self.set_turn_activity(TurnActivity::WaitingForResponse)
            }
            HarnessEvent::ReasoningStarted { reasoning_id, .. } => {
                self.set_turn_activity(TurnActivity::Thinking);
                let index = self.transcript.len();
                self.transcript.push(TranscriptEntry::Thinking {
                    id: reasoning_id.clone(),
                    text: String::new(),
                    running: true,
                    elapsed_ms: None,
                    expanded: true,
                });
                self.thinking_entries
                    .insert(reasoning_id, (index, Instant::now()));
            }
            HarnessEvent::ReasoningDelta {
                reasoning_id, text, ..
            } => {
                if let Some((index, _)) = self.thinking_entries.get(&reasoning_id)
                    && let Some(TranscriptEntry::Thinking { text: body, .. }) =
                        self.transcript.get_mut(*index)
                {
                    body.push_str(&text);
                }
            }
            HarnessEvent::ReasoningFinished { reasoning_id, .. } => {
                if let Some((index, started_at)) = self.thinking_entries.remove(&reasoning_id)
                    && let Some(TranscriptEntry::Thinking {
                        running,
                        elapsed_ms,
                        expanded,
                        ..
                    }) = self.transcript.get_mut(index)
                {
                    *running = false;
                    *elapsed_ms = Some(started_at.elapsed().as_millis());
                    *expanded = false;
                }
            }
            HarnessEvent::TextStarted { text_id, .. } => {
                self.set_turn_activity(TurnActivity::Responding);
                let index = self.transcript.len();
                self.transcript.push(TranscriptEntry::Assistant {
                    id: text_id.clone(),
                    text: String::new(),
                    streaming: true,
                });
                self.assistant_entries.insert(text_id, index);
            }
            HarnessEvent::TextDelta { text_id, text, .. } => {
                if let Some(index) = self.assistant_entries.get(&text_id)
                    && let Some(TranscriptEntry::Assistant { text: body, .. }) =
                        self.transcript.get_mut(*index)
                {
                    body.push_str(&text);
                }
            }
            HarnessEvent::TextFinished { text_id, .. } => {
                if let Some(index) = self.assistant_entries.remove(&text_id)
                    && let Some(TranscriptEntry::Assistant { streaming, .. }) =
                        self.transcript.get_mut(index)
                {
                    *streaming = false;
                }
                if self.assistant_entries.is_empty()
                    && self.thinking_entries.is_empty()
                    && self.tool_entries.is_empty()
                    && let Some(turn) = self.turn.as_mut()
                {
                    turn.status_visible = false;
                }
            }
            HarnessEvent::ToolStarted {
                call_id,
                name,
                description,
                input,
                ..
            } => {
                self.set_turn_activity(TurnActivity::RunningTool(name.clone()));
                let index = self.transcript.len();
                self.transcript.push(TranscriptEntry::Tool {
                    call_id: call_id.clone(),
                    name,
                    description,
                    input,
                    output: String::new(),
                    state: ToolVisualState::Running,
                    elapsed_ms: None,
                    expanded: false,
                    diffs: Vec::new(),
                });
                self.tool_entries.insert(call_id, (index, Instant::now()));
            }
            HarnessEvent::ToolOutput { call_id, text, .. } => {
                if let Some((index, _)) = self.tool_entries.get(&call_id)
                    && let Some(TranscriptEntry::Tool { output, .. }) =
                        self.transcript.get_mut(*index)
                {
                    output.push_str(&text);
                }
            }
            HarnessEvent::ToolFinished {
                call_id,
                title,
                output,
                diffs,
                ..
            } => {
                if let Some((index, started_at)) = self.tool_entries.remove(&call_id)
                    && let Some(TranscriptEntry::Tool {
                        description,
                        output: body,
                        state,
                        elapsed_ms,
                        diffs: body_diffs,
                        ..
                    }) = self.transcript.get_mut(index)
                {
                    if !title.trim().is_empty() {
                        *description = title;
                    }
                    if !output.is_empty() {
                        *body = output;
                    }
                    *state = ToolVisualState::Succeeded;
                    *elapsed_ms = Some(started_at.elapsed().as_millis());
                    *body_diffs = diffs;
                }
                self.set_turn_activity(TurnActivity::WaitingForResponse);
            }
            HarnessEvent::ToolFailed {
                call_id, message, ..
            } => {
                if let Some((index, started_at)) = self.tool_entries.remove(&call_id)
                    && let Some(TranscriptEntry::Tool {
                        state,
                        elapsed_ms,
                        expanded,
                        ..
                    }) = self.transcript.get_mut(index)
                {
                    *state = ToolVisualState::Failed(message);
                    *elapsed_ms = Some(started_at.elapsed().as_millis());
                    *expanded = true;
                }
            }
            HarnessEvent::PermissionRequested {
                request_id,
                patterns,
                description,
                ..
            } => {
                self.permission = Some(PermissionPrompt {
                    request_id,
                    description,
                    patterns,
                });
                self.set_turn_activity(TurnActivity::WaitingForPermission);
            }
            HarnessEvent::RetryScheduled { attempt, .. } => {
                self.set_turn_activity(TurnActivity::Retrying(attempt));
            }
            HarnessEvent::CompactionStarted { .. } => {
                self.set_turn_activity(TurnActivity::Compacting);
            }
            HarnessEvent::CompactionFinished { .. } => {
                self.set_turn_activity(TurnActivity::WaitingForResponse);
                self.transcript
                    .push(TranscriptEntry::Event("Context compacted.".to_string()));
            }
            HarnessEvent::CompactionRequired { .. } => self.transcript.push(
                TranscriptEntry::Event("Conversation compaction required.".to_string()),
            ),
            HarnessEvent::RunError { message, .. } => {
                self.transcript.push(TranscriptEntry::Event(message));
            }
            HarnessEvent::RunFinished { run_id, outcome } => {
                self.finish_turn(run_id, outcome);
            }
        }
    }

    pub fn toggle_fold(&mut self, index: usize) {
        match self.transcript.get_mut(index) {
            Some(TranscriptEntry::Thinking { expanded, .. })
            | Some(TranscriptEntry::Tool { expanded, .. }) => {
                *expanded = !*expanded;
                self.text_selection = None;
            }
            _ => {}
        }
    }

    pub fn toggle_all_thinking(&mut self) {
        let expand = self.transcript.iter().any(|entry| {
            matches!(
                entry,
                TranscriptEntry::Thinking {
                    expanded: false,
                    ..
                }
            )
        });
        for entry in &mut self.transcript {
            if let TranscriptEntry::Thinking { expanded, .. } = entry {
                *expanded = expand;
            }
        }
    }

    pub fn cancel_turn(&mut self) {
        if let Some(turn) = self.turn.as_mut() {
            turn.set_activity(TurnActivity::Cancelling);
        }
    }

    fn set_turn_activity(&mut self, activity: TurnActivity) {
        if let Some(turn) = self.turn.as_mut() {
            turn.set_activity(activity);
        }
    }

    fn finish_turn(&mut self, run_id: u64, outcome: RunOutcome) {
        let Some(turn) = self.turn.take() else {
            return;
        };
        self.permission = None;
        self.thinking_entries.clear();
        self.assistant_entries.clear();
        self.tool_entries.clear();
        let elapsed = format_elapsed(turn.started_at.elapsed().as_millis());
        let message = match outcome {
            RunOutcome::Completed => {
                let verb = if run_id.is_multiple_of(2) {
                    "Delegated"
                } else {
                    "Worked"
                };
                format!("{verb} for {elapsed}")
            }
            RunOutcome::Compacted => format!("Compacted in {elapsed}"),
            RunOutcome::Scheduled => format!("Scheduled in {elapsed}"),
            RunOutcome::Cancelled => format!("Turn cancelled by user in {elapsed}."),
            RunOutcome::Failed => format!("Turn failed in {elapsed}."),
            RunOutcome::CompactionRequired => format!("Paused for compaction after {elapsed}."),
            RunOutcome::StepLimitReached => format!("Step limit reached in {elapsed}."),
        };
        self.transcript.push(TranscriptEntry::Event(message));
        self.submit_next_queued_prompt();
    }

    fn submit_next_queued_prompt(&mut self) {
        let Some(text) = self.queued_prompts.pop_front() else {
            return;
        };
        let slash_tokens = recognized_slash_tokens(&text);
        self.transcript.push(TranscriptEntry::User {
            text: text.clone(),
            slash_tokens,
        });
        self.turn = Some(ActiveTurn::new());
        self.pending_submission = Some(text);
    }

    fn submit_internal_prompt(&mut self, status: &str, prompt: &str) {
        self.transcript
            .push(TranscriptEntry::Event(status.to_string()));
        if self.turn.is_some() {
            self.queued_prompts.push_back(prompt.to_string());
        } else {
            self.turn = Some(ActiveTurn::new());
            self.pending_submission = Some(prompt.to_string());
        }
    }

    pub fn on_tick(&mut self) {
        self.animation_tick = self.animation_tick.wrapping_add(1);
        self.sync_transcript_timestamps();
        if self
            .mode_banner
            .as_ref()
            .is_some_and(|banner| banner.shown_at.elapsed() >= Duration::from_millis(2_300))
        {
            self.mode_banner = None;
        }
        if self.animation_tick.is_multiple_of(2) {
            match self.selection_autoscroll {
                -1 => self.scroll_transcript_up(1),
                1 => self.scroll_transcript_down(1),
                _ => {}
            }
        }
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
            self.transcript
                .push(TranscriptEntry::Event(format!("Usage: {}", command.usage)));
            return true;
        }

        match command.name {
            "quit" => self.running = false,
            "new" => self.pending_session_command = Some(SessionCommand::New),
            "fork" => self.pending_session_command = Some(SessionCommand::Fork),
            "resume" => self.pending_session_command = Some(SessionCommand::OpenResume),
            "rewind" => self.pending_session_command = Some(SessionCommand::Rewind),
            "edit-prompt" => self.pending_session_command = Some(SessionCommand::EditPrompt),
            "copy" => {
                if let Some(response) = self.last_response() {
                    self.pending_session_command = Some(SessionCommand::Copy(response));
                } else {
                    self.transcript.push(TranscriptEntry::Event(
                        "There is no response to copy.".to_string(),
                    ));
                }
            }
            "rename" => {
                self.pending_session_command = Some(SessionCommand::Rename(args.to_string()))
            }
            "compact" => {
                self.turn = Some(ActiveTurn::new());
                self.pending_session_command = Some(SessionCommand::Compact);
            }
            "plan" => {
                let mode = if self.session_mode == SessionMode::Plan {
                    SessionMode::Normal
                } else {
                    SessionMode::Plan
                };
                self.pending_session_command = Some(SessionCommand::SetMode(mode));
            }
            "always-approve" => {
                let mode = match args {
                    "" => {
                        if self.session_mode == SessionMode::AlwaysApprove {
                            SessionMode::Normal
                        } else {
                            SessionMode::AlwaysApprove
                        }
                    }
                    "on" => SessionMode::AlwaysApprove,
                    "off" => SessionMode::Normal,
                    _ => {
                        self.transcript.push(TranscriptEntry::Event(
                            "Usage: /always-approve [on|off]".to_string(),
                        ));
                        return true;
                    }
                };
                self.pending_session_command = Some(SessionCommand::SetMode(mode));
            }
            "session-info" => self.pending_session_command = Some(SessionCommand::SessionInfo),
            "export" => {
                self.pending_session_command =
                    Some(SessionCommand::Export(self.transcript_markdown()));
            }
            "doctor" => self.pending_session_command = Some(SessionCommand::Doctor),
            "delete" => {
                if let Some(session_id) = self.session_id.clone() {
                    self.delete_confirmation = Some(DeleteConfirmation {
                        session_id,
                        title: self
                            .session_title
                            .clone()
                            .unwrap_or_else(|| "Untitled session".to_string()),
                    });
                } else {
                    self.transcript.push(TranscriptEntry::Event(
                        "This conversation has no saved session to delete.".to_string(),
                    ));
                }
            }
            "home" => {
                self.transcript.clear();
                self.turn = None;
            }
            "transcript" => {
                self.open_document("Full transcript", self.transcript_markdown());
            }
            "timeline" => {
                let timeline = self.timeline_text();
                self.open_document("Session timeline", timeline);
            }
            "find" => {
                let query = args.to_lowercase();
                let matches = self
                    .transcript
                    .iter()
                    .enumerate()
                    .filter_map(|(index, entry)| {
                        let (label, text) = transcript_entry_text(entry);
                        text.to_lowercase().contains(&query).then(|| {
                            format!(
                                "{}. {} — {}",
                                index + 1,
                                label,
                                text.lines().next().unwrap_or_default()
                            )
                        })
                    })
                    .collect::<Vec<_>>();
                self.open_document(
                    format!("Find: {args}"),
                    if matches.is_empty() {
                        "No matching transcript entries.".to_string()
                    } else {
                        matches.join("\n")
                    },
                );
            }
            "expand" => {
                for entry in &mut self.transcript {
                    match entry {
                        TranscriptEntry::Thinking { expanded, .. }
                        | TranscriptEntry::Tool { expanded, .. } => *expanded = true,
                        _ => {}
                    }
                }
                self.report_session_error("Expanded all reasoning and tool details.");
            }
            "multiline" => match parse_toggle(args, self.multiline_mode) {
                Some(enabled) => {
                    self.multiline_mode = enabled;
                    self.report_session_error(if enabled {
                        "Multiline input enabled. Press Ctrl+Enter to submit."
                    } else {
                        "Multiline input disabled. Press Enter to submit."
                    });
                }
                None => self.report_session_error("Usage: /multiline [on|off]"),
            },
            "timestamps" => match parse_toggle(args, self.timestamps_enabled) {
                Some(enabled) => {
                    self.timestamps_enabled = enabled;
                    self.sync_transcript_timestamps();
                    self.report_session_error(if enabled {
                        "Message timestamps enabled."
                    } else {
                        "Message timestamps disabled."
                    });
                }
                None => self.report_session_error("Usage: /timestamps [on|off]"),
            },
            "privacy" => self.open_document(
                "Privacy",
                "Your prompts, code, API keys, and session history remain on this device. Indus does not collect or retain your data. Model requests are sent only to the Compatible Interim Provider you select.",
            ),
            "release-notes" => {
                self.open_browser_catalog("Release notes", features::release_notes())
            }
            "mcps" => self.open_browser_catalog("MCP servers", features::mcp_catalog()),
            "skills" => {
                let items = features::installed_skills(&self.cwd);
                if items.is_empty() {
                    self.open_document("Skills", "No installed skills were found.");
                } else {
                    self.open_browser_catalog("Installed skills", items);
                }
            }
            "history" => {
                let prompts = self
                    .transcript
                    .iter()
                    .filter_map(|entry| match entry {
                        TranscriptEntry::User { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .enumerate()
                    .map(|(index, prompt)| format!("{}. {prompt}", index + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.open_document(
                    "Prompt history",
                    if prompts.is_empty() {
                        "No prompts in this session.".to_string()
                    } else {
                        prompts
                    },
                );
            }
            "queue" => self.open_document(
                "Queued prompts",
                if self.queued_prompts.is_empty() {
                    "No prompts are queued.".to_string()
                } else {
                    self.queued_prompts
                        .iter()
                        .enumerate()
                        .map(|(index, prompt)| format!("{}. {prompt}", index + 1))
                        .collect::<Vec<_>>()
                        .join("\n")
                },
            ),
            "recap" => self.submit_internal_prompt(
                "Generating a session recap…",
                "Provide a concise recap of this session: summarize the goal, completed work, important decisions, current state, and clear next steps. Do not use tools unless needed to verify the current state.",
            ),
            "jobs" => self.submit_internal_prompt(
                "Setting up Jobs…",
                &format!(
                    "Set up persistent Jobs for the following request. Use the job tool to create the required schedule and instructions, then confirm exactly what was scheduled:\n\n{args}"
                ),
            ),
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
                    self.transcript.push(TranscriptEntry::Event(format!(
                        "Unknown theme: {args}. Available: auto, indus-night, indusday, indus-midnight, indus-warm."
                    )));
                    return true;
                };
                self.theme_kind = next;
                self.preview_theme = None;
                persist_theme_preference(next);
            }
            "model" => self.open_provider_catalog(),
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

fn transcript_entry_text(entry: &TranscriptEntry) -> (&'static str, String) {
    match entry {
        TranscriptEntry::User { text, .. } => ("User", text.clone()),
        TranscriptEntry::Thinking { text, .. } => ("Thinking", text.clone()),
        TranscriptEntry::Assistant { text, .. } => ("Indus", text.clone()),
        TranscriptEntry::Tool {
            name,
            input,
            output,
            ..
        } => ("Tool", format!("{name}\nInput: {input}\nOutput: {output}")),
        TranscriptEntry::Event(text) => ("Event", text.clone()),
    }
}

fn parse_toggle(value: &str, current: bool) -> Option<bool> {
    match value {
        "" => Some(!current),
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn format_timestamp(timestamp: i64) -> String {
    let seconds = timestamp.rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        seconds % 3_600 / 60,
        seconds % 60
    )
}

fn byte_at_display_column(line: &str, column: usize) -> usize {
    let mut display_column = 0usize;
    let mut last = 0;
    for (byte, ch) in line.char_indices() {
        last = byte;
        let width = ch.width().unwrap_or(0);
        if column < display_column.saturating_add(width.max(1)) {
            return byte;
        }
        display_column = display_column.saturating_add(width);
    }
    last
}

fn ordered_selection(selection: TextSelection) -> (SelectionPoint, SelectionPoint) {
    if selection.anchor <= selection.head {
        (selection.anchor, selection.head)
    } else {
        (selection.head, selection.anchor)
    }
}

fn selected_byte_range(selection: TextSelection, row: usize, line: &str) -> Option<Range<usize>> {
    let (first, last) = ordered_selection(selection);
    if row < first.row || row > last.row {
        return None;
    }
    let start = if row == first.row {
        first.byte.min(line.len())
    } else {
        0
    };
    let end = if row == last.row {
        next_char_boundary(line, last.byte)
    } else {
        line.len()
    };
    (start <= end).then_some(start..end)
}

fn next_char_boundary(line: &str, byte: usize) -> usize {
    if byte >= line.len() {
        return line.len();
    }
    line[byte..]
        .chars()
        .next()
        .map_or(line.len(), |ch| byte + ch.len_utf8())
}

impl From<ModelCatalogView> for CatalogModal {
    fn from(view: ModelCatalogView) -> Self {
        Self::Models(view)
    }
}

fn provider_index(provider: ProviderId) -> usize {
    ProviderId::ALL
        .iter()
        .position(|candidate| *candidate == provider)
        .unwrap_or(0)
}

fn recent_model_index(recent: Option<&str>, models: &[ModelRecord]) -> usize {
    recent
        .and_then(|id| models.iter().position(|model| model.id == id))
        .unwrap_or(0)
}

fn theme_config_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .map(|root| root.join("indus").join("theme"))
}

fn load_theme_preference() -> Option<ThemeKind> {
    let path = theme_config_path()?;
    let value = std::fs::read_to_string(path).ok()?;
    ThemeKind::from_name(&value)
}

fn persist_theme_preference(kind: ThemeKind) {
    let Some(path) = theme_config_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_ok() {
        let _ = std::fs::write(path, kind.name());
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
    fn session_commands_dispatch_real_actions() {
        let mut app = App::new();
        app.transcript.push(TranscriptEntry::Assistant {
            id: "answer".into(),
            text: "copy this".into(),
            streaming: false,
        });
        app.composer.set("/copy");
        app.submit();
        assert_eq!(
            app.take_session_command(),
            Some(SessionCommand::Copy("copy this".into()))
        );

        app.composer.set("/rename Focused Session");
        app.submit();
        assert_eq!(
            app.take_session_command(),
            Some(SessionCommand::Rename("Focused Session".into()))
        );

        app.composer.set("/edit-prompt");
        app.submit();
        assert_eq!(app.take_session_command(), Some(SessionCommand::EditPrompt));

        app.composer.set("/resume");
        app.submit();
        assert_eq!(app.take_session_command(), Some(SessionCommand::OpenResume));

        app.composer.set("/new");
        app.submit();
        assert_eq!(app.take_session_command(), Some(SessionCommand::New));
    }

    #[test]
    fn mode_controls_follow_the_classifier_free_cycle() {
        let mut app = App::new();
        app.request_next_mode();
        assert_eq!(
            app.take_session_command(),
            Some(SessionCommand::SetMode(SessionMode::Plan))
        );
        app.confirm_mode(SessionMode::Plan);
        assert!(
            app.mode_banner().is_some_and(
                |(message, opacity)| message == "Switched to mode: Plan" && opacity > 0.0
            )
        );

        app.request_next_mode();
        assert_eq!(
            app.take_session_command(),
            Some(SessionCommand::SetMode(SessionMode::AlwaysApprove))
        );
        app.confirm_mode(SessionMode::AlwaysApprove);
        app.request_next_mode();
        assert_eq!(
            app.take_session_command(),
            Some(SessionCommand::SetMode(SessionMode::Normal))
        );
    }

    #[test]
    fn compact_command_starts_system_compaction_without_arguments() {
        let mut app = App::new();
        app.composer.set("/compact");
        app.submit();

        assert!(app.turn.is_some());
        assert_eq!(app.take_session_command(), Some(SessionCommand::Compact));
    }

    #[test]
    fn delete_requires_confirmation_for_an_allocated_session() {
        let mut app = App::new();
        app.session_id = Some("ses-i_example".into());
        app.session_title = Some("Example".into());
        app.composer.set("/delete");
        app.submit();

        assert!(app.delete_confirmation.is_some());
        assert!(app.confirm_delete());
        assert_eq!(app.take_session_command(), Some(SessionCommand::Delete));
    }

    #[test]
    fn turn_completion_creates_worked_for_event() {
        let mut app = App::new();
        app.composer.set("hello");
        app.submit();
        app.apply_harness_event(HarnessEvent::RunStarted { run_id: 1 });
        app.apply_harness_event(HarnessEvent::ReasoningStarted {
            run_id: 1,
            reasoning_id: "r1".into(),
        });
        app.apply_harness_event(HarnessEvent::ReasoningDelta {
            run_id: 1,
            reasoning_id: "r1".into(),
            text: "Considering the request".into(),
        });
        app.apply_harness_event(HarnessEvent::ReasoningFinished {
            run_id: 1,
            reasoning_id: "r1".into(),
        });
        app.apply_harness_event(HarnessEvent::RunFinished {
            run_id: 1,
            outcome: RunOutcome::Completed,
        });
        assert!(matches!(
            app.transcript.last(),
            Some(TranscriptEntry::Event(text)) if text.starts_with("Worked for ")
        ));
    }

    #[test]
    fn prompts_queue_fifo_until_each_turn_finishes() {
        let mut app = App::new();
        app.composer.set("first");
        app.submit();
        assert_eq!(app.take_submission().as_deref(), Some("first"));

        app.composer.set("second");
        app.submit();
        app.composer.set("third");
        app.submit();
        assert!(app.has_queued_prompts());

        app.apply_harness_event(HarnessEvent::RunFinished {
            run_id: 1,
            outcome: RunOutcome::Completed,
        });
        assert_eq!(app.take_submission().as_deref(), Some("second"));

        app.apply_harness_event(HarnessEvent::RunFinished {
            run_id: 2,
            outcome: RunOutcome::Failed,
        });
        assert_eq!(app.take_submission().as_deref(), Some("third"));
        assert!(!app.has_queued_prompts());
    }

    #[test]
    fn finished_thinking_collapses_with_elapsed_time() {
        let mut app = App::new();
        app.composer.set("hello");
        app.submit();
        app.apply_harness_event(HarnessEvent::ReasoningStarted {
            run_id: 1,
            reasoning_id: "r1".into(),
        });
        app.apply_harness_event(HarnessEvent::ReasoningFinished {
            run_id: 1,
            reasoning_id: "r1".into(),
        });
        assert!(matches!(
            &app.transcript[1],
            TranscriptEntry::Thinking {
                running: false,
                elapsed_ms: Some(_),
                expanded: false,
                ..
            }
        ));
    }

    #[test]
    fn responding_status_hides_when_the_response_stream_finishes() {
        let mut app = App::new();
        app.composer.set("hello");
        app.submit();
        app.apply_harness_event(HarnessEvent::RunStarted { run_id: 1 });
        app.apply_harness_event(HarnessEvent::TextStarted {
            run_id: 1,
            text_id: "answer".into(),
        });
        assert!(app.turn.as_ref().is_some_and(|turn| {
            turn.status_visible && turn.activity == TurnActivity::Responding
        }));

        app.apply_harness_event(HarnessEvent::TextFinished {
            run_id: 1,
            text_id: "answer".into(),
        });

        assert!(app.turn.as_ref().is_some_and(|turn| !turn.status_visible));
    }

    #[test]
    fn compacting_state_becomes_visible_after_a_completed_response() {
        let mut app = App::new();
        app.composer.set("hello");
        app.submit();
        app.apply_harness_event(HarnessEvent::TextStarted {
            run_id: 1,
            text_id: "answer".into(),
        });
        app.apply_harness_event(HarnessEvent::TextFinished {
            run_id: 1,
            text_id: "answer".into(),
        });
        app.apply_harness_event(HarnessEvent::CompactionStarted { run_id: 1 });

        assert!(app.turn.as_ref().is_some_and(|turn| {
            turn.status_visible && turn.activity == TurnActivity::Compacting
        }));
    }

    #[test]
    fn transcript_scrolling_leaves_follow_mode_until_bottom_is_reached() {
        let mut app = App::new();
        let area = Rect::new(2, 3, 20, 2);
        let rows = ["one", "two", "three", "four", "five"]
            .map(str::to_string)
            .to_vec();
        assert_eq!(app.sync_transcript_viewport(area, rows.clone()), 3);

        app.scroll_transcript_up(2);
        assert_eq!(app.sync_transcript_viewport(area, rows.clone()), 1);

        let mut extended = rows;
        extended.push("six".to_string());
        assert_eq!(app.sync_transcript_viewport(area, extended.clone()), 1);

        app.scroll_transcript_down(usize::MAX);
        assert_eq!(app.sync_transcript_viewport(area, extended), 4);
    }

    #[test]
    fn drag_selection_reconstructs_unicode_text_across_rows() {
        let mut app = App::new();
        let area = Rect::new(2, 5, 20, 3);
        app.sync_transcript_viewport(
            area,
            ["hello", "héllo", "world"].map(str::to_string).to_vec(),
        );

        assert!(app.begin_text_selection(2, 5));
        assert!(app.update_text_selection(3, 6));
        assert_eq!(app.finish_text_selection().as_deref(), Some("hello\nhé"));
        assert_eq!(app.selection_display_range(0), Some((0, 5)));
        assert_eq!(app.selection_display_range(1), Some((0, 2)));
    }

    #[test]
    fn a_click_without_a_drag_does_not_create_a_selection() {
        let mut app = App::new();
        app.sync_transcript_viewport(Rect::new(0, 0, 20, 1), vec!["hello".to_string()]);
        assert!(app.begin_text_selection(1, 0));
        assert_eq!(app.finish_text_selection(), None);
        assert_eq!(app.selection_display_range(0), None);
    }
}
