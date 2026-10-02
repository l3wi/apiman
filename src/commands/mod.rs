//! Command definitions. Each module owns its options, output types, logic and
//! human rendering.

pub mod account;
pub mod sessions;
pub mod tm4;

use std::time::Duration;

use trackman::cloud::api::Api;
use trackman::cloud::auth;
use trackman::{Error, Result};
use incurs::command::{McpAnnotations, McpCommandOptions, TypedContext, TypedResult};
use incurs::output::{CtaBlock, CtaEntry};

pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("trackman/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .expect("TLS backend initialises")
}

pub async fn api(http: &reqwest::Client) -> Result<Api> {
    Ok(Api::new(http.clone(), auth::access_token(http).await?))
}

/// A person at a terminal who did not ask for a machine format.
pub fn is_human<A, O, E>(ctx: &TypedContext<A, O, E>) -> bool {
    !ctx.agent && !ctx.format_explicit
}

/// MCP annotation for commands that only read, from the cloud or a TM4.
pub fn read_only() -> McpCommandOptions {
    McpCommandOptions {
        annotations: Some(McpAnnotations {
            read_only_hint: Some(true),
            open_world_hint: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Converts a handler result into the incurs result. A person at a terminal
/// (`human`) gets `render`'s text on stdout; everyone else gets the structured
/// value. `cta` suggests follow-up commands, which incurs prints for people
/// and embeds for agents.
pub fn present<T>(
    human: bool,
    result: Result<T>,
    render: impl FnOnce(&T) -> String,
    cta: impl FnOnce(&T) -> Option<CtaBlock>,
) -> TypedResult<T> {
    match result {
        Ok(value) => {
            if human {
                println!("{}", render(&value));
            }
            match cta(&value) {
                Some(block) => TypedResult::ok_with_cta(value, block),
                None => TypedResult::ok(value),
            }
        }
        Err(error) => error_result(error),
    }
}

fn error_result<T>(error: Error) -> TypedResult<T> {
    TypedResult::Error {
        code: error.code().into(),
        message: error.to_string(),
        retryable: error.is_retryable(),
        exit_code: None,
        cta: error.needs_login().then(|| suggest(&[("login", "Sign in to TrackMan")])),
    }
}

/// Builds a CTA block from `(command, description)` pairs; incurs prefixes `trackman`.
pub fn suggest(commands: &[(&str, &str)]) -> CtaBlock {
    CtaBlock {
        commands: commands
            .iter()
            .map(|(command, description)| CtaEntry::Detailed {
                command: (*command).to_owned(),
                description: Some((*description).to_owned()),
            })
            .collect(),
        description: Some("Next:".into()),
    }
}
