//! TrackMan cloud login: OAuth 2.0 device authorization grant against
//! login.trackmangolf.com, with a refreshable token cached on disk.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const AUTHORITY: &str = "https://login.trackmangolf.com";
/// Public client of the TrackMan Golf Android app (no secret). It is accepted
/// for the device authorization grant.
const CLIENT_ID: &str = "old-golf-app.c686e909-5102-45ac-9860-8d0b789073ae";
/// The scopes the TrackMan Golf app requests.
const SCOPES: &str = "openid offline_access profile https://auth.trackman.com/dr/simulate \
                      https://auth.trackman.com/dr/cloud https://auth.trackman.com/login/autoconsent";
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Refresh this long before the access token expires.
const EXPIRY_MARGIN: chrono::Duration = chrono::Duration::seconds(60);

/// Raw bearer token override; skips the token cache entirely.
pub const TOKEN_ENV: &str = "TRACKMAN_TOKEN";
/// Token cache location override.
pub const TOKEN_FILE_ENV: &str = "TRACKMAN_TOKEN_FILE";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub scope: Option<String>,
}

#[derive(Deserialize)]
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: u64,
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: i64,
    scope: Option<String>,
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
    error_description: Option<String>,
}

pub fn token_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(TOKEN_FILE_ENV) {
        return Ok(PathBuf::from(path));
    }
    let dir = dirs::config_dir()
        .ok_or_else(|| Error::Invalid(format!("no config directory; set {TOKEN_FILE_ENV}")))?;
    Ok(dir.join("trackman").join("token.json"))
}

pub fn load() -> Result<Option<TokenSet>> {
    let path = token_path()?;
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn save(tokens: &TokenSet) -> Result<PathBuf> {
    let path = token_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(tokens)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Deletes the cached token. Returns whether one existed.
pub fn clear() -> Result<bool> {
    match std::fs::remove_file(token_path()?) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Runs the device authorization grant. `prompt` receives the verification URL
/// and user code. The call returns once the user approves, denies, or the code expires.
pub async fn device_login(
    http: &reqwest::Client,
    prompt: impl FnOnce(&str, &str),
) -> Result<TokenSet> {
    let auth: DeviceAuthorization = oauth_post(
        http,
        "/connect/deviceauthorization",
        &[("client_id", CLIENT_ID), ("scope", SCOPES)],
    )
    .await?;
    let url = auth
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&auth.verification_uri);
    prompt(url, &auth.user_code);

    let mut interval = Duration::from_secs(auth.interval.unwrap_or(5).max(1));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(auth.expires_in);
    loop {
        tokio::time::sleep(interval).await;
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Auth("device code expired before approval".into()));
        }
        let response = http
            .post(format!("{AUTHORITY}/connect/token"))
            .form(&[
                ("grant_type", DEVICE_CODE_GRANT),
                ("client_id", CLIENT_ID),
                ("device_code", auth.device_code.as_str()),
            ])
            .send()
            .await?;
        if response.status().is_success() {
            return Ok(response.json::<TokenResponse>().await?.into());
        }
        let error: OAuthError = response.json().await?;
        match error.error.as_str() {
            "authorization_pending" => {}
            "slow_down" => interval += Duration::from_secs(5),
            _ => return Err(oauth_error(error)),
        }
    }
}

/// Returns a valid bearer token, refreshing and re-caching it if needed.
pub async fn access_token(http: &reqwest::Client) -> Result<String> {
    if let Ok(token) = std::env::var(TOKEN_ENV) {
        if !token.trim().is_empty() {
            return Ok(token.trim().to_owned());
        }
    }
    let tokens = load()?.ok_or(Error::NotLoggedIn)?;
    if tokens.expires_at - EXPIRY_MARGIN > Utc::now() {
        return Ok(tokens.access_token);
    }
    let refresh = tokens.refresh_token.as_deref().ok_or(Error::NotLoggedIn)?;
    let mut refreshed: TokenSet = oauth_post::<TokenResponse>(
        http,
        "/connect/token",
        &[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh),
        ],
    )
    .await
    .map_err(|e| Error::Auth(format!("the saved login expired and could not be refreshed ({e})")))?
    .into();
    // Servers may omit the refresh token when it is not rotated.
    if refreshed.refresh_token.is_none() {
        refreshed.refresh_token = tokens.refresh_token;
    }
    save(&refreshed)?;
    Ok(refreshed.access_token)
}

async fn oauth_post<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    path: &str,
    form: &[(&str, &str)],
) -> Result<T> {
    let response = http.post(format!("{AUTHORITY}{path}")).form(form).send().await?;
    if response.status().is_success() {
        Ok(response.json().await?)
    } else {
        let status = response.status();
        let body = response.text().await?;
        Err(match serde_json::from_str::<OAuthError>(&body) {
            Ok(error) => oauth_error(error),
            Err(_) => Error::Auth(format!("{path} returned {status}: {body}")),
        })
    }
}

fn oauth_error(error: OAuthError) -> Error {
    Error::Auth(match error.error_description {
        Some(description) => format!("{}: {description}", error.error),
        None => error.error,
    })
}

impl From<TokenResponse> for TokenSet {
    fn from(response: TokenResponse) -> Self {
        TokenSet {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            expires_at: Utc::now() + chrono::Duration::seconds(response.expires_in),
            scope: response.scope,
        }
    }
}
