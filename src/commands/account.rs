//! `login`, `logout`, `whoami`: TrackMan cloud account.

use chrono::{DateTime, Utc};
use incurs::command::{CommandDef, TypedContext};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use apiman::Result;
use apiman::cloud::api::Api;
use apiman::cloud::auth;

use super::{api, http, is_human, present, read_only, suggest};
use crate::ui;

// ─── login ──────────────────────────────────────────────────────────────────

#[derive(Deserialize, incurs::Options)]
pub struct LoginOptions {
    /// Only print the verification URL; do not open a browser.
    print_url: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct LoginOutput {
    /// Player display name.
    name: Option<String>,
    email: Option<String>,
    /// Token cache file.
    token_file: String,
    /// Access-token expiry. With `renews`, the login outlives it.
    expires_at: DateTime<Utc>,
    /// Whether a refresh token was issued, so the login renews automatically.
    renews: bool,
}

async fn login(options: LoginOptions) -> Result<LoginOutput> {
    let http = http();
    let tokens = auth::device_login(&http, |url, code| {
        eprintln!("To sign in to TrackMan, open\n\n    {}\n", ui::err_bold(url));
        eprintln!("and check the page shows code {}.", ui::err_bold(&group_digits(code)));
        if !options.print_url {
            let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
            let _ = std::process::Command::new(opener).arg(url).status();
        }
        eprintln!("{}", ui::err_dim("Waiting for approval… (Ctrl-C to cancel)"));
    })
    .await?;
    let path = auth::save(&tokens)?;
    // Best effort: the login already succeeded even if the profile is unavailable.
    let profile = Api::new(http.clone(), tokens.access_token.clone())
        .me()
        .await
        .unwrap_or(Value::Null);
    Ok(LoginOutput {
        name: display_name(&profile),
        email: profile["email"].as_str().map(str::to_owned),
        token_file: path.display().to_string(),
        expires_at: tokens.expires_at,
        renews: tokens.refresh_token.is_some(),
    })
}

/// `772805855` → `772 805 855`, matching how TrackMan shows the code.
fn group_digits(code: &str) -> String {
    let chars: Vec<char> = code.chars().collect();
    chars.chunks(3).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" ")
}

fn display_name(profile: &Value) -> Option<String> {
    ["nickName", "fullName", "playerName"]
        .iter()
        .find_map(|k| profile[*k].as_str().filter(|s| !s.trim().is_empty()))
        .map(str::to_owned)
}

fn render_login(out: &LoginOutput) -> String {
    let who = match (&out.name, &out.email) {
        (Some(n), Some(e)) => format!("{} {}", ui::bold(n), ui::dim(&format!("<{e}>"))),
        (Some(n), None) => ui::bold(n),
        (None, Some(e)) => ui::bold(e),
        (None, None) => "TrackMan".into(),
    };
    format!(
        "{} Logged in as {who}\n\n{}",
        ui::green("✓"),
        ui::fields(&[
            ("Login", session_line(Some(out.expires_at), out.renews)),
            ("Saved to", out.token_file.clone()),
        ])
    )
}

fn session_line(expires_at: Option<DateTime<Utc>>, renews: bool) -> String {
    match expires_at {
        Some(t) if renews => format!(
            "renews automatically {}",
            ui::dim(&format!("(current token valid until {}, {})", ui::local_time(t), ui::relative(t)))
        ),
        Some(t) if t > Utc::now() => format!("valid until {} ({})", ui::local_time(t), ui::relative(t)),
        Some(t) => ui::yellow(&format!("expired {}", ui::relative(t))),
        None => format!("from ${}", auth::TOKEN_ENV),
    }
}

pub fn login_command() -> CommandDef {
    CommandDef::typed::<(), LoginOptions, (), LoginOutput, _, _>(
        "login",
        |ctx: TypedContext<(), LoginOptions, ()>| async move {
            let human = is_human(&ctx);
            present(human, login(ctx.options).await, render_login, |_| {
                Some(suggest(&[("sessions list", "See your TrackMan sessions")]))
            })
        },
    )
    .description("Log in to TrackMan cloud with a device code (opens a browser) and cache the token")
    .done()
}

// ─── logout ─────────────────────────────────────────────────────────────────

#[derive(Serialize, JsonSchema)]
pub struct LogoutOutput {
    token_file: String,
    /// Whether a cached token was deleted.
    removed: bool,
}

pub fn logout_command() -> CommandDef {
    CommandDef::typed::<(), (), (), LogoutOutput, _, _>("logout", |ctx: TypedContext<(), (), ()>| async move {
        let result = auth::token_path().and_then(|path| {
            Ok(LogoutOutput { removed: auth::clear()?, token_file: path.display().to_string() })
        });
        present(
            is_human(&ctx),
            result,
            |out| {
                if out.removed {
                    format!("{} Logged out {}", ui::green("✓"), ui::dim(&format!("(removed {})", out.token_file)))
                } else {
                    "Already logged out.".to_owned()
                }
            },
            |_| None,
        )
    })
    .description("Delete the cached TrackMan cloud token")
    .done()
}

// ─── whoami ─────────────────────────────────────────────────────────────────

#[derive(Serialize, JsonSchema)]
pub struct WhoamiOutput {
    /// Player profile from `me.profile`.
    profile: Value,
    /// Access-token expiry when the token came from the cache; null for `TRACKMAN_TOKEN`.
    expires_at: Option<DateTime<Utc>>,
    /// Whether the cached login renews automatically.
    renews: bool,
}

async fn whoami() -> Result<WhoamiOutput> {
    let http = http();
    let profile = api(&http).await?.me().await?;
    let cached = if std::env::var_os(auth::TOKEN_ENV).is_some() { None } else { auth::load()? };
    Ok(WhoamiOutput {
        profile,
        expires_at: cached.as_ref().map(|t| t.expires_at),
        renews: cached.is_some_and(|t| t.refresh_token.is_some()),
    })
}

fn render_whoami(out: &WhoamiOutput) -> String {
    let p = &out.profile;
    let mut header = ui::bold(&display_name(p).unwrap_or_else(|| "TrackMan player".into()));
    if let Some(email) = p["email"].as_str() {
        header.push_str(&format!("  {}", ui::dim(email)));
    }
    let mut rows = Vec::new();
    if let Some(id) = p["dbId"].as_str() {
        rows.push(("Player ID", id.to_owned()));
    }
    rows.push(("Login", session_line(out.expires_at, out.renews)));
    format!("{header}\n\n{}", ui::fields(&rows))
}

pub fn whoami_command() -> CommandDef {
    CommandDef::typed::<(), (), (), WhoamiOutput, _, _>("whoami", |ctx: TypedContext<(), (), ()>| async move {
        present(is_human(&ctx), whoami().await, render_whoami, |_| None)
    })
    .description("Show the logged-in TrackMan player profile")
    .mcp(read_only())
    .done()
}
