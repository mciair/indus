mod app;
pub mod harness;
mod provider;
mod slash;
mod theme;
mod ui;

use std::{
    env,
    io::{self, Write},
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::Result;
use app::{App, SessionCommand};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use harness::{Harness, event::PermissionReply};
use ratatui::{Terminal, backend::CrosstermBackend};
use slash::CompletionPhase;

fn main() -> Result<()> {
    let resume_id = parse_resume_argument()?;
    let harness = Harness::configured_with_session(resume_id.as_deref())?;
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run(&mut terminal, &harness);
    let session = harness.session_snapshot();
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    if result.is_ok()
        && let Some(hint) = resume_hint(&session)
    {
        writeln!(io::stdout(), "{hint}")?;
    }
    result
}

fn resume_hint(session: &harness::session::Session) -> Option<String> {
    session
        .is_allocated()
        .then(|| format!("Resume this session:\n  indus --resume {}", session.id))
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, harness: &Harness) -> Result<()> {
    let mut app = App::new();
    let session = harness.session_snapshot();
    if session.is_allocated() {
        app.load_session(&session);
    }
    while app.running {
        app.process_model_discovery();
        harness.poll_jobs();
        for event in harness.drain_events() {
            app.apply_harness_event(event);
        }
        terminal.draw(|frame| ui::render(frame, &mut app))?;
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    handle_key(&mut app, harness, key)
                }
                Event::Mouse(mouse) => handle_mouse(&mut app, mouse),
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
        dispatch_app_commands(&mut app, harness);
        app.on_tick();
    }
    Ok(())
}

fn handle_key(app: &mut App, harness: &Harness, key: KeyEvent) {
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('q') {
        app.running = false;
        return;
    }
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('u') {
        open_alpha();
        return;
    }
    if app.delete_confirmation.is_some() {
        if key.modifiers.is_empty() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    app.confirm_delete();
                }
                KeyCode::Char('n') | KeyCode::Esc => app.cancel_delete(),
                _ => {}
            }
        }
        return;
    }
    if is_mode_cycle_key(key)
        && app.resume_panel.is_none()
        && app.catalog_modal.is_none()
        && app.permission.is_none()
    {
        app.request_next_mode();
        return;
    }
    if app.resume_panel.is_some() {
        handle_resume_key(app, key);
        return;
    }
    if app.catalog_modal.is_some() {
        handle_catalog_key(app, key);
        return;
    }
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('e') {
        app.toggle_all_thinking();
        return;
    }
    if app.permission.is_some() && key.modifiers.is_empty() {
        let reply = match key.code {
            KeyCode::Char('y') => Some(PermissionReply::AllowOnce),
            KeyCode::Char('a') => Some(PermissionReply::AllowAlways),
            KeyCode::Char('n') | KeyCode::Esc => Some(PermissionReply::Reject),
            _ => None,
        };
        if let Some(reply) = reply {
            app.resolve_permission(reply);
        }
        return;
    }
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
        if app.turn.is_some() {
            harness.cancel();
            app.cancel_turn();
        } else if !app.composer.is_empty() {
            app.edit_composer(|composer| {
                composer.clear();
            });
        }
        return;
    }

    if app.slash.open && handle_slash_key(app, key) {
        return;
    }

    if app.catalog_modal.is_none() && !app.transcript.is_empty() {
        match key.code {
            KeyCode::PageUp => {
                app.page_transcript_up();
                return;
            }
            KeyCode::PageDown => {
                app.page_transcript_down();
                return;
            }
            KeyCode::Home if key.modifiers == KeyModifiers::CONTROL => {
                app.scroll_transcript_to_top();
                return;
            }
            KeyCode::End if key.modifiers == KeyModifiers::CONTROL => {
                app.scroll_transcript_to_bottom();
                return;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Esc => {
            if app.turn.is_some() {
                harness.cancel();
                app.cancel_turn();
            } else if !app.composer.is_empty() {
                app.edit_composer(|composer| {
                    composer.clear();
                });
            }
        }
        KeyCode::Up if app.transcript.is_empty() && app.composer.is_empty() => {
            app.selected_menu = app.selected_menu.saturating_sub(1);
        }
        KeyCode::Down if app.transcript.is_empty() && app.composer.is_empty() => {
            app.selected_menu = (app.selected_menu + 1).min(app::HOME_MENU.len() - 1);
        }
        KeyCode::Enter
            if key.modifiers.contains(KeyModifiers::SHIFT)
                || key.modifiers.contains(KeyModifiers::ALT) =>
        {
            app.edit_composer(|composer| composer.insert_newline());
        }
        KeyCode::Enter => app.submit(),
        KeyCode::Backspace => app.edit_composer(|composer| composer.backspace()),
        KeyCode::Delete => app.edit_composer(|composer| composer.delete()),
        KeyCode::Left => app.edit_composer(|composer| composer.move_left()),
        KeyCode::Right => app.edit_composer(|composer| composer.move_right()),
        KeyCode::Home => app.edit_composer(|composer| composer.move_home()),
        KeyCode::End => app.edit_composer(|composer| composer.move_end()),
        KeyCode::Char(ch)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            app.edit_composer(|composer| composer.insert_char(ch));
        }
        _ => {}
    }
}

fn handle_resume_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.close_resume_panel(),
        KeyCode::Up => app.move_resume_selection(-1),
        KeyCode::Down => app.move_resume_selection(1),
        KeyCode::Char('p') if key.modifiers == KeyModifiers::CONTROL => {
            app.move_resume_selection(-1)
        }
        KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => {
            app.move_resume_selection(1)
        }
        KeyCode::Tab | KeyCode::Right => app.toggle_resume_details(),
        KeyCode::Enter if key.modifiers.is_empty() => app.submit_resume_selection(),
        KeyCode::Backspace => app.edit_resume_query(|query| query.backspace()),
        KeyCode::Delete => app.edit_resume_query(|query| query.delete()),
        KeyCode::Left => app.edit_resume_query(|query| query.move_left()),
        KeyCode::Home => app.edit_resume_query(|query| query.move_home()),
        KeyCode::End => app.edit_resume_query(|query| query.move_end()),
        KeyCode::Char(character)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            app.edit_resume_query(|query| query.insert_char(character));
        }
        _ => {}
    }
}

fn is_mode_cycle_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::BackTab)
        || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
}

fn handle_catalog_key(app: &mut App, key: KeyEvent) {
    if matches!(app.catalog_modal, Some(app::CatalogModal::ApiKey { .. })) {
        match key.code {
            KeyCode::Esc => app.close_catalog_level(),
            KeyCode::Enter if key.modifiers.is_empty() => app.submit_catalog_selection(),
            KeyCode::Backspace => app.edit_api_key(|input| input.backspace()),
            KeyCode::Delete => app.edit_api_key(|input| input.delete()),
            KeyCode::Left => app.edit_api_key(|input| input.move_left()),
            KeyCode::Right => app.edit_api_key(|input| input.move_right()),
            KeyCode::Home => app.edit_api_key(|input| input.move_home()),
            KeyCode::End => app.edit_api_key(|input| input.move_end()),
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                app.edit_api_key(|input| input.insert_char(ch));
            }
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Esc => app.close_catalog_level(),
        KeyCode::Up => app.move_catalog_selection(-1),
        KeyCode::Down => app.move_catalog_selection(1),
        KeyCode::Char('p') if key.modifiers == KeyModifiers::CONTROL => {
            app.move_catalog_selection(-1)
        }
        KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => {
            app.move_catalog_selection(1)
        }
        KeyCode::Enter if key.modifiers.is_empty() => app.submit_catalog_selection(),
        KeyCode::Char('r') if key.modifiers.is_empty() => app.refresh_model_catalog(),
        KeyCode::Char('k') if key.modifiers.is_empty() => app.replace_provider_key(),
        _ => {}
    }
}

fn handle_slash_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Up => {
            app.move_slash_selection(-1);
            true
        }
        KeyCode::Down => {
            app.move_slash_selection(1);
            true
        }
        KeyCode::Char('p') if key.modifiers == KeyModifiers::CONTROL => {
            app.move_slash_selection(-1);
            true
        }
        KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => {
            app.move_slash_selection(1);
            true
        }
        KeyCode::Tab => {
            app.accept_slash_completion();
            true
        }
        KeyCode::Esc => {
            app.close_slash();
            true
        }
        KeyCode::Enter if key.modifiers.is_empty() => {
            let phase = app.slash.phase;
            let chains = app
                .slash
                .selection()
                .is_some_and(|row| row.insert_text.ends_with(' '));
            if app.accept_slash_completion()
                && (matches!(phase, CompletionPhase::Arguments { .. }) || !chains)
            {
                app.submit();
            }
            true
        }
        _ => false,
    }
}

fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    let x = mouse.column;
    let y = mouse.row;
    if app.resume_panel.is_some() {
        match mouse.kind {
            MouseEventKind::ScrollUp => app.move_resume_selection(-1),
            MouseEventKind::ScrollDown => app.move_resume_selection(1),
            MouseEventKind::Down(MouseButton::Left) => {
                let resume_hit = app
                    .hit_zones
                    .resume_rows
                    .iter()
                    .find_map(|(area, index)| contains(*area, x, y).then_some(*index));
                if let Some(index) = resume_hit {
                    app.select_resume_index(index);
                    app.submit_resume_selection();
                }
            }
            _ => {}
        }
        return;
    }
    match mouse.kind {
        MouseEventKind::ScrollUp
            if app.catalog_modal.is_none() && app.transcript_contains(x, y) =>
        {
            app.scroll_transcript_up(3);
            return;
        }
        MouseEventKind::ScrollDown
            if app.catalog_modal.is_none() && app.transcript_contains(x, y) =>
        {
            app.scroll_transcript_down(3);
            return;
        }
        MouseEventKind::Drag(MouseButton::Left) if app.is_selecting_text() => {
            app.update_text_selection(x, y);
            return;
        }
        MouseEventKind::Up(MouseButton::Left) if app.is_selecting_text() => {
            if let Some(text) = app.finish_text_selection() {
                copy_to_clipboard(&text);
                return;
            }
            let fold_hit = app
                .hit_zones
                .fold_rows
                .iter()
                .find_map(|(area, index)| contains(*area, x, y).then_some(*index));
            if let Some(index) = fold_hit {
                app.toggle_fold(index);
            }
            return;
        }
        MouseEventKind::Down(MouseButton::Left) => {}
        _ => return,
    }
    if app.catalog_modal.is_some() {
        let catalog_hit = app
            .hit_zones
            .catalog_rows
            .iter()
            .find_map(|(area, index)| contains(*area, x, y).then_some(*index));
        if let Some(index) = catalog_hit {
            app.select_catalog_index(index);
            app.submit_catalog_selection();
        }
        return;
    }
    if app.hit_zones.alpha.is_some_and(|area| contains(area, x, y)) {
        open_alpha();
        return;
    }

    let slash_hit = app
        .hit_zones
        .slash_rows
        .iter()
        .find_map(|(area, index)| contains(*area, x, y).then_some(*index));
    if let Some(index) = slash_hit {
        let phase = app.slash.phase;
        app.select_slash_index(index);
        if app.accept_slash_completion() && matches!(phase, CompletionPhase::Arguments { .. }) {
            app.submit();
        }
        return;
    }

    let menu_hit = app
        .hit_zones
        .menu
        .iter()
        .enumerate()
        .find_map(|(index, (area, action))| contains(*area, x, y).then_some((index, *action)));
    if let Some((index, action)) = menu_hit {
        app.selected_menu = index;
        app.run_menu_action(action);
        return;
    }

    if app.begin_text_selection(x, y) {
        return;
    }

    let fold_hit = app
        .hit_zones
        .fold_rows
        .iter()
        .find_map(|(area, index)| contains(*area, x, y).then_some(*index));
    if let Some(index) = fold_hit {
        app.toggle_fold(index);
    }
}

fn dispatch_app_commands(app: &mut App, harness: &Harness) {
    if let Some(command) = app.take_session_command() {
        match command {
            SessionCommand::OpenResume => match harness.list_sessions(None) {
                Ok(sessions) => app.open_resume_panel(sessions),
                Err(error) => {
                    app.report_session_error(format!("Could not load session history: {error:#}"))
                }
            },
            SessionCommand::Resume(session_id) => match harness.resume_session(&session_id) {
                Ok(session) => app.load_session(&session),
                Err(error) => {
                    app.report_session_error(format!("Could not resume {session_id}: {error:#}"))
                }
            },
            SessionCommand::New => match harness.new_session() {
                Ok(session) => app.load_session(&session),
                Err(error) => {
                    app.report_session_error(format!("Could not start a new session: {error:#}"))
                }
            },
            SessionCommand::EditPrompt => match harness.edit_previous_prompt() {
                Ok((session, prompt)) => app.restore_edited_prompt(&session, prompt),
                Err(error) => app
                    .report_session_error(format!("Could not edit the previous prompt: {error:#}")),
            },
            SessionCommand::Copy(response) => {
                copy_to_clipboard(&response);
                app.report_session_error("Last response copied to the clipboard.");
            }
            SessionCommand::Rename(title) => match harness.rename_session(&title) {
                Ok(title) => {
                    app.set_session_title(title.clone());
                    app.report_session_error(format!("Session renamed to {title}."));
                }
                Err(error) => {
                    app.report_session_error(format!("Could not rename the session: {error:#}"))
                }
            },
            SessionCommand::Compact(instructions) => {
                if let Err(error) = harness.compact_context(instructions) {
                    app.apply_harness_event(harness::event::HarnessEvent::RunError {
                        run_id: 0,
                        message: format!("Could not compact the conversation: {error:#}"),
                    });
                    app.apply_harness_event(harness::event::HarnessEvent::RunFinished {
                        run_id: 0,
                        outcome: harness::event::RunOutcome::Failed,
                    });
                }
            }
            SessionCommand::SetMode(mode) => match harness.set_mode(mode) {
                Ok(()) => app.confirm_mode(mode),
                Err(error) => {
                    app.report_session_error(format!("Could not switch modes: {error:#}"))
                }
            },
            SessionCommand::SessionInfo => app.report_session_info(harness.session_info()),
            SessionCommand::Delete => match harness.delete_session() {
                Ok((session, deleted_id)) => {
                    app.load_session(&session);
                    app.report_session_error(format!("Deleted session {deleted_id}."));
                }
                Err(error) => {
                    app.report_session_error(format!("Could not delete the session: {error:#}"))
                }
            },
        }
    }
    if let Some(prompt) = app.take_submission()
        && let Err(error) = harness.submit(prompt)
    {
        app.apply_harness_event(harness::event::HarnessEvent::RunError {
            run_id: 0,
            message: error.to_string(),
        });
        app.apply_harness_event(harness::event::HarnessEvent::RunFinished {
            run_id: 0,
            outcome: harness::event::RunOutcome::Failed,
        });
    }
    if let Some((request_id, reply)) = app.take_permission_reply() {
        harness.reply_permission(request_id, reply);
    }
}

fn parse_resume_argument() -> Result<Option<String>> {
    let mut arguments = env::args().skip(1);
    let Some(argument) = arguments.next() else {
        return Ok(None);
    };
    let session_id = if argument == "--resume" {
        arguments
            .next()
            .ok_or_else(|| anyhow::anyhow!("--resume requires a session ID"))?
    } else if let Some(session_id) = argument.strip_prefix("--resume=") {
        session_id.to_string()
    } else {
        return Err(anyhow::anyhow!(
            "Unknown argument: {argument}. Usage: indus [--resume ses-i_…]"
        ));
    };
    if arguments.next().is_some() {
        return Err(anyhow::anyhow!("Usage: indus [--resume ses-i_…]"));
    }
    if !session_id.starts_with("ses-i_") || session_id.len() <= "ses-i_".len() {
        return Err(anyhow::anyhow!("Invalid Indus session ID: {session_id}"));
    }
    Ok(Some(session_id))
}

fn contains(area: ratatui::layout::Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.right() && y >= area.y && y < area.bottom()
}

fn open_alpha() {
    let _ = if cfg!(target_os = "macos") {
        Command::new("open").arg(ui::ALPHA_URL).status()
    } else if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/C", "start", ui::ALPHA_URL])
            .status()
    } else {
        Command::new("xdg-open").arg(ui::ALPHA_URL).status()
    };
}

fn copy_to_clipboard(text: &str) {
    let commands: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if cfg!(target_os = "windows") {
        &[("clip", &[])]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
            ("clip.exe", &[]),
        ]
    };
    if commands
        .iter()
        .any(|(program, arguments)| write_clipboard_command(program, arguments, text))
    {
        return;
    }

    let encoded = encode_base64(text.as_bytes());
    let mut stdout = io::stdout();
    let _ = write!(stdout, "\x1b]52;c;{encoded}\x07");
    let _ = stdout.flush();
}

fn write_clipboard_command(program: &str, arguments: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(program)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let wrote = child
        .stdin
        .take()
        .is_some_and(|mut input| input.write_all(text.as_bytes()).is_ok());
    let succeeded = child.wait().is_ok_and(|status| status.success());
    wrote && succeeded
}

fn encode_base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[(((first & 0b11) << 4) | (second >> 4)) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[(((second & 0b1111) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(third & 0b11_1111) as usize] as char
        } else {
            '='
        });
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{encode_base64, resume_hint};
    use crate::harness::session::Session;

    #[test]
    fn osc52_payload_uses_standard_base64_padding() {
        assert_eq!(encode_base64(b"Indus"), "SW5kdXM=");
        assert_eq!(encode_base64(b"AI"), "QUk=");
        assert_eq!(encode_base64(b"CLI"), "Q0xJ");
    }

    #[test]
    fn allocated_sessions_print_an_exact_resume_command() {
        let mut session = Session::unallocated("/workspace");
        assert!(session.allocate("ses-i_example", "Example Session", None, None));

        assert_eq!(
            resume_hint(&session).as_deref(),
            Some("Resume this session:\n  indus --resume ses-i_example")
        );
    }

    #[test]
    fn unallocated_conversations_do_not_print_a_resume_command() {
        assert_eq!(resume_hint(&Session::unallocated("/workspace")), None);
    }
}
