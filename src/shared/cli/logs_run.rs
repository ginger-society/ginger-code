// src/bin/logs_run.rs  (new module — see bottom of file for cli.rs wiring diff)
//
// Implements `ginger-code logs-run <RUN_NAME>`: connects to the
// tekton-sidekick SSE endpoint (`GET /runs/<run_name>/stream`) and renders
// the event stream live in the terminal.
//
// Not a TUI — no alternate screen, no redraw-in-place. It's a flat,
// append-only stream, and deliberately does NOT use nested box-drawing
// (┌─/│/└─) per task. That looked good for a single linear task-by-task
// run, but tekton-sidekick streams tasks CONCURRENTLY when they have no
// dependency on each other — multiple tasks' logs can interleave at the
// line level, in true arrival order. A single terminal cursor can't keep
// several "open boxes" visually nested at once without a real TUI doing
// redraw, so instead every line gets a fixed-width `[task   step  ]`
// prefix, colored consistently per task, and lines just interleave
// naturally as they arrive:
//
//   ▶ PipelineRun parallel-ci-pipeline  my-run  [live: tekton]
//
//     ○ fetch       1 step
//     ○ lint        0 steps
//     ○ test        0 steps
//
//   [fetch   ▸clone    ] 06:36:05  ==> Cloning...
//   [fetch   ▸clone    ] 06:36:18  ==> Clone complete.
//   [fetch              ] ✓ succeeded  Succeeded
//   [lint    ▸lint      ] 06:36:26  ==> Running linter...
//   [test    ▸unit-test ] 06:36:27  ==> Running test suite...
//   [lint    ▸lint      ] 06:36:31  [lint] no issues found
//   [test    ▸unit-test ] 06:36:31  [PASS] test_case_1
//   [lint               ] ✓ succeeded  Succeeded
//   [test               ] ✓ succeeded  Succeeded
//
//   ✓ PipelineRun my-run succeeded  (42s)
//
// Each task name gets one color from a small fixed palette (stable for
// the life of the process, assigned the first time that task is seen),
// so your eye can track a task down the column even as other tasks'
// lines interleave between its own. Status color (green/red/yellow/dim)
// still carries the ✓/✗/● icons. All of this degrades gracefully when
// piped to a file: ANSI codes strip, the bracketed prefix alone is still
// enough to tell which line belongs to which task/step.

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde::Deserialize;

// ── ANSI helpers ─────────────────────────────────────────────────────────
//
// Kept as plain ANSI codes rather than reaching for crossterm's styling
// API. crossterm is already a dependency here for this binary's TUI
// commands, but its style API is built around cursor-positioned terminal
// control — overkill for a linear, append-only, scroll-as-you-go stream
// like this one, which just needs to behave when piped or redirected.

fn green(s: &str) -> String { format!("\x1b[1;32m{s}\x1b[0m") }
fn red(s: &str) -> String { format!("\x1b[1;31m{s}\x1b[0m") }
fn yellow(s: &str) -> String { format!("\x1b[1;33m{s}\x1b[0m") }
fn dim(s: &str) -> String { format!("\x1b[2m{s}\x1b[0m") }
fn bold(s: &str) -> String { format!("\x1b[1m{s}\x1b[0m") }
fn cyan(s: &str) -> String { format!("\x1b[36m{s}\x1b[0m") }

/// Small fixed palette for per-task coloring of the `[task ...]` prefix.
/// Picked to stay readable on both light and dark terminal backgrounds
/// and to stay visually distinct from the status colors (green/red/
/// yellow are reserved for ✓/✗/● and intentionally excluded here).
const TASK_PALETTE: &[u8] = &[
    34, // blue
    35, // magenta
    36, // cyan
    33, // yellow/orange-ish (kept dim enough not to clash with status yellow)
    32, // green (dimmed below so it doesn't read as a status color)
    37, // white/gray
];

fn task_color(code: u8, s: &str) -> String {
    format!("\x1b[1;{code}m{s}\x1b[0m")
}

// ── Wire types — mirror tekton-sidekick's models::run_stream payloads ──
//
// Kept local rather than pulled from a shared crate, matching how this
// CLI already seems to vendor small response-shape structs per command.
// If this project later grows a shared "sidekick-client" crate, these
// move there.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RunSource {
    Tekton,
    Archive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RunStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

impl RunStatus {
    fn icon_and_color(&self) -> String {
        match self {
            RunStatus::Succeeded => green("✓"),
            RunStatus::Failed => red("✗"),
            RunStatus::Running => yellow("●"),
            RunStatus::Pending => dim("○"),
            RunStatus::Unknown => dim("?"),
        }
    }

    fn label(&self) -> String {
        match self {
            RunStatus::Succeeded => green("succeeded"),
            RunStatus::Failed => red("failed"),
            RunStatus::Running => yellow("running"),
            RunStatus::Pending => dim("pending"),
            RunStatus::Unknown => dim("unknown"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct StepMeta {
    name: String,
    #[allow(dead_code)]
    container: String,
    status: RunStatus,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskMeta {
    name: String,
    #[allow(dead_code)]
    task_ref: Option<String>,
    #[allow(dead_code)]
    taskrun_name: String,
    #[allow(dead_code)]
    pod_name: Option<String>,
    status: RunStatus,
    #[allow(dead_code)]
    reason: Option<String>,
    steps: Vec<StepMeta>,
}

#[derive(Debug, Deserialize)]
struct RunMeta {
    run_name: String,
    source: RunSource,
    pipeline_name: Option<String>,
    #[allow(dead_code)]
    status: RunStatus,
    #[allow(dead_code)]
    reason: Option<String>,
    tasks: Vec<TaskMeta>,
}

#[derive(Debug, Deserialize)]
struct LogLine {
    task: String,
    step: String,
    line: String,
    timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StepStatusUpdate {
    task: String,
    step: String,
    status: RunStatus,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskStatusUpdate {
    task: String,
    status: RunStatus,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RunDone {
    run_name: String,
    status: RunStatus,
    reason: Option<String>,
    duration_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct StreamError {
    message: String,
}

// ── Rendering state ─────────────────────────────────────────────────────
//
// Tracks just enough to know when we've moved from one task/step to
// another, so a header prints exactly once and log lines stay indented
// underneath it — no redraw needed, just "have we already announced this
// task/step or not".

struct Renderer {
    /// Width of the task-name column, used both in the upfront skeleton
    /// and in every line's `[task ...]` prefix, so columns stay aligned
    /// down the page even as different tasks' lines interleave.
    task_name_width: usize,
    /// Width of the step-name column within the prefix, same reasoning.
    step_name_width: usize,
    /// Stable color assignment per task, first-seen-first-assigned, so a
    /// given task keeps the same color for the life of the stream
    /// regardless of how its lines interleave with other tasks'.
    task_colors: std::collections::HashMap<String, u8>,
    next_color_idx: usize,
}

impl Renderer {
    fn new(tasks: &[TaskMeta]) -> Self {
        let task_name_width = tasks.iter().map(|t| t.name.len()).max().unwrap_or(8).max(8);
        let step_name_width = tasks
            .iter()
            .flat_map(|t| t.steps.iter())
            .map(|s| s.name.len())
            .max()
            .unwrap_or(8)
            .max(8);
        Renderer {
            task_name_width,
            step_name_width,
            task_colors: std::collections::HashMap::new(),
            next_color_idx: 0,
        }
    }

    /// Get (assigning on first use) this task's color code.
    fn color_for(&mut self, task_name: &str) -> u8 {
        if let Some(&c) = self.task_colors.get(task_name) {
            return c;
        }
        let c = TASK_PALETTE[self.next_color_idx % TASK_PALETTE.len()];
        self.next_color_idx += 1;
        self.task_colors.insert(task_name.to_string(), c);
        c
    }

    /// Build the `[task    step    ]` prefix for a line. `step` is
    /// optional -- task-level lines (status updates) pass `None` and get
    /// blank padding instead of a step name, so the bracket still lines
    /// up with step-level lines above and below it.
    fn prefix(&mut self, task_name: &str, step_name: Option<&str>) -> String {
        let color = self.color_for(task_name);
        let task_part = format!("{:<width$}", task_name, width = self.task_name_width);
        let step_part = match step_name {
            Some(s) => format!("▸{:<width$}", s, width = self.step_name_width),
            None => " ".repeat(self.step_name_width + 1),
        };
        format!("[{} {}]", task_color(color, &task_part), dim(&step_part))
    }

    fn print_run_banner(&mut self, meta: &RunMeta) {
        let source_tag = match meta.source {
            RunSource::Tekton => dim("[live: tekton]"),
            RunSource::Archive => dim("[archived: postgres + loki]"),
        };
        println!();
        println!(
            "{} {}  {}  {}",
            bold("▶"),
            bold(&format!(
                "PipelineRun {}",
                meta.pipeline_name.as_deref().unwrap_or(&meta.run_name)
            )),
            cyan(&meta.run_name),
            source_tag,
        );
        println!();

        // Render the full skeleton up front, dimmed, so the person sees
        // the whole shape of the run before any logs arrive -- this is
        // what makes the "send metadata immediately" behavior visible.
        // Pre-assign every task a color here too, in pipeline-declared
        // order, so colors are consistent and predictable rather than
        // depending on which task happens to log its first line first.
        for task in &meta.tasks {
            self.color_for(&task.name);
            let icon = task.status.icon_and_color();
            println!(
                "  {} {}",
                icon,
                dim(&format!(
                    "{:<width$}  {} step{}",
                    task.name,
                    task.steps.len(),
                    if task.steps.len() == 1 { "" } else { "s" },
                    width = self.task_name_width
                ))
            );
        }
        println!();
    }

    fn print_log_line(&mut self, log: &LogLine) {
        let prefix = self.prefix(&log.task, Some(&log.step));
        let ts = log
            .timestamp
            .as_deref()
            .and_then(|t| t.split('T').nth(1)) // keep just the time-of-day part
            .map(|t| t.trim_end_matches('Z'))
            .map(|t| t.split('.').next().unwrap_or(t))
            .unwrap_or("--:--:--");
        println!("{} {}  {}", prefix, dim(ts), log.line);
    }

    fn print_step_status(&mut self, upd: &StepStatusUpdate) {
        let prefix = self.prefix(&upd.task, Some(&upd.step));
        let icon = upd.status.icon_and_color();
        let reason = upd
            .reason
            .as_deref()
            .map(|r| format!("  {}", dim(r)))
            .unwrap_or_default();
        println!("{} {} {}{}", prefix, icon, upd.status.label(), reason);
    }

    fn print_task_status(&mut self, upd: &TaskStatusUpdate) {
        let prefix = self.prefix(&upd.task, None);
        let icon = upd.status.icon_and_color();
        let reason = upd
            .reason
            .as_deref()
            .map(|r| format!("  {}", dim(r)))
            .unwrap_or_default();
        println!("{} {} {}{}", prefix, icon, upd.status.label(), reason);
    }

    fn print_done(&self, done: &RunDone) {
        let icon = done.status.icon_and_color();
        let duration = done
            .duration_seconds
            .map(|d| format!("  ({d}s)"))
            .unwrap_or_default();
        let reason = done
            .reason
            .as_deref()
            .map(|r| format!("  {}", dim(r)))
            .unwrap_or_default();
        println!();
        println!(
            "{} PipelineRun {} {}{}{}",
            icon,
            bold(&done.run_name),
            done.status.label(),
            duration,
            reason
        );
        println!();
    }

    fn print_error(&self, err: &StreamError) {
        eprintln!("{}  {}", red("✗"), err.message);
    }
}

// ── Entry point ──────────────────────────────────────────────────────────

/// Stream a pipeline run's logs from tekton-sidekick and render them live.
///
/// `base_url` is the sidekick service's base URL (e.g.
/// `http://tekton-sidekick.default.svc.cluster.local`, or
/// `http://localhost:8000` via port-forward) -- passed in by the caller
/// rather than hardcoded, since it differs between local dev and in-cluster
/// use.
pub async fn stream_run_logs(base_url: &str, run_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!(
        "{}/runs/{}/stream",
        base_url.trim_end_matches('/'),
        urlencode_path_segment(run_name)
    );

    let client = reqwest::Client::new();
    let response = client.get(&url).send().await?;

    if !response.status().is_success() {
        eprintln!(
            "{}  sidekick returned HTTP {} for run '{}'",
            red("✗"),
            response.status(),
            run_name
        );
        std::process::exit(1);
    }

    let mut stream = response.bytes_stream().eventsource();
    let mut renderer: Option<Renderer> = None;
    // Holds events that arrive before `meta` (shouldn't happen per the
    // protocol over a single ordered HTTP stream, but cheap insurance
    // against rendering out of order if it ever does).
    let mut pending: Vec<(String, String)> = Vec::new();

    while let Some(event) = stream.next().await {
        let event = match event {
            Ok(e) => e,
            Err(e) => {
                eprintln!("{}  connection error: {e}", red("✗"));
                break;
            }
        };

        if renderer.is_none() && event.event != "meta" {
            pending.push((event.event.clone(), event.data.clone()));
            continue;
        }

        if event.event == "meta" {
            let meta: RunMeta = serde_json::from_str(&event.data)?;
            let mut r = Renderer::new(&meta.tasks);
            r.print_run_banner(&meta);
            renderer = Some(r);

            for (ev, data) in pending.drain(..) {
                dispatch(&ev, &data, renderer.as_mut().unwrap())?;
            }
            continue;
        }

        dispatch(&event.event, &event.data, renderer.as_mut().unwrap())?;

        if event.event == "done" {
            break;
        }
    }

    Ok(())
}

fn dispatch(event_name: &str, data: &str, renderer: &mut Renderer) -> Result<(), Box<dyn std::error::Error>> {
    match event_name {
        "log" => renderer.print_log_line(&serde_json::from_str::<LogLine>(data)?),
        "step-status" => renderer.print_step_status(&serde_json::from_str::<StepStatusUpdate>(data)?),
        "task-status" => renderer.print_task_status(&serde_json::from_str::<TaskStatusUpdate>(data)?),
        "done" => renderer.print_done(&serde_json::from_str::<RunDone>(data)?),
        "error" => renderer.print_error(&serde_json::from_str::<StreamError>(data)?),
        other => {
            eprintln!("{}  (ignoring unknown event type '{other}')", dim("·"));
        }
    }
    Ok(())
}

/// Minimal path-segment escaping for the run name. Tekton run names are
/// Kubernetes resource names (DNS-1123 subdomains: lowercase alphanumerics
/// and `-`), so this never actually needs to escape anything in practice --
/// it's one line of insurance against a malformed/copy-pasted name with a
/// stray space or slash breaking the URL.
fn urlencode_path_segment(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32)
            }
        })
        .collect()
}

// =============================================================================
// Wiring into src/bin/cli.rs -- diff against the file you shared
// =============================================================================
//
// 1. Add to Cargo.toml's [dependencies]:
//
//      reqwest = { version = "0.12", default-features = false, features = ["json", "stream", "rustls-tls"] }
//      eventsource-stream = "0.2"
//
//    (futures-util doesn't need to be added separately -- nothing in this
//    file beyond what's already a transitive dependency via `futures`,
//    but if cargo complains about resolving `StreamExt` add:
//      futures-util = "0.3"
//    explicitly.)
//
// 2. Save this file as src/bin/logs_run.rs and declare it as a module at
//    the top of src/bin/cli.rs:
//
//      mod logs_run;
//
// 3. Add a subcommand variant to the `Cmd` enum:
//
//      /// Stream logs for a Tekton PipelineRun (live or archived)
//      LogsRun {
//          /// The PipelineRun's generated name
//          run_name: String,
//
//          /// Base URL of the tekton-sidekick service
//          #[arg(long, env = "SIDEKICK_URL", default_value = "http://localhost:8000")]
//          sidekick_url: String,
//      },
//
// 4. Handle it in main(), before the generic `send(...)` dispatch block --
//    LogsRun talks directly to tekton-sidekick over HTTP rather than
//    going through the daemon's unix-socket `send()`:
//
//      if let Some(Cmd::LogsRun { run_name, sidekick_url }) = &cli.command {
//          if let Err(e) = logs_run::stream_run_logs(sidekick_url, run_name).await {
//              eprintln!("✗  {e}");
//              std::process::exit(1);
//          }
//          return;
//      }
//
//    Usage once wired up:
//
//      ginger-code logs-run nightly-build-042
//      ginger-code logs-run nightly-build-042 --sidekick-url http://tekton-sidekick.mycluster.dev
//      SIDEKICK_URL=http://localhost:8000 ginger-code logs-run nightly-build-042