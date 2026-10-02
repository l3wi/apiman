# apiman

A Trackman compatible CLI & MCP client.

- **Live (TM4 on your network):** discover a TrackMan 4, read its status and clock, and record every live event to JSONL. It connects in viewer role and needs no login, so it can run while TPS or the TrackMan app operates the unit.
- **Historical (TrackMan cloud):** sign in with a device code, list your sessions, and export them to JSON with every stroke and every `Measurement` field.

This repo builds the `trackman` package: a Rust library (`trackman::tm4`, `trackman::cloud`) plus the `trackman` CLI, built on [incurs](https://crates.io/crates/incurs). The same commands are therefore available as an MCP server named `trackman`. The protocol details are in [docs/protocol.md](docs/protocol.md).

> Unofficial. Not affiliated with or endorsed by TrackMan A/S. "TrackMan" is a trademark of its owner.

## Install

```sh
cargo install --path .        # installs `trackman` into ~/.cargo/bin
```

This requires Rust 1.88 or newer.

## CLI

### Live TM4

```sh
trackman tm4 discover                       # find TM4 units on this network (SSDP)
trackman tm4 status --host 192.168.1.40     # state, mode, ball/club, time source, clock offset
trackman tm4 log --host 192.168.1.40 --out range.jsonl
```

- **Stopping:** `tm4 log` records until it receives Ctrl-C, or until `--duration <s>` or `--shots <n>` is reached.
- **Connection:** it reconnects automatically after Wi-Fi drops, and without `--host` it uses the first TM4 that answers discovery.
- **Progress:** each completed shot is printed to stderr as a table row; `--quiet` suppresses this.

Every JSONL line is one object with `record` and `host_time` (host UTC, nanosecond RFC 3339):

| `record` | Contents |
|---|---|
| `session` | Written once: the device, `systeminfo`, `status`, `setup` (mode, tee and target positions, ball type), `timesync` |
| `sntp` | `sample: {offset_s, delay_s, stratum}`, where `offset_s` is the TM4 clock minus the host clock. Sampled at start, every `--sntp-interval` seconds (default 30) and at stop |
| `connection` | WebSocket `connected`, `disconnected` or `failed` |
| `message` | A TM4 event, verbatim: `type`, `subtype`, `id`, `payload` |

**Shots.** Each shot arrives as several `Measurement` messages that share `payload.Id`: `PreLaunchData` → `LaunchData` → `LiveApex` → `FlightData` → `Measurement`. Use the one where `payload.Kind == "Measurement"`.
- Values are SI: m/s, degrees, rpm, metres and seconds.
- `payload.Time` is on the TM4 clock. Convert to host time with `host_time = payload.Time − offset_s`.

### TrackMan cloud

```sh
trackman login                              # device code: approve in the browser that opens
trackman whoami
trackman sessions list --from 2026-09-01    # stroke-bearing kinds; --all-kinds for everything
trackman sessions pull <ID> [<ID>…] --out sessions.json
trackman sessions pull --from 2026-09-01 --kind SESSION --out september.json
```

- **Login** uses the OAuth device authorization grant against `login.trackmangolf.com`.
- **Token cache:** the token is stored at `$TRACKMAN_TOKEN_FILE`, or by default at `<config dir>/trackman/token.json` (mode 0600), and refreshes automatically. `TRACKMAN_TOKEN=<bearer>` overrides the cache.
- **`sessions pull`** writes `{source, exported_at, units, sessions: [...]}`. Each stroke includes `time`, `club`, `ball`, `tags`, `clubData`, `impactLocation`, `measurement` and `normalizedMeasurement`. Each measurement has all 72 scalar fields plus the ball and club trajectory polynomials.
- **Default kinds:** without `--kind`, `list` and `pull` use the activity kinds that carry strokes: `SESSION`, `SHOT_ANALYSIS`, `MAP_MY_BAG`, `VIRTUAL_RANGE`, `PERFORMANCE_PUTTING`, `SIMULATOR`, `TRACY`, `TEST` and `COMBINE_TEST`.

### Output and MCP

- **At a terminal:** each command prints a formatted summary (tables, local times, mph and yards) followed by suggested next commands.
- **Piped, or with `--json` / `--format yaml|toon|md|jsonl`:** the command prints its structured result, with values in SI.
- **MCP:** `trackman --mcp` runs a stdio MCP server, and `trackman mcp add` registers it with detected clients. `whoami`, `sessions list`, `tm4 discover` and `tm4 status` are marked read-only.
- **Schemas:** every command supports `--schema`, and `--llms` prints an agent-readable manifest.

## Library

```toml
[dependencies]
trackman = { git = "https://github.com/l3wi/apiman", default-features = false }
```

Without the `cli` feature, incurs is not pulled in.

```rust
use std::time::Duration;
use trackman::tm4::{device, live};

let http = reqwest::Client::new();
let tm4 = device::resolve(&http, Some("192.168.1.40"), Duration::from_secs(4)).await?;
let summary = live::run(
    &http,
    tm4,
    live::LogConfig {
        out: "range.jsonl".into(),
        topics: live::DEFAULT_TOPICS.iter().map(|t| t.to_string()).collect(),
        duration: None,
        shots: Some(10),
        sntp_interval: Duration::from_secs(30),
        sntp_port: 123,
    },
    async { let _ = tokio::signal::ctrl_c().await; },
    |event| {
        if let live::LiveEvent::Shot { number, measurement } = event {
            println!("shot {number}: {} m/s", measurement["BallSpeed"]);
        }
    },
)
.await?;
```

```rust
use trackman::cloud::{api::{ActivityFilter, Api, STROKE_KINDS}, auth};

let http = reqwest::Client::new();
let api = Api::new(http.clone(), auth::access_token(&http).await?);
let filter = ActivityFilter {
    kinds: Some(STROKE_KINDS.iter().map(|k| k.to_string()).collect()),
    limit: Some(5),
    ..Default::default()
};
for activity in api.activities(&filter).await? {
    let session = api.session(&activity.id).await?; // every stroke and Measurement field
}
```

## Notes

- **macOS Local Network permission:** the first multicast `tm4 discover` from a newly built binary can return nothing until macOS grants Local Network access to the terminal. If discovery stays empty, pass `--host`.
- **TM4 state:** `trackman` never logs in to the TM4, never changes its setup and never sets its clock.
