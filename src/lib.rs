//! A Trackman compatible client library.
//!
//! - [`tm4`]: a TrackMan 4 on the local network. Discovery (SSDP), the UPnP
//!   device description, the REST API, live events over WebSocket in viewer
//!   role, and the unit's SNTP clock.
//! - [`cloud`]: TrackMan cloud. OAuth device-code login against
//!   `login.trackmangolf.com` and the GraphQL API at `api.trackmangolf.com`
//!   for historical sessions with every stroke and measurement.
//!
//! The `trackman` binary (feature `cli`, on by default) exposes both as a CLI and
//! an MCP server.

pub mod cloud;
pub mod error;
pub mod tm4;

pub use error::{Error, Result};
