//! `trackman`: a Trackman compatible CLI & MCP client.
//!
//! - `trackman tm4 …`: a TrackMan 4 on the local network (discovery, status,
//!   live capture to JSONL).
//! - `trackman login` / `sessions …`: TrackMan cloud, for historical sessions.
//!
//! People at a terminal get formatted text (each command renders its own);
//! agents, pipes, MCP and any explicit `--format` get structured data.

mod commands;
mod ui;

use incurs::cli::Cli;
use incurs::output::OutputPolicy;

fn build_cli() -> Cli {
    Cli::create("trackman")
        .version(env!("CARGO_PKG_VERSION"))
        .description("A Trackman compatible CLI & MCP client")
        .output_policy(OutputPolicy::AgentOnly)
        .command("login", commands::account::login_command())
        .command("logout", commands::account::logout_command())
        .command("whoami", commands::account::whoami_command())
        .group(commands::sessions::group())
        .group(commands::tm4::group())
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    build_cli().serve().await
}
