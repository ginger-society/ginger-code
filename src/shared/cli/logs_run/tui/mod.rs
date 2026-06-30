// src/bin/logs_run/tui/mod.rs

pub mod events;
pub mod state;
pub mod ui;

use crossterm::event::{KeyCode, KeyModifiers};
use crossterm::{execute, terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen}};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;

use self::events::{AppEvent, SseEvent};
use self::state::{AppState, Focus};
use super::RunTarget;

const TICK_RATE_MS: u64 = 250;

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

pub async fn run(
    base_url: &str,
    targets: Vec<RunTarget>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (tx, mut rx) = mpsc::channel::<AppEvent>(512);

    for target in &targets {
        tokio::spawn(events::run_sse_task(
            base_url.to_string(),
            target.namespace.clone(),
            target.run_name.clone(),
            tx.clone(),
        ));
    }

    events::spawn_key_task(tx.clone());
    events::spawn_tick_task(tx.clone(), TICK_RATE_MS);

    let run_names: Vec<String> = targets.iter().map(|t| t.run_name.clone()).collect();
    let mut state = AppState::new(run_names);

    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    terminal.draw(|f| ui::draw(f, &state))?;

    loop {
        let event = match rx.recv().await {
            Some(e) => e,
            None => break,
        };

        match event {
            AppEvent::Tick => {
                terminal.draw(|f| ui::draw(f, &state))?;
            }

            AppEvent::Key(key) => {
                if events::is_quit(&key) {
                    break;
                }

                match state.focus {
                    // ── Pipeline list (leftmost panel) ────────────────────
                    Focus::PipelineList => match key.code {
                        KeyCode::Right => {
                            state.focus = Focus::TaskLog;
                        }
                        KeyCode::Up => state.select_prev_pipeline(),
                        KeyCode::Down => state.select_next_pipeline(),
                        _ => {}
                    },

                    // ── Task/step list (middle panel) ─────────────────────
                    Focus::TaskLog => match key.code {
                        KeyCode::Left => {
                            state.focus = Focus::PipelineList;
                        }
                        KeyCode::Right => {
                            state.focus = Focus::Logs;
                        }
                        KeyCode::Up => {
                            if let Some(p) = state.current_mut() { p.move_up(); }
                        }
                        KeyCode::Down => {
                            if let Some(p) = state.current_mut() { p.move_down(); }
                        }
                        _ => {}
                    },

                    // ── Logs panel (rightmost panel) ──────────────────────
                    // ── Logs panel (rightmost panel) ──────────────────────
                    Focus::Logs => match (key.code, key.modifiers) {
                        (KeyCode::Left, _) => {
                            state.focus = Focus::TaskLog;
                        }
                        (KeyCode::Char(' '), _) => {
                            if let Some(p) = state.current_mut() { p.toggle_logs_collapse(); }
                        }
                        (KeyCode::Up, _) => {
                            if let Some(p) = state.current_mut() { p.scroll_log_up(); }
                        }
                        (KeyCode::Down, KeyModifiers::CONTROL) => {
                            // Ctrl+↓ re-enables auto-scroll / follow mode
                            if let Some(p) = state.current_mut() {
                                p.follow_logs();
                            }
                        }
                        (KeyCode::Down, _) => {
                            let height = (terminal.size()?.height / 2) as usize;
                            if let Some(p) = state.current_mut() {
                                p.scroll_log_down(height);
                            }
                        }
                        (KeyCode::PageUp, _) => {
                            let half = (terminal.size()?.height / 2) as usize;
                            if let Some(p) = state.current_mut() {
                                p.page_log_up(half);
                            }
                        }
                        (KeyCode::PageDown, _) => {
                            let half = (terminal.size()?.height / 2) as usize;
                            if let Some(p) = state.current_mut() {
                                p.page_log_down(half, half);
                            }
                        }
                        _ => {}
                    },
                }
                terminal.draw(|f| ui::draw(f, &state))?;
            }

            AppEvent::Sse(tagged) => {
                if let Some(pipeline) = state.pipeline_mut(&tagged.run_name) {
                    match tagged.event {
                        SseEvent::Meta(meta)           => pipeline.apply_meta(meta),
                        SseEvent::Log(log)             => pipeline.apply_log_line(log),
                        SseEvent::StepStatus(upd)      => pipeline.apply_step_status(upd),
                        SseEvent::TaskStatus(upd)      => pipeline.apply_task_status(upd),
                        SseEvent::Done(done)           => pipeline.apply_done(done),
                        SseEvent::Error(err)           => pipeline.error = Some(err.message),
                        SseEvent::ConnectionError(msg) => pipeline.error = Some(msg),
                        SseEvent::UnknownEvent(_)      => {}
                    }
                }
                terminal.draw(|f| ui::draw(f, &state))?;
            }
        }
    }

    Ok(())
}