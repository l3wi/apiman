//! TrackMan cloud GraphQL (`https://api.trackmangolf.com/graphql`).

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{Error, Result};

const ENDPOINT: &str = "https://api.trackmangolf.com/graphql";
const PAGE_SIZE: u32 = 100;

const ACTIVITIES: &str = include_str!("../graphql/activities.graphql");
const SESSION: &str = include_str!("../graphql/session.graphql");
const ME: &str = include_str!("../graphql/me.graphql");

/// Activity kinds whose GraphQL type implements `SessionActivityInterface`,
/// i.e. carries per-stroke `Measurement`s.
pub const STROKE_KINDS: &[&str] = &[
    "SESSION",
    "SHOT_ANALYSIS",
    "MAP_MY_BAG",
    "VIRTUAL_RANGE",
    "PERFORMANCE_PUTTING",
    "SIMULATOR",
    "TRACY",
    "TEST",
    "COMBINE_TEST",
];

pub struct Api {
    http: reqwest::Client,
    token: String,
}

#[derive(Debug, Clone, Default)]
pub struct ActivityFilter {
    /// `None` lists every kind.
    pub kinds: Option<Vec<String>>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub include_hidden: bool,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    /// Relay node ID; pass to `sessions pull`.
    pub id: String,
    /// Activity kind, e.g. `SESSION` or `SHOT_ANALYSIS`.
    pub kind: String,
    /// Activity start time (UTC).
    pub time: DateTime<Utc>,
    /// GraphQL type name.
    #[serde(rename(deserialize = "__typename"))]
    pub typename: String,
    #[serde(default)]
    pub is_hidden: Option<bool>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub stroke_count: Option<u32>,
}

#[derive(Deserialize)]
struct GraphQlResponse {
    data: Option<Value>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

#[derive(Deserialize)]
struct GraphQlError {
    message: String,
    #[serde(default)]
    path: Vec<Value>,
    #[serde(default)]
    extensions: Value,
}

impl Api {
    pub fn new(http: reqwest::Client, token: String) -> Self {
        Api { http, token }
    }

    async fn query<T: DeserializeOwned>(&self, document: &str, variables: Value) -> Result<T> {
        let response = self
            .http
            .post(ENDPOINT)
            .bearer_auth(&self.token)
            .json(&json!({ "query": document, "variables": variables }))
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        let parsed: GraphQlResponse = serde_json::from_str(&body).map_err(|_| {
            Error::GraphQl(format!("HTTP {status}: {}", body.chars().take(500).collect::<String>()))
        })?;
        // Errors first: they usually come with `data` fields nulled out. Strict:
        // a partially resolved session would silently drop strokes.
        if !parsed.errors.is_empty() {
            let unauthenticated = parsed
                .errors
                .iter()
                .any(|e| e.extensions["code"].as_str().is_some_and(|c| c.starts_with("AUTH_")));
            let joined = parsed
                .errors
                .iter()
                .map(|e| {
                    let path: Vec<String> = e.path.iter().map(|p| p.to_string().replace('"', "")).collect();
                    if path.is_empty() { e.message.clone() } else { format!("{} (at {})", e.message, path.join(".")) }
                })
                .collect::<Vec<_>>()
                .join("; ");
            return Err(if unauthenticated {
                Error::Unauthorized(joined)
            } else {
                Error::GraphQl(joined)
            });
        }
        let data = parsed.data.ok_or_else(|| Error::GraphQl(format!("HTTP {status}: response has no data")))?;
        Ok(serde_json::from_value(data)?)
    }

    pub async fn me(&self) -> Result<Value> {
        let data: Value = self.query(ME, json!({})).await?;
        Ok(data["me"]["profile"].clone())
    }

    pub async fn activities(&self, filter: &ActivityFilter) -> Result<Vec<Activity>> {
        #[derive(Deserialize)]
        struct Data {
            me: Me,
        }
        #[derive(Deserialize)]
        struct Me {
            activities: Page,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            items: Vec<Activity>,
            page_info: PageInfo,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct PageInfo {
            has_next_page: bool,
        }

        let mut out = Vec::new();
        loop {
            let remaining = filter.limit.map(|l| l.saturating_sub(out.len() as u32));
            if remaining == Some(0) {
                break;
            }
            let take = remaining.map_or(PAGE_SIZE, |r| r.min(PAGE_SIZE));
            let mut variables = json!({
                "kinds": filter.kinds,
                "timeFrom": filter.from,
                "timeTo": filter.to,
                "includeHidden": filter.include_hidden,
                "skip": out.len(),
                "take": take,
            });
            // An explicit null can mean "match null" rather than "no filter".
            if let Value::Object(map) = &mut variables {
                map.retain(|_, v| !v.is_null());
            }
            let page = self.query::<Data>(ACTIVITIES, variables).await?.me.activities;
            let count = page.items.len();
            out.extend(page.items);
            if !page.page_info.has_next_page || count == 0 {
                break;
            }
        }
        Ok(out)
    }

    /// Fetches one activity with every stroke and every `Measurement` field,
    /// exactly as the API returns it.
    pub async fn session(&self, id: &str) -> Result<Value> {
        let data: Value = self.query(SESSION, json!({ "id": id })).await?;
        let node = data["node"].clone();
        if node.is_null() {
            return Err(Error::GraphQl(format!("activity {id} not found")));
        }
        if node.get("strokes").is_none() {
            return Err(Error::Invalid(format!(
                "activity {id} is a {} and has no strokes; stroke-bearing kinds: {}",
                node["__typename"].as_str().unwrap_or("?"),
                STROKE_KINDS.join(", ")
            )));
        }
        Ok(node)
    }
}
