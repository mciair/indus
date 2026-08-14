use std::{path::Path, process::Command};

use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::{
        App, CatalogModal, HOME_MENU, HitZones, ModelCatalogView, ToolVisualState, TranscriptEntry,
        TurnActivity,
    },
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
    let turn_height = if app.permission.is_some() {
        3
    } else {
        u16::from(app.turn.is_some())
    };
    let prompt_gap = u16::from(turn_height == 0 && area.height > 16);
    let [top, body, _, turn, prompt] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(4),
        Constraint::Length(prompt_gap),
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

    let mut zones = HitZones::default();
    render_top_bar(frame, top, app, &theme);
    if app.transcript.is_empty() {
        render_home(frame, content, app, &theme, &mut zones);
    } else {
        render_transcript(frame, content, app, &theme, &mut zones);
    }
    if app.turn.is_some() {
        render_turn_status(frame, turn, app, &theme);
    }
    render_composer(frame, prompt, app, &theme);
    if app.slash.open {
        render_slash_dropdown(frame, prompt, app, &theme, &mut zones);
    }
    if app.catalog_modal.is_some() {
        render_catalog_modal(frame, prompt, app, &theme, &mut zones);
    }
    app.hit_zones = zones;
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
    for (index, entry) in app.transcript.iter().enumerate() {
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
            for segment in wrap_text(text, width.saturating_sub(2).max(1)) {
                rows.push(TranscriptRow {
                    line: Line::from(vec![
                        Span::raw("  "),
                        Span::styled(segment, Style::default().fg(theme.text_primary)),
                    ]),
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
        TurnActivity::Classifying
        | TurnActivity::Compacting
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

    let info = app.active_model().map_or_else(
        || format!(" indus · {} ", app.theme_kind.name()),
        |active| format!(" {} · {} ", active.model_name, active.provider.name()),
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
        let is_key = matches!(token, "enter" | "esc" | "↑↓" | "r" | "k");
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
    fn streamed_thinking_shows_its_body_until_completion() {
        let theme = Theme::for_preference(crate::theme::ThemeKind::IndusNight);
        let entry = TranscriptEntry::Thinking {
            id: "r1".into(),
            text: "Inspecting the workspace".into(),
            running: true,
            elapsed_ms: None,
            expanded: true,
        };
        let mut rows = Vec::new();
        build_transcript_rows(&mut rows, 0, &entry, 80, &theme);
        assert_eq!(plain_line(&rows[0].line), "● Thinking…");
        assert!(
            rows.iter()
                .any(|row| plain_line(&row.line).contains("Inspecting the workspace"))
        );
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

    fn plain_line(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }
}
