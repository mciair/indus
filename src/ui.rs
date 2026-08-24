use std::{path::Path, process::Command};

use chrono::{TimeZone, Utc};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Alignment, Constraint, Flex, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::{
        App, BtwPanel, CatalogModal, HOME_MENU, HitZones, ModelCatalogView, ToolVisualState,
        TranscriptEntry, TurnActivity, UsageCard,
    },
    features::BrowserAction,
    harness::event::{DiffKind, FileDiff},
    provider::ProviderId,
    theme::Theme,
};

const PRODUCT: &str = "Indus";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const ALPHA_LABEL: &str = "Meet Alpha: Where Ideas Diverge";
pub const ALPHA_URL: &str = "https://mciair.in/hc";
const MAX_SLASH_ROWS: usize = 6;

pub fn render(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    let theme = Theme::for_preference(app.effective_theme_kind());
    frame.render_widget(Block::default().style(theme.base()), area);

    let prompt_width = area.width.saturating_sub(4).max(4);
    let prompt_height = composer_height(app.composer.text(), prompt_width.saturating_sub(8));
    let turn_status_visible = app.turn.as_ref().is_some_and(|turn| turn.status_visible);
    let btw_height = live_btw_height(app.btw_panel.as_ref(), prompt_width);
    let turn_height = if app.permission.is_some() {
        3
    } else {
        u16::from(turn_status_visible)
    };
    let prompt_gap = u16::from(turn_height == 0 && area.height > 16);
    let [top, body, banner, btw, turn, prompt] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(4),
        Constraint::Length(prompt_gap),
        Constraint::Length(btw_height),
        Constraint::Length(turn_height),
        Constraint::Length(prompt_height),
    ])
    .areas(area);

    let [_, content, _] = Layout::horizontal([
        Constraint::Length(2),
        Constraint::Min(4),
        Constraint::Length(2),
    ])
    .areas(body);
    let prompt = Rect {
        x: area.x + 2,
        width: prompt_width,
        ..prompt
    };
    let turn = Rect {
        x: prompt.x + 2,
        width: prompt.width.saturating_sub(4),
        ..turn
    };
    let btw = Rect {
        x: prompt.x,
        width: prompt.width,
        ..btw
    };

    let mut zones = HitZones::default();
    render_top_bar(frame, top, app, &theme);
    if app.transcript.is_empty() {
        render_home(frame, content, app, &theme, &mut zones);
    } else {
        render_transcript(frame, content, app, &theme, &mut zones);
    }
    if turn_status_visible {
        render_turn_status(frame, turn, app, &theme);
    } else if let Some((message, opacity)) = app.mode_banner() {
        render_mode_banner(frame, banner, message, opacity, &theme);
    }
    if let Some(panel) = app.btw_panel.as_ref() {
        render_live_btw(frame, btw, panel, app.animation_tick, &theme);
    }
    render_composer(frame, prompt, app, &theme);
    if app.slash.open {
        render_slash_dropdown(frame, prompt, app, &theme, &mut zones);
    }
    if app.catalog_modal.is_some() {
        render_catalog_modal(frame, prompt, app, &theme, &mut zones);
    }
    if let Some(panel) = app.resume_panel.as_ref() {
        render_resume_panel(frame, panel, &theme, &mut zones);
    }
    if app.browser_panel.is_some() {
        render_browser_panel(frame, app, &theme, &mut zones);
    }
    if let Some(confirmation) = app.delete_confirmation.as_ref() {
        render_delete_confirmation(frame, area, confirmation, &theme);
    }
    app.hit_zones = zones;
}

fn live_btw_height(panel: Option<&BtwPanel>, width: u16) -> u16 {
    match panel {
        None => 0,
        Some(BtwPanel::Loading { .. }) | Some(BtwPanel::Error { .. }) => 3,
        Some(BtwPanel::Done { answer, .. }) => {
            let body_width = width.saturating_sub(4).max(1) as usize;
            let lines = answer
                .lines()
                .map(|line| wrap_text(line, body_width).len().max(1))
                .sum::<usize>()
                .clamp(1, 12);
            lines as u16 + 2
        }
    }
}

fn render_live_btw(
    frame: &mut Frame<'_>,
    area: Rect,
    panel: &BtwPanel,
    animation_tick: u64,
    theme: &Theme,
) {
    if area.width < 12 || area.height < 3 {
        return;
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.gray_dim))
            .style(Style::default().bg(theme.bg_base)),
        area,
    );
    let hint = " [Esc] ";
    frame.render_widget(
        Paragraph::new(Line::styled(
            hint,
            Style::default().fg(theme.gray).bg(theme.bg_base),
        )),
        Rect::new(
            area.right().saturating_sub(hint.width() as u16 + 1),
            area.y,
            hint.width() as u16,
            1,
        ),
    );
    let max_title = area.width.saturating_sub(hint.width() as u16 + 6) as usize;
    let title = format!(" /btw {} ", truncate(panel.question(), max_title));
    frame.render_widget(
        Paragraph::new(Line::styled(
            title,
            Style::default()
                .fg(theme.accent_user)
                .bg(theme.bg_base)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(area.x + 1, area.y, area.width.saturating_sub(10), 1),
    );
    let body = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    match panel {
        BtwPanel::Loading { .. } => {
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let spinner = frames[(animation_tick as usize / 2) % frames.len()];
            frame.render_widget(
                Paragraph::new(Line::styled(
                    format!("{spinner} Answering…"),
                    Style::default().fg(theme.gray),
                )),
                body,
            );
        }
        BtwPanel::Done { answer, .. } => {
            frame.render_widget(
                Paragraph::new(markdown_lines(answer, body.width.max(1) as usize, theme)),
                body,
            );
        }
        BtwPanel::Error { message, .. } => frame.render_widget(
            Paragraph::new(Line::styled(
                truncate(message, body.width as usize),
                Style::default().fg(theme.accent_error),
            )),
            body,
        ),
    }
}

fn render_mode_banner(
    frame: &mut Frame<'_>,
    area: Rect,
    message: &str,
    opacity: f32,
    theme: &Theme,
) {
    if area.height == 0 {
        return;
    }
    let foreground = blend_color(theme.bg_base, theme.text_secondary, opacity);
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!("  {message}"),
            Style::default().fg(foreground).bg(theme.bg_base),
        )),
        area,
    );
}

fn render_top_bar(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let branch = current_branch().unwrap_or_else(|| "no git branch".to_string());
    let line = Line::from(vec![
        Span::styled(" ", Style::default().fg(theme.gray)),
        Span::styled(branch, Style::default().fg(theme.text_secondary)),
        Span::raw("  "),
        Span::styled(collapse_home(&app.cwd), Style::default().fg(theme.gray_dim)),
    ]);
    frame.render_widget(Paragraph::new(line).style(theme.base()), area);
    if let Some(title) = app.session_title.as_deref() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                truncate(title, area.width.saturating_sub(8) as usize),
                Style::default().fg(theme.text_secondary),
            ))
            .alignment(Alignment::Right),
            area,
        );
    }
}

fn render_resume_panel(
    frame: &mut Frame<'_>,
    state: &crate::app::ResumePanel,
    theme: &Theme,
    zones: &mut HitZones,
) {
    let sessions = state.visible_sessions();
    let shown = sessions.len().min(8);
    let detail_height = u16::from(state.expanded && !sessions.is_empty()) * 8;
    let desired_height = 8 + shown as u16 + detail_height;
    let width = frame.area().width.saturating_sub(4).clamp(48, 96);
    let height = desired_height
        .min(frame.area().height.saturating_sub(2))
        .max(7);
    let panel = centered_rect(frame.area(), width.min(frame.area().width), height);

    frame.render_widget(Clear, panel);
    frame.render_widget(
        Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.prompt_border))
            .style(Style::default().bg(theme.bg_light)),
        panel,
    );
    let inner = panel.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    frame.render_widget(
        Paragraph::new(Line::styled(
            "Resume session",
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Line::styled("esc", Style::default().fg(theme.gray)))
            .alignment(Alignment::Right),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let search = Rect::new(inner.x, inner.y + 2, inner.width, 1);
    let search_text = if state.query.is_empty() {
        "Search sessions".to_string()
    } else {
        state.query.text().to_string()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("› ", Style::default().fg(theme.fuzzy_accent)),
            Span::styled(
                search_text,
                Style::default().fg(if state.query.is_empty() {
                    theme.gray
                } else {
                    theme.text_primary
                }),
            ),
        ])),
        search,
    );

    let rows_y = search.y + 2;
    if sessions.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                if state.query.is_empty() {
                    "No saved sessions yet"
                } else {
                    "No matching sessions"
                },
                Style::default().fg(theme.gray),
            )),
            Rect::new(inner.x, rows_y, inner.width, 1),
        );
    } else {
        let start = state.selected.saturating_add(1).saturating_sub(shown);
        for (offset, (index, session)) in sessions
            .iter()
            .enumerate()
            .skip(start)
            .take(shown)
            .enumerate()
        {
            let row = Rect::new(inner.x, rows_y + offset as u16, inner.width, 1);
            let selected = index == state.selected;
            let background = if selected {
                theme.bg_visual
            } else {
                theme.bg_light
            };
            fill(frame.buffer_mut(), row, Style::default().bg(background));
            let relative = relative_time(session.updated_at);
            let available = row.width.saturating_sub(relative.width() as u16 + 5) as usize;
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        if selected { "› " } else { "  " },
                        Style::default().fg(theme.fuzzy_accent).bg(background),
                    ),
                    Span::styled(
                        truncate(&session.title, available),
                        Style::default()
                            .fg(theme.text_primary)
                            .bg(background)
                            .add_modifier(if selected {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                    ),
                ])),
                row,
            );
            frame.render_widget(
                Paragraph::new(Line::styled(
                    relative,
                    Style::default().fg(theme.gray).bg(background),
                ))
                .alignment(Alignment::Right),
                row,
            );
            zones.resume_rows.push((row, index));
        }

        if state.expanded
            && let Some(session) = sessions.get(state.selected)
        {
            let details_y = rows_y + shown as u16 + 1;
            let model = match (&session.provider_id, &session.model_id) {
                (Some(provider), Some(model)) => format!("{provider} · {model}"),
                (_, Some(model)) => model.clone(),
                _ => "Unknown".to_string(),
            };
            let details = [
                ("ID", session.id.clone()),
                ("CWD", session.directory.clone()),
                ("Model", model),
                ("Created", absolute_time(session.created_at)),
                ("Updated", absolute_time(session.updated_at)),
                ("Messages", session.message_count.to_string()),
                ("Prompt", session.first_prompt.replace('\n', " ")),
            ];
            for (offset, (label, value)) in details.into_iter().enumerate() {
                let y = details_y + offset as u16;
                if y >= inner.bottom().saturating_sub(1) {
                    break;
                }
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(format!("{label:<9}"), Style::default().fg(theme.gray)),
                        Span::styled(
                            truncate(&value, inner.width.saturating_sub(10) as usize),
                            Style::default().fg(theme.text_secondary),
                        ),
                    ])),
                    Rect::new(inner.x + 2, y, inner.width.saturating_sub(2), 1),
                );
            }
        }
    }

    let footer = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
    frame.render_widget(
        Paragraph::new(Line::styled(
            "enter resume   ↑↓ navigate   tab details   esc close",
            Style::default().fg(theme.gray),
        )),
        footer,
    );
    let cursor = state.query.text()[..state.query.cursor()].width() as u16;
    frame.set_cursor_position((
        search.x + 2 + cursor.min(search.width.saturating_sub(3)),
        search.y,
    ));
}

fn render_browser_panel(frame: &mut Frame<'_>, app: &mut App, theme: &Theme, zones: &mut HitZones) {
    let Some(state) = app.browser_panel.clone() else {
        return;
    };
    let width = frame.area().width.saturating_sub(4).clamp(52, 96);
    let height = frame.area().height.saturating_sub(4).clamp(12, 28);
    let panel = centered_rect(frame.area(), width.min(frame.area().width), height);
    frame.render_widget(Clear, panel);
    frame.render_widget(
        Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.prompt_border))
            .style(Style::default().bg(theme.bg_light)),
        panel,
    );
    let inner = panel.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    let heading = state
        .detail
        .as_ref()
        .map_or(state.title.as_str(), |detail| detail.title.as_str());
    frame.render_widget(
        Paragraph::new(Line::styled(
            truncate(heading, inner.width.saturating_sub(8) as usize),
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Line::styled("esc", Style::default().fg(theme.gray)))
            .alignment(Alignment::Right),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let content = Rect::new(
        inner.x,
        inner.y + 2,
        inner.width,
        inner.height.saturating_sub(4),
    );
    if let Some(detail) = &state.detail {
        let lines = detail
            .body
            .lines()
            .flat_map(|line| {
                if line.is_empty() {
                    vec![String::new()]
                } else {
                    wrap_text(line, content.width.max(1) as usize)
                }
            })
            .collect::<Vec<_>>();
        let start = app.sync_browser_viewport(content, lines.clone());
        for (offset, line) in lines
            .iter()
            .skip(start)
            .take(content.height as usize)
            .enumerate()
        {
            frame.render_widget(
                Paragraph::new(Line::styled(
                    line.clone(),
                    Style::default().fg(if line.is_empty() {
                        theme.gray_dim
                    } else {
                        theme.text_secondary
                    }),
                )),
                Rect::new(content.x, content.y + offset as u16, content.width, 1),
            );
        }
        render_browser_text_selection(frame.buffer_mut(), content, app, start);
    } else if state.items.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "Nothing is available yet.",
                Style::default().fg(theme.gray),
            )),
            content,
        );
    } else {
        let shown = content.height as usize;
        let start = state.selected.saturating_add(1).saturating_sub(shown);
        for (offset, (index, item)) in state
            .items
            .iter()
            .enumerate()
            .skip(start)
            .take(shown)
            .enumerate()
        {
            let row = Rect::new(content.x, content.y + offset as u16, content.width, 1);
            let selected = index == state.selected;
            let background = if selected {
                theme.bg_visual
            } else {
                theme.bg_light
            };
            fill(frame.buffer_mut(), row, Style::default().bg(background));
            let available = row.width.saturating_sub(4) as usize;
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        if selected { "› " } else { "  " },
                        Style::default().fg(theme.fuzzy_accent).bg(background),
                    ),
                    Span::styled(
                        truncate(&item.title, available / 2),
                        Style::default()
                            .fg(theme.text_primary)
                            .bg(background)
                            .add_modifier(if selected {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                    ),
                    Span::styled(
                        format!("  {}", truncate(&item.description, available / 2)),
                        Style::default().fg(theme.gray).bg(background),
                    ),
                ])),
                row,
            );
            zones.browser_rows.push((row, index));
        }
    }

    let footer_text = match state.detail.as_ref().map(|detail| &detail.action) {
        Some(BrowserAction::InsertSkill(_)) => "enter insert skill   ↑↓ scroll   esc back",
        Some(BrowserAction::SelectRelease(_)) => "enter select version   ↑↓ scroll   esc back",
        Some(BrowserAction::None) => "↑↓ scroll   esc back",
        None => "enter open   ↑↓ navigate   esc close",
    };
    frame.render_widget(
        Paragraph::new(Line::styled(footer_text, Style::default().fg(theme.gray))),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );
}

fn render_browser_text_selection(buffer: &mut Buffer, area: Rect, app: &App, start: usize) {
    let selection_style = Style::default().add_modifier(Modifier::REVERSED);
    for viewport_row in 0..area.height as usize {
        let Some((selection_start, selection_end)) =
            app.browser_selection_display_range(start + viewport_row)
        else {
            continue;
        };
        let first = selection_start.min(area.width as usize);
        let last = selection_end.min(area.width as usize);
        for column in first..last {
            if let Some(cell) =
                buffer.cell_mut((area.x + column as u16, area.y + viewport_row as u16))
            {
                cell.set_style(selection_style);
            }
        }
    }
}

fn relative_time(timestamp: i64) -> String {
    let seconds = (Utc::now().timestamp_millis() - timestamp).max(0) / 1_000;
    match seconds {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{}m ago", seconds / 60),
        3_600..=86_399 => format!("{}h ago", seconds / 3_600),
        86_400..=604_799 => format!("{}d ago", seconds / 86_400),
        _ => absolute_time(timestamp),
    }
}

fn absolute_time(timestamp: i64) -> String {
    Utc.timestamp_millis_opt(timestamp)
        .single()
        .map(|value| value.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "Unknown".to_string())
}

fn render_home(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme, zones: &mut HitZones) {
    let width = area.width.saturating_sub(4).clamp(56, 104);
    let height = 14u16.min(area.height.saturating_sub(1)).max(10);
    let [_, vertical, _] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(height),
        Constraint::Min(0),
    ])
    .flex(Flex::Center)
    .areas(area);
    let [_, card, _] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(width.min(area.width)),
        Constraint::Min(0),
    ])
    .flex(Flex::Center)
    .areas(vertical);

    frame.render_widget(Clear, card);
    frame.render_widget(
        Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.prompt_border))
            .style(Style::default().bg(theme.bg_base)),
        card,
    );

    let inner = card.inner(Margin {
        horizontal: 3,
        vertical: 1,
    });
    let [heading, subtitle, cta, _, menu] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(HOME_MENU.len() as u16),
    ])
    .areas(inner);

    let version = format!("v{VERSION}");
    let title_gap = inner
        .width
        .saturating_sub(PRODUCT.width() as u16 + version.width() as u16);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                PRODUCT,
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ".repeat(title_gap as usize), theme.base()),
            Span::styled(version, Style::default().fg(theme.gray)),
        ])),
        heading,
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            "India's AI-native CLI",
            Style::default().fg(theme.text_secondary),
        )),
        subtitle,
    );

    let cta_text = format!("[{ALPHA_LABEL}]");
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                cta_text.clone(),
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Ctrl+U", Style::default().fg(theme.gray_dim)),
        ])),
        cta,
    );
    zones.alpha = Some(Rect::new(cta.x, cta.y, cta_text.width() as u16, 1));
    zones.menu = render_menu(frame, menu, app, theme);
}

fn render_menu(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &Theme,
) -> Vec<(Rect, crate::app::MenuAction)> {
    let row_width = 44u16.min(area.width);
    let [_, rows_area, _] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(row_width),
        Constraint::Min(0),
    ])
    .flex(Flex::Center)
    .areas(area);
    let mut hit_rows = Vec::with_capacity(HOME_MENU.len());

    for (index, item) in HOME_MENU.iter().enumerate() {
        let row = Rect::new(rows_area.x, rows_area.y + index as u16, rows_area.width, 1);
        let selected = index == app.selected_menu;
        let bg = if selected {
            theme.bg_visual
        } else {
            theme.bg_base
        };
        fill(frame.buffer_mut(), row, Style::default().bg(bg));
        let key = format!("{} ", item.key);
        let gap = row
            .width
            .saturating_sub(item.label.width() as u16 + key.width() as u16);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    item.label,
                    Style::default()
                        .fg(theme.text_primary)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" ".repeat(gap as usize), Style::default().bg(bg)),
                Span::styled(key, Style::default().fg(theme.gray_bright).bg(bg)),
            ])),
            row,
        );
        hit_rows.push((row, item.action));
    }
    hit_rows
}

#[derive(Clone)]
struct TranscriptRow {
    line: Line<'static>,
    background: Color,
    fold_entry: Option<usize>,
}

fn render_transcript(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &Theme,
    zones: &mut HitZones,
) {
    fill(frame.buffer_mut(), area, theme.base());
    let mut rows = Vec::new();
    let width = area.width.max(1) as usize;
    app.sync_transcript_timestamps();
    for (index, entry) in app.transcript.iter().enumerate() {
        if let Some(timestamp) = app.transcript_timestamp(index) {
            rows.push(TranscriptRow {
                line: Line::styled(timestamp, Style::default().fg(theme.gray_dim)),
                background: theme.bg_base,
                fold_entry: None,
            });
        }
        build_transcript_rows(&mut rows, index, entry, width, theme);
    }
    let visible = area.height as usize;
    let row_text = rows.iter().map(transcript_row_text).collect();
    let start = app.sync_transcript_viewport(area, row_text);
    for (offset, row) in rows.iter().skip(start).take(visible).enumerate() {
        let target = Rect::new(area.x, area.y + offset as u16, area.width, 1);
        fill(
            frame.buffer_mut(),
            target,
            Style::default().bg(row.background),
        );
        frame.render_widget(Paragraph::new(row.line.clone()), target);
        if let Some(index) = row.fold_entry {
            zones.fold_rows.push((target, index));
        }
    }
    render_text_selection(frame.buffer_mut(), area, app, start);
    render_transcript_scrollbar(frame.buffer_mut(), area, app, theme);
}

fn transcript_row_text(row: &TranscriptRow) -> String {
    row.line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn render_text_selection(buffer: &mut Buffer, area: Rect, app: &App, start: usize) {
    let selection_style = Style::default().add_modifier(Modifier::REVERSED);
    for viewport_row in 0..area.height as usize {
        let Some((selection_start, selection_end)) =
            app.selection_display_range(start + viewport_row)
        else {
            continue;
        };
        let first = selection_start.min(area.width as usize);
        let last = selection_end.min(area.width as usize);
        for column in first..last {
            if let Some(cell) =
                buffer.cell_mut((area.x + column as u16, area.y + viewport_row as u16))
            {
                cell.set_style(selection_style);
            }
        }
    }
}

fn render_transcript_scrollbar(buffer: &mut Buffer, area: Rect, app: &App, theme: &Theme) {
    let (position, visible, total) = app.transcript_scroll_metrics();
    if area.width == 0 || area.height == 0 || total <= visible {
        return;
    }
    let track = area.height as usize;
    let thumb_size = (visible.saturating_mul(track) / total).clamp(1, track);
    let maximum = total.saturating_sub(visible);
    let thumb_top = position.saturating_mul(track.saturating_sub(thumb_size)) / maximum.max(1);
    let x = area.right().saturating_sub(1);
    for offset in 0..track {
        if let Some(cell) = buffer.cell_mut((x, area.y + offset as u16)) {
            let in_thumb = offset >= thumb_top && offset < thumb_top + thumb_size;
            cell.set_char('▐');
            cell.set_style(Style::default().fg(if in_thumb {
                theme.scrollbar_fg
            } else {
                theme.scrollbar_bg
            }));
        }
    }
}

fn build_transcript_rows(
    rows: &mut Vec<TranscriptRow>,
    entry_index: usize,
    entry: &TranscriptEntry,
    width: usize,
    theme: &Theme,
) {
    match entry {
        TranscriptEntry::User { text, slash_tokens } => {
            if !rows.is_empty() {
                rows.push(blank_row(theme.bg_base));
            }
            rows.push(blank_row(theme.bg_light));
            let content_width = width.saturating_sub(4).max(1);
            let wrapped = wrap_text(text, content_width);
            for (line_index, segment) in wrapped.iter().enumerate() {
                let prefix = if line_index == 0 { "❯ " } else { "  " };
                let mut spans = vec![Span::styled(
                    prefix.to_string(),
                    Style::default().fg(theme.accent_user).bg(theme.bg_light),
                )];
                if slash_tokens.is_empty() || line_index > 0 {
                    spans.push(Span::styled(
                        segment.clone(),
                        Style::default().fg(theme.text_primary).bg(theme.bg_light),
                    ));
                } else {
                    spans.extend(highlight_slash_tokens(
                        segment,
                        slash_tokens,
                        theme.bg_light,
                        theme,
                    ));
                }
                rows.push(TranscriptRow {
                    line: Line::from(spans),
                    background: theme.bg_light,
                    fold_entry: None,
                });
            }
            rows.push(blank_row(theme.bg_light));
        }
        TranscriptEntry::Thinking {
            text,
            running,
            elapsed_ms,
            expanded,
            ..
        } => {
            let label = if *running {
                "Thinking…".to_string()
            } else if let Some(milliseconds) = elapsed_ms {
                format!("Thought for {}", format_milliseconds(*milliseconds))
            } else {
                "Thought".to_string()
            };
            let bullet = if *running {
                theme.accent_thinking
            } else {
                theme.gray
            };
            let mut spans = vec![
                Span::styled("● ", Style::default().fg(bullet)),
                Span::styled(
                    label,
                    Style::default().fg(theme.gray).add_modifier(Modifier::BOLD),
                ),
            ];
            if !*expanded && !*running {
                spans.push(Span::styled(
                    "  (ctrl+e to expand)",
                    Style::default().fg(theme.gray_dim),
                ));
            }
            rows.push(TranscriptRow {
                line: Line::from(spans),
                background: theme.bg_base,
                fold_entry: Some(entry_index),
            });
            if *expanded && !text.is_empty() {
                rows.push(foldable_blank_row(theme.bg_base, entry_index));
                for segment in wrap_text(text, width.saturating_sub(2).max(1)) {
                    rows.push(TranscriptRow {
                        line: Line::from(vec![
                            Span::raw("  "),
                            Span::styled(
                                segment,
                                Style::default()
                                    .fg(theme.text_secondary)
                                    .add_modifier(Modifier::DIM | Modifier::ITALIC),
                            ),
                        ]),
                        background: theme.bg_base,
                        fold_entry: Some(entry_index),
                    });
                }
            }
        }
        TranscriptEntry::Assistant { text, .. } => {
            for line in markdown_lines(text, width.saturating_sub(2).max(1), theme) {
                let mut spans = vec![Span::raw("  ")];
                spans.extend(line.spans);
                rows.push(TranscriptRow {
                    line: Line::from(spans),
                    background: theme.bg_base,
                    fold_entry: None,
                });
            }
        }
        TranscriptEntry::Tool {
            name,
            description,
            input,
            output,
            state,
            expanded,
            diffs,
            ..
        } => {
            let accent = match state {
                ToolVisualState::Running => theme.accent_thinking,
                ToolVisualState::Succeeded => theme.accent_success,
                ToolVisualState::Failed(_) => theme.accent_error,
            };
            let (insertions, deletions) = diff_counts(diffs);
            let mut header = vec![Span::styled("● ", Style::default().fg(accent))];
            if diffs.is_empty() {
                let title = run_title(name, description, input);
                header.push(Span::styled(
                    "Run ",
                    Style::default()
                        .fg(theme.text_primary)
                        .add_modifier(Modifier::BOLD),
                ));
                header.push(Span::styled(title, Style::default().fg(theme.text_primary)));
            } else {
                let target = if diffs.len() == 1 {
                    diffs[0].path.clone()
                } else {
                    format!("{} files", diffs.len())
                };
                header.push(Span::styled(
                    "Edit ",
                    Style::default()
                        .fg(theme.text_primary)
                        .add_modifier(Modifier::BOLD),
                ));
                header.push(Span::styled(
                    target,
                    Style::default().fg(theme.accent_skill),
                ));
                header.push(Span::styled(
                    format!(" +{insertions}"),
                    Style::default().fg(theme.diff_insert_fg),
                ));
                header.push(Span::styled("/", Style::default().fg(theme.gray_dim)));
                header.push(Span::styled(
                    format!("-{deletions}"),
                    Style::default().fg(theme.diff_delete_fg),
                ));
            }
            rows.push(TranscriptRow {
                line: Line::from(header),
                background: theme.bg_base,
                fold_entry: Some(entry_index),
            });
            if *expanded {
                if !input.trim().is_empty() && diffs.is_empty() {
                    for (line_index, segment) in wrap_text(input, width.saturating_sub(4).max(1))
                        .into_iter()
                        .enumerate()
                    {
                        let prefix = if line_index == 0 { "  $ " } else { "    " };
                        rows.push(TranscriptRow {
                            line: Line::from(vec![
                                Span::styled(prefix, Style::default().fg(theme.gray_dim)),
                                Span::styled(segment, Style::default().fg(theme.command)),
                            ]),
                            background: theme.bg_base,
                            fold_entry: Some(entry_index),
                        });
                    }
                }
                if !output.trim().is_empty() {
                    for segment in wrap_text(output, width.saturating_sub(4).max(1)) {
                        rows.push(TranscriptRow {
                            line: Line::from(vec![
                                Span::raw("    "),
                                Span::styled(segment, Style::default().fg(theme.text_secondary)),
                            ]),
                            background: theme.bg_base,
                            fold_entry: Some(entry_index),
                        });
                    }
                }
                if let ToolVisualState::Failed(message) = state {
                    for segment in wrap_text(message, width.saturating_sub(4).max(1)) {
                        rows.push(TranscriptRow {
                            line: Line::from(vec![
                                Span::raw("    "),
                                Span::styled(segment, Style::default().fg(theme.accent_error)),
                            ]),
                            background: theme.bg_base,
                            fold_entry: Some(entry_index),
                        });
                    }
                }
                render_file_diffs(rows, diffs, width, entry_index, theme);
            }
        }
        TranscriptEntry::Btw {
            question,
            answer,
            expanded,
        } => {
            rows.push(TranscriptRow {
                line: Line::from(vec![
                    Span::styled(
                        "◇ /btw ",
                        Style::default()
                            .fg(theme.accent_user)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        truncate(question, width.saturating_sub(20)),
                        Style::default().fg(theme.text_secondary),
                    ),
                    Span::styled(
                        if *expanded {
                            "  (ctrl+e to collapse)"
                        } else {
                            "  (ctrl+e to expand)"
                        },
                        Style::default().fg(theme.gray_dim),
                    ),
                ]),
                background: theme.bg_base,
                fold_entry: Some(entry_index),
            });
            if *expanded {
                for line in markdown_lines(answer, width.saturating_sub(2).max(1), theme) {
                    let mut spans = vec![Span::raw("  ")];
                    spans.extend(line.spans);
                    rows.push(TranscriptRow {
                        line: Line::from(spans),
                        background: theme.bg_base,
                        fold_entry: Some(entry_index),
                    });
                }
            }
        }
        TranscriptEntry::Usage(card) => render_usage_card(rows, card, width, theme),
        TranscriptEntry::Event(text) => {
            if !rows.is_empty() {
                rows.push(blank_row(theme.bg_base));
            }
            rows.push(TranscriptRow {
                line: Line::from(vec![
                    Span::styled("● ", Style::default().fg(theme.gray_dim)),
                    Span::styled(text.clone(), Style::default().fg(theme.gray)),
                ]),
                background: theme.bg_base,
                fold_entry: None,
            });
        }
    }
}

fn render_usage_card(rows: &mut Vec<TranscriptRow>, card: &UsageCard, width: usize, theme: &Theme) {
    if !rows.is_empty() {
        rows.push(blank_row(theme.bg_base));
    }
    let card_width = width.clamp(32, 96);
    let horizontal = "─".repeat(card_width.saturating_sub(2));
    rows.push(usage_row(
        Line::from(vec![
            Span::styled("┌─ ", Style::default().fg(theme.gray_dim)),
            Span::styled(
                "Indus",
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {}┐", "─".repeat(card_width.saturating_sub(10))),
                Style::default().fg(theme.gray_dim),
            ),
        ]),
        theme,
    ));
    let fields = [
        ("Model", card.model.clone()),
        ("Directory", card.directory.clone()),
        ("Permissions", card.permissions.clone()),
        ("Session", card.session.clone()),
    ];
    for (label, value) in fields {
        rows.push(usage_field_row(label, &value, card_width, theme));
    }
    let context = match card.context_window.filter(|window| *window > 0) {
        Some(window) => {
            let used = card.context_used.min(window);
            let percent_left = 100u64.saturating_sub(used.saturating_mul(100) / window);
            format!(
                "{}  {percent_left}% left  {} used / {}",
                context_progress_bar(percent_left),
                format_tokens(used),
                format_tokens(window)
            )
        }
        None => "[░░░░░░░░░░░░░░░░░░░░]  Unknown".to_string(),
    };
    rows.push(usage_field_row(
        "Context Window",
        &context,
        card_width,
        theme,
    ));
    rows.push(usage_row(
        Line::styled(
            format!("└{horizontal}┘"),
            Style::default().fg(theme.gray_dim),
        ),
        theme,
    ));
}

fn usage_field_row(label: &str, value: &str, width: usize, theme: &Theme) -> TranscriptRow {
    let prefix = format!("│  {label:<16} ");
    let available = width.saturating_sub(prefix.width() + 2);
    let value = truncate(value, available);
    let gap = available.saturating_sub(value.width());
    usage_row(
        Line::from(vec![
            Span::styled("│  ", Style::default().fg(theme.gray_dim)),
            Span::styled(
                format!("{label:<16} "),
                Style::default().fg(theme.text_secondary),
            ),
            Span::styled(value, Style::default().fg(theme.text_primary)),
            Span::raw(" ".repeat(gap)),
            Span::styled("│", Style::default().fg(theme.gray_dim)),
        ]),
        theme,
    )
}

fn usage_row(line: Line<'static>, theme: &Theme) -> TranscriptRow {
    TranscriptRow {
        line,
        background: theme.bg_base,
        fold_entry: None,
    }
}

fn context_progress_bar(percent_left: u64) -> String {
    let filled = ((percent_left.min(100) * 20 + 50) / 100) as usize;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(20 - filled))
}

fn format_tokens(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}K", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn blank_row(background: Color) -> TranscriptRow {
    TranscriptRow {
        line: Line::default(),
        background,
        fold_entry: None,
    }
}

fn foldable_blank_row(background: Color, entry_index: usize) -> TranscriptRow {
    TranscriptRow {
        line: Line::default(),
        background,
        fold_entry: Some(entry_index),
    }
}

fn run_title(name: &str, description: &str, input: &str) -> String {
    let description = description.trim().replace('\n', " ");
    let lower = description.to_ascii_lowercase();
    let description = if let Some(rest) = lower.strip_prefix("running ") {
        description[description.len() - rest.len()..].trim()
    } else if let Some(rest) = lower.strip_prefix("run ") {
        description[description.len() - rest.len()..].trim()
    } else {
        description.as_str()
    };
    if !description.is_empty() {
        description.to_string()
    } else if !input.trim().is_empty() {
        input.trim().replace('\n', " ")
    } else if !name.trim().is_empty() {
        name.to_string()
    } else {
        "…".to_string()
    }
}

fn diff_counts(diffs: &[FileDiff]) -> (usize, usize) {
    diffs
        .iter()
        .flat_map(|diff| &diff.lines)
        .fold((0, 0), |(added, removed), line| match line.kind {
            DiffKind::Added => (added + 1, removed),
            DiffKind::Removed => (added, removed + 1),
            DiffKind::Context => (added, removed),
        })
}

fn render_file_diffs(
    rows: &mut Vec<TranscriptRow>,
    diffs: &[FileDiff],
    width: usize,
    entry_index: usize,
    theme: &Theme,
) {
    for diff in diffs {
        rows.push(foldable_blank_row(theme.bg_base, entry_index));
        rows.push(TranscriptRow {
            line: Line::from(vec![
                Span::styled("  Edit ", Style::default().fg(theme.gray)),
                Span::styled(
                    diff.path.clone(),
                    Style::default()
                        .fg(theme.accent_skill)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            background: theme.bg_base,
            fold_entry: Some(entry_index),
        });

        let line_width = diff
            .lines
            .iter()
            .flat_map(|line| [line.old_line, line.new_line])
            .flatten()
            .max()
            .unwrap_or(0)
            .to_string()
            .len()
            .max(1);
        let gutter_width = 2 + line_width * 2 + 3;
        let content_width = width.saturating_sub(gutter_width).max(1);
        for line in &diff.lines {
            let old = line.old_line.map_or_else(
                || " ".repeat(line_width),
                |value| format!("{value:>line_width$}"),
            );
            let new = line.new_line.map_or_else(
                || " ".repeat(line_width),
                |value| format!("{value:>line_width$}"),
            );
            let (foreground, background) = match line.kind {
                DiffKind::Context => (theme.diff_equal_fg, theme.bg_base),
                DiffKind::Added => (theme.diff_insert_fg, theme.diff_insert_bg),
                DiffKind::Removed => (theme.diff_delete_fg, theme.diff_delete_bg),
            };
            rows.push(TranscriptRow {
                line: Line::from(vec![
                    Span::raw("  "),
                    Span::styled(old, Style::default().fg(theme.diff_gutter_fg)),
                    Span::raw(" "),
                    Span::styled(new, Style::default().fg(theme.diff_gutter_fg)),
                    Span::raw("  "),
                    Span::styled(
                        truncate(line.text.trim_end_matches(['\r', '\n']), content_width),
                        Style::default().fg(foreground).bg(background),
                    ),
                ]),
                background,
                fold_entry: Some(entry_index),
            });
        }
    }
}

fn highlight_slash_tokens(
    line: &str,
    ranges: &[std::ops::Range<usize>],
    background: Color,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut cursor = 0;
    for range in ranges {
        if range.start >= line.len() || range.end > line.len() || range.start < cursor {
            continue;
        }
        if range.start > cursor {
            spans.push(Span::styled(
                line[cursor..range.start].to_string(),
                Style::default().fg(theme.text_primary).bg(background),
            ));
        }
        spans.push(Span::styled(
            line[range.clone()].to_string(),
            Style::default().fg(theme.accent_skill).bg(background),
        ));
        cursor = range.end;
    }
    if cursor < line.len() {
        spans.push(Span::styled(
            line[cursor..].to_string(),
            Style::default().fg(theme.text_primary).bg(background),
        ));
    }
    spans
}

fn render_turn_status(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let Some(turn) = &app.turn else {
        return;
    };
    fill(frame.buffer_mut(), area, theme.base());
    if let Some(permission) = &app.permission {
        let description = truncate(
            &permission.description,
            area.width.saturating_sub(2) as usize,
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("◆ ", Style::default().fg(theme.warning)),
                Span::styled(
                    description,
                    Style::default()
                        .fg(theme.text_primary)
                        .add_modifier(Modifier::BOLD),
                ),
            ])),
            Rect::new(area.x, area.y, area.width, 1),
        );
        let pattern = permission.patterns.join(", ");
        frame.render_widget(
            Paragraph::new(Line::styled(
                truncate(&pattern, area.width.saturating_sub(2) as usize),
                Style::default().fg(theme.gray),
            )),
            Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(2), 1),
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("[y] once", Style::default().fg(theme.accent_success)),
                Span::styled("  [a] always", Style::default().fg(theme.accent_skill)),
                Span::styled("  [n] reject", Style::default().fg(theme.accent_error)),
            ])),
            Rect::new(area.x + 2, area.y + 2, area.width.saturating_sub(2), 1),
        );
        return;
    }
    let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let spinner = frames[(app.animation_tick as usize / 2) % frames.len()];
    let activity_color = match turn.activity {
        TurnActivity::RunningTool(_) | TurnActivity::RunningJob(_) => theme.accent_success,
        TurnActivity::Retrying(_) => theme.warning,
        TurnActivity::Cancelling => theme.accent_error,
        TurnActivity::WaitingForPermission => theme.warning,
        TurnActivity::Compacting
        | TurnActivity::Thinking
        | TurnActivity::Responding
        | TurnActivity::WaitingForResponse => theme.text_secondary,
    };
    let label = turn.activity.label();
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{spinner} "), Style::default().fg(activity_color)),
            Span::styled(label, Style::default().fg(activity_color)),
        ])),
        area,
    );
    let stop = "[stop]";
    if area.width > stop.width() as u16 + 4 {
        frame.render_widget(
            Paragraph::new(Line::styled(stop, Style::default().fg(theme.gray))),
            Rect::new(
                area.right().saturating_sub(stop.width() as u16),
                area.y,
                stop.width() as u16,
                1,
            ),
        );
    }
}

fn render_composer(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    fill(frame.buffer_mut(), area, theme.base());
    let border = theme.prompt_border_active;
    draw_horizontal_border(
        frame.buffer_mut(),
        area.x,
        area.right(),
        area.y,
        '╭',
        '╮',
        border,
        theme.bg_base,
    );
    let bottom = area.bottom() - 1;
    draw_horizontal_border(
        frame.buffer_mut(),
        area.x,
        area.right(),
        bottom,
        '╰',
        '╯',
        border,
        theme.bg_base,
    );
    for y in area.y + 1..bottom {
        set_cell(frame.buffer_mut(), area.x, y, '│', border, theme.bg_base);
        set_cell(
            frame.buffer_mut(),
            area.right() - 1,
            y,
            '│',
            border,
            theme.bg_base,
        );
    }

    let content = Rect::new(
        area.x + 3,
        area.y + 1,
        area.width.saturating_sub(6),
        area.height.saturating_sub(2),
    );
    frame.render_widget(
        Paragraph::new(Line::styled("❯ ", Style::default().fg(theme.accent_user))),
        Rect::new(content.x, content.y, 2, 1),
    );

    let text_area = Rect::new(
        content.x + 2,
        content.y,
        content.width.saturating_sub(2),
        content.height,
    );
    let layout = layout_composer(app.composer.text(), app.composer.cursor(), text_area.width);
    if app.composer.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "Build anything",
                Style::default().fg(theme.gray_dim),
            )),
            text_area,
        );
    } else {
        for (row, text) in layout.lines.iter().enumerate() {
            if row >= text_area.height as usize {
                break;
            }
            frame.render_widget(
                Paragraph::new(Line::styled(
                    text.clone(),
                    Style::default().fg(theme.text_primary),
                )),
                Rect::new(text_area.x, text_area.y + row as u16, text_area.width, 1),
            );
        }
        paint_command_token(frame.buffer_mut(), text_area, app, theme, &layout);
    }

    if app.slash.argument_placeholder.is_some()
        && app
            .slash
            .argument_range
            .as_ref()
            .is_some_and(|range| range.start == range.end)
        && let Some(placeholder) = app.slash.argument_placeholder
    {
        let x = text_area.x + layout.cursor_column;
        if x < text_area.right() {
            frame.render_widget(
                Paragraph::new(Line::styled(placeholder, Style::default().fg(theme.gray))),
                Rect::new(
                    x,
                    text_area.y + layout.cursor_row,
                    text_area.right().saturating_sub(x),
                    1,
                ),
            );
        }
    }

    let multiline_hint = if app.multiline_mode {
        " · multiline · Ctrl+Enter"
    } else {
        ""
    };
    let vim_hint = if app.vim_mode {
        if app.vim_insert_mode {
            " · VIM INSERT"
        } else {
            " · VIM NORMAL"
        }
    } else {
        ""
    };
    let input_hint = format!("{multiline_hint}{vim_hint}");
    let info = app.active_model().map_or_else(
        || format!(" indus · {}{} ", app.theme_kind.name(), input_hint),
        |active| {
            format!(
                " {} · {}{} ",
                active.model_name,
                active.provider.name(),
                input_hint
            )
        },
    );
    if info.width() + 4 < area.width as usize {
        frame.render_widget(
            Paragraph::new(Line::styled(
                info.clone(),
                Style::default().fg(theme.gray).bg(theme.bg_base),
            )),
            Rect::new(area.x + 2, bottom, info.width() as u16, 1),
        );
    }

    let cursor_x = text_area.x + layout.cursor_column;
    let cursor_y = text_area.y + layout.cursor_row;
    if cursor_x < text_area.right() && cursor_y < text_area.bottom() {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn render_catalog_modal(
    frame: &mut Frame<'_>,
    prompt: Rect,
    app: &App,
    theme: &Theme,
    zones: &mut HitZones,
) {
    match app.catalog_modal.as_ref() {
        Some(CatalogModal::Providers { selected }) => {
            render_provider_catalog(frame, prompt, *selected, theme, zones)
        }
        Some(CatalogModal::Models(view)) => {
            render_model_catalog(frame, prompt, view, app.animation_tick, theme, zones)
        }
        Some(CatalogModal::ApiKey {
            provider,
            input,
            error,
        }) => render_api_key_popover(
            frame,
            frame.area(),
            *provider,
            input,
            error.as_deref(),
            theme,
        ),
        None => {}
    }
}

fn render_provider_catalog(
    frame: &mut Frame<'_>,
    prompt: Rect,
    selected: usize,
    theme: &Theme,
    zones: &mut HitZones,
) {
    let Some(panel) = render_slash_panel(
        frame,
        prompt,
        ProviderId::ALL.len(),
        ProviderId::ALL.len(),
        theme,
    ) else {
        return;
    };
    for (index, provider) in ProviderId::ALL.iter().enumerate() {
        let row = render_catalog_row(
            frame,
            panel,
            index,
            index == selected,
            provider.name(),
            "Compatible Interim Provider",
            theme,
        );
        zones.catalog_rows.push((row, index));
    }
}

fn render_model_catalog(
    frame: &mut Frame<'_>,
    prompt: Rect,
    view: &ModelCatalogView,
    animation_tick: u64,
    theme: &Theme,
    zones: &mut HitZones,
) {
    let status = if view.models.is_empty() {
        if view.loading {
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let spinner = frames[(animation_tick as usize / 2) % frames.len()];
            Some((
                format!("{spinner} Fetching {} models…", view.provider.name()),
                view.provider.base_url().to_string(),
                theme.text_secondary,
            ))
        } else if let Some(error) = &view.error {
            Some((
                "Model discovery failed".to_string(),
                error.clone(),
                theme.accent_error,
            ))
        } else {
            Some((
                "No compatible models".to_string(),
                view.provider.base_url().to_string(),
                theme.gray,
            ))
        }
    } else {
        None
    };
    let shown = if status.is_some() {
        1
    } else {
        view.models.len().min(MAX_SLASH_ROWS)
    };
    let Some(panel) = render_slash_panel(frame, prompt, view.models.len(), shown, theme) else {
        return;
    };

    if let Some((label, description, color)) = status {
        let row = Rect::new(panel.x + 1, panel.y + 1, panel.width.saturating_sub(1), 1);
        fill(frame.buffer_mut(), row, Style::default().bg(theme.bg_light));
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  ", Style::default().bg(theme.bg_light)),
                Span::styled(label, Style::default().fg(color).bg(theme.bg_light)),
                Span::styled("  ", Style::default().bg(theme.bg_light)),
                Span::styled(
                    truncate(&description, row.width.saturating_sub(28) as usize),
                    Style::default().fg(theme.gray).bg(theme.bg_light),
                ),
            ])),
            row,
        );
        return;
    }

    let start = view
        .selected
        .saturating_add(1)
        .saturating_sub(MAX_SLASH_ROWS);
    for (offset, (index, model)) in view
        .models
        .iter()
        .enumerate()
        .skip(start)
        .take(MAX_SLASH_ROWS)
        .enumerate()
    {
        let context = model
            .context_window
            .map(format_context_window)
            .unwrap_or_default();
        let description = if model.name == model.id {
            context
        } else if context.is_empty() {
            model.id.clone()
        } else {
            format!("{} · {context}", model.id)
        };
        let row = render_catalog_row(
            frame,
            panel,
            offset,
            index == view.selected,
            &model.name,
            &description,
            theme,
        );
        zones.catalog_rows.push((row, index));
    }
}

fn render_api_key_popover(
    frame: &mut Frame<'_>,
    area: Rect,
    _provider: ProviderId,
    input: &crate::app::Composer,
    error: Option<&str>,
    theme: &Theme,
) {
    let width = ((area.width as u32 * 76) / 100) as u16;
    let width = width.clamp(40, 90).min(area.width);
    let height = 9u16.min(area.height).max(7.min(area.height));
    let panel = centered_rect(area, width, height);
    render_popover_surface(frame, panel, theme);
    render_popover_header(frame, panel, "API key", theme);

    let input_area = Rect::new(panel.x + 3, panel.y + 3, panel.width.saturating_sub(6), 1);
    let masked = "•".repeat(input.text().chars().count());
    let value = if masked.is_empty() {
        "API key".to_string()
    } else {
        masked
    };
    let color = if input.is_empty() {
        theme.gray
    } else {
        theme.text_primary
    };
    frame.render_widget(
        Paragraph::new(Line::styled(value, Style::default().fg(color))),
        input_area,
    );
    if let Some(error) = error {
        frame.render_widget(
            Paragraph::new(Line::styled(
                truncate(error, input_area.width as usize),
                Style::default().fg(theme.accent_error),
            )),
            Rect::new(input_area.x, input_area.y + 2, input_area.width, 1),
        );
    }
    render_popover_footer(frame, panel, "enter submit", theme);

    let cursor_column = input.text()[..input.cursor()].chars().count() as u16;
    let cursor_x = input_area.x + cursor_column.min(input_area.width.saturating_sub(1));
    frame.set_cursor_position((cursor_x, input_area.y));
}

fn render_delete_confirmation(
    frame: &mut Frame<'_>,
    area: Rect,
    confirmation: &crate::app::DeleteConfirmation,
    theme: &Theme,
) {
    let width = 64u16.min(area.width).max(36.min(area.width));
    let height = 10u16.min(area.height).max(8.min(area.height));
    let panel = centered_rect(area, width, height);
    render_popover_surface(frame, panel, theme);
    render_popover_header(frame, panel, "Delete session?", theme);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                truncate(&confirmation.title, panel.width.saturating_sub(6) as usize),
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                truncate(
                    &confirmation.session_id,
                    panel.width.saturating_sub(6) as usize,
                ),
                Style::default().fg(theme.gray),
            ),
            Line::default(),
            Line::styled(
                "This permanently removes the saved conversation history.",
                Style::default().fg(theme.accent_error),
            ),
        ]),
        Rect::new(
            panel.x + 3,
            panel.y + 3,
            panel.width.saturating_sub(6),
            panel.height.saturating_sub(5),
        ),
    );
    render_popover_footer(frame, panel, "y/enter delete   n/esc cancel", theme);
}

fn render_popover_surface(frame: &mut Frame<'_>, panel: Rect, theme: &Theme) {
    frame.render_widget(Clear, panel);
    fill(
        frame.buffer_mut(),
        panel,
        Style::default().fg(theme.text_primary).bg(theme.bg_light),
    );
}

fn render_popover_header(frame: &mut Frame<'_>, panel: Rect, title: &str, theme: &Theme) {
    let header = Rect::new(panel.x + 3, panel.y + 1, panel.width.saturating_sub(6), 1);
    frame.render_widget(
        Paragraph::new(Line::styled(
            title.to_string(),
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        )),
        header,
    );
    let escape = "esc";
    frame.render_widget(
        Paragraph::new(Line::styled(escape, Style::default().fg(theme.gray))),
        Rect::new(
            panel.right().saturating_sub(escape.width() as u16 + 3),
            header.y,
            escape.width() as u16,
            1,
        ),
    );
}

fn render_popover_footer(frame: &mut Frame<'_>, panel: Rect, text: &str, theme: &Theme) {
    let mut spans = Vec::new();
    for (index, token) in text.split_whitespace().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" "));
        }
        let is_key = matches!(
            token,
            "enter" | "esc" | "y/enter" | "n/esc" | "↑↓" | "r" | "k"
        );
        spans.push(Span::styled(
            token.to_string(),
            Style::default().fg(if is_key {
                theme.text_primary
            } else {
                theme.gray
            }),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect::new(
            panel.x + 3,
            panel.bottom().saturating_sub(2),
            panel.width.saturating_sub(6),
            1,
        ),
    );
}

fn blend_color(background: Color, foreground: Color, opacity: f32) -> Color {
    let Some((background_red, background_green, background_blue)) = color_rgb(background) else {
        return foreground;
    };
    let Some((foreground_red, foreground_green, foreground_blue)) = color_rgb(foreground) else {
        return foreground;
    };
    let blend = |background: u8, foreground: u8| {
        (f32::from(background)
            + (f32::from(foreground) - f32::from(background)) * opacity.clamp(0.0, 1.0))
        .round() as u8
    };
    Color::Rgb(
        blend(background_red, foreground_red),
        blend(background_green, foreground_green),
        blend(background_blue, foreground_blue),
    )
}

fn color_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Black => Some((0, 0, 0)),
        Color::White => Some((255, 255, 255)),
        Color::Gray => Some((128, 128, 128)),
        Color::DarkGray => Some((64, 64, 64)),
        Color::Rgb(red, green, blue) => Some((red, green, blue)),
        _ => None,
    }
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn format_context_window(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{}M ctx", tokens / 1_000_000)
    } else if tokens >= 1_000 {
        format!("{}K ctx", tokens / 1_000)
    } else {
        format!("{tokens} ctx")
    }
}

fn paint_command_token(
    buffer: &mut Buffer,
    text_area: Rect,
    app: &App,
    theme: &Theme,
    layout: &ComposerLayout,
) {
    if !app.composer.text().starts_with('/') {
        return;
    }
    let end = app
        .composer
        .text()
        .find(char::is_whitespace)
        .unwrap_or(app.composer.text().len());
    for (byte, (row, column)) in &layout.positions {
        if *byte >= end {
            break;
        }
        if let Some(cell) = buffer.cell_mut((text_area.x + *column, text_area.y + *row)) {
            cell.set_fg(theme.accent_skill);
        }
    }
}

fn render_slash_dropdown(
    frame: &mut Frame<'_>,
    prompt: Rect,
    app: &App,
    theme: &Theme,
    zones: &mut HitZones,
) {
    let count = app.slash.suggestions.len();
    let shown = count.saturating_sub(app.slash_scroll).min(MAX_SLASH_ROWS);
    if shown == 0 {
        return;
    }
    let Some(panel) = render_slash_panel(frame, prompt, count, shown, theme) else {
        return;
    };

    let row_width = panel.width.saturating_sub(2) as usize;
    let label_width = (row_width * 3 / 5).min(40);
    for (visible, (index, suggestion)) in app
        .slash
        .suggestions
        .iter()
        .enumerate()
        .skip(app.slash_scroll)
        .take(shown)
        .enumerate()
    {
        let row = Rect::new(
            panel.x + 1,
            panel.y + 1 + visible as u16,
            panel.width - 1,
            1,
        );
        let selected = index == app.slash.selected;
        let bg = if selected {
            theme.bg_visual
        } else {
            theme.bg_light
        };
        fill(frame.buffer_mut(), row, Style::default().bg(bg));
        let prefix = if selected { "❯ " } else { "  " };
        let display = truncate(&suggestion.display, label_width);
        let padding = label_width.saturating_sub(display.width()) + 2;
        let description_width = row_width.saturating_sub(2 + label_width + 2);
        let description = truncate(&suggestion.description, description_width);
        let mut spans = vec![Span::styled(
            prefix.to_string(),
            Style::default().fg(theme.text_primary).bg(bg),
        )];
        spans.extend(highlighted_label(
            &display,
            &suggestion.matched_indices,
            selected,
            bg,
            theme,
        ));
        spans.push(Span::styled(" ".repeat(padding), Style::default().bg(bg)));
        spans.push(Span::styled(
            description,
            Style::default().fg(theme.gray).bg(bg),
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), row);
        zones.slash_rows.push((row, index));
    }
}

fn render_slash_panel(
    frame: &mut Frame<'_>,
    prompt: Rect,
    count: usize,
    shown: usize,
    theme: &Theme,
) -> Option<Rect> {
    if shown == 0 {
        return None;
    }
    let height = shown as u16 + 2;
    if prompt.y < height {
        return None;
    }
    let panel = Rect::new(prompt.x, prompt.y - height, prompt.width, height);
    frame.render_widget(Clear, panel);
    fill(
        frame.buffer_mut(),
        panel,
        Style::default().bg(theme.bg_light),
    );
    let border_style = Style::default().fg(theme.bg_visual).bg(theme.bg_base);
    frame.render_widget(
        Paragraph::new(Line::styled("─".repeat(panel.width as usize), border_style)),
        Rect::new(panel.x, panel.y, panel.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Line::styled("─".repeat(panel.width as usize), border_style)),
        Rect::new(panel.x, panel.bottom() - 1, panel.width, 1),
    );
    let count_text = count.to_string();
    frame.render_widget(
        Paragraph::new(Line::styled(
            count_text.clone(),
            Style::default().fg(theme.gray).bg(theme.bg_base),
        )),
        Rect::new(
            panel.right().saturating_sub(count_text.width() as u16 + 1),
            panel.y,
            count_text.width() as u16,
            1,
        ),
    );
    Some(panel)
}

fn render_catalog_row(
    frame: &mut Frame<'_>,
    panel: Rect,
    visible: usize,
    selected: bool,
    label: &str,
    description: &str,
    theme: &Theme,
) -> Rect {
    let row = Rect::new(
        panel.x + 1,
        panel.y + 1 + visible as u16,
        panel.width.saturating_sub(1),
        1,
    );
    let background = if selected {
        theme.bg_visual
    } else {
        theme.bg_light
    };
    fill(frame.buffer_mut(), row, Style::default().bg(background));
    let row_width = panel.width.saturating_sub(2) as usize;
    let label_width = (row_width * 3 / 5).min(40);
    let label = truncate(label, label_width);
    let padding = label_width.saturating_sub(label.width()) + 2;
    let description_width = row_width.saturating_sub(2 + label_width + 2);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                if selected { "❯ " } else { "  " },
                Style::default().fg(theme.text_primary).bg(background),
            ),
            Span::styled(
                label,
                Style::default()
                    .fg(theme.text_primary)
                    .bg(background)
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(" ".repeat(padding), Style::default().bg(background)),
            Span::styled(
                truncate(description, description_width),
                Style::default().fg(theme.gray).bg(background),
            ),
        ])),
        row,
    );
    row
}

fn highlighted_label(
    text: &str,
    indices: &[usize],
    selected: bool,
    background: Color,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let modifier = if selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };
    for (index, ch) in text.chars().enumerate() {
        let color = if indices.contains(&index) {
            theme.fuzzy_accent
        } else {
            theme.text_primary
        };
        spans.push(Span::styled(
            ch.to_string(),
            Style::default()
                .fg(color)
                .bg(background)
                .add_modifier(modifier),
        ));
    }
    spans
}

struct ComposerLayout {
    lines: Vec<String>,
    cursor_row: u16,
    cursor_column: u16,
    positions: Vec<(usize, (u16, u16))>,
}

fn layout_composer(text: &str, cursor: usize, width: u16) -> ComposerLayout {
    let width = width.max(1);
    let mut lines = vec![String::new()];
    let mut row = 0u16;
    let mut column = 0u16;
    let mut cursor_position = None;
    let mut positions = Vec::new();

    for (byte, ch) in text.char_indices() {
        if byte == cursor {
            cursor_position = Some((row, column));
        }
        if ch == '\n' {
            row += 1;
            column = 0;
            lines.push(String::new());
            continue;
        }
        let char_width = ch.width().unwrap_or(0) as u16;
        if column > 0 && column + char_width > width {
            row += 1;
            column = 0;
            lines.push(String::new());
        }
        positions.push((byte, (row, column)));
        lines[row as usize].push(ch);
        column += char_width;
    }
    let (cursor_row, cursor_column) = cursor_position.unwrap_or((row, column));
    ComposerLayout {
        lines,
        cursor_row,
        cursor_column,
        positions,
    }
}

fn composer_height(text: &str, width: u16) -> u16 {
    let rows = layout_composer(text, text.len(), width).lines.len() as u16;
    rows.clamp(1, 5) + 2
}

fn markdown_lines(text: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let logical = text.lines().collect::<Vec<_>>();
    let mut output = Vec::new();
    let mut index = 0;
    let mut code_block = false;
    while index < logical.len() {
        let source = logical[index];
        let trimmed = source.trim();
        if trimmed.starts_with("```") {
            code_block = !code_block;
            index += 1;
            continue;
        }
        if code_block {
            output.extend(wrap_styled_spans(
                vec![Span::styled(
                    source.to_string(),
                    Style::default().fg(theme.command).bg(theme.bg_dark),
                )],
                width,
            ));
            index += 1;
            continue;
        }
        if index + 1 < logical.len()
            && source.contains('|')
            && is_table_separator(logical[index + 1])
        {
            let mut table = vec![table_cells(source)];
            index += 2;
            while index < logical.len() && logical[index].contains('|') {
                table.push(table_cells(logical[index]));
                index += 1;
            }
            output.extend(render_markdown_table(&table, width, theme));
            continue;
        }
        if trimmed.is_empty() {
            output.push(Line::default());
            index += 1;
            continue;
        }

        let (prefix, body, style) = if let Some(body) = trimmed.strip_prefix('>') {
            (
                "│ ",
                body.trim_start(),
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::ITALIC),
            )
        } else if let Some((level, body)) = markdown_heading(trimmed) {
            let modifier = if level <= 2 {
                Modifier::BOLD | Modifier::UNDERLINED
            } else {
                Modifier::BOLD
            };
            (
                "",
                body,
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(modifier),
            )
        } else if let Some(body) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            ("• ", body, Style::default().fg(theme.text_primary))
        } else if let Some((prefix, body)) = ordered_list_item(trimmed) {
            (prefix, body, Style::default().fg(theme.text_primary))
        } else if is_horizontal_rule(trimmed) {
            output.push(Line::styled(
                "─".repeat(width),
                Style::default().fg(theme.gray_dim),
            ));
            index += 1;
            continue;
        } else {
            ("", source, Style::default().fg(theme.text_primary))
        };
        let mut spans = Vec::new();
        if !prefix.is_empty() {
            spans.push(Span::styled(
                prefix.to_string(),
                Style::default().fg(if prefix == "│ " {
                    theme.fuzzy_accent
                } else {
                    theme.gray_bright
                }),
            ));
        }
        spans.extend(parse_inline_markdown(body, style, theme));
        output.extend(wrap_styled_spans(spans, width));
        index += 1;
    }
    if output.is_empty() {
        output.push(Line::default());
    }
    output
}

fn parse_inline_markdown(input: &str, style: Style, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut remaining = input;
    while !remaining.is_empty() {
        if let Some(rest) = remaining.strip_prefix('\\')
            && let Some(character) = rest.chars().next()
        {
            spans.push(Span::styled(character.to_string(), style));
            remaining = &rest[character.len_utf8()..];
            continue;
        }
        let delimiter = [
            ("***", Modifier::BOLD | Modifier::ITALIC),
            ("___", Modifier::BOLD | Modifier::ITALIC),
            ("**", Modifier::BOLD),
            ("__", Modifier::BOLD),
            ("~~", Modifier::CROSSED_OUT),
            ("*", Modifier::ITALIC),
            ("_", Modifier::ITALIC),
        ]
        .into_iter()
        .find_map(|(delimiter, modifier)| {
            let body = remaining.strip_prefix(delimiter)?;
            let end = body.find(delimiter)?;
            Some((
                delimiter,
                modifier,
                &body[..end],
                &body[end + delimiter.len()..],
            ))
        });
        if let Some((_delimiter, modifier, body, rest)) = delimiter {
            spans.extend(parse_inline_markdown(
                body,
                style.add_modifier(modifier),
                theme,
            ));
            remaining = rest;
            continue;
        }
        if let Some(body) = remaining.strip_prefix('`')
            && let Some(end) = body.find('`')
        {
            spans.push(Span::styled(
                body[..end].to_string(),
                style.fg(theme.command).bg(theme.bg_dark),
            ));
            remaining = &body[end + 1..];
            continue;
        }
        if let Some(label) = remaining.strip_prefix('[')
            && let Some(label_end) = label.find("](")
            && let Some(url_end) = label[label_end + 2..].find(')')
        {
            let url_start = label_end + 2;
            let url_end = url_start + url_end;
            spans.extend(parse_inline_markdown(
                &label[..label_end],
                style
                    .fg(theme.fuzzy_accent)
                    .add_modifier(Modifier::UNDERLINED),
                theme,
            ));
            spans.push(Span::styled(
                format!(" ({})", &label[url_start..url_end]),
                Style::default().fg(theme.gray),
            ));
            remaining = &label[url_end + 1..];
            continue;
        }

        let next = remaining
            .char_indices()
            .skip(1)
            .find_map(|(index, character)| {
                matches!(character, '\\' | '*' | '_' | '~' | '`' | '[').then_some(index)
            })
            .unwrap_or(remaining.len());
        let next = if next == 0 {
            remaining
                .chars()
                .next()
                .map_or(remaining.len(), char::len_utf8)
        } else {
            next
        };
        spans.push(Span::styled(remaining[..next].to_string(), style));
        remaining = &remaining[next..];
    }
    spans
}

fn wrap_styled_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut output = Vec::new();
    let mut current = Vec::new();
    let mut current_width = 0;
    for span in spans {
        let style = span.style;
        for token in span.content.split_inclusive(char::is_whitespace) {
            let mut token = token.to_string();
            if current_width == 0 {
                token = token.trim_start().to_string();
            }
            if token.is_empty() {
                continue;
            }
            let token_width = token.width();
            if current_width > 0 && current_width + token_width > width {
                output.push(Line::from(std::mem::take(&mut current)));
                current_width = 0;
                token = token.trim_start().to_string();
            }
            if token.width() <= width {
                current_width += token.width();
                current.push(Span::styled(token, style));
                continue;
            }
            let mut fragment = String::new();
            for character in token.chars() {
                let character_width = character.width().unwrap_or(0);
                if current_width + fragment.width() + character_width > width {
                    if !fragment.is_empty() {
                        current.push(Span::styled(std::mem::take(&mut fragment), style));
                    }
                    output.push(Line::from(std::mem::take(&mut current)));
                    current_width = 0;
                }
                fragment.push(character);
            }
            if !fragment.is_empty() {
                current_width += fragment.width();
                current.push(Span::styled(fragment, style));
            }
        }
    }
    if !current.is_empty() || output.is_empty() {
        output.push(Line::from(current));
    }
    output
}

fn markdown_heading(line: &str) -> Option<(usize, &str)> {
    let level = line
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if level == 0 || level > 6 || line.as_bytes().get(level) != Some(&b' ') {
        return None;
    }
    line.get(level + 1..)
        .map(str::trim)
        .map(|heading| (level, heading))
}

fn ordered_list_item(line: &str) -> Option<(&str, &str)> {
    let end = line.find(". ")?;
    line[..end]
        .chars()
        .all(|character| character.is_ascii_digit())
        .then_some((&line[..end + 2], &line[end + 2..]))
}

fn is_horizontal_rule(line: &str) -> bool {
    let compact = line.replace(' ', "");
    compact.len() >= 3
        && compact
            .chars()
            .all(|character| character == '-' || character == '*' || character == '_')
}

fn table_cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

fn is_table_separator(line: &str) -> bool {
    let cells = table_cells(line);
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let body = cell.trim_matches(':').trim();
            body.len() >= 3 && body.chars().all(|character| character == '-')
        })
}

fn render_markdown_table(rows: &[Vec<String>], width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 {
        return Vec::new();
    }
    let overhead = columns.saturating_mul(3).saturating_add(1);
    let available = width.saturating_sub(overhead).max(columns);
    let mut widths = (0..columns)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|cell| markdown_plain_text(cell).width())
                .max()
                .unwrap_or(1)
                .max(1)
        })
        .collect::<Vec<_>>();
    if widths.iter().sum::<usize>() > available {
        let each = available / columns;
        let remainder = available % columns;
        for (index, column) in widths.iter_mut().enumerate() {
            *column = each + usize::from(index < remainder);
        }
    }
    let border = |left: char, middle: char, right: char| {
        let mut value = String::new();
        value.push(left);
        for (index, width) in widths.iter().enumerate() {
            value.push_str(&"─".repeat(width + 2));
            value.push(if index + 1 == widths.len() {
                right
            } else {
                middle
            });
        }
        Line::styled(value, Style::default().fg(theme.gray_dim))
    };
    let mut output = vec![border('┌', '┬', '┐')];
    for (row_index, row) in rows.iter().enumerate() {
        let mut spans = vec![Span::styled("│ ", Style::default().fg(theme.gray_dim))];
        for (column, width) in widths.iter().copied().enumerate() {
            let cell = row.get(column).map(String::as_str).unwrap_or_default();
            let value = truncate(&markdown_plain_text(cell), width);
            let padding = width.saturating_sub(value.width());
            spans.push(Span::styled(
                value,
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(if row_index == 0 {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ));
            spans.push(Span::raw(" ".repeat(padding + 1)));
            spans.push(Span::styled(
                if column + 1 == columns { "│" } else { "│ " },
                Style::default().fg(theme.gray_dim),
            ));
        }
        output.push(Line::from(spans));
        if row_index == 0 {
            output.push(border('├', '┼', '┤'));
        }
    }
    output.push(border('└', '┴', '┘'));
    output
}

fn markdown_plain_text(value: &str) -> String {
    parse_inline_markdown(
        value,
        Style::default(),
        &Theme::for_preference(crate::theme::ThemeKind::IndusNight),
    )
    .into_iter()
    .map(|span| span.content.into_owned())
    .collect()
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut output = Vec::new();
    for logical in text.lines() {
        if logical.is_empty() {
            output.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in logical.split_inclusive(char::is_whitespace) {
            if !current.is_empty() && current.width() + word.width() > width {
                output.push(current.trim_end().to_string());
                current.clear();
            }
            if word.width() > width {
                for ch in word.chars() {
                    if current.width() + ch.width().unwrap_or(0) > width {
                        output.push(std::mem::take(&mut current));
                    }
                    current.push(ch);
                }
            } else {
                current.push_str(word);
            }
        }
        output.push(current.trim_end().to_string());
    }
    output
}

#[allow(clippy::too_many_arguments)]
fn draw_horizontal_border(
    buffer: &mut Buffer,
    left: u16,
    right: u16,
    y: u16,
    left_corner: char,
    right_corner: char,
    foreground: Color,
    background: Color,
) {
    for x in left..right {
        let ch = if x == left {
            left_corner
        } else if x + 1 == right {
            right_corner
        } else {
            '─'
        };
        set_cell(buffer, x, y, ch, foreground, background);
    }
}

fn set_cell(buffer: &mut Buffer, x: u16, y: u16, ch: char, foreground: Color, background: Color) {
    if let Some(cell) = buffer.cell_mut((x, y)) {
        cell.set_char(ch);
        cell.set_style(Style::default().fg(foreground).bg(background));
    }
}

fn fill(buffer: &mut Buffer, area: Rect, style: Style) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_style(style);
                cell.set_char(' ');
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

fn collapse_home(path: &Path) -> String {
    let value = path.display().to_string();
    std::env::var("HOME")
        .ok()
        .and_then(|home| value.strip_prefix(&home).map(|rest| format!("~{rest}")))
        .unwrap_or(value)
}

fn truncate(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    let mut output = String::new();
    for ch in value.chars() {
        if output.width() + ch.width().unwrap_or(0) >= width {
            break;
        }
        output.push(ch);
    }
    output.push('…');
    output
}

fn format_milliseconds(milliseconds: u128) -> String {
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
    fn composer_layout_tracks_unicode_cursor() {
        let text = "Indus नदी";
        let layout = layout_composer(text, text.len(), 40);
        assert_eq!(layout.cursor_row, 0);
        assert_eq!(layout.cursor_column as usize, text.width());
    }

    #[test]
    fn composer_wraps_without_losing_text() {
        let layout = layout_composer("abcdefgh", 8, 4);
        assert_eq!(layout.lines, vec!["abcd", "efgh"]);
        assert_eq!((layout.cursor_row, layout.cursor_column), (1, 4));
    }

    #[test]
    fn slash_dropdown_stays_at_six_visible_rows() {
        assert_eq!(MAX_SLASH_ROWS, 6);
    }

    #[test]
    fn assistant_markdown_applies_emphasis_links_and_quotes() {
        let theme = Theme::for_preference(crate::theme::ThemeKind::IndusNight);
        let lines = markdown_lines(
            "**bold** *italic* ***both*** [Indus](https://mciair.in)\n> quoted",
            100,
            &theme,
        );
        let spans = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .collect::<Vec<_>>();
        assert!(spans.iter().any(|span| {
            span.content == "bold" && span.style.add_modifier.contains(Modifier::BOLD)
        }));
        assert!(spans.iter().any(|span| {
            span.content == "italic" && span.style.add_modifier.contains(Modifier::ITALIC)
        }));
        assert!(spans.iter().any(|span| {
            span.content == "both"
                && span.style.add_modifier.contains(Modifier::BOLD)
                && span.style.add_modifier.contains(Modifier::ITALIC)
        }));
        assert!(spans.iter().any(|span| {
            span.content == "Indus" && span.style.add_modifier.contains(Modifier::UNDERLINED)
        }));
        assert!(
            lines
                .iter()
                .any(|line| plain_line(line).starts_with("│ quoted"))
        );
    }

    #[test]
    fn assistant_markdown_renders_tables_with_terminal_borders() {
        let theme = Theme::for_preference(crate::theme::ThemeKind::IndusNight);
        let lines = markdown_lines(
            "| Name | State |\n| --- | --- |\n| Indus | Ready |",
            60,
            &theme,
        );
        let rendered = lines.iter().map(plain_line).collect::<Vec<_>>().join("\n");
        assert!(rendered.contains('┌'));
        assert!(rendered.contains("Indus"));
        assert!(rendered.contains("Ready"));
        assert!(rendered.contains('└'));
    }

    #[test]
    fn incomplete_markdown_headings_never_slice_past_the_line() {
        for line in ["#", "##", "######", "#######", "###text"] {
            assert_eq!(markdown_heading(line), None);
            let rendered = markdown_lines(
                line,
                40,
                &Theme::for_preference(crate::theme::ThemeKind::IndusNight),
            );
            assert!(!rendered.is_empty());
        }
        assert_eq!(
            markdown_heading("## Valid heading"),
            Some((2, "Valid heading"))
        );
    }

    #[test]
    fn streamed_thinking_stays_collapsed_by_default() {
        let theme = Theme::for_preference(crate::theme::ThemeKind::IndusNight);
        let entry = TranscriptEntry::Thinking {
            id: "r1".into(),
            text: "Inspecting the workspace".into(),
            running: true,
            elapsed_ms: None,
            expanded: false,
        };
        let mut rows = Vec::new();
        build_transcript_rows(&mut rows, 0, &entry, 80, &theme);
        assert_eq!(plain_line(&rows[0].line), "● Thinking…");
        assert_eq!(rows.len(), 1);
        assert!(!plain_line(&rows[0].line).contains("Inspecting the workspace"));
    }

    #[test]
    fn completed_thinking_collapses_to_a_timed_header() {
        let theme = Theme::for_preference(crate::theme::ThemeKind::IndusNight);
        let entry = TranscriptEntry::Thinking {
            id: "r1".into(),
            text: "Hidden reasoning".into(),
            running: false,
            elapsed_ms: Some(1_250),
            expanded: false,
        };
        let mut rows = Vec::new();
        build_transcript_rows(&mut rows, 0, &entry, 80, &theme);
        assert_eq!(rows.len(), 1);
        assert!(plain_line(&rows[0].line).starts_with("● Thought for 1.2s"));
    }

    #[test]
    fn collapsed_edit_header_reports_line_changes() {
        let theme = Theme::for_preference(crate::theme::ThemeKind::IndusNight);
        let entry = TranscriptEntry::Tool {
            call_id: "call-1".into(),
            name: "edit".into(),
            description: "Edit source".into(),
            input: String::new(),
            output: String::new(),
            state: ToolVisualState::Succeeded,
            elapsed_ms: Some(100),
            expanded: false,
            diffs: vec![FileDiff {
                path: "src/main.rs".into(),
                lines: vec![
                    crate::harness::event::DiffLine {
                        old_line: Some(1),
                        new_line: None,
                        kind: DiffKind::Removed,
                        text: "old".into(),
                    },
                    crate::harness::event::DiffLine {
                        old_line: None,
                        new_line: Some(1),
                        kind: DiffKind::Added,
                        text: "new".into(),
                    },
                ],
            }],
        };
        let mut rows = Vec::new();
        build_transcript_rows(&mut rows, 0, &entry, 80, &theme);
        assert_eq!(plain_line(&rows[0].line), "● Edit src/main.rs +1/-1");
        assert_eq!(rows[0].line.spans[0].style.fg, Some(theme.accent_success));
    }

    #[test]
    fn usage_card_contains_only_requested_status_fields_and_context_bar() {
        let theme = Theme::for_preference(crate::theme::ThemeKind::IndusNight);
        let card = UsageCard {
            model: "Model One · Provider".into(),
            directory: "/workspace".into(),
            permissions: "Plan · read-only".into(),
            session: "ses-i_example".into(),
            context_used: 25_000,
            context_window: Some(100_000),
        };
        let mut rows = Vec::new();
        render_usage_card(&mut rows, &card, 96, &theme);
        let rendered = rows
            .iter()
            .map(|row| plain_line(&row.line))
            .collect::<Vec<_>>()
            .join("\n");
        for label in [
            "Indus",
            "Model",
            "Directory",
            "Permissions",
            "Session",
            "Context Window",
        ] {
            assert!(rendered.contains(label), "missing {label}: {rendered}");
        }
        assert!(rendered.contains("75% left"));
        assert!(rendered.contains("25.0K used / 100.0K"));
        assert!(rendered.contains("███████████████░░░░░"));
        assert!(rendered.contains('┌'));
        assert!(rendered.contains('┐'));
        assert!(rendered.contains('└'));
        assert!(rendered.contains('┘'));
        assert!(
            !['╭', '╮', '╰', '╯']
                .iter()
                .any(|corner| rendered.contains(*corner))
        );
        assert!(!rendered.contains("Visit"));
        assert!(!rendered.contains(">_"));
    }

    fn plain_line(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }
}
