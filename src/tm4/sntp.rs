//! Minimal SNTPv4 client (RFC 4330). Used to measure the offset of the
//! TM4 clock, which timestamps `Measurement.Time`, from the host clock.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::Serialize;
use tokio::net::UdpSocket;

use crate::error::{Error, Result};

/// Seconds from 1900-01-01 (NTP era 0) to 1970-01-01.
const NTP_UNIX_OFFSET: f64 = 2_208_988_800.0;

#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
pub struct SntpSample {
    /// TM4 clock minus host clock, seconds. host_time = tm4_time - offset_s.
    pub offset_s: f64,
    /// Round-trip delay, seconds. Offset error is bounded by delay_s / 2.
    pub delay_s: f64,
    /// Server stratum (0 = unsynchronised or kiss-of-death).
    pub stratum: u8,
}

fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_secs_f64()
}

fn to_ntp(unix: f64) -> [u8; 8] {
    let ntp = unix + NTP_UNIX_OFFSET;
    let secs = ntp.trunc() as u32;
    let frac = (ntp.fract() * 4_294_967_296.0) as u32;
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&secs.to_be_bytes());
    out[4..].copy_from_slice(&frac.to_be_bytes());
    out
}

fn from_ntp(bytes: &[u8]) -> f64 {
    let secs = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as f64;
    let frac = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as f64 / 4_294_967_296.0;
    secs + frac - NTP_UNIX_OFFSET
}

pub async fn query(host: &str, port: u16, timeout: Duration) -> Result<SntpSample> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect((host, port)).await?;

    let mut request = [0u8; 48];
    request[0] = (4 << 3) | 3; // LI 0, version 4, mode 3 (client)
    let t1 = now_unix();
    let t1_wire = to_ntp(t1);
    request[40..48].copy_from_slice(&t1_wire);
    socket.send(&request).await?;

    let mut response = [0u8; 48];
    let received = tokio::time::timeout(timeout, socket.recv(&mut response))
        .await
        .map_err(|_| Error::Tm4(format!("SNTP {host}:{port} timed out")))??;
    let t4 = now_unix();
    if received < 48 {
        return Err(Error::Tm4(format!("SNTP reply too short ({received} bytes)")));
    }
    let mode = response[0] & 0x7;
    if mode != 4 {
        return Err(Error::Tm4(format!("SNTP reply has mode {mode}, expected 4 (server)")));
    }
    // The origin timestamp must echo our transmit timestamp.
    if response[24..32] != t1_wire {
        return Err(Error::Tm4("SNTP reply does not match request".into()));
    }
    let t2 = from_ntp(&response[32..40]);
    let t3 = from_ntp(&response[40..48]);
    Ok(SntpSample {
        offset_s: ((t2 - t1) + (t3 - t4)) / 2.0,
        delay_s: (t4 - t1) - (t3 - t2),
        stratum: response[1],
    })
}
