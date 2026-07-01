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

    let mut log_visible_height: usize = 0;
    let mut state = AppState::new(&targets);

    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    terminal.draw(|f| { log_visible_height = ui::draw(f, &state); })?;

    loop {
        let event = match rx.recv().await {
            Some(e) => e,
            None => break,
        };

        match event {
            AppEvent::Tick => {
                terminal.draw(|f| { log_visible_height = ui::draw(f, &state); })?;
            }

            AppEvent::Key(key) => {
                state.user_touched = true;

                if events::is_quit(&key) {
                    break;
                }

                match state.focus {
                    // ── Pipeline list ─────────────────────────────────────
                    Focus::PipelineList => match key.code {
                        KeyCode::Right => { state.focus = Focus::TaskLog; }
                        KeyCode::Up    => state.select_prev_pipeline(),
                        KeyCode::Down  => state.select_next_pipeline(),
                        _ => {}
                    },

                    // ── Task list ─────────────────────────────────────────
                    Focus::TaskLog => match key.code {
                        KeyCode::Left  => { state.focus = Focus::PipelineList; }
                        KeyCode::Right => { state.focus = Focus::Logs; }
                        KeyCode::Up    => { if let Some(p) = state.current_mut() { p.move_up(); } }
                        KeyCode::Down  => { if let Some(p) = state.current_mut() { p.move_down(); } }
                        _ => {}
                    },

                    // ── Logs panel ────────────────────────────────────────
                    Focus::Logs => {
                        let collapsed = state.current().map(|p| p.logs_collapsed).unwrap_or(false);

                        match (key.code, key.modifiers) {
                            // ── Panel navigation (no modifier) ────────────
                            (KeyCode::Left, km) if km == KeyModifiers::NONE => {
                                state.focus = Focus::TaskLog;
                            }

                            // ── Horizontal scroll (Shift + arrow) ─────────
                            (KeyCode::Left, km) if km.contains(KeyModifiers::SHIFT) => {
                                if let Some(p) = state.current_mut() { p.scroll_log_left(8); }
                            }
                            (KeyCode::Right, km) if km.contains(KeyModifiers::SHIFT) => {
                                if let Some(p) = state.current_mut() { p.scroll_log_right(8); }
                            }

                            // ── Collapse / expand all (Space) ─────────────
                            (KeyCode::Char(' '), _) => {
                                if let Some(p) = state.current_mut() { p.toggle_logs_collapse(); }
                            }

                            // ── Enter: expand to selected step ────────────
                            (KeyCode::Enter, _) if collapsed => {
                                if let Some(p) = state.current_mut() { p.expand_to_step(); }
                            }

                            // ── Follow mode ───────────────────────────────
                            (KeyCode::Down, KeyModifiers::CONTROL) => {
                                if let Some(p) = state.current_mut() { p.follow_logs(); }
                            }

                            // ── Up: step cursor (collapsed) / scroll (expanded) ──
                            (KeyCode::Up, _) => {
                                if let Some(p) = state.current_mut() {
                                    if collapsed {
                                        p.collapsed_select_up();
                                    } else {
                                        p.scroll_log_up();
                                    }
                                }
                            }

                            // ── Down: step cursor (collapsed) / scroll (expanded) ─
                            (KeyCode::Down, _) => {
                                if let Some(p) = state.current_mut() {
                                    if collapsed {
                                        p.collapsed_select_down(log_visible_height);
                                    } else {
                                        p.scroll_log_down(log_visible_height);
                                    }
                                }
                            }

                            // ── Page scroll ───────────────────────────────
                            (KeyCode::PageUp, _) => {
                                let half = log_visible_height / 2;
                                if let Some(p) = state.current_mut() { p.page_log_up(half); }
                            }
                            (KeyCode::PageDown, _) => {
                                let half = log_visible_height / 2;
                                if let Some(p) = state.current_mut() {
                                    p.page_log_down(half, log_visible_height);
                                }
                            }

                            _ => {}
                        }
                    }
                }

                terminal.draw(|f| { log_visible_height = ui::draw(f, &state); })?;
            }

            AppEvent::Sse(tagged) => {
                let user_touched = state.user_touched;
                if let Some(pipeline) = state.pipeline_mut(&tagged.run_name) {
                    let task_name = match &tagged.event {
                        SseEvent::Log(log)        => Some(log.task.clone()),
                        SseEvent::StepStatus(upd) => Some(upd.task.clone()),
                        SseEvent::TaskStatus(upd) => Some(upd.task.clone()),
                        _ => None,
                    };

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

                    if let Some(task_name) = task_name {
                        pipeline.maybe_auto_advance(user_touched, &task_name);
                    }
                }
                terminal.draw(|f| { log_visible_height = ui::draw(f, &state); })?;
            }
        }
    }

    Ok(())
}