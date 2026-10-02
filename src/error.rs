//! Error type shared by the TM4 and cloud clients.

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No cached TrackMan login and no `TRACKMAN_TOKEN`.
    #[error("not logged in to TrackMan")]
    NotLoggedIn,
    /// Device-code login or token refresh failed.
    #[error("login failed: {0}")]
    Auth(String),
    /// TrackMan cloud rejected the access token.
    #[error("TrackMan rejected the saved login: {0}")]
    Unauthorized(String),
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// The GraphQL API returned errors.
    #[error("TrackMan API returned errors: {0}")]
    GraphQl(String),
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON handling failed: {0}")]
    Json(#[from] serde_json::Error),
    /// TM4 unreachable or not behaving as a TM4.
    #[error("{0}")]
    Tm4(String),
    #[error("TM4 WebSocket failed: {0}")]
    WebSocket(Box<tokio_tungstenite::tungstenite::Error>),
    #[error("{0}")]
    Invalid(String),
}

impl From<tokio_tungstenite::tungstenite::Error> for Error {
    fn from(error: tokio_tungstenite::tungstenite::Error) -> Self {
        Error::WebSocket(Box::new(error))
    }
}

impl Error {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Error::NotLoggedIn => "NOT_LOGGED_IN",
            Error::Auth(_) => "AUTH_FAILED",
            Error::Http(_) => "HTTP_FAILED",
            Error::GraphQl(_) => "API_ERROR",
            Error::Unauthorized(_) => "UNAUTHORIZED",
            Error::Io(_) => "IO_FAILED",
            Error::Json(_) => "JSON_FAILED",
            Error::Tm4(_) => "TM4_FAILED",
            Error::WebSocket(_) => "TM4_WEBSOCKET_FAILED",
            Error::Invalid(_) => "INVALID_INPUT",
        }
    }

    /// Whether signing in again could fix this error.
    pub fn needs_login(&self) -> bool {
        matches!(self, Error::NotLoggedIn | Error::Unauthorized(_) | Error::Auth(_))
    }

    /// Whether retrying the same call may succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Error::Http(_) | Error::WebSocket(_))
    }
}
