//! `tm4 discover`, `tm4 status`, `tm4 log`: a TrackMan 4 on the local network.

use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use incurs::cli::Cli;
use incurs::command::{CommandDef, TypedContext};
use incurs::output::CtaBlock;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use apiman::Result;
use apiman::tm4::device::{self, Device};
use apiman::tm4::live::{self, LiveEvent, LogConfig, LogSummary};
use apiman::tm4::sntp::{self, SntpSample};

use super::{http, is_human, present, read_only, suggest};
use crate::ui::{self, Align};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(4);

fn host_of(device: &Device) -> String {
    url::Url::parse(&device.location)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| device.location.clone())
}

fn device_cta(host: &str) -> CtaBlock {
    let status = format!("tm4 status --host {host}");
    let log = format!("tm4 log --host {host}");
    suggest(&[(&status, "Check mode and clock sync"), (&log, "Record shots to JSONL")])
}

/// One line naming the unit: `Mock TM4  TM4 · S/N TM4-MOCK-1 · API 2.1 · 127.0.0.1`.
fn device_line(device: &Device) -> String {
    let mut parts = Vec::new();
    if let Some(m) = &device.model_number {
        parts.push(m.clone());
    }
    if let Some(s) = &device.serial_number {
        parts.push(format!("S/N {s}"));
    }
    parts.push(format!("API {}", device.api_version));
    parts.push(host_of(device));
    format!("{}  {}", ui::bold(&device.friendly_name), ui::dim(&parts.join(" · ")))
}

fn clock_sentence(sample: &SntpSample) -> String {
    ui::clock_sentence(sample.offset_s, sample.delay_s, ui::dim)
}

// ─── discover ───────────────────────────────────────────────────────────────

#[derive(Deserialize, incurs::Options)]
pub struct DiscoverOptions {
    /// Seconds to wait for SSDP replies.
    #[incurs(default = 4)]
    timeout: u64,
}

#[derive(Serialize, JsonSchema)]
pub struct DiscoverOutput {
    devices: Vec<Device>,
    /// Description URLs that answered SSDP but could not be read.
    errors: Vec<String>,
}

async fn discover(options: DiscoverOptions) -> Result<DiscoverOutput> {
    let http = http();
    let mut devices = Vec::new();
    let mut errors = Vec::new();
    for location in device::discover(Duration::from_secs(options.timeout), false).await? {
        match device::describe(&http, &location).await {
            Ok(d) => devices.push(d),
            Err(e) => errors.push(format!("{location}: {e}")),
        }
    }
    Ok(DiscoverOutput { devices, errors })
}

fn render_discover(out: &DiscoverOutput) -> String {
    let mut text = if out.devices.is_empty() {
        format!(
            "No TM4 found on this network.\n\n{}",
            ui::dim(
                "• Join the TM4's network (its own Wi-Fi, or the LAN it is connected to).\n\
                 • macOS may be blocking discovery: allow your terminal under\n  \
                 System Settings → Privacy & Security → Local Network.\n\
                 • Or skip discovery and pass the IP shown on the TM4 or in TPS with --host."
            )
        )
    } else {
        let rows: Vec<Vec<String>> = out
            .devices
            .iter()
            .map(|d| {
                vec![
                    d.friendly_name.clone(),
                    d.model_number.clone().unwrap_or_default(),
                    d.serial_number.clone().unwrap_or_default(),
                    host_of(d),
                    d.api_version.clone(),
                ]
            })
            .collect();
        let n = out.devices.len();
        format!(
            "Found {n} TM4{}\n\n{}",
            if n == 1 { "" } else { "s" },
            ui::table(
                &[
                    ("Name", Align::Left),
                    ("Model", Align::Left),
                    ("Serial", Align::Left),
                    ("Address", Align::Left),
                    ("API", Align::Left),
                ],
                &rows
            )
        )
    };
    for error in &out.errors {
        text.push_str(&format!("\n{} {error}", ui::yellow("! Could not read")));
    }
    text
}

pub fn discover_command() -> CommandDef {
    CommandDef::typed::<(), DiscoverOptions, (), DiscoverOutput, _, _>(
        "discover",
        |ctx: TypedContext<(), DiscoverOptions, ()>| async move {
            let human = is_human(&ctx);
            present(human, discover(ctx.options).await, render_discover, |out| {
                Some(match out.devices.first() {
                    Some(d) => device_cta(&host_of(d)),
                    None => suggest(&[("tm4 status --host <TM4-IP>", "Connect without discovery")]),
                })
            })
        },
    )
    .description("Find TM4 units on the local network (SSDP)")
    .mcp(read_only())
    .done()
}

// ─── status ─────────────────────────────────────────────────────────────────

#[derive(Deserialize, incurs::Options)]
pub struct HostOptions {
    /// TM4 IP, hostname or device-description URL. Omit to use the first TM4 found by SSDP.
    host: Option<String>,
    /// SNTP port on the TM4.
    #[incurs(default = 123)]
    ntp_port: u16,
}

#[derive(Serialize, JsonSchema)]
pub struct StatusOutput {
    device: Device,
    systeminfo: Option<Value>,
    status: Option<Value>,
    setup: Option<Value>,
    timesync: Option<Value>,
    sntp: Option<SntpSample>,
    /// Per-resource failures.
    errors: Vec<String>,
}

async fn status(options: HostOptions) -> Result<StatusOutput> {
    let http = http();
    let device = device::resolve(&http, options.host.as_deref(), DISCOVERY_TIMEOUT).await?;
    let mut errors = Vec::new();
    let mut get = async |route: &str| match device::api_get(&http, &device, route).await {
        Ok(v) => Some(v),
        Err(e) => {
            errors.push(format!("{route}: {e}"));
            None
        }
    };
    let systeminfo = get("systeminfo").await;
    let status = get("status").await;
    let setup = get("setup").await;
    let timesync = get("timesync/status").await;
    let sntp = match sntp::query(&device.ntp_host, options.ntp_port, Duration::from_secs(2)).await {
        Ok(s) => Some(s),
        Err(e) => {
            errors.push(format!("clock offset (SNTP): {e}"));
            None
        }
    };
    Ok(StatusOutput { device, systeminfo, status, setup, timesync, sntp, errors })
}

fn text(v: Option<&Value>, path: &[&str]) -> Option<String> {
    let mut cur = v?;
    for key in path {
        cur = cur.get(*key)?;
    }
    match cur {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn render_status(out: &StatusOutput) -> String {
    let mut rows: Vec<(&str, String)> = Vec::new();

    let system = text(out.status.as_ref(), &["SystemState"]).or_else(|| text(out.systeminfo.as_ref(), &["SystemState"]));
    let tracker = text(out.status.as_ref(), &["TrackerState"]);
    if system.is_some() || tracker.is_some() {
        let mut s = system.unwrap_or_else(|| "unknown".into());
        if let Some(t) = tracker {
            s.push_str(&ui::dim(&format!(" · tracker {t}")));
        }
        rows.push(("State", s));
    }

    if let Some(mode) = text(out.setup.as_ref(), &["MeasurementMode"]) {
        let measuring = out.setup.as_ref().and_then(|s| s["IsMeasuring"].as_bool());
        let suffix = match measuring {
            Some(true) => ui::dim(" · measuring"),
            Some(false) => ui::yellow(" · not measuring"),
            None => String::new(),
        };
        rows.push(("Mode", format!("{}{suffix}", mode.replace('.', " · "))));
    }
    if let Some(ball) = text(out.setup.as_ref(), &["Conditions", "BallType"]) {
        rows.push(("Ball", ball));
    }
    if let Some(club) = text(out.setup.as_ref(), &["Conditions", "ClubType"]) {
        rows.push(("Club", club));
    }
    if let Some(player) = text(out.setup.as_ref(), &["Conditions", "PlayerName"]) {
        rows.push(("Player", player));
    }

    if let Some(source) = text(out.timesync.as_ref(), &["sync_source"]) {
        let server = text(out.timesync.as_ref(), &["ntp", "server"]);
        rows.push((
            "Time source",
            match server {
                Some(s) => format!("{} {}", source.to_uppercase(), ui::dim(&format!("({s})"))),
                None => source.to_uppercase(),
            },
        ));
    }
    match &out.sntp {
        Some(sample) => rows.push(("Clock", clock_sentence(sample))),
        None => rows.push(("Clock", ui::yellow("offset unavailable"))),
    }

    let device_errors: Vec<String> = out
        .systeminfo
        .as_ref()
        .and_then(|s| s["Errors"].as_array())
        .map(|a| a.iter().filter_map(|e| e.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    if !device_errors.is_empty() {
        rows.push(("TM4 errors", ui::yellow(&device_errors.join("; "))));
    }

    let mut text_out = format!("{}\n\n{}", device_line(&out.device), ui::fields(&rows));
    for error in &out.errors {
        text_out.push_str(&format!("\n{} {error}", ui::yellow("! Could not read")));
    }
    text_out
}

pub fn status_command() -> CommandDef {
    CommandDef::typed::<(), HostOptions, (), StatusOutput, _, _>(
        "status",
        |ctx: TypedContext<(), HostOptions, ()>| async move {
            let human = is_human(&ctx);
            present(human, status(ctx.options).await, render_status, |out| {
                let log = format!("tm4 log --host {}", host_of(&out.device));
                Some(suggest(&[(&log, "Record shots to JSONL")]))
            })
        },
    )
    .description("Read TM4 system info, setup, time-sync state and clock offset")
    .mcp(read_only())
    .done()
}

// ─── log ────────────────────────────────────────────────────────────────────

#[derive(Deserialize, incurs::Options)]
pub struct LogOptions {
    /// TM4 IP, hostname or device-description URL. Omit to use the first TM4 found by SSDP.
    host: Option<String>,
    /// SNTP port on the TM4.
    #[incurs(default = 123)]
    ntp_port: u16,
    /// Output JSONL file (appended). Defaults to tm4-<UTC timestamp>.jsonl.
    out: Option<String>,
    /// Event topic to subscribe to. Repeatable. Defaults to measurements, trajectories and state.
    #[serde(default)]
    topic: Vec<String>,
    /// Stop after this many seconds.
    duration: Option<u64>,
    /// Stop after this many completed shots.
    shots: Option<u64>,
    /// Seconds between SNTP clock-offset samples.
    #[incurs(default = 30)]
    sntp_interval: u64,
    /// Do not print connection and shot lines to stderr.
    quiet: bool,
}

async fn log(options: LogOptions) -> Result<LogSummary> {
    let http = http();
    let device = device::resolve(&http, options.host.as_deref(), DISCOVERY_TIMEOUT).await?;
    let out = options.out.map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(format!("tm4-{}.jsonl", Utc::now().format("%Y%m%dT%H%M%SZ")))
    });
    let topics = if options.topic.is_empty() {
        live::DEFAULT_TOPICS.iter().map(|t| (*t).to_owned()).collect()
    } else {
        options.topic
    };
    let echo = !options.quiet;
    let (name, path) = (device.friendly_name.clone(), out.display().to_string());
    let stop = async {
        // If the handler cannot be installed, only --duration/--shots stop the capture.
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    live::run(
        &http,
        device,
        LogConfig {
            out,
            topics,
            duration: options.duration.map(Duration::from_secs),
            shots: options.shots,
            sntp_interval: Duration::from_secs(options.sntp_interval.max(1)),
            sntp_port: options.ntp_port,
        },
        stop,
        |event| {
            if echo {
                eprintln!("{}", describe_event(&event, &name, &path));
            }
        },
    )
    .await
}

/// Live progress for stderr: a header on first connect, then one table row per shot.
fn describe_event(event: &LiveEvent<'_>, device: &str, path: &str) -> String {
    match event {
        LiveEvent::Connected { first: true, clock } => {
            let clock = clock.map_or_else(
                || ui::err_yellow("TM4 clock offset unavailable"),
                |s| ui::clock_sentence(s.offset_s, s.delay_s, ui::err_dim),
            );
            format!(
                "Recording {} → {}\n{clock}\n{}\n\n{}",
                ui::err_bold(device),
                ui::err_bold(path),
                ui::err_dim("Waiting for shots. Press Ctrl-C to stop."),
                ui::err_bold(&shot_row(&SHOT_HEADER.map(str::to_owned)))
            )
        }
        LiveEvent::Connected { first: false, .. } => ui::err_dim("  Reconnected."),
        LiveEvent::ConnectFailed { error } => {
            ui::err_yellow(&format!("! Can't reach the TM4 WebSocket ({error}); retrying…"))
        }
        LiveEvent::Disconnected { reason } => ui::err_yellow(&format!("! Connection lost ({reason}); reconnecting…")),
        LiveEvent::Shot { number, measurement } => shot_row(&shot_cells(*number, measurement)),
    }
}

const SHOT_HEADER: [&str; 8] = ["#", "Time", "Ball speed", "Launch", "Direction", "Spin rate", "Spin axis", "Carry"];
const SHOT_WIDTHS: [usize; 8] = [3, 8, 10, 7, 9, 9, 9, 8];

/// One fixed-width row of the live shot table; column 1 (time) is left-aligned.
fn shot_row(cells: &[String; 8]) -> String {
    cells
        .iter()
        .zip(SHOT_WIDTHS)
        .enumerate()
        .map(|(i, (cell, w))| if i == 1 { format!("{cell:<w$}") } else { format!("{cell:>w$}") })
        .collect::<Vec<_>>()
        .join("  ")
}

/// Display units for people: mph and yards (the JSONL keeps SI).
fn shot_cells(n: u64, m: &Value) -> [String; 8] {
    let f = |key: &str, scale: f64, decimals: usize, unit: &str| {
        m[key].as_f64().map_or_else(|| "–".to_owned(), |v| format!("{:.decimals$}{unit}", v * scale))
    };
    [
        n.to_string(),
        chrono::Local::now().format("%H:%M:%S").to_string(),
        f("BallSpeed", ui::MPH_PER_MPS, 1, " mph"),
        f("LaunchAngle", 1.0, 1, "°"),
        f("LaunchDirection", 1.0, 1, "°"),
        f("SpinRate", 1.0, 0, " rpm"),
        f("SpinAxis", 1.0, 1, "°"),
        f("Carry", ui::YD_PER_M, 1, " yd"),
    ]
}

fn render_log(out: &LogSummary) -> String {
    let seconds = (out.ended - out.started).num_milliseconds() as f64 / 1000.0;
    let stopped = match out.stop_reason.as_str() {
        "stopped" => "stopped with Ctrl-C",
        "duration" => "time limit reached",
        "shots" => "shot limit reached",
        other => other,
    };
    let mut rows = vec![
        ("File", ui::bold(&out.out)),
        ("Events", format!("{} {}", out.messages, ui::dim(&format!("over {}", ui::duration(seconds))))),
        ("Stopped", stopped.to_owned()),
    ];
    if out.reconnects > 0 {
        rows.push(("Reconnects", ui::yellow(&out.reconnects.to_string())));
    }
    rows.push((
        "Clock",
        out.last_sntp.as_ref().map_or_else(|| ui::yellow("offset unavailable"), clock_sentence),
    ));
    format!(
        "\n{} Recorded {} {} from {}\n\n{}",
        ui::green("✓"),
        out.shots,
        if out.shots == 1 { "shot" } else { "shots" },
        out.device.friendly_name,
        ui::fields(&rows)
    )
}

pub fn log_command() -> CommandDef {
    CommandDef::typed::<(), LogOptions, (), LogSummary, _, _>(
        "log",
        |ctx: TypedContext<(), LogOptions, ()>| async move {
            let human = is_human(&ctx);
            present(human, log(ctx.options).await, render_log, |_| None)
        },
    )
    .description("Record live TM4 events to JSONL as a viewer (no login) until Ctrl-C, --duration or --shots")
    .done()
}

pub fn group() -> Cli {
    Cli::create("tm4")
        .description("TrackMan 4 on the local network")
        .command("discover", discover_command())
        .command("status", status_command())
        .command("log", log_command())
}
