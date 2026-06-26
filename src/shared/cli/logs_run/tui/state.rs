// src/bin/logs_run/tui/state.rs
//
// `AppState` is the single source of truth for the TUI. Every incoming SSE
// event mutates it in place; the UI reads from it on every frame. Nothing
// is ever "printed once" — status icons, task states, and the run header
// always reflect current state at render time.

use std::collections::HashMap;
use crate::shared::cli::logs_run::wire::{
    LogLine, RunDone, RunMeta, RunSource, RunStatus, StepStatusUpdate, TaskStatusUpdate,
};

// ── Selection model ───────────────────────────────────────────────────────
//
// The left pane is a flat list of rows: one row per task, optionally
// followed by indented step rows when that task is expanded. The cursor
// moves over this flat list. `Selection` records what's currently under
// the cursor so the right pane knows which logs to show.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Task(String),
    Step { task: String, step: String },
}

impl Selection {
    pub fn task_name(&self) -> &str {
        match self {
            Selection::Task(t) => t,
            Selection::Step { task, .. } => task,
        }
    }
}

// ── Live task / step state ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct StepState {
    pub name: String,
    pub status: RunStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaskState {
    pub name: String,
    pub status: RunStatus,
    pub reason: Option<String>,
    pub steps: Vec<StepState>,
}

// ── Log storage ───────────────────────────────────────────────────────────
//
// Each log line is stored with a display timestamp (just the HH:MM:SS
// portion of whatever the server sent). The right pane filters from
// `AppState::log_lines` by matching `(task, step)` against the current
// selection.

#[derive(Debug, Clone)]
pub struct StoredLogLine {
    pub task: String,
    pub step: String,
    pub timestamp: String,
    pub text: String,
}

// ── The app state ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct AppState {
    pub run_name: String,
    pub pipeline_name: Option<String>,
    pub source: RunSource,
    pub run_status: RunStatus,
    pub run_done: bool,
    pub duration_seconds: Option<i64>,
    pub error: Option<String>,

    /// Task list in pipeline-declared order (preserves DAG ordering from
    /// the server).
    pub tasks: Vec<TaskState>,

    /// Index into `tasks` for fast lookup by name.
    task_index: HashMap<String, usize>,

    /// All log lines, in arrival order. Right pane filters on demand.
    pub log_lines: Vec<StoredLogLine>,

    // ── Navigation ────────────────────────────────────────────────────
    /// The currently-highlighted item in the flat left-pane list.
    pub selection: Option<Selection>,
    /// Flat list of rows currently visible in the left pane, rebuilt on
    /// every state-mutating call that might change task/step expansion.
    /// Each entry is a `Selection` variant so `cursor_pos` indexes into it.
    pub flat_rows: Vec<Selection>,
    /// Index into `flat_rows` for the cursor.
    pub cursor_pos: usize,

    /// Vertical scroll offset for the right (log) pane.
    pub log_scroll: usize,
    /// Whether to auto-scroll the log pane to the bottom as new lines arrive.
    pub log_follow: bool,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            run_name: String::new(),
            pipeline_name: None,
            source: RunSource::Tekton,
            run_status: RunStatus::Pending,
            run_done: false,
            duration_seconds: None,
            error: None,
            tasks: Vec::new(),
            task_index: HashMap::new(),
            log_lines: Vec::new(),
            selection: None,
            flat_rows: Vec::new(),
            cursor_pos: 0,
            log_scroll: 0,
            log_follow: true,
        }
    }

    // ── SSE event handlers ────────────────────────────────────────────────

    pub fn apply_meta(&mut self, meta: RunMeta) {
        self.run_name = meta.run_name;
        self.pipeline_name = meta.pipeline_name;
        self.source = meta.source;
        self.run_status = meta.status;

        for (i, task) in meta.tasks.into_iter().enumerate() {
            let ts = TaskState {
                name: task.name.clone(),
                status: task.status,
                reason: task.reason,
                steps: task
                    .steps
                    .into_iter()
                    .map(|s| StepState {
                        name: s.name,
                        status: s.status,
                        reason: s.reason,
                    })
                    .collect(),
            };
            self.task_index.insert(task.name, i);
            self.tasks.push(ts);
        }

        // Default selection: first task.
        if !self.tasks.is_empty() {
            self.selection = Some(Selection::Task(self.tasks[0].name.clone()));
        }
        self.rebuild_flat_rows();
    }

    pub fn apply_log_line(&mut self, log: LogLine) {
        let ts = log
            .timestamp
            .as_deref()
            .and_then(|t| t.split('T').nth(1))
            .map(|t| t.trim_end_matches('Z'))
            .map(|t| t.split('.').next().unwrap_or(t))
            .unwrap_or("--:--:--")
            .to_string();

        let text = log
            .line
            .split('\r')
            .filter(|s| !s.is_empty())
            .last()
            .unwrap_or(&log.line)
            .trim_end()
            .to_string();

        if text.is_empty() {
            return;
        }

        self.log_lines.push(StoredLogLine {
            task: log.task.clone(),
            step: log.step.clone(),
            timestamp: ts,
            text,
        });

        // If we're receiving logs for a task/step that's still Pending or
        // Unknown, it must be running — promote it. We never demote: a step
        // already Succeeded or Failed stays that way even if a stale log
        // line arrives out of order (can happen with archived runs).
        if let Some(&ti) = self.task_index.get(&log.task) {
            if matches!(self.tasks[ti].status, RunStatus::Pending | RunStatus::Unknown) {
                self.tasks[ti].status = RunStatus::Running;
            }

            if let Some(step) = self.tasks[ti].steps.iter_mut().find(|s| s.name == log.step) {
                if matches!(step.status, RunStatus::Pending | RunStatus::Unknown) {
                    step.status = RunStatus::Running;
                }
            } else {
                // Step not yet declared (meta sent 0 steps) — create it as Running.
                self.tasks[ti].steps.push(StepState {
                    name: log.step,
                    status: RunStatus::Running,
                    reason: None,
                });
                self.rebuild_flat_rows();
            }
        }

        if self.log_follow {
            self.log_scroll = usize::MAX;
        }
    }

    pub fn apply_step_status(&mut self, upd: StepStatusUpdate) {
        if let Some(&ti) = self.task_index.get(&upd.task) {
            if let Some(step) = self.tasks[ti].steps.iter_mut().find(|s| s.name == upd.step) {
                step.status = upd.status;
                step.reason = upd.reason;
            } else {
                // Step wasn't declared in meta (server sent 0 steps for this
                // task) — create it on first status update so it appears in
                // the list and can be expanded/selected.
                self.tasks[ti].steps.push(StepState {
                    name: upd.step,
                    status: upd.status,
                    reason: upd.reason,
                });
                self.rebuild_flat_rows();
            }
        }
    }

    pub fn apply_task_status(&mut self, upd: TaskStatusUpdate) {
        if let Some(&ti) = self.task_index.get(&upd.task) {
            self.tasks[ti].status = upd.status;
            self.tasks[ti].reason = upd.reason;
        }
    }

    pub fn apply_done(&mut self, done: RunDone) {
        self.run_status = done.status;
        self.run_done = true;
        self.duration_seconds = done.duration_seconds;
    }

    pub fn apply_error(&mut self, msg: String) {
        self.error = Some(msg);
    }

    // ── Navigation ────────────────────────────────────────────────────────

    /// Rebuild the flat list of navigable rows from current task list +
    /// expansion state. Called after any expansion toggle or meta update.
    pub fn rebuild_flat_rows(&mut self) {
        self.flat_rows.clear();
        for task in &self.tasks {
            self.flat_rows.push(Selection::Task(task.name.clone()));
            for step in &task.steps {
                self.flat_rows.push(Selection::Step {
                    task: task.name.clone(),
                    step: step.name.clone(),
                });
            }
        }
        if self.cursor_pos >= self.flat_rows.len() {
            self.cursor_pos = self.flat_rows.len().saturating_sub(1);
        }
        self.selection = self.flat_rows.get(self.cursor_pos).cloned();
    }

    pub fn move_up(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
            self.selection = self.flat_rows.get(self.cursor_pos).cloned();
            self.log_scroll = usize::MAX; // auto-scroll to bottom for new selection
            self.log_follow = true;
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor_pos + 1 < self.flat_rows.len() {
            self.cursor_pos += 1;
            self.selection = self.flat_rows.get(self.cursor_pos).cloned();
            self.log_scroll = usize::MAX;
            self.log_follow = true;
        }
    }

    /// Expand (→) or collapse (←) the selected task. If a step is
    /// selected, collapse takes you back to the parent task row.
    pub fn expand_or_collapse(&mut self, expand: bool) {
        match self.selection.clone() {
            Some(Selection::Task(ref task_name)) => {
                if let Some(&ti) = self.task_index.get(task_name) {
                    self.rebuild_flat_rows();
                    // After expanding, move cursor to first step if it exists.
                    if expand && !self.tasks[ti].steps.is_empty() {
                        // Find the step row that was just added.
                        if let Some(pos) = self.flat_rows.iter().position(|r| {
                            matches!(r, Selection::Step { task, .. } if task == task_name)
                        }) {
                            self.cursor_pos = pos;
                            self.selection = self.flat_rows.get(pos).cloned();
                        }
                    }
                }
            }
            Some(Selection::Step { ref task, .. }) => {
                if !expand {
                    // Collapse: move cursor to parent task row.
                    if let Some(&ti) = self.task_index.get(task) {
                        self.rebuild_flat_rows();
                        if let Some(pos) = self
                            .flat_rows
                            .iter()
                            .position(|r| matches!(r, Selection::Task(t) if t == task))
                        {
                            self.cursor_pos = pos;
                            self.selection = self.flat_rows.get(pos).cloned();
                        }
                    }
                }
            }
            None => {}
        }
        self.log_scroll = usize::MAX;
        self.log_follow = true;
    }

    pub fn scroll_log_up(&mut self) {
        self.log_follow = false;
        self.log_scroll = self.log_scroll.saturating_sub(1);
    }

    pub fn scroll_log_down(&mut self, visible_lines: usize) {
        let count = self.visible_log_lines().len();
        let max = count.saturating_sub(visible_lines);
        if self.log_scroll < max {
            self.log_scroll += 1;
        } else {
            self.log_follow = true;
        }
    }

    // ── Queries ───────────────────────────────────────────────────────────

    /// Lines to display in the right pane, filtered by current selection.
    pub fn visible_log_lines(&self) -> Vec<&StoredLogLine> {
        match &self.selection {
            None => vec![],
            Some(Selection::Task(task)) => self
                .log_lines
                .iter()
                .filter(|l| &l.task == task)
                .collect(),
            Some(Selection::Step { task, step }) => self
                .log_lines
                .iter()
                .filter(|l| &l.task == task && &l.step == step)
                .collect(),
        }
    }

    /// Resolved log scroll offset, clamped to valid range.
    pub fn clamped_log_scroll(&self, visible_height: usize) -> usize {
        let count = self.visible_log_lines().len();
        let max = count.saturating_sub(visible_height);
        if self.log_follow || self.log_scroll > max {
            max
        } else {
            self.log_scroll
        }
    }
}