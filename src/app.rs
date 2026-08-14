use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
    time::Instant,
};

use ratatui::layout::Rect;

use crate::{
    harness::event::{FileDiff, HarnessEvent, PermissionReply, RunOutcome},
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

    #[cfg(test)]
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
    WaitingForResponse,
    Thinking,
    Responding,
    RunningTool(String),
    Retrying(u16),
    WaitingForPermission,
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
            Self::WaitingForPermission => "Waiting for permission…".to_string(),
            Self::Cancelling => "Cancelling…".to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ActiveTurn {
    pub run_id: Option<u64>,
    pub activity: TurnActivity,
    pub started_at: Instant,
    pub activity_started_at: Instant,
}

impl ActiveTurn {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            run_id: None,
            activity: TurnActivity::WaitingForResponse,
            started_at: now,
            activity_started_at: now,
        }
    }

    fn set_activity(&mut self, activity: TurnActivity) {
        self.activity = activity;
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
    pub animation_tick: u64,
    pub running: bool,
    pub hit_zones: HitZones,
    pending_submission: Option<String>,
    pending_permission_reply: Option<(u64, PermissionReply)>,
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
            animation_tick: 0,
            running: true,
            hit_zones: HitZones::default(),
            pending_submission: None,
            pending_permission_reply: None,
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

    pub fn take_permission_reply(&mut self) -> Option<(u64, PermissionReply)> {
        self.pending_permission_reply.take()
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

    pub fn apply_harness_event(&mut self, event: HarnessEvent) {
        match event {
            HarnessEvent::RunStarted { run_id } => {
                if let Some(turn) = self.turn.as_mut() {
                    turn.run_id = Some(run_id);
                    turn.set_activity(TurnActivity::WaitingForResponse);
                }
            }
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
            | Some(TranscriptEntry::Tool { expanded, .. }) => *expanded = !*expanded,
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
                let verb = if run_id % 2 == 0 {
                    "Delegated"
                } else {
                    "Worked"
                };
                format!("{verb} for {elapsed}")
            }
            RunOutcome::Cancelled => format!("Turn cancelled by user in {elapsed}."),
            RunOutcome::Failed => format!("Turn failed in {elapsed}."),
            RunOutcome::CompactionRequired => format!("Paused for compaction after {elapsed}."),
            RunOutcome::StepLimitReached => format!("Step limit reached in {elapsed}."),
        };
        self.transcript.push(TranscriptEntry::Event(message));
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
            self.transcript
                .push(TranscriptEntry::Event(format!("Usage: {}", command.usage)));
            return true;
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
}
