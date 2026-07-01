// src/bin/logs_run/tui/state.rs

use std::collections::{HashMap, HashSet};
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

/// One renderable row in the accordion log view: either a step header
/// (always shown) or a log line belonging to the currently-expanded step.
#[derive(Debug)]
pub enum LogRow<'a> {
    Header { step: String, status: RunStatus },
    Line(&'a StoredLogLine),
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

    pub commit_sha: Option<String>,
    pub commit_message: Option<String>,

    pub tasks: Vec<TaskState>,
    task_index: HashMap<String, usize>,
    pub log_lines: Vec<StoredLogLine>,

    pub selected_task: Option<String>,
    pub cursor_pos: usize,

    pub log_scroll: usize,
    pub log_follow: bool,
    /// When true all step sections are collapsed to headers only.
    pub logs_collapsed: bool,
    /// Which step header is highlighted while collapsed (index into
    /// current task's steps vec). Ignored when expanded.
    pub step_cursor: usize,
    /// Horizontal scroll offset for log line *text* (chars). The
    /// timestamp column is frozen and always visible.
    pub log_h_scroll: usize,

    activated_tasks: HashSet<String>,
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
            commit_sha: None,
            commit_message: None,
            tasks: Vec::new(),
            task_index: HashMap::new(),
            log_lines: Vec::new(),
            selected_task: None,
            cursor_pos: 0,
            log_scroll: 0,
            log_follow: true,
            logs_collapsed: false,
            step_cursor: 0,
            log_h_scroll: 0,
            activated_tasks: HashSet::new(),
        }
    }

    pub fn with_commit(mut self, sha: Option<String>, message: Option<String>) -> Self {
        self.commit_sha = sha;
        self.commit_message = message;
        self
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

        if !self.tasks.is_empty() && self.selected_task.is_none() {
            self.cursor_pos = 0;
            let first = self.tasks[0].name.clone();
            self.selected_task = Some(first.clone());
            self.activated_tasks.insert(first);
        }
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
                self.tasks[ti].steps.push(StepState {
                    name: upd.step,
                    status: upd.status,
                    reason: upd.reason,
                });
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

    // ── Auto-advance ──────────────────────────────────────────────────────

    pub fn maybe_auto_advance(&mut self, user_touched: bool, task_name: &str) {
        let first_time = !self.activated_tasks.contains(task_name);
        if first_time {
            self.activated_tasks.insert(task_name.to_string());
        }
        if user_touched || !first_time { return; }

        let Some(&new_idx) = self.task_index.get(task_name) else { return; };
        let is_ahead = match self.selected_task.as_ref() {
            None => true,
            Some(cur) => self.task_index.get(cur).map_or(true, |&ci| new_idx > ci),
        };
        if is_ahead {
            self.cursor_pos = new_idx;
            self.selected_task = Some(task_name.to_string());
            self.log_follow = true;
            self.log_scroll = usize::MAX;
            self.step_cursor = 0;
            self.log_h_scroll = 0;
        }
    }

    // ── Task navigation ────────────────────────────────────────────────────

    pub fn move_up(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
            self.selected_task = self.tasks.get(self.cursor_pos).map(|t| t.name.clone());
            self.log_scroll = usize::MAX;
            self.log_follow = true;
            self.step_cursor = 0;
            self.log_h_scroll = 0;
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor_pos + 1 < self.tasks.len() {
            self.cursor_pos += 1;
            self.selected_task = self.tasks.get(self.cursor_pos).map(|t| t.name.clone());
            self.log_scroll = usize::MAX;
            self.log_follow = true;
            self.step_cursor = 0;
            self.log_h_scroll = 0;
        }
    }

    // ── Log accordion collapse/expand ─────────────────────────────────────

    /// Space: toggle. Collapsing resets the step cursor to the top.
    /// Expanding via Space jumps to follow mode (same as Ctrl+↓).
    pub fn toggle_logs_collapse(&mut self) {
        if self.logs_collapsed {
            // Expand — jump to follow mode so the user sees live output.
            self.logs_collapsed = false;
            self.log_follow = true;
            self.log_scroll = usize::MAX;
        } else {
            // Collapse — reset step cursor to the top of the step list.
            self.logs_collapsed = true;
            self.step_cursor = 0;
            self.log_scroll = 0;
            self.log_follow = false;
        }
    }

    /// Enter (while collapsed): expand and scroll so the selected step's
    /// header is at the top of the content area.
    pub fn expand_to_step(&mut self) {
        let task_name = match self.selected_task.clone() { Some(t) => t, None => return };
        // Collect step names first to avoid holding a borrow while mutating self.
        let step_names: Vec<String> = self.tasks.iter()
            .find(|t| t.name == task_name)
            .map(|t| t.steps.iter().map(|s| s.name.clone()).collect())
            .unwrap_or_default();

        let mut row_idx: usize = 0;
        for (i, step_name) in step_names.iter().enumerate() {
            if i == self.step_cursor {
                self.logs_collapsed = false;
                self.log_follow = false;
                self.log_scroll = row_idx;
                return;
            }
            row_idx += 1; // header row
            row_idx += self.log_lines.iter()
                .filter(|l| l.task == task_name && l.step == *step_name)
                .count();
        }
        // step_cursor out of range — just expand from the top
        self.logs_collapsed = false;
        self.log_follow = true;
        self.log_scroll = usize::MAX;
    }

    // ── Step cursor navigation (used while collapsed) ─────────────────────

    pub fn collapsed_select_up(&mut self) {
        if self.step_cursor == 0 { return; }
        self.step_cursor -= 1;
        self.log_follow = false;
        // Scroll up if the cursor went above the visible window.
        if self.step_cursor < self.log_scroll {
            self.log_scroll = self.step_cursor;
        }
    }

    pub fn collapsed_select_down(&mut self, visible_height: usize) {
        let step_count = self.selected_task.as_ref()
            .and_then(|tn| self.tasks.iter().find(|t| &t.name == tn))
            .map(|t| t.steps.len())
            .unwrap_or(0);
        if self.step_cursor + 1 >= step_count { return; }
        self.step_cursor += 1;
        self.log_follow = false;
        // Scroll down if the cursor went below the visible window.
        if visible_height > 0 && self.step_cursor >= self.log_scroll + visible_height {
            self.log_scroll = self.step_cursor + 1 - visible_height;
        }
    }

    // ── Horizontal scroll ─────────────────────────────────────────────────

    /// Scroll log line text left (towards col 0).
    pub fn scroll_log_left(&mut self, amount: usize) {
        self.log_h_scroll = self.log_h_scroll.saturating_sub(amount);
    }

    /// Scroll log line text right.
    pub fn scroll_log_right(&mut self, amount: usize) {
        self.log_h_scroll += amount;
    }

    // ── Vertical scroll (expanded mode) ───────────────────────────────────

    pub fn scroll_log_up(&mut self) {
        let count = self.visible_log_rows().len();
        if count == 0 { return; }
        let current = self.clamped_log_scroll_raw(0);
        if current == 0 { return; }
        self.log_follow = false;
        self.log_scroll = current.saturating_sub(1);
    }

    pub fn scroll_log_down(&mut self, visible_height: usize) {
        let count = self.visible_log_rows().len();
        let max = count.saturating_sub(visible_height);
        let current = self.clamped_log_scroll_raw(visible_height);
        if current >= max {
            self.log_follow = true;
            self.log_scroll = usize::MAX;
        } else {
            self.log_follow = false;
            self.log_scroll = current + 1;
        }
    }

    pub fn page_log_up(&mut self, page: usize) {
        let count = self.visible_log_rows().len();
        if count == 0 { return; }
        let current = self.clamped_log_scroll_raw(0);
        if current == 0 { return; }
        self.log_follow = false;
        self.log_scroll = current.saturating_sub(page);
    }

    pub fn page_log_down(&mut self, page: usize, visible_height: usize) {
        let count = self.visible_log_rows().len();
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

    pub fn follow_logs(&mut self) {
        self.log_follow = true;
        self.log_scroll = usize::MAX;
        self.log_h_scroll = 0;
    }

    // ── Accordion rows ────────────────────────────────────────────────────

    pub fn visible_log_rows(&self) -> Vec<LogRow<'_>> {
        let mut rows = Vec::new();
        let Some(task_name) = self.selected_task.as_ref() else { return rows; };
        let Some(task) = self.tasks.iter().find(|t| &t.name == task_name) else { return rows; };

        for step in &task.steps {
            rows.push(LogRow::Header { step: step.name.clone(), status: step.status });
            if !self.logs_collapsed {
                for line in self.log_lines.iter()
                    .filter(|l| &l.task == task_name && l.step == step.name)
                {
                    rows.push(LogRow::Line(line));
                }
            }
        }
        rows
    }

    // ── Scroll helpers ────────────────────────────────────────────────────

    pub fn clamped_log_scroll(&self, visible_height: usize) -> usize {
        self.clamped_log_scroll_raw(visible_height)
    }

    fn clamped_log_scroll_raw(&self, visible_height: usize) -> usize {
        let count = self.visible_log_rows().len();
        let max = count.saturating_sub(visible_height);
        if self.log_follow || self.log_scroll > max { max } else { self.log_scroll }
    }

    /// Returns the name of the step currently under the step cursor,
    /// used by the UI to highlight that header row.
    pub fn selected_step_name(&self) -> Option<&str> {
        if !self.logs_collapsed { return None; }
        self.selected_task.as_ref()
            .and_then(|tn| self.tasks.iter().find(|t| &t.name == tn))
            .and_then(|t| t.steps.get(self.step_cursor))
            .map(|s| s.name.as_str())
    }
}

// ── Top-level app state ───────────────────────────────────────────────────

#[derive(Debug)]
pub struct AppState {
    pub pipelines: Vec<PipelineState>,
    pub selected_pipeline: usize,
    pub focus: Focus,
    pub user_touched: bool,
}

impl AppState {
    pub fn new(targets: &[super::super::RunTarget]) -> Self {
        let pipelines = targets
            .iter()
            .map(|t| {
                PipelineState::new(t.run_name.clone())
                    .with_commit(t.commit_sha.clone(), t.commit_message.clone())
            })
            .collect();
        AppState {
            pipelines,
            selected_pipeline: 0,
            focus: Focus::TaskLog,
            user_touched: false,
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
        if self.selected_pipeline > 0 { self.selected_pipeline -= 1; }
    }

    pub fn select_next_pipeline(&mut self) {
        if self.selected_pipeline + 1 < self.pipelines.len() { self.selected_pipeline += 1; }
    }
}