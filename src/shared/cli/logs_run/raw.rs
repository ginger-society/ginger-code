// src/bin/logs_run/raw.rs
//
// `--raw` mode: connects to the SSE stream and emits one NDJSON line per
// event, tagged with an `"event"` field. No ANSI color, no TUI, no
// rendering state — meant for AI coding agents or other tooling.

use eventsource_stream::Eventsource;
use futures_util::StreamExt;

use super::wire::urlencode_path_segment;

/// Print one NDJSON line: parse the event data as a JSON object, splice
/// in an `"event"` field, re-serialize. Preserves all server-side fields
/// exactly (including any this CLI's typed structs don't declare) without
/// string-splicing JSON text directly.
fn print_raw_line(event_name: &str, data_json: &str) {
    let mut value: serde_json::Value = match serde_json::from_str(data_json) {
        Ok(v) => v,
        Err(_) => {
            println!(
                "{}",
                serde_json::json!({ "event": event_name, "raw": data_json })
            );
            return;
        }
    };

    if let serde_json::Value::Object(ref mut map) = value {
        map.insert(
            "event".to_string(),
            serde_json::Value::String(event_name.to_string()),
        );
        println!("{}", value);
    } else {
        println!(
            "{}",
            serde_json::json!({ "event": event_name, "data": value })
        );
    }
}

pub async fn run(
    base_url: &str,
    run_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!(
        "{}/runs/{}/stream",
        base_url.trim_end_matches('/'),
        urlencode_path_segment(run_name)
    );

    let client = reqwest::Client::new();
    let response = client.get(&url).send().await?;

    if !response.status().is_success() {
        println!(
            "{}",
            serde_json::json!({
                "event": "error",
                "message": format!(
                    "sidekick returned HTTP {} for run '{}'",
                    response.status(),
                    run_name
                )
            })
        );
        std::process::exit(1);
    }

    let mut stream = response.bytes_stream().eventsource();

    while let Some(event) = stream.next().await {
        let event = match event {
            Ok(e) => e,
            Err(e) => {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "error",
                        "message": format!("connection error: {e}")
                    })
                );
                break;
            }
        };
        print_raw_line(&event.event, &event.data);
        if event.event == "done" {
            break;
        }
    }

    Ok(())
}