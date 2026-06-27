// src/bin/logs_run/tui/state.rs

use std::collections::HashMap;
use crate::shared::cli::logs_run::wire::{
    LogLine, RunDone, RunMeta, RunSource, RunStatus, StepStatusUpdate, TaskStatusUpdate,
};

// ── Focus ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Focus {
    PipelineList,
    TaskLog,
    Logs,
}

// ── Per-pipeline task selection ───────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskSelection {
    Task(String),
    Step { task: String, step: String },
}

impl TaskSelection {
    pub fn task_name(&self) -> &str {
        match self {
            TaskSelection::Task(t) => t,
            TaskSelection::Step { task, .. } => task,
        }
    }
}

// ── Per-pipeline task/step state ──────────────────────────────────────────

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

#[derive(Debug, Clone)]
pub struct StoredLogLine {
    pub task: String,
    pub step: String,
    pub timestamp: String,
    pub text: String,
}

// ── Per-pipeline state ────────────────────────────────────────────────────

#[derive(Debug)]
pub struct PipelineState {
    pub run_name: String,
    pub pipeline_name: Option<String>,
    pub source: RunSource,
    pub run_status: RunStatus,
    pub run_done: bool,
    pub duration_seconds: Option<i64>,
    pub error: Option<String>,

    pub tasks: Vec<TaskState>,
    task_index: HashMap<String, usize>,
    pub log_lines: Vec<StoredLogLine>,

    pub selection: Option<TaskSelection>,
    pub flat_rows: Vec<TaskSelection>,
    pub cursor_pos: usize,

    pub log_scroll: usize,
    pub log_follow: bool,
}

impl PipelineState {
    pub fn new(run_name: String) -> Self {
        PipelineState {
            run_name,
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

    // ── SSE handlers ──────────────────────────────────────────────────────

    pub fn apply_meta(&mut self, meta: RunMeta) {
        self.pipeline_name = meta.pipeline_name;
        self.source = meta.source;
        self.run_status = meta.status;

        for (i, task) in meta.tasks.into_iter().enumerate() {
            let ts = TaskState {
                name: task.name.clone(),
                status: task.status,
                reason: task.reason,
                steps: task.steps.into_iter().map(|s| StepState {
                    name: s.name,
                    status: s.status,
                    reason: s.reason,
                }).collect(),
            };
            self.task_index.insert(task.name, i);
            self.tasks.push(ts);
        }

        if !self.tasks.is_empty() {
            self.selection = Some(TaskSelection::Task(self.tasks[0].name.clone()));
        }
        self.rebuild_flat_rows();
    }

    pub fn apply_log_line(&mut self, log: LogLine) {
        let ts = log.timestamp.as_deref().map(|t| {
            if t.contains('T') {
                t.split('T').nth(1)
                    .map(|t| t.trim_end_matches('Z'))
                    .map(|t| t.split('.').next().unwrap_or(t))
                    .unwrap_or("--:--:--")
                    .to_string()
            } else {
                t.parse::<u64>().map(|ns| {
                    let secs = ns / 1_000_000_000;
                    format!("{:02}:{:02}:{:02}",
                        (secs % 86400) / 3600,
                        (secs % 3600) / 60,
                        secs % 60)
                }).unwrap_or_else(|_| "--:--:--".to_string())
            }
        }).unwrap_or_else(|| "--:--:--".to_string());

        let text = log.line.split('\r')
            .filter(|s| !s.is_empty())
            .last()
            .unwrap_or(&log.line)
            .trim_end()
            .to_string();

        if text.is_empty() { return; }

        self.log_lines.push(StoredLogLine {
            task: log.task.clone(),
            step: log.step.clone(),
            timestamp: ts,
            text,
        });

        if let Some(&ti) = self.task_index.get(&log.task) {
            if matches!(self.tasks[ti].status, RunStatus::Pending | RunStatus::Unknown) {
                self.tasks[ti].status = RunStatus::Running;
            }
            if let Some(step) = self.tasks[ti].steps.iter_mut().find(|s| s.name == log.step) {
                if matches!(step.status, RunStatus::Pending | RunStatus::Unknown) {
                    step.status = RunStatus::Running;
                }
            } else {
                self.tasks[ti].steps.push(StepState {
                    name: log.step,
                    status: RunStatus::Running,
                    reason: None,
                });
                self.rebuild_flat_rows();
            }
        }

        // Only auto-scroll if follow mode is on — if the user has
        // manually scrolled up, don't yank them back to the bottom.
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

    // ── Task navigation ───────────────────────────────────────────────────

    pub fn rebuild_flat_rows(&mut self) {
        self.flat_rows.clear();
        for task in &self.tasks {
            self.flat_rows.push(TaskSelection::Task(task.name.clone()));
            for step in &task.steps {
                self.flat_rows.push(TaskSelection::Step {
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
            // switching task resets log view to follow bottom
            self.log_scroll = usize::MAX;
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

    // ── Log scrolling ─────────────────────────────────────────────────────

    /// Scroll up one line — disables follow mode so new lines don't yank
    /// the view back to the bottom while the user is reading.
    pub fn scroll_log_up(&mut self) {
        let count = self.visible_log_lines().len();
        if count == 0 { return; }
        // Resolve current offset before mutating follow flag
        let current = self.clamped_log_scroll_raw(0); // height=0 → unclamped
        if current == 0 { return; } // already at top, nothing to do
        self.log_follow = false;
        self.log_scroll = current.saturating_sub(1);
    }

    /// Scroll down one line. Re-enables follow mode if we reach the bottom.
    pub fn scroll_log_down(&mut self, visible_height: usize) {
        let count = self.visible_log_lines().len();
        let max = count.saturating_sub(visible_height);
        let current = self.clamped_log_scroll_raw(visible_height);
        if current >= max {
            // At or past the bottom — snap to follow
            self.log_follow = true;
            self.log_scroll = usize::MAX;
        } else {
            self.log_follow = false;
            self.log_scroll = current + 1;
        }
    }

    /// Page up by `page` lines.
    pub fn page_log_up(&mut self, page: usize) {
        let count = self.visible_log_lines().len();
        if count == 0 { return; }
        let current = self.clamped_log_scroll_raw(0);
        if current == 0 { return; }
        self.log_follow = false;
        self.log_scroll = current.saturating_sub(page);
    }

    /// Page down by `page` lines. Re-enables follow if we reach the bottom.
    pub fn page_log_down(&mut self, page: usize, visible_height: usize) {
        let count = self.visible_log_lines().len();
        let max = count.saturating_sub(visible_height);
        let current = self.clamped_log_scroll_raw(visible_height);
        let next = (current + page).min(max);
        if next >= max {
            self.log_follow = true;
            self.log_scroll = usize::MAX;
        } else {
            self.log_follow = false;
            self.log_scroll = next;
        }
    }

    /// Re-enable auto-scroll and jump to the bottom.
    pub fn follow_logs(&mut self) {
        self.log_follow = true;
        self.log_scroll = usize::MAX;
    }

    // ── Queries ───────────────────────────────────────────────────────────

    pub fn visible_log_lines(&self) -> Vec<&StoredLogLine> {
        match &self.selection {
            None => vec![],
            Some(TaskSelection::Task(task)) => self.log_lines.iter()
                .filter(|l| &l.task == task).collect(),
            Some(TaskSelection::Step { task, step }) => self.log_lines.iter()
                .filter(|l| &l.task == task && &l.step == step).collect(),
        }
    }

    /// Clamped scroll offset for rendering. Pass the actual visible height.
    pub fn clamped_log_scroll(&self, visible_height: usize) -> usize {
        self.clamped_log_scroll_raw(visible_height)
    }

    /// Internal — computes the real offset regardless of follow flag,
    /// used by scroll methods to know where we currently are.
    fn clamped_log_scroll_raw(&self, visible_height: usize) -> usize {
        let count = self.visible_log_lines().len();
        let max = count.saturating_sub(visible_height);
        if self.log_follow || self.log_scroll > max { max } else { self.log_scroll }
    }
}

// ── Top-level app state ───────────────────────────────────────────────────

#[derive(Debug)]
pub struct AppState {
    pub pipelines: Vec<PipelineState>,
    pub selected_pipeline: usize,
    pub focus: Focus,
}

impl AppState {
    pub fn new(run_names: Vec<String>) -> Self {
        // Skip pipeline list panel if there's only one run — go straight
        // to tasks so there's no extra keypress for the common case.
        let focus = if run_names.len() == 1 {
            Focus::TaskLog
        } else {
            Focus::PipelineList
        };
        let pipelines = run_names.into_iter().map(PipelineState::new).collect();
        AppState {
            pipelines,
            selected_pipeline: 0,
            focus,
        }
    }

    pub fn pipeline_mut(&mut self, run_name: &str) -> Option<&mut PipelineState> {
        self.pipelines.iter_mut().find(|p| p.run_name == run_name)
    }

    pub fn current(&self) -> Option<&PipelineState> {
        self.pipelines.get(self.selected_pipeline)
    }

    pub fn current_mut(&mut self) -> Option<&mut PipelineState> {
        self.pipelines.get_mut(self.selected_pipeline)
    }

    pub fn select_prev_pipeline(&mut self) {
        if self.selected_pipeline > 0 {
            self.selected_pipeline -= 1;
        }
    }

    pub fn select_next_pipeline(&mut self) {
        if self.selected_pipeline + 1 < self.pipelines.len() {
            self.selected_pipeline += 1;
        }
    }
}