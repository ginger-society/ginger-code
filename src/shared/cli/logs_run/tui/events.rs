// src/bin/logs_run/tui/events.rs

use crossterm::event::{self, KeyCode, KeyEvent, KeyModifiers};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::shared::cli::logs_run::wire::{
    urlencode_path_segment, LogLine, RunDone, RunMeta, StepStatusUpdate, StreamError,
    TaskStatusUpdate,
};

// ── Event enum ────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum SseEvent {
    Meta(RunMeta),
    Log(LogLine),
    StepStatus(StepStatusUpdate),
    TaskStatus(TaskStatusUpdate),
    Done(RunDone),
    Error(StreamError),
    ConnectionError(String),
    UnknownEvent(String),
}

/// SSE event tagged with which pipeline run it came from.
#[derive(Debug)]
pub struct TaggedSseEvent {
    pub run_name: String,
    pub event: SseEvent,
}

#[derive(Debug)]
pub enum AppEvent {
    Sse(TaggedSseEvent),
    Key(KeyEvent),
    Tick,
}

// ── Key polling task ──────────────────────────────────────────────────────

pub fn spawn_key_task(tx: mpsc::Sender<AppEvent>) {
    std::thread::spawn(move || {
        loop {
            match event::poll(std::time::Duration::from_millis(100)) {
                Ok(true) => {
                    if let Ok(event::Event::Key(key)) = event::read() {
                        if tx.blocking_send(AppEvent::Key(key)).is_err() {
                            break;
                        }
                    }
                }
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });
}

// ── Tick task ─────────────────────────────────────────────────────────────

pub fn spawn_tick_task(tx: mpsc::Sender<AppEvent>, interval_ms: u64) {
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_millis(interval_ms));
        loop {
            interval.tick().await;
            if tx.send(AppEvent::Tick).await.is_err() {
                break;
            }
        }
    });
}

// ── SSE task — one per pipeline run ───────────────────────────────────────

pub async fn run_sse_task(
    base_url: String,
    namespace: String,
    run_name: String,
    tx: mpsc::Sender<AppEvent>,
) {
    let url = format!(
        "{}/runs/{}/{}/stream",
        base_url.trim_end_matches('/'),
        urlencode_path_segment(&namespace),
        urlencode_path_segment(&run_name),
    );

    let client = reqwest::Client::new();
    let response = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(AppEvent::Sse(TaggedSseEvent {
                run_name: run_name.clone(),
                event: SseEvent::ConnectionError(e.to_string()),
            })).await;
            return;
        }
    };

    if !response.status().is_success() {
        let msg = format!(
            "sidekick returned HTTP {} for run '{}'",
            response.status(),
            run_name
        );
        let _ = tx.send(AppEvent::Sse(TaggedSseEvent {
            run_name: run_name.clone(),
            event: SseEvent::ConnectionError(msg),
        })).await;
        return;
    }

    let mut stream = response.bytes_stream().eventsource();
    while let Some(event) = stream.next().await {
        let event = match event {
            Ok(e) => e,
            Err(e) => {
                let _ = tx.send(AppEvent::Sse(TaggedSseEvent {
                    run_name: run_name.clone(),
                    event: SseEvent::ConnectionError(e.to_string()),
                })).await;
                break;
            }
        };

        let sse = parse_sse_event(&event.event, &event.data);
        let is_done = matches!(sse, SseEvent::Done(_));
        if tx.send(AppEvent::Sse(TaggedSseEvent {
            run_name: run_name.clone(),
            event: sse,
        })).await.is_err() {
            break;
        }
        if is_done {
            break;
        }
    }
}

fn parse_sse_event(event_name: &str, data: &str) -> SseEvent {
    match event_name {
        "meta" => match serde_json::from_str::<RunMeta>(data) {
            Ok(m) => SseEvent::Meta(m),
            Err(e) => SseEvent::ConnectionError(format!("bad meta JSON: {e}")),
        },
        "log" => match serde_json::from_str::<LogLine>(data) {
            Ok(l) => SseEvent::Log(l),
            Err(e) => SseEvent::ConnectionError(format!("bad log JSON: {e}")),
        },
        "step-status" => match serde_json::from_str::<StepStatusUpdate>(data) {
            Ok(u) => SseEvent::StepStatus(u),
            Err(e) => SseEvent::ConnectionError(format!("bad step-status JSON: {e}")),
        },
        "task-status" => match serde_json::from_str::<TaskStatusUpdate>(data) {
            Ok(u) => SseEvent::TaskStatus(u),
            Err(e) => SseEvent::ConnectionError(format!("bad task-status JSON: {e}")),
        },
        "done" => match serde_json::from_str::<RunDone>(data) {
            Ok(d) => SseEvent::Done(d),
            Err(e) => SseEvent::ConnectionError(format!("bad done JSON: {e}")),
        },
        "error" => match serde_json::from_str::<StreamError>(data) {
            Ok(e) => SseEvent::Error(e),
            Err(e) => SseEvent::ConnectionError(format!("bad error JSON: {e}")),
        },
        other => SseEvent::UnknownEvent(other.to_string()),
    }
}

// ── Key binding helpers ───────────────────────────────────────────────────

pub fn is_quit(key: &KeyEvent) -> bool {
    matches!(
        key,
        KeyEvent { code: KeyCode::Char('q'), .. }
        | KeyEvent { code: KeyCode::Char('c'), modifiers: KeyModifiers::CONTROL, .. }
    )
}