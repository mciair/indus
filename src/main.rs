use std::{
    io,
    path::PathBuf,
    process::Command,
    time::Duration,
};

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const PRODUCT: &str = "Indus";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const ALPHA_LABEL: &str = "Meet Alpha: Where Ideas Diverge";
const ALPHA_URL: &str = "https://mciair.in/hc";

const HOME_MENU: &[MenuItem] = &[
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

const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand::new("quit", "Quit Indus", "/quit"),
    SlashCommand::new("help", "Show available commands", "/help"),
    SlashCommand::new("docs", "Open documentation", "/docs"),
    SlashCommand::new("home", "Return to the home screen", "/home"),
    SlashCommand::new("delete", "Delete the current session", "/delete"),
    SlashCommand::new("new", "Start a new session", "/new"),
    SlashCommand::new("fork", "Fork the current session", "/fork"),
    SlashCommand::new(
        "compact",
        "Compact conversation context",
        "/compact [instructions]",
    ),
    SlashCommand::new("copy", "Copy the last response", "/copy"),
    SlashCommand::new("find", "Search scrollback", "/find <query>"),
    SlashCommand::new("history", "Open prompt history", "/history"),
    SlashCommand::new("export", "Export the current transcript", "/export"),
    SlashCommand::new("transcript", "View the full transcript", "/transcript"),
    SlashCommand::new("edit-prompt", "Edit the previous prompt", "/edit-prompt"),
    SlashCommand::new("expand", "Expand the current response", "/expand"),
    SlashCommand::new("context", "Show loaded project context", "/context"),
    SlashCommand::new("model", "Change the active model", "/model"),
    SlashCommand::new("effort", "Set reasoning effort", "/effort"),
    SlashCommand::new(
        "always-approve",
        "Toggle approval behavior",
        "/always-approve",
    ),
    SlashCommand::new("auto", "Toggle autonomous execution", "/auto"),
    SlashCommand::new("multiline", "Toggle multiline input", "/multiline"),
    SlashCommand::new("compact-mode", "Toggle compact UI mode", "/compact-mode"),
    SlashCommand::new("vim-mode", "Toggle Vim keybindings", "/vim-mode"),
    SlashCommand::new("share", "Prepare a shareable session link", "/share"),
    SlashCommand::new("session-info", "Show session metadata", "/session-info"),
    SlashCommand::new("rename", "Rename the current session", "/rename <name>"),
    SlashCommand::new("dashboard", "Open the session dashboard", "/dashboard"),
    SlashCommand::new("cd", "Change working directory", "/cd <path>"),
    SlashCommand::new(
        "theme",
        "Change terminal theme",
        "/theme <auto|indus-night|indusday|indus-midnight|indus-warm>",
    ),
    SlashCommand::new("feedback", "Send product feedback", "/feedback [message]"),
    SlashCommand::new(
        "announcements",
        "Show product announcements",
        "/announcements",
    ),
    SlashCommand::new("remember", "Store a project memory", "/remember <note>"),
    SlashCommand::new("plan", "Enter planning mode", "/plan"),
    SlashCommand::new("view-plan", "View the current plan", "/view-plan"),
    SlashCommand::new("resume", "Resume a previous session", "/resume"),
    SlashCommand::new("mcps", "Manage MCP servers", "/mcps"),
    SlashCommand::new("workflows", "View saved workflows", "/workflows"),
    SlashCommand::new("btw", "Add a side note to the agent", "/btw <note>"),
    SlashCommand::new("recap", "Generate a session recap", "/recap"),
    SlashCommand::new("doctor", "Run environment diagnostics", "/doctor"),
    SlashCommand::new("voice", "Toggle voice input", "/voice"),
    SlashCommand::new("loop", "Schedule repeated work", "/loop"),
    SlashCommand::new("timestamps", "Toggle message timestamps", "/timestamps"),
    SlashCommand::new("timeline", "Open session timeline", "/timeline"),
    SlashCommand::new("settings", "Open settings", "/settings"),
    SlashCommand::new("privacy", "Show privacy controls", "/privacy"),
    SlashCommand::new("rewind", "Rewind to an earlier turn", "/rewind"),
    SlashCommand::new("jump", "Jump to a session item", "/jump"),
    SlashCommand::new("login", "Sign in to Indus", "/login"),
    SlashCommand::new("logout", "Sign out of Indus", "/logout"),
    SlashCommand::new("usage", "Show usage summary", "/usage"),
    SlashCommand::new("queue", "Show queued prompts", "/queue"),
    SlashCommand::new("tasks", "Show background tasks", "/tasks"),
    SlashCommand::new("release-notes", "Show release notes", "/release-notes"),
    SlashCommand::new("tutorial", "Open the interactive tutorial", "/tutorial"),
    SlashCommand::new(
        "config-agents",
        "Configure project agents",
        "/config-agents",
    ),
    SlashCommand::new("personas", "Manage personas", "/personas"),
];

#[derive(Clone, Copy)]
struct MenuItem {
    label: &'static str,
    key: &'static str,
    action: MenuAction,
}

#[derive(Clone, Copy)]
enum MenuAction {
    Changelog,
    Resume,
    Worktree,
    Quit,
}

#[derive(Clone, Copy)]
struct SlashCommand {
    name: &'static str,
    description: &'static str,
    usage: &'static str,
}

impl SlashCommand {
    const fn new(name: &'static str, description: &'static str, usage: &'static str) -> Self {
        Self {
            name,
            description,
            usage,
        }
    }

    fn display(self) -> String {
        format!("/{}", self.name)
    }
}

#[derive(Default)]
struct HitZones {
    menu: Vec<(Rect, MenuAction)>,
    alpha: Option<Rect>,
    slash_rows: Vec<(Rect, usize)>,
}

struct App {
    cwd: PathBuf,
    input: String,
    messages: Vec<String>,
    selected_menu: usize,
    slash_selected: usize,
    slash_scroll: usize,
    theme_kind: ThemeKind,
    status: String,
    running: bool,
    hit_zones: HitZones,
}

impl App {
    fn new() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            input: String::new(),
            messages: Vec::new(),
            selected_menu: 0,
            slash_selected: 0,
            slash_scroll: 0,
            theme_kind: ThemeKind::IndusNight,
            status: "Ready. Type a prompt or / for commands.".to_string(),
            running: true,
            hit_zones: HitZones::default(),
        }
    }

    fn slash_open(&self) -> bool {
        self.input.trim_start().starts_with('/')
    }

    fn slash_query(&self) -> &str {
        self.input
            .trim_start()
            .strip_prefix('/')
            .unwrap_or_default()
            .split_whitespace()
            .next()
            .unwrap_or_default()
    }

    fn slash_matches(&self) -> Vec<SlashCommand> {
        let query = self.slash_query();
        SLASH_COMMANDS
            .iter()
            .copied()
            .filter(|cmd| {
                query.is_empty()
                    || cmd.name.contains(query)
                    || cmd
                        .description
                        .to_lowercase()
                        .contains(&query.to_lowercase())
            })
            .collect()
    }

    fn clamp_slash_selection(&mut self) {
        let len = self.slash_matches().len();
        if len == 0 {
            self.slash_selected = 0;
            self.slash_scroll = 0;
        } else if self.slash_selected >= len {
            self.slash_selected = len - 1;
        }
    }

    fn move_slash_selection(&mut self, delta: isize) {
        let len = self.slash_matches().len();
        if len == 0 {
            return;
        }
        self.slash_selected = if delta.is_negative() {
            self.slash_selected.saturating_sub(delta.unsigned_abs())
        } else {
            (self.slash_selected + delta as usize).min(len - 1)
        };
    }

    fn select_slash(&mut self) {
        let matches = self.slash_matches();
        if let Some(cmd) = matches.get(self.slash_selected) {
            self.input = format!("/{} ", cmd.name);
            self.status = format!("Selected {}", cmd.usage);
        }
    }

    fn submit_input(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            self.run_menu_action(HOME_MENU[self.selected_menu].action);
            return;
        }
        if text == "/quit" || text == "/exit" {
            self.running = false;
            return;
        }
        if let Some(args) = text.strip_prefix("/theme") {
            self.apply_theme_command(args.trim());
            self.input.clear();
            self.slash_selected = 0;
            self.slash_scroll = 0;
            return;
        }
        self.status = format!("Queued: {text}");
        self.messages.push(text);
        self.input.clear();
        self.slash_selected = 0;
        self.slash_scroll = 0;
    }

    fn apply_theme_command(&mut self, args: &str) {
        if args.is_empty() {
            self.status =
                "Themes: auto, indus-night, indusday, indus-midnight, indus-warm".to_string();
            return;
        }
        let selected = args.split_whitespace().next().unwrap_or_default();
        match ThemeKind::from_name(selected) {
            Some(kind) => {
                self.theme_kind = kind;
                self.status = format!("Theme set to {}", kind.name());
            }
            None => {
                self.status = format!(
                    "Unknown theme '{selected}'. Use auto, indus-night, indusday, indus-midnight, or indus-warm."
                );
            }
        }
    }

    fn run_menu_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::Changelog => {
                self.status = "Changelog selected. Release notes wiring comes next.".to_string();
            }
            MenuAction::Resume => {
                self.status =
                    "Resume Session selected. Session loader wiring comes next.".to_string();
            }
            MenuAction::Worktree => {
                self.status =
                    "New Worktree selected. Worktree creation wiring comes next.".to_string();
            }
            MenuAction::Quit => self.running = false,
        }
    }

    fn open_alpha(&mut self) {
        let result = if cfg!(target_os = "macos") {
            Command::new("open").arg(ALPHA_URL).status()
        } else if cfg!(target_os = "windows") {
            Command::new("cmd")
                .args(["/C", "start", ALPHA_URL])
                .status()
        } else {
            Command::new("xdg-open").arg(ALPHA_URL).status()
        };
        self.status = match result {
            Ok(status) if status.success() => format!("Opened {ALPHA_URL}"),
            _ => format!("Open this link: {ALPHA_URL}"),
        };
    }
}

fn main() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run(&mut terminal);
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let mut app = App::new();
    while app.running {
        terminal.draw(|frame| render(frame, &mut app))?;
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => handle_key(&mut app, key),
                Event::Mouse(mouse) => handle_mouse(&mut app, mouse),
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, key: crossterm::event::KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.running = false;
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
        app.running = false;
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('u') {
        app.open_alpha();
        return;
    }

    match key.code {
        KeyCode::Esc => {
            if app.slash_open() {
                app.input.clear();
            } else {
                app.running = false;
            }
        }
        KeyCode::Up => {
            if app.slash_open() {
                app.move_slash_selection(-1);
            } else {
                app.selected_menu = app.selected_menu.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if app.slash_open() {
                app.move_slash_selection(1);
            } else {
                app.selected_menu = (app.selected_menu + 1).min(HOME_MENU.len() - 1);
            }
        }
        KeyCode::Tab => {
            if app.slash_open() {
                app.select_slash();
            }
        }
        KeyCode::Enter => {
            if app.slash_open()
                && !app.slash_matches().is_empty()
                && app.input.split_whitespace().count() == 1
            {
                app.select_slash();
            } else {
                app.submit_input();
            }
        }
        KeyCode::Backspace => {
            app.input.pop();
            app.clamp_slash_selection();
        }
        KeyCode::Char(ch) => {
            app.input.push(ch);
            app.clamp_slash_selection();
        }
        _ => {}
    }
}

fn handle_mouse(app: &mut App, mouse: crossterm::event::MouseEvent) {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return;
    }
    let x = mouse.column;
    let y = mouse.row;
    if let Some(rect) = app.hit_zones.alpha
        && contains(rect, x, y)
    {
        app.open_alpha();
        return;
    }
    let slash_hit = app
        .hit_zones
        .slash_rows
        .iter()
        .find_map(|(rect, idx)| contains(*rect, x, y).then_some(*idx));
    if let Some(idx) = slash_hit {
        app.slash_selected = idx;
        app.select_slash();
        return;
    }
    let menu_hit = app
        .hit_zones
        .menu
        .iter()
        .enumerate()
        .find_map(|(idx, (rect, action))| contains(*rect, x, y).then_some((idx, *action)));
    if let Some((idx, action)) = menu_hit {
        app.selected_menu = idx;
        app.run_menu_action(action);
    }
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

fn render(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    let mut zones = HitZones::default();
    let theme = Theme::from_kind(app.theme_kind);
    let base = Block::default().style(Style::default().bg(theme.bg));
    frame.render_widget(base, area);

    let [top, body, status, input] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(12),
        Constraint::Length(1),
        Constraint::Length(3),
    ])
    .areas(area);

    render_top_bar(frame, top, app, &theme);
    if app.messages.is_empty() {
        render_home(frame, body, app, &theme, &mut zones);
    } else {
        render_chat(frame, body, app, &theme);
    }
    render_status(frame, status, app, &theme);
    render_input(frame, input, app, &theme);
    if app.slash_open() {
        render_slash_dropdown(frame, input, app, &theme, &mut zones);
    }

    app.hit_zones = zones;
}

fn render_top_bar(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let branch = current_branch().unwrap_or_else(|| "no git branch".to_string());
    let left = Line::from(vec![
        Span::styled(" ", Style::default().fg(theme.muted)),
        Span::styled(
            branch,
            Style::default().fg(theme.text).add_modifier(Modifier::DIM),
        ),
        Span::raw("  "),
        Span::styled(collapse_home(&app.cwd), Style::default().fg(theme.dim)),
    ]);
    frame.render_widget(Paragraph::new(left), area);
}

fn render_home(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme, zones: &mut HitZones) {
    let min_box_width = 72;
    let box_width = area
        .width
        .saturating_sub(8)
        .min(112)
        .max(min_box_width.min(area.width));
    let box_height = 14u16.min(area.height.saturating_sub(2)).max(10);
    let [_, box_area, _] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(box_height),
        Constraint::Min(0),
    ])
    .flex(Flex::Center)
    .areas(area);
    let [_, card, _] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(box_width),
        Constraint::Min(0),
    ])
    .flex(Flex::Center)
    .areas(box_area);

    frame.render_widget(Clear, card);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.panel));
    frame.render_widget(block, card);

    let inner = card.inner(Margin {
        horizontal: 3,
        vertical: 1,
    });
    let [title, cta, gap, menu] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(HOME_MENU.len() as u16),
    ])
    .areas(inner);

    let title_line = Line::from(vec![
        Span::styled(
            PRODUCT,
            Style::default()
                .fg(theme.text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default()),
        Span::styled("India's AI-native CLI", Style::default().fg(theme.text)),
        Span::styled(format!("  v{VERSION}"), Style::default().fg(theme.muted)),
    ]);
    frame.render_widget(Paragraph::new(title_line), title);

    let cta_text = format!("[{ALPHA_LABEL}]");
    let cta_line = Line::from(vec![
        Span::styled(
            cta_text.clone(),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(ALPHA_URL, Style::default().fg(theme.muted)),
        Span::styled("  Ctrl+U", Style::default().fg(theme.dim)),
    ]);
    frame.render_widget(Paragraph::new(cta_line), cta);
    zones.alpha = Some(Rect {
        x: cta.x,
        y: cta.y,
        width: (cta_text.width() + 1 + ALPHA_URL.width()) as u16,
        height: 1,
    });

    let _ = gap;
    zones.menu = render_menu_rows(frame, menu, app, theme);
}

fn render_chat(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    fill(frame.buffer_mut(), area, Style::default().bg(theme.chat_bg));
    let content = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    let mut y = content.bottom().saturating_sub(1);
    for message in app.messages.iter().rev() {
        if y < content.y {
            break;
        }
        let width = message.width().min(content.width.saturating_sub(6) as usize) as u16 + 4;
        let x = content.right().saturating_sub(width);
        let row = Rect {
            x,
            y,
            width,
            height: 1,
        };
        fill(frame.buffer_mut(), row, Style::default().bg(theme.sent_bg));
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {}  ", truncate(message, width.saturating_sub(4) as usize)),
                Style::default().fg(theme.sent_text).bg(theme.sent_bg),
            ))),
            row,
        );
        y = y.saturating_sub(2);
    }
}

fn render_menu_rows(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &Theme,
) -> Vec<(Rect, MenuAction)> {
    let row_width = 42u16.min(area.width);
    let [_, menu_area, _] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(row_width),
        Constraint::Min(0),
    ])
    .flex(Flex::Center)
    .areas(area);

    let mut rows = Vec::with_capacity(HOME_MENU.len());
    for (idx, item) in HOME_MENU.iter().enumerate() {
        let row = Rect {
            x: menu_area.x,
            y: menu_area.y + idx as u16,
            width: menu_area.width,
            height: 1,
        };
        rows.push((row, item.action));
        let selected = app.selected_menu == idx;
        let bg = if selected {
            theme.highlight
        } else {
            theme.panel
        };
        fill(frame.buffer_mut(), row, Style::default().bg(bg));
        let label_style = Style::default()
            .fg(theme.text)
            .bg(bg)
            .add_modifier(Modifier::BOLD);
        let key_style = Style::default().fg(theme.muted).bg(bg);
        let key = format!("{} ", item.key);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(item.label, label_style),
                Span::styled(
                    " ".repeat(
                        row.width
                            .saturating_sub(item.label.width() as u16 + key.width() as u16)
                            as usize,
                    ),
                    Style::default().bg(bg),
                ),
                Span::styled(key, key_style),
            ])),
            row,
        );
    }
    rows
}

fn render_status(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let line = Line::from(vec![
        Span::styled("● ", Style::default().fg(theme.accent)),
        Span::styled(&app.status, Style::default().fg(theme.muted)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_input(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.prompt_border_active))
        .style(Style::default().bg(theme.input_bg));
    let text = if app.input.is_empty() {
        Line::from(Span::styled(
            "Type a message...",
            Style::default().fg(theme.input_placeholder),
        ))
    } else {
        Line::from(Span::styled(
            &app.input,
            Style::default().fg(theme.input_text),
        ))
    };
    frame.render_widget(Paragraph::new(text).block(block), area);
    let cursor_x = area.x + 1 + app.input.width() as u16;
    let cursor_y = area.y + 1;
    if cursor_x < area.right().saturating_sub(1) {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn render_slash_dropdown(
    frame: &mut Frame<'_>,
    input_area: Rect,
    app: &mut App,
    theme: &Theme,
    zones: &mut HitZones,
) {
    let matches = app.slash_matches();
    if matches.is_empty() {
        return;
    }
    let max_rows = 9usize.min(matches.len());
    if app.slash_selected < app.slash_scroll {
        app.slash_scroll = app.slash_selected;
    } else if app.slash_selected >= app.slash_scroll + max_rows {
        app.slash_scroll = app.slash_selected + 1 - max_rows;
    }

    let width = input_area.width.saturating_sub(2).min(92);
    let height = (max_rows as u16 + 2).min(input_area.y);
    let area = Rect {
        x: input_area.x + 1,
        y: input_area.y.saturating_sub(height),
        width,
        height,
    };
    frame.render_widget(Clear, area);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.panel));
    frame.render_widget(block, area);

    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    zones.slash_rows.clear();
    for (visible_idx, cmd) in matches
        .iter()
        .enumerate()
        .skip(app.slash_scroll)
        .take(max_rows)
    {
        let row = Rect {
            x: inner.x,
            y: inner.y + zones.slash_rows.len() as u16,
            width: inner.width,
            height: 1,
        };
        zones.slash_rows.push((row, visible_idx));
        let selected = visible_idx == app.slash_selected;
        let bg = if selected {
            theme.highlight
        } else {
            theme.panel
        };
        fill(frame.buffer_mut(), row, Style::default().bg(bg));
        let display = cmd.display();
        let desc_width = row.width.saturating_sub(26) as usize;
        let desc = truncate(&cmd.description, desc_width);
        let line = Line::from(vec![
            Span::styled(
                if selected { "❯ " } else { "  " },
                Style::default().fg(theme.accent).bg(bg),
            ),
            Span::styled(
                pad(&display, 22),
                Style::default()
                    .fg(theme.text)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(desc, Style::default().fg(theme.muted).bg(bg)),
        ]);
        frame.render_widget(Paragraph::new(line), row);
    }
}

fn fill(buf: &mut Buffer, area: Rect, style: Style) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(style);
            }
        }
    }
}

fn current_branch() -> Option<String> {
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!branch.is_empty()).then_some(branch)
}

fn collapse_home(path: &PathBuf) -> String {
    let value = path.display().to_string();
    std::env::var("HOME")
        .ok()
        .and_then(|home| value.strip_prefix(&home).map(|rest| format!("~{rest}")))
        .unwrap_or(value)
}

fn pad(value: &str, width: usize) -> String {
    let used = value.width();
    if used >= width {
        truncate(value, width)
    } else {
        format!("{value}{}", " ".repeat(width - used))
    }
}

fn truncate(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    for ch in value.chars() {
        if out.width() + ch.width().unwrap_or(0) >= width {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

#[derive(Clone, Copy)]
enum ThemeKind {
    Auto,
    IndusNight,
    IndusDay,
    IndusMidnight,
    IndusWarm,
}

impl ThemeKind {
    fn from_name(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "indus-night" | "indusnight" | "night" | "dark" => Some(Self::IndusNight),
            "indusday" | "indus-day" | "day" | "light" => Some(Self::IndusDay),
            "indus-midnight" | "indusmidnight" | "midnight" | "oscura" => {
                Some(Self::IndusMidnight)
            }
            "indus-warm" | "induswarm" | "warm" => Some(Self::IndusWarm),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::IndusNight => "indus-night",
            Self::IndusDay => "indusday",
            Self::IndusMidnight => "indus-midnight",
            Self::IndusWarm => "indus-warm",
        }
    }

    fn resolved(self) -> Self {
        match self {
            Self::Auto => Self::IndusNight,
            other => other,
        }
    }
}

#[derive(Clone, Copy)]
struct Theme {
    bg: Color,
    panel: Color,
    chat_bg: Color,
    highlight: Color,
    border: Color,
    prompt_border_active: Color,
    text: Color,
    muted: Color,
    dim: Color,
    accent: Color,
    warning: Color,
    input_bg: Color,
    input_text: Color,
    input_placeholder: Color,
    sent_bg: Color,
    sent_text: Color,
}

impl Theme {
    fn from_kind(kind: ThemeKind) -> Self {
        match kind.resolved() {
            ThemeKind::Auto | ThemeKind::IndusNight => Self::indus_night(),
            ThemeKind::IndusDay => Self::indus_day(),
            ThemeKind::IndusMidnight => Self::indus_midnight(),
            ThemeKind::IndusWarm => Self::indus_warm(),
        }
    }

    fn indus_night() -> Self {
        Self {
            bg: rgb(20, 20, 20),
            panel: rgb(28, 28, 28),
            chat_bg: rgb(20, 20, 20),
            highlight: rgb(54, 54, 54),
            border: rgb(60, 60, 65),
            prompt_border_active: rgb(80, 80, 88),
            text: rgb(236, 238, 242),
            muted: rgb(170, 170, 170),
            dim: rgb(88, 88, 88),
            accent: rgb(224, 175, 104),
            warning: rgb(224, 175, 104),
            input_bg: rgb(20, 20, 20),
            input_text: rgb(236, 238, 242),
            input_placeholder: rgb(88, 88, 88),
            sent_bg: rgb(28, 28, 28),
            sent_text: rgb(236, 238, 242),
        }
    }

    fn indus_day() -> Self {
        Self {
            bg: rgb(224, 224, 224),
            panel: rgb(238, 238, 238),
            chat_bg: rgb(224, 224, 224),
            highlight: rgb(198, 198, 198),
            border: rgb(185, 185, 190),
            prompt_border_active: rgb(165, 165, 175),
            text: rgb(35, 35, 35),
            muted: rgb(90, 90, 90),
            dim: rgb(150, 150, 150),
            accent: rgb(168, 120, 10),
            warning: rgb(224, 175, 104),
            input_bg: rgb(224, 224, 224),
            input_text: rgb(35, 35, 35),
            input_placeholder: rgb(120, 120, 120),
            sent_bg: rgb(238, 238, 238),
            sent_text: rgb(35, 35, 35),
        }
    }

    fn indus_midnight() -> Self {
        Self {
            bg: rgb(4, 5, 10),
            panel: rgb(14, 16, 24),
            chat_bg: rgb(4, 5, 10),
            highlight: rgb(31, 35, 49),
            border: rgb(45, 50, 70),
            prompt_border_active: rgb(72, 79, 108),
            text: rgb(235, 239, 245),
            muted: rgb(145, 151, 168),
            dim: rgb(91, 98, 118),
            accent: rgb(224, 175, 104),
            warning: rgb(224, 175, 104),
            input_bg: rgb(4, 5, 10),
            input_text: rgb(235, 239, 245),
            input_placeholder: rgb(91, 98, 118),
            sent_bg: rgb(14, 16, 24),
            sent_text: rgb(235, 239, 245),
        }
    }

    fn indus_warm() -> Self {
        Self {
            bg: rgb(220, 173, 83),
            panel: rgb(220, 173, 83),
            chat_bg: rgb(220, 173, 83),
            highlight: rgb(192, 121, 78),
            border: rgb(120, 72, 45),
            prompt_border_active: rgb(92, 54, 36),
            text: Color::Black,
            muted: Color::Black,
            dim: Color::Black,
            accent: Color::Black,
            warning: Color::Black,
            input_bg: rgb(196, 104, 70),
            input_text: Color::Black,
            input_placeholder: Color::Black,
            sent_bg: rgb(196, 104, 70),
            sent_text: Color::Black,
        }
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}
