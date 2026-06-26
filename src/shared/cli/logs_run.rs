// src/bin/logs_run.rs
//
// Entry point for `ginger-code logs-run <RUN_NAME>`.
//
// Delegates to one of two backends depending on the `--raw` / `--tui` flags:
//
//   (default / --tui)   tui::run   — full ratatui TUI: alternate screen,
//                                    left pane task/step list, right pane
//                                    scrollable logs, keyboard navigation.
//
//   --raw               raw::run   — newline-delimited JSON (NDJSON) on
//                                    stdout, one object per SSE event,
//                                    tagged with an `"event"` field.
//                                    No ANSI color, no banner, no TUI.
//                                    Intended for AI coding agents or other
//                                    tooling consuming this stream
//                                    programmatically.
//
// Wire types shared by both backends live in `wire`.

pub mod raw;
pub mod tui;
pub mod wire;

// ── Entry point ───────────────────────────────────────────────────────────

pub async fn stream_run_logs(
    base_url: &str,
    run_name: &str,
    raw: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if raw {
        self::raw::run(base_url, run_name).await
    } else {
        self::tui::run(base_url, run_name).await
    }
}