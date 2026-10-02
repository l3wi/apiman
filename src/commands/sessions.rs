//! `sessions list`, `sessions pull`: historical sessions from TrackMan cloud.

use std::io::IsTerminal as _;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use incurs::cli::Cli;
use incurs::command::{CommandDef, TypedContext};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use apiman::cloud::api::{Activity, ActivityFilter, STROKE_KINDS};
use apiman::{Error, Result};

use super::{api, http, is_human, present, read_only, suggest};
use crate::ui::{self, Align};

fn parse_time(value: Option<&str>, flag: &str) -> Result<Option<DateTime<Utc>>> {
    value
        .map(|v| {
            DateTime::parse_from_rfc3339(v)
                .map(|t| t.with_timezone(&Utc))
                .or_else(|_| {
                    chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d")
                        .map(|d| d.and_hms_opt(0, 0, 0).expect("midnight exists").and_utc())
                })
                .map_err(|_| Error::Invalid(format!("--{flag} {v:?} is not RFC 3339 or YYYY-MM-DD")))
        })
        .transpose()
}

/// Builds the activity filter shared by `list` and `pull`. With no `--kind`,
/// only stroke-bearing kinds are used unless `all_kinds` is set.
fn activity_filter(
    kind: &[String],
    all_kinds: bool,
    from: Option<&str>,
    to: Option<&str>,
    limit: Option<u32>,
    include_hidden: bool,
) -> Result<ActivityFilter> {
    let kinds = if !kind.is_empty() {
        Some(kind.iter().map(|k| k.to_ascii_uppercase()).collect())
    } else if all_kinds {
        None
    } else {
        Some(STROKE_KINDS.iter().map(|k| (*k).to_owned()).collect())
    };
    Ok(ActivityFilter {
        kinds,
        from: parse_time(from, "from")?,
        to: parse_time(to, "to")?,
        include_hidden,
        limit,
    })
}

fn kind_cell(kind: &str, hidden: bool) -> String {
    let label = ui::kind_label(kind);
    if hidden { format!("{label} (hidden)") } else { label }
}

// ─── list ───────────────────────────────────────────────────────────────────

#[derive(Deserialize, incurs::Options)]
pub struct ListOptions {
    /// Activity kind filter, e.g. SESSION or SHOT_ANALYSIS. Repeatable. Defaults to every stroke-bearing kind.
    #[serde(default)]
    kind: Vec<String>,
    /// List every activity kind, including ones without strokes.
    all_kinds: bool,
    /// Only activities at or after this time (RFC 3339 or YYYY-MM-DD, UTC).
    from: Option<String>,
    /// Only activities before this time (RFC 3339 or YYYY-MM-DD, UTC).
    to: Option<String>,
    /// Maximum number of activities, newest first.
    limit: Option<u32>,
    /// Include activities hidden in the TrackMan app.
    include_hidden: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct ListOutput {
    activities: Vec<Activity>,
}

fn render_list(out: &ListOutput) -> String {
    if out.activities.is_empty() {
        return "No sessions found.".into();
    }
    let shots: u32 = out.activities.iter().filter_map(|a| a.stroke_count).sum();
    let noun = if out.activities.len() == 1 { "activity" } else { "activities" };
    let rows: Vec<Vec<String>> = out
        .activities
        .iter()
        .map(|a| {
            vec![
                ui::local_time(a.time),
                kind_cell(&a.kind, a.is_hidden == Some(true)),
                a.stroke_count.map_or_else(|| "–".into(), |n| n.to_string()),
                a.id.clone(),
            ]
        })
        .collect();
    format!(
        "{} {noun}, {shots} shots\n\n{}",
        out.activities.len(),
        ui::table(
            &[("Date", Align::Left), ("Kind", Align::Left), ("Shots", Align::Right), ("ID", Align::Left)],
            &rows
        )
    )
}

pub fn list_command() -> CommandDef {
    CommandDef::typed::<(), ListOptions, (), ListOutput, _, _>(
        "list",
        |ctx: TypedContext<(), ListOptions, ()>| async move {
            let human = is_human(&ctx);
            let result = async {
                let o = &ctx.options;
                let filter =
                    activity_filter(&o.kind, o.all_kinds, o.from.as_deref(), o.to.as_deref(), o.limit, o.include_hidden)?;
                let http = http();
                Ok(ListOutput { activities: api(&http).await?.activities(&filter).await? })
            }
            .await;
            present(human, result, render_list, |out| {
                Some(if out.activities.is_empty() {
                    suggest(&[("sessions list --all-kinds", "Include activities without shot data")])
                } else {
                    suggest(&[
                        ("sessions pull <ID> --out session.json", "Download one session"),
                        ("sessions pull --from <YYYY-MM-DD> --out sessions.json", "Download everything since a date"),
                    ])
                })
            })
        },
    )
    .description("List TrackMan cloud activities (stroke-bearing kinds by default)")
    .mcp(read_only())
    .done()
}

// ─── pull ───────────────────────────────────────────────────────────────────

#[derive(Deserialize, incurs::Args)]
pub struct PullArgs {
    /// Activity IDs from `sessions list`. Omit to pull every activity matching the filter options.
    id: Option<Vec<String>>,
}

#[derive(Deserialize, incurs::Options)]
pub struct PullOptions {
    /// Output JSON file. Defaults to sessions-<UTC timestamp>.json.
    out: Option<String>,
    /// With no IDs: activity kind filter. Repeatable. Defaults to every stroke-bearing kind.
    #[serde(default)]
    kind: Vec<String>,
    /// With no IDs: only activities at or after this time (RFC 3339 or YYYY-MM-DD, UTC).
    from: Option<String>,
    /// With no IDs: only activities before this time (RFC 3339 or YYYY-MM-DD, UTC).
    to: Option<String>,
    /// With no IDs: maximum number of activities, newest first.
    limit: Option<u32>,
    /// With no IDs: include activities hidden in the TrackMan app.
    include_hidden: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct ClubCount {
    club: String,
    shots: usize,
}

#[derive(Serialize, JsonSchema)]
pub struct SessionSummary {
    id: String,
    kind: String,
    time: Option<DateTime<Utc>>,
    shots: usize,
    /// Shots per club, in order of first use.
    clubs: Vec<ClubCount>,
}

#[derive(Serialize, JsonSchema)]
pub struct PullOutput {
    /// JSON file written.
    out: String,
    /// File size in bytes.
    bytes: u64,
    /// Total shots across sessions.
    shots: usize,
    sessions: Vec<SessionSummary>,
}

fn summarise(session: &Value) -> SessionSummary {
    let strokes = session["strokes"].as_array().map(Vec::as_slice).unwrap_or_default();
    let mut clubs: Vec<ClubCount> = Vec::new();
    for stroke in strokes {
        let club = stroke["club"].as_str().filter(|c| !c.is_empty()).unwrap_or("Unknown club");
        match clubs.iter_mut().find(|c| c.club == club) {
            Some(c) => c.shots += 1,
            None => clubs.push(ClubCount { club: club.to_owned(), shots: 1 }),
        }
    }
    SessionSummary {
        id: session["id"].as_str().unwrap_or_default().to_owned(),
        kind: session["kind"].as_str().unwrap_or_default().to_owned(),
        time: session["time"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map(|t| t.with_timezone(&Utc)),
        shots: strokes.len(),
        clubs,
    }
}

async fn pull(args: PullArgs, options: PullOptions) -> Result<PullOutput> {
    let http = http();
    let api = api(&http).await?;
    let ids: Vec<String> = match args.id.filter(|ids| !ids.is_empty()) {
        Some(ids) => ids,
        None => {
            let o = &options;
            let filter =
                activity_filter(&o.kind, false, o.from.as_deref(), o.to.as_deref(), o.limit, o.include_hidden)?;
            api.activities(&filter).await?.into_iter().map(|a| a.id).collect()
        }
    };
    if ids.is_empty() {
        return Err(Error::Invalid("no sessions match the filter; nothing written".into()));
    }

    let progress = std::io::stderr().is_terminal();
    let mut sessions = Vec::with_capacity(ids.len());
    for (i, id) in ids.iter().enumerate() {
        if progress {
            eprint!("\r\x1b[2K{}", ui::err_dim(&format!("Downloading session {}/{}…", i + 1, ids.len())));
        }
        sessions.push(api.session(id).await?);
    }
    if progress {
        eprint!("\r\x1b[2K");
    }

    let out = options.out.map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(format!("sessions-{}.json", Utc::now().format("%Y%m%dT%H%M%SZ")))
    });
    let summaries: Vec<SessionSummary> = sessions.iter().map(summarise).collect();
    let document = json!({
        "source": "https://api.trackmangolf.com/graphql",
        "exported_at": Utc::now(),
        "units": "SI: m, m/s, s, deg, rpm; trajectory fits are ascending-power polynomials in s",
        "sessions": sessions,
    });
    let bytes = serde_json::to_vec_pretty(&document)?;
    std::fs::write(&out, &bytes)?;
    Ok(PullOutput {
        out: out.display().to_string(),
        bytes: bytes.len() as u64,
        shots: summaries.iter().map(|s| s.shots).sum(),
        sessions: summaries,
    })
}

fn render_pull(out: &PullOutput) -> String {
    let n = out.sessions.len();
    let rows: Vec<Vec<String>> = out
        .sessions
        .iter()
        .map(|s| {
            vec![
                s.time.map(ui::local_time).unwrap_or_default(),
                ui::kind_label(&s.kind),
                s.shots.to_string(),
                s.clubs
                    .iter()
                    .map(|c| format!("{} ×{}", ui::club_label(&c.club), c.shots))
                    .collect::<Vec<_>>()
                    .join(", "),
            ]
        })
        .collect();
    format!(
        "{} Saved {n} {} ({} shots) to {} {}\n\n{}",
        ui::green("✓"),
        if n == 1 { "session" } else { "sessions" },
        out.shots,
        ui::bold(&out.out),
        ui::dim(&format!("({})", ui::bytes(out.bytes))),
        ui::table(
            &[("Date", Align::Left), ("Kind", Align::Left), ("Shots", Align::Right), ("Clubs", Align::Left)],
            &rows
        )
    )
}

pub fn pull_command() -> CommandDef {
    CommandDef::typed::<PullArgs, PullOptions, (), PullOutput, _, _>(
        "pull",
        |ctx: TypedContext<PullArgs, PullOptions, ()>| async move {
            let human = is_human(&ctx);
            present(human, pull(ctx.args, ctx.options).await, render_pull, |_| None)
        },
    )
    .description("Download sessions with every stroke and Measurement field to one JSON file")
    .done()
}

pub fn group() -> Cli {
    Cli::create("sessions")
        .description("Historical sessions from TrackMan cloud")
        .command("list", list_command())
        .command("pull", pull_command())
}
