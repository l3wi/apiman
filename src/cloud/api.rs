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
const ROUND: &str = include_str!("../graphql/round.graphql");
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

/// Exportable activity kinds, including rounds whose shots live on a scorecard.
/// `STROKE_KINDS` remains the set implementing `SessionActivityInterface`.
pub const SESSION_KINDS: &[&str] = &[
    "SESSION",
    "SHOT_ANALYSIS",
    "MAP_MY_BAG",
    "VIRTUAL_RANGE",
    "PERFORMANCE_PUTTING",
    "SIMULATOR",
    "TRACY",
    "TEST",
    "COMBINE_TEST",
    "COURSE_PLAY",
];

pub struct Api {
    endpoint: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub course: Option<CourseSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scorecard: Option<ScorecardReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CourseSummary {
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScorecardReference {
    pub id: String,
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
        Api {
            http,
            token,
            endpoint: ENDPOINT.into(),
        }
    }

    async fn query<T: DeserializeOwned>(&self, document: &str, variables: Value) -> Result<T> {
        let response = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .json(&json!({ "query": document, "variables": variables }))
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        let parsed: GraphQlResponse = serde_json::from_str(&body)
            .map_err(|_| Error::GraphQl(format!("HTTP {status}: {}", body.chars().take(500).collect::<String>())))?;
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
                    if path.is_empty() {
                        e.message.clone()
                    } else {
                        format!("{} (at {})", e.message, path.join("."))
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");
            return Err(if unauthenticated {
                Error::Unauthorized(joined)
            } else {
                Error::GraphQl(joined)
            });
        }
        let data = parsed
            .data
            .ok_or_else(|| Error::GraphQl(format!("HTTP {status}: response has no data")))?;
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

    /// Fetch a scorecard by its Relay node ID, including regular and playoff holes.
    /// GraphQL errors fail the request rather than silently exporting partial shots.
    pub async fn round(&self, id: &str) -> Result<Value> {
        let data: Value = self.query(ROUND, json!({ "id": id })).await?;
        let node = data["node"].clone();
        if node.is_null() {
            return Err(Error::GraphQl(format!("scorecard {id} not found")));
        }
        if node["__typename"] != "Scorecard" {
            return Err(Error::Invalid(format!("node {id} is not a scorecard")));
        }
        Ok(node)
    }

    /// Fetches one activity with every stroke and every `Measurement` field,
    /// preserving the API fields. Course-play activities include a full `scorecard`
    /// and flattened `strokes` with `holeNumber` and `isPlayoff` annotations.
    /// A scorecard's gross score may include penalties and automatic putts that
    /// are not shot records; `strokeCount` counts only returned shot records.
    pub async fn session(&self, id: &str) -> Result<Value> {
        let data: Value = self.query(SESSION, json!({ "id": id })).await?;
        let mut node = data["node"].clone();
        if node.is_null() {
            return Err(Error::GraphQl(format!("activity {id} not found")));
        }
        if node["__typename"] == "CoursePlayActivity" {
            let scorecard_id = node["scorecard"]["id"]
                .as_str()
                .ok_or_else(|| Error::Invalid(format!("course activity {id} has no scorecard")))?;
            node["scorecard"] = self.round(scorecard_id).await?;
            return course_session(node);
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

// Keep the activity ID/kind/time and original scorecard, while exposing shots
// through the same top-level collection as practice and combine sessions.
fn course_session(mut activity: Value) -> Result<Value> {
    let mut strokes = Vec::new();
    for (field, is_playoff) in [("holes", false), ("playOffHoles", true)] {
        if let Some(holes) = activity["scorecard"][field].as_array() {
            for hole in holes {
                if let Some(shots) = hole["shots"].as_array() {
                    for shot in shots {
                        if !shot.is_object() {
                            return Err(Error::GraphQl("scorecard contains an unresolved shot".into()));
                        }
                        let mut stroke = shot.clone();
                        stroke["holeNumber"] = hole["holeNumber"].clone();
                        stroke["isPlayoff"] = json!(is_playoff);
                        strokes.push(stroke);
                    }
                }
            }
        }
    }
    activity["strokeCount"] = json!(strokes.len());
    activity["strokes"] = json!(strokes);
    Ok(activity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Check complete HTTP requests as well as dispatch/response handling.
    async fn mock_api(responses: Vec<Value>) -> (Api, tokio::task::JoinHandle<Vec<Value>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let body_start = loop {
                    let mut buf = [0; 4096];
                    let n = stream.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&bytes[..body_start]);
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                while bytes.len() < body_start + length {
                    let mut buf = [0; 4096];
                    let n = stream.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buf[..n]);
                }
                requests.push(serde_json::from_slice(&bytes[body_start..body_start + length]).unwrap());
                let body = response.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        let mut api = Api::new(reqwest::Client::new(), "test-token".into());
        api.endpoint = endpoint;
        (api, task)
    }

    fn course() -> Value {
        json!({"id":"activity", "__typename":"CoursePlayActivity", "kind":"COURSE_PLAY",
            "time":"2026-10-01T00:00:00Z", "scorecard":{"id":"card"}})
    }

    #[tokio::test]
    async fn course_round_resolves_scorecard_and_preserves_holes_and_null_measurements() {
        let scorecard = json!({"__typename":"Scorecard", "id":"card", "grossScore":8,
            "holes":[{"holeNumber":1,"shots":[{"id":"shot1", "club":"Driver", "measurement":null}]}],
            "playOffHoles":[{"holeNumber":1,"shots":[{"id":"shot2", "club":"Putter"}]}]});
        let (api, server) = mock_api(vec![
            json!({"data":{"node":course()}}),
            json!({"data":{"node":scorecard}}),
        ])
        .await;
        let session = api.session("activity").await.unwrap();
        assert_eq!(session["id"], "activity");
        assert_eq!(session["kind"], "COURSE_PLAY");
        assert_eq!(session["scorecard"], scorecard);
        assert_eq!(session["strokeCount"], 2);
        assert_eq!(session["strokes"][0]["holeNumber"], 1);
        assert_eq!(session["strokes"][0]["isPlayoff"], false);
        assert!(session["strokes"][0]["measurement"].is_null());
        assert_eq!(session["strokes"][1]["isPlayoff"], true);
        let requests = server.await.unwrap();
        assert_eq!(requests[0]["variables"]["id"], "activity");
        assert_eq!(requests[1]["variables"]["id"], "card");
        assert!(
            requests[1]["query"]
                .as_str()
                .unwrap()
                .contains("fragment RoundMeasurement on ShotMeasurement")
        );
    }

    #[tokio::test]
    async fn practice_session_stays_unchanged() {
        let practice = json!({"__typename":"SessionActivity", "id":"practice", "strokes":[]});
        let (api, server) = mock_api(vec![json!({"data":{"node":practice}})]).await;
        assert_eq!(api.session("practice").await.unwrap(), practice);
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn missing_scorecards_and_partial_errors_fail() {
        let mut activity = course();
        activity["scorecard"] = Value::Null;
        let (api, server) = mock_api(vec![json!({"data":{"node":activity}})]).await;
        assert!(matches!(api.session("activity").await, Err(Error::Invalid(_))));
        server.await.unwrap();

        let (api, server) = mock_api(vec![json!({"data":{"node":course()}}),
            json!({"data":{"node":{"id":"card"}}, "errors":[{"message":"shot resolver failed", "path":["node","holes",0,"shots"]}]})]).await;
        let error = api.session("activity").await.unwrap_err();
        assert!(matches!(error, Error::GraphQl(_)));
        assert!(error.to_string().contains("shot resolver failed"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn listing_course_metadata_keeps_pagination() {
        let mut first = course();
        first["course"] = json!({"displayName":"Old Macdonald"});
        first["isHidden"] = json!(false);
        let (api, server) = mock_api(vec![
            json!({"data":{"me":{"activities":{"items":[first],"pageInfo":{"hasNextPage":true}}}}}),
            json!({"data":{"me":{"activities":{"items":[],"pageInfo":{"hasNextPage":false}}}}}),
        ])
        .await;
        let activities = api.activities(&ActivityFilter::default()).await.unwrap();
        assert_eq!(activities[0].scorecard.as_ref().unwrap().id, "card");
        assert_eq!(
            activities[0].course.as_ref().unwrap().display_name.as_deref(),
            Some("Old Macdonald")
        );
        assert_eq!(activities[0].stroke_count, None);
        let requests = server.await.unwrap();
        assert_eq!(requests[0]["variables"]["skip"], 0);
        assert_eq!(requests[1]["variables"]["skip"], 1);
        assert!(requests[0]["variables"].get("kinds").is_none());
    }

    #[test]
    fn empty_round_has_no_fabricated_shots() {
        let mut activity = course();
        activity["scorecard"] = json!({"holes":[{"holeNumber":1,"shots":null}], "playOffHoles":null});
        let session = course_session(activity).unwrap();
        assert_eq!(session["strokeCount"], 0);
        assert_eq!(session["strokes"], json!([]));
    }
}
