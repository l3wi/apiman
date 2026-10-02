//! TM4 discovery (SSDP) and the UPnP device description. This mirrors
//! `TrackMan.Api.Devices.Tracker.Create` and `SimpleServiceDiscovery`.

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use tokio::net::UdpSocket;
use url::Url;

use crate::error::{Error, Result};

pub const DEVICE_TYPE: &str = "urn:schemas-upnp-org:device:TrackMan:1";
const UPNP_NS: &str = "urn:schemas-upnp-org:device-1-0";
const DESCRIPTION_PORT: u16 = 2869;
const SSDP_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const SSDP_PORT: u16 = 1900;
const MIN_API_VERSION: (u32, u32) = (2, 0);

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Device {
    /// URL of the UPnP device description.
    pub location: String,
    pub friendly_name: String,
    pub serial_number: Option<String>,
    pub model_number: Option<String>,
    /// REST API base URL.
    pub api: String,
    /// Event WebSocket URL.
    pub websocket: String,
    pub camera_api: Option<String>,
    /// Host the TM4 serves SNTP on.
    pub ntp_host: String,
    /// REST API version reported by `GET <api>`.
    pub api_version: String,
}

/// Resolves `--host` (IP, hostname or description URL) to a device. With no
/// host, the first TM4 that answers SSDP is used.
pub async fn resolve(http: &reqwest::Client, host: Option<&str>, timeout: Duration) -> Result<Device> {
    let location = match host {
        Some(h) if h.contains("://") => h.to_owned(),
        Some(h) => format!("http://{h}:{DESCRIPTION_PORT}/"),
        None => discover(timeout, true)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| Error::Tm4("no TM4 answered on this network; pass --host <TM4-IP>".into()))?,
    };
    describe(http, &location).await
}

/// SSDP M-SEARCH on every up IPv4 interface. Returns description URLs, deduplicated by USN.
pub async fn discover(timeout: Duration, first_only: bool) -> Result<Vec<String>> {
    let search = format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: {SSDP_ADDR}:{SSDP_PORT}\r\nST:{DEVICE_TYPE}\r\nMAN:\"ssdp:discover\"\r\nMX:3\r\n\r\n"
    );
    let mut addrs: Vec<Ipv4Addr> = if_addrs::get_if_addrs()?
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            std::net::IpAddr::V4(v4) => Some(v4),
            _ => None,
        })
        .collect();
    if addrs.is_empty() {
        addrs.push(Ipv4Addr::UNSPECIFIED);
    }
    let mut sockets = Vec::new();
    for addr in addrs {
        if let Ok(socket) = UdpSocket::bind(SocketAddr::from((addr, 0))).await {
            socket.set_multicast_ttl_v4(2)?;
            sockets.push(socket);
        }
    }
    if sockets.is_empty() {
        return Err(Error::Tm4("could not bind any interface for SSDP".into()));
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut tasks = Vec::new();
    for socket in sockets {
        let tx = tx.clone();
        let search = search.clone();
        tasks.push(tokio::spawn(async move {
            let socket = std::sync::Arc::new(socket);
            let sender = socket.clone();
            let send = tokio::spawn(async move {
                for _ in 0..3 {
                    let _ = sender.send_to(search.as_bytes(), (SSDP_ADDR, SSDP_PORT)).await;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            });
            let mut buf = [0u8; 2048];
            while let Ok((n, _)) = socket.recv_from(&mut buf).await {
                if tx.send(String::from_utf8_lossy(&buf[..n]).into_owned()).is_err() {
                    break;
                }
            }
            send.abort();
        }));
    }
    drop(tx);

    let mut seen = HashSet::new();
    let mut found = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    while let Ok(Some(reply)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        let Some((usn, location)) = parse_ssdp_reply(&reply) else { continue };
        if seen.insert(usn) {
            found.push(location);
            if first_only {
                break;
            }
        }
    }
    for task in tasks {
        task.abort();
    }
    Ok(found)
}

/// Returns `(USN, LOCATION)` for a TrackMan reply.
fn parse_ssdp_reply(reply: &str) -> Option<(String, String)> {
    let mut st = None;
    let mut location = None;
    let mut usn = None;
    for line in reply.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim().to_owned();
        match key.trim().to_ascii_uppercase().as_str() {
            "ST" | "NT" => st = Some(value),
            "LOCATION" => location = Some(value),
            "USN" => usn = Some(value),
            _ => {}
        }
    }
    if st.as_deref() != Some(DEVICE_TYPE) {
        return None;
    }
    let location = location?;
    Some((usn.unwrap_or_else(|| location.clone()), location))
}

/// Turns a failed request to the TM4 into a message naming the root cause.
fn request_failed(what: &str, url: &str, error: &reqwest::Error) -> Error {
    if let Some(status) = error.status() {
        return Error::Tm4(format!("the TM4 {what} at {url} returned HTTP {status}"));
    }
    let detail = if error.is_timeout() {
        "timed out".to_owned()
    } else {
        let mut cause: &dyn std::error::Error = error;
        while let Some(next) = cause.source() {
            cause = next;
        }
        cause.to_string()
    };
    Error::Tm4(format!(
        "can't reach the TM4 {what} at {url} ({detail}); check the address and that this computer is on the TM4's network"
    ))
}

/// Fetches and parses the UPnP device description, then checks the API version.
pub async fn describe(http: &reqwest::Client, location: &str) -> Result<Device> {
    let location_url = Url::parse(location).map_err(|e| Error::Invalid(format!("{location}: {e}")))?;
    let host = location_url
        .host_str()
        .ok_or_else(|| Error::Invalid(format!("{location} has no host")))?
        .to_owned();
    let xml = http
        .get(location)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| request_failed("device description", location, &e))?
        .text()
        .await?;
    let doc = roxmltree::Document::parse(&xml)
        .map_err(|e| Error::Tm4(format!("device description is not XML: {e}")))?;
    let element = |name: &str| -> Option<String> {
        doc.descendants()
            .find(|n| n.tag_name().name() == name && n.tag_name().namespace() == Some(UPNP_NS))
            .and_then(|n| n.text())
            .map(|t| t.trim().to_owned())
    };
    let required = |name: &str| element(name).ok_or_else(|| Error::Tm4(format!("device description lacks <{name}>")));

    let device_type = required("deviceType")?;
    if device_type != DEVICE_TYPE {
        return Err(Error::Tm4(format!("{location} is a {device_type}, not a TrackMan")));
    }
    // Like the SDK, rebase every advertised URL onto the host we reached.
    let rebase = |raw: String| -> Result<String> {
        let mut url = Url::parse(&raw).map_err(|e| Error::Tm4(format!("bad URL {raw}: {e}")))?;
        url.set_host(Some(&host)).map_err(|e| Error::Tm4(format!("bad host {host}: {e}")))?;
        Ok(url.into())
    };
    let api = rebase(required("api")?)?;
    let websocket = rebase(required("webSocket")?)?;
    let camera_api = element("cameraApi").map(rebase).transpose()?;

    let version: Value = http
        .get(&api)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| request_failed("API", &api, &e))?
        .json()
        .await?;
    let api_version = version["Version"].as_str().unwrap_or_default().to_owned();
    if !version_supported(&api_version) {
        return Err(Error::Tm4(format!(
            "unsupported TM4 API version {api_version:?}; need >= {}.{}",
            MIN_API_VERSION.0, MIN_API_VERSION.1
        )));
    }

    Ok(Device {
        location: location.to_owned(),
        friendly_name: required("friendlyName")?,
        serial_number: element("serialNumber"),
        model_number: element("modelNumber"),
        api,
        websocket,
        camera_api,
        // The SDK queries SNTP on the API host; `ntpServer` is advisory.
        ntp_host: host,
        api_version,
    })
}

fn version_supported(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u32>());
    match (parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor))) => (major, minor) >= MIN_API_VERSION,
        (Some(Ok(major)), None) => (major, 0) >= MIN_API_VERSION,
        _ => false,
    }
}

/// `GET <api>/<route>` as JSON.
pub async fn api_get(http: &reqwest::Client, device: &Device, route: &str) -> Result<Value> {
    let url = Url::parse(&device.api)
        .and_then(|base| {
            let base = if base.path().ends_with('/') { base } else { Url::parse(&format!("{base}/"))? };
            base.join(route)
        })
        .map_err(|e| Error::Invalid(format!("route {route}: {e}")))?;
    Ok(http
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())?
        .json()
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssdp_reply_must_be_trackman() {
        let reply = "HTTP/1.1 200 OK\r\nST: urn:schemas-upnp-org:device:TrackMan:1\r\n\
                     LOCATION: http://10.0.0.5:2869/\r\nUSN: uuid:abc::urn:schemas-upnp-org:device:TrackMan:1\r\n\r\n";
        assert_eq!(
            parse_ssdp_reply(reply),
            Some((
                "uuid:abc::urn:schemas-upnp-org:device:TrackMan:1".into(),
                "http://10.0.0.5:2869/".into()
            ))
        );
        assert_eq!(parse_ssdp_reply(&reply.replace("TrackMan:1", "MediaRenderer:1")), None);
    }

    #[test]
    fn api_version_gate() {
        assert!(version_supported("2.0"));
        assert!(version_supported("3.1"));
        assert!(!version_supported("1.9"));
        assert!(!version_supported(""));
    }
}
