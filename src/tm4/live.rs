//! Live TM4 event capture to JSONL, in WebSocket viewer role (no login).
//!
//! Record kinds, one JSON object per line, each with `record` and `host_time`
//! (host UTC, RFC 3339 with nanoseconds):
//! - `session`: device, `systeminfo`, `status`, `setup` and `timesync/status`
//!   snapshots, written once at start.
//! - `sntp`: TM4 minus host clock offset.
//! - `connection`: WebSocket connected or disconnected.
//! - `message`: every TM4 event except `Ping`, verbatim as `type`, `subtype`, `id`
//!   and `payload`.
//!
//! The capture reconnects on its own and reports progress through
//! [`LiveEvent`]s; it never prints.

use std::collections::HashSet;
use std::fs::File;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use futures_util::{SinkExt as _, StreamExt as _};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use super::device::{self, Device};
use super::sntp::{self, SntpSample};
use crate::error::Result;

/// Topics subscribed to when the caller names none.
pub const DEFAULT_TOPICS: &[&str] = &[
    "Measurement",
    "MeasurementDetail",
    "LiveTrajectory",
    "TrackerState",
    "SystemState",
    "Setup",
    "SessionOpened",
    "SessionClosed",
];
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const SNTP_TIMEOUT: Duration = Duration::from_secs(2);

pub struct LogConfig {
    /// JSONL file, appended to.
    pub out: PathBuf,
    /// WebSocket topics, e.g. [`DEFAULT_TOPICS`].
    pub topics: Vec<String>,
    /// Stop after this long.
    pub duration: Option<Duration>,
    /// Stop after this many distinct shots.
    pub shots: Option<u64>,
    /// Interval between clock-offset samples.
    pub sntp_interval: Duration,
    /// SNTP port on the TM4 (123).
    pub sntp_port: u16,
}

/// Progress reported to the caller while capturing.
pub enum LiveEvent<'a> {
    /// Subscribed to the WebSocket. `first` is false after a reconnect;
    /// `clock` is the most recent clock offset.
    Connected { first: bool, clock: Option<SntpSample> },
    /// Could not open the WebSocket; retrying.
    ConnectFailed { error: &'a str },
    /// The WebSocket dropped; reconnecting.
    Disconnected { reason: &'a str },
    /// A shot's final measurement (`Kind: "Measurement"`). `number` counts
    /// distinct shots from 1.
    Shot { number: u64, measurement: &'a Value },
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LogSummary {
    /// JSONL file written.
    pub out: String,
    pub device: Device,
    pub started: DateTime<Utc>,
    pub ended: DateTime<Utc>,
    /// Why capture ended: `stopped` (the caller's stop future), `duration` or `shots`.
    pub stop_reason: String,
    /// TM4 events written (excluding Ping).
    pub messages: u64,
    /// Distinct shots with a final `Kind: "Measurement"` record.
    pub shots: u64,
    pub reconnects: u64,
    /// Most recent clock offset (TM4 - host).
    pub last_sntp: Option<SntpSample>,
}

struct Sink {
    file: File,
    messages: u64,
}

impl Sink {
    /// Writes one record and flushes, so a crash loses at most the line in flight.
    fn write(&mut self, mut record: Value) -> Result<()> {
        record["host_time"] = json!(Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true));
        let mut line = serde_json::to_vec(&record)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        Ok(())
    }
}

/// Captures TM4 events to `config.out` until `stop` resolves, `config.duration`
/// elapses or `config.shots` shots arrive.
pub async fn run(
    http: &reqwest::Client,
    device: Device,
    config: LogConfig,
    stop: impl Future<Output = ()>,
    mut on_event: impl FnMut(LiveEvent<'_>),
) -> Result<LogSummary> {
    let file = std::fs::OpenOptions::new().create(true).append(true).open(&config.out)?;
    let mut sink = Sink { file, messages: 0 };
    let started = Utc::now();

    let mut snapshot = json!({ "record": "session", "device": device, "topics": config.topics });
    let mut errors = serde_json::Map::new();
    for (key, route) in [
        ("systeminfo", "systeminfo"),
        ("status", "status"),
        ("setup", "setup"),
        ("timesync", "timesync/status"),
    ] {
        match device::api_get(http, &device, route).await {
            Ok(value) => snapshot[key] = value,
            Err(e) => {
                errors.insert(key.into(), json!(e.to_string()));
            }
        }
    }
    snapshot["errors"] = Value::Object(errors);
    sink.write(snapshot)?;

    let mut last_sntp = sample_sntp(&mut sink, &device, config.sntp_port).await?;
    let mut sntp_timer = tokio::time::interval(config.sntp_interval);
    sntp_timer.reset(); // the first sample was just taken
    let stop_at = config.duration.map(|d| tokio::time::Instant::now() + d);
    let stop_timer = async {
        match stop_at {
            Some(at) => tokio::time::sleep_until(at).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(stop_timer);
    tokio::pin!(stop);

    let mut shot_ids = HashSet::new();
    let mut reconnects = 0u64;
    let mut connected_before = false;
    let stop_reason = 'session: loop {
        let socket = tokio::select! {
            r = tokio_tungstenite::connect_async(device.websocket.as_str()) => r,
            _ = &mut stop => break 'session "stopped",
            _ = &mut stop_timer => break 'session "duration",
        };
        let mut socket = match socket {
            Ok((socket, _)) => socket,
            Err(e) => {
                let error = e.to_string();
                sink.write(json!({ "record": "connection", "state": "failed", "reason": error }))?;
                on_event(LiveEvent::ConnectFailed { error: &error });
                reconnects += 1;
                tokio::select! {
                    _ = tokio::time::sleep(RECONNECT_DELAY) => continue 'session,
                    _ = &mut stop => break 'session "stopped",
                    _ = &mut stop_timer => break 'session "duration",
                }
            }
        };
        let subscribe = json!({ "Type": "Subscribe", "Payload": { "MessageList": config.topics } });
        socket.send(Message::text(subscribe.to_string())).await?;
        sink.write(json!({ "record": "connection", "state": "connected", "url": device.websocket }))?;
        on_event(LiveEvent::Connected { first: !connected_before, clock: last_sntp });
        connected_before = true;

        let reason: String = loop {
            tokio::select! {
                frame = socket.next() => {
                    let text = match frame {
                        Some(Ok(Message::Text(text))) => text,
                        Some(Ok(Message::Close(close))) => break match close {
                            Some(frame) if !frame.reason.is_empty() => {
                                format!("closed by TM4, code {}: {}", u16::from(frame.code), frame.reason)
                            }
                            Some(frame) => format!("closed by TM4, code {}", u16::from(frame.code)),
                            None => "closed by TM4".into(),
                        },
                        Some(Ok(_)) => continue,
                        Some(Err(e)) => break e.to_string(),
                        None => break "connection ended".into(),
                    };
                    let message: Value = match serde_json::from_str(text.as_str()) {
                        Ok(v) => v,
                        Err(e) => {
                            sink.write(json!({ "record": "unparsed", "error": e.to_string(), "raw": text.as_str() }))?;
                            continue;
                        }
                    };
                    if message["Type"].as_str() == Some("Ping") {
                        socket.send(Message::text(r#"{"Type":"Pong"}"#)).await?;
                        continue;
                    }
                    sink.write(json!({
                        "record": "message",
                        "type": message["Type"],
                        "subtype": message["SubType"],
                        "id": message["Id"],
                        "payload": message["Payload"],
                    }))?;
                    sink.messages += 1;
                    if is_final_measurement(&message) {
                        let id = message["Payload"]["Id"].as_str().or(message["Id"].as_str()).unwrap_or_default().to_owned();
                        if shot_ids.insert(id) {
                            let number = shot_ids.len() as u64;
                            on_event(LiveEvent::Shot { number, measurement: &message["Payload"] });
                            if config.shots.is_some_and(|n| number >= n) {
                                break 'session "shots";
                            }
                        }
                    }
                }
                _ = sntp_timer.tick() => {
                    last_sntp = sample_sntp(&mut sink, &device, config.sntp_port).await?.or(last_sntp);
                }
                _ = &mut stop => break 'session "stopped",
                _ = &mut stop_timer => break 'session "duration",
            }
        };
        sink.write(json!({ "record": "connection", "state": "disconnected", "reason": reason }))?;
        on_event(LiveEvent::Disconnected { reason: &reason });
        reconnects += 1;
        tokio::select! {
            _ = tokio::time::sleep(RECONNECT_DELAY) => {}
            _ = &mut stop => break 'session "stopped",
            _ = &mut stop_timer => break 'session "duration",
        }
    };

    // A closing offset brackets the capture for drift estimation.
    last_sntp = sample_sntp(&mut sink, &device, config.sntp_port).await?.or(last_sntp);
    Ok(LogSummary {
        out: config.out.display().to_string(),
        device,
        started,
        ended: Utc::now(),
        stop_reason: stop_reason.into(),
        messages: sink.messages,
        shots: shot_ids.len() as u64,
        reconnects,
        last_sntp,
    })
}

/// SNTP failures are recorded rather than fatal; a capture without timing is still usable.
async fn sample_sntp(sink: &mut Sink, device: &Device, port: u16) -> Result<Option<SntpSample>> {
    match sntp::query(&device.ntp_host, port, SNTP_TIMEOUT).await {
        Ok(sample) => {
            sink.write(json!({ "record": "sntp", "server": device.ntp_host, "sample": sample }))?;
            Ok(Some(sample))
        }
        Err(e) => {
            sink.write(json!({ "record": "sntp", "server": device.ntp_host, "error": e.to_string() }))?;
            Ok(None)
        }
    }
}

/// The TM4 emits several `Measurement` events per shot (`PreLaunchData`,
/// `LaunchData`, `LiveApex`, `FlightData`, `Measurement`); `Kind: "Measurement"` is the final one.
fn is_final_measurement(message: &Value) -> bool {
    matches!(message["Type"].as_str(), Some("Measurement" | "LaunchData"))
        && message["Payload"]["Kind"].as_str() == Some("Measurement")
}
