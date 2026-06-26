// src/bin/logs_run/tui/mod.rs
//
// TUI entry point. Sets up the terminal, spawns background tasks, then
// drives the event loop: receive AppEvents from the mpsc channel and
// either mutate AppState (SSE events) or handle navigation (key events),
// then redraw.
//
// Terminal setup/teardown is guarded by a RAII wrapper so panics don't
// leave the terminal in raw mode.

pub mod events;
pub mod state;
pub mod ui;

use crossterm::{
    event::KeyCode,
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;

use self::events::{AppEvent, SseEvent};
use self::state::AppState;
use crate::shared::cli::logs_run::wire::{LogLine, RunDone, RunMeta, StepStatusUpdate, StreamError, TaskStatusUpdate};

const TICK_RATE_MS: u64 = 250;

// ── Terminal RAII guard ───────────────────────────────────────────────────

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> std::io::Result<Self> {
        enable_raw_mode()?;
        execute!(std::io::stdout(), EnterAlternateScreen)?;
        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
    }
}

// ── Entry point ───────────────────────────────────────────────────────────

pub async fn run(
    base_url: &str,
    run_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (tx, mut rx) = mpsc::channel::<AppEvent>(512);

    // Spawn background tasks.
    events::spawn_key_task(tx.clone());
    events::spawn_tick_task(tx.clone(), TICK_RATE_MS);
    tokio::spawn(events::run_sse_task(
        base_url.to_string(),
        run_name.to_string(),
        tx.clone(),
    ));

    // Set up terminal.
    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut state = AppState::new();

    // Initial blank frame while we wait for meta.
    terminal.draw(|f| ui::draw(f, &state))?;

    // Main event loop.
    loop {
        let event = match rx.recv().await {
            Some(e) => e,
            None => break,
        };

        match event {
            AppEvent::Tick => {
                // Just redraw — state may have been mutated by SSE events.
                terminal.draw(|f| ui::draw(f, &state))?;
            }

            AppEvent::Key(key) => {
                if events::is_quit(&key) {
                    break;
                }
                match key.code {
                    KeyCode::Up => {
                        state.move_up();
                        terminal.draw(|f| ui::draw(f, &state))?;
                    }
                    KeyCode::Down => {
                        state.move_down();
                        terminal.draw(|f| ui::draw(f, &state))?;
                    }
                    KeyCode::PageUp => {
                        let half = (terminal.size()?.height / 2) as usize;
                        for _ in 0..half { state.scroll_log_up(); }
                        terminal.draw(|f| ui::draw(f, &state))?;
                    }
                    KeyCode::PageDown => {
                        let half = (terminal.size()?.height / 2) as usize;
                        for _ in 0..half { state.scroll_log_down(half); }
                        terminal.draw(|f| ui::draw(f, &state))?;
                    }
                    _ => {}
                }
            }

            AppEvent::Sse(sse) => {
                handle_sse_event(sse, &mut state);
                // Redraw immediately on every SSE event so the user sees
                // log lines as they arrive without waiting for the tick.
                terminal.draw(|f| ui::draw(f, &state))?;

                // If the run is done, do one final draw and wait for
                // a quit key rather than auto-exiting. The user might
                // want to scroll through logs.
                // (We continue the loop normally; the footer hints will
                // say "Run complete." and `q` will exit.)
            }
        }
    }

    Ok(())
}

// ── SSE dispatch ──────────────────────────────────────────────────────────

fn handle_sse_event(sse: SseEvent, state: &mut AppState) {
    match sse {
        SseEvent::Meta(meta) => state.apply_meta(meta),
        SseEvent::Log(log) => state.apply_log_line(log),
        SseEvent::StepStatus(upd) => state.apply_step_status(upd),
        SseEvent::TaskStatus(upd) => state.apply_task_status(upd),
        SseEvent::Done(done) => state.apply_done(done),
        SseEvent::Error(err) => state.apply_error(err.message),
        SseEvent::ConnectionError(msg) => state.apply_error(msg),
        SseEvent::UnknownEvent(_) => {} // silently ignored, same as flat mode
    }
}