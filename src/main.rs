mod app;
mod harness;
mod slash;
mod theme;
mod ui;

use std::{io, process::Command, time::Duration};

use anyhow::Result;
use app::App;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use slash::CompletionPhase;

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
        terminal.draw(|frame| ui::render(frame, &mut app))?;
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => handle_key(&mut app, key),
                Event::Mouse(mouse) => handle_mouse(&mut app, mouse),
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
        app.on_tick();
    }
    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) {
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('q') {
        app.running = false;
        return;
    }
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('u') {
        open_alpha();
        return;
    }
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
        if app.turn.is_some() {
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

    match key.code {
        KeyCode::Esc => {
            if app.turn.is_some() {
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
            if app.accept_slash_completion() {
                if matches!(phase, CompletionPhase::Arguments { .. }) || !chains {
                    app.submit();
                }
            }
            true
        }
        _ => false,
    }
}

fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return;
    }
    let x = mouse.column;
    let y = mouse.row;
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
    }
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
