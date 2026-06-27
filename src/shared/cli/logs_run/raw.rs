// src/bin/logs_run/raw.rs

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use super::wire::urlencode_path_segment;
use super::RunTarget;

#[derive(Debug)]
struct RawEvent {
    run_name: String,
    event_name: String,
    data: String,
}

fn print_raw_line(run_name: &str, event_name: &str, data_json: &str) {
    let mut value: serde_json::Value = match serde_json::from_str(data_json) {
        Ok(v) => v,
        Err(_) => {
            println!(
                "{}",
                serde_json::json!({
                    "run_name": run_name,
                    "event": event_name,
                    "raw": data_json
                })
            );
            return;
        }
    };

    if let serde_json::Value::Object(ref mut map) = value {
        // Inject both event type and run_name so agent can route/filter
        map.insert("event".into(), serde_json::Value::String(event_name.to_string()));
        map.insert("run_name".into(), serde_json::Value::String(run_name.to_string()));
        println!("{}", value);
    } else {
        println!(
            "{}",
            serde_json::json!({
                "run_name": run_name,
                "event": event_name,
                "data": value
            })
        );
    }
}

async fn stream_one(
    base_url: String,
    target: RunTarget,
    tx: mpsc::UnboundedSender<RawEvent>,
) {
    let url = format!(
        "{}/runs/{}/{}/stream",
        base_url.trim_end_matches('/'),
        urlencode_path_segment(&target.namespace),
        urlencode_path_segment(&target.run_name),
    );

    let client = reqwest::Client::new();
    let response = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(RawEvent {
                run_name: target.run_name.clone(),
                event_name: "error".into(),
                data: serde_json::json!({ "message": e.to_string() }).to_string(),
            });
            return;
        }
    };

    if !response.status().is_success() {
        let _ = tx.send(RawEvent {
            run_name: target.run_name.clone(),
            event_name: "error".into(),
            data: serde_json::json!({
                "message": format!(
                    "sidekick returned HTTP {} for run '{}'",
                    response.status(),
                    target.run_name
                )
            }).to_string(),
        });
        return;
    }

    let mut stream = response.bytes_stream().eventsource();
    while let Some(event) = stream.next().await {
        let event = match event {
            Ok(e) => e,
            Err(e) => {
                let _ = tx.send(RawEvent {
                    run_name: target.run_name.clone(),
                    event_name: "error".into(),
                    data: serde_json::json!({ "message": e.to_string() }).to_string(),
                });
                break;
            }
        };
        let is_done = event.event == "done";
        let _ = tx.send(RawEvent {
            run_name: target.run_name.clone(),
            event_name: event.event,
            data: event.data,
        });
        if is_done {
            break;
        }
    }
}

pub async fn run(
    base_url: &str,
    targets: Vec<RunTarget>,
) -> Result<(), Box<dyn std::error::Error>> {
    if targets.is_empty() {
        return Ok(());
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<RawEvent>();
    let mut pending = targets.len();

    for target in targets {
        let tx = tx.clone();
        let base_url = base_url.to_string();
        tokio::spawn(stream_one(base_url, target, tx));
    }
    drop(tx); // so channel closes when all workers finish

    while let Some(evt) = rx.recv().await {
        print_raw_line(&evt.run_name, &evt.event_name, &evt.data);
        if evt.event_name == "done" {
            pending -= 1;
            if pending == 0 {
                break;
            }
        }
    }

    Ok(())
}