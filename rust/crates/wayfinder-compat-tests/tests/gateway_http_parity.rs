use std::collections::BTreeMap;
use std::error::Error;

use axum::body::{Body, to_bytes};
use http::{Request, StatusCode};
use serde::Deserialize;
use serde_json::{Map, Value};
use tower::ServiceExt;
use wayfinder_gateway::{AppState, ConfiguredModel, build_router};
use wayfinder_routing_core::RoutingConfig;

const GATEWAY_HTTP_VECTORS: &str = include_str!("../fixtures/gateway-http.json");
#[derive(Debug, Deserialize)]
struct Corpus {
    schema: u64,
    version: String,
    known_rust_mismatches: Vec<Value>,
    header_contract: Vec<String>,
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    state: StateName,
    request: RequestFixture,
    response: ResponseFixture,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StateName {
    ConfiguredDryRun,
    DecisionOnly,
}

#[derive(Debug, Deserialize)]
struct RequestFixture {
    method: String,
    path: String,
    json: Option<Value>,
    raw_body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponseFixture {
    status: u16,
    headers: BTreeMap<String, String>,
    body: BodyFixture,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum BodyFixture {
    Json(Value),
    Text(String),
}

#[derive(Default)]
struct Normalizer {
    request_ids: BTreeMap<String, String>,
}

impl Normalizer {
    fn request_id(&mut self, value: &str) -> String {
        if let Some(marker) = self.request_ids.get(value) {
            return marker.clone();
        }
        let marker = format!("<request_id:{}>", self.request_ids.len() + 1);
        self.request_ids.insert(value.to_owned(), marker.clone());
        marker
    }

    fn json_value(&mut self, value: Value, key: Option<&str>) -> Value {
        match (key, value) {
            (Some("request_id"), Value::String(value)) => Value::String(self.request_id(&value)),
            (Some("ts"), Value::Number(_)) => Value::String("<timestamp>".to_owned()),
            (_, Value::Array(values)) => Value::Array(
                values
                    .into_iter()
                    .map(|value| self.json_value(value, None))
                    .collect(),
            ),
            (_, Value::Object(values)) => Value::Object(
                values
                    .into_iter()
                    .map(|(name, value)| {
                        let normalized = self.json_value(value, Some(&name));
                        (name, normalized)
                    })
                    .collect::<Map<_, _>>(),
            ),
            (_, value) => value,
        }
    }
}

fn configured_state() -> AppState {
    AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "provider-local",
                None,
                false,
            ),
            ConfiguredModel::new(
                "cloud",
                "https://cloud.example/v1",
                "provider-cloud",
                Some("WAYFINDER_HTTP_CORPUS_MISSING_KEY".to_owned()),
                false,
            ),
        ],
        true,
        "2026.7.0",
    )
    .with_dry_run(true)
}

fn decision_only_state() -> AppState {
    AppState::new(RoutingConfig::binary(0.5), Vec::new(), false, "2026.7.0")
}

fn latency_metrics(text: &str) -> String {
    let prefix = "wayfinder_router_decision_latency_seconds";
    let mut normalized = String::new();
    for line in text.lines() {
        let line =
            if line.starts_with(&format!("{prefix}_bucket{{")) && !line.contains("le=\"+Inf\"") {
                line.rsplit_once(' ').map_or_else(
                    || line.to_owned(),
                    |(labels, _)| format!("{labels} <latency-bucket>"),
                )
            } else if line.starts_with(&format!("{prefix}_sum ")) {
                format!("{prefix}_sum <latency-sum>")
            } else {
                line.to_owned()
            };
        normalized.push_str(&line);
        normalized.push('\n');
    }
    if !text.ends_with('\n') {
        let _ = normalized.pop();
    }
    normalized
}

// The fixture is immutable migration evidence. Metrics added after the Rust cutover
// have dedicated Rust tests and are intentionally outside that frozen snapshot.
fn migration_metric_families(text: &str) -> String {
    const RUST_ONLY_PREFIXES: [&str; 5] = [
        "wayfinder_router_in_flight_deliveries",
        "wayfinder_router_peak_in_flight_deliveries",
        "wayfinder_router_admission_queue_wait_seconds",
        "wayfinder_router_admission_rejected_total",
        "wayfinder_state_degraded",
    ];
    let mut normalized = String::new();
    for line in text.lines() {
        let metric = line
            .strip_prefix("# HELP ")
            .or_else(|| line.strip_prefix("# TYPE "))
            .unwrap_or(line)
            .split(['{', ' '])
            .next()
            .unwrap_or_default();
        if RUST_ONLY_PREFIXES
            .iter()
            .any(|prefix| metric.starts_with(prefix))
        {
            continue;
        }
        normalized.push_str(line);
        normalized.push('\n');
    }
    if !text.ends_with('\n') {
        let _ = normalized.pop();
    }
    normalized
}

#[tokio::test]
async fn gateway_http_preserves_the_frozen_migration_contract() -> Result<(), Box<dyn Error>> {
    let corpus: Corpus = serde_json::from_str(GATEWAY_HTTP_VECTORS)?;
    assert_eq!(corpus.schema, 1);
    assert_eq!(corpus.version, "2026.7.0");
    assert_eq!(corpus.cases.len(), 20);
    assert!(corpus.known_rust_mismatches.is_empty());

    let configured = configured_state();
    let decision_only = decision_only_state();
    let mut normalizer = Normalizer::default();
    let mut mismatches = Vec::new();

    for case in corpus.cases {
        let state = match case.state {
            StateName::ConfiguredDryRun => configured.clone(),
            StateName::DecisionOnly => decision_only.clone(),
        };
        let mut builder = Request::builder()
            .method(case.request.method.as_str())
            .uri(&case.request.path);
        let body = if let Some(json) = &case.request.json {
            builder = builder.header("content-type", "application/json");
            Body::from(serde_json::to_vec(json)?)
        } else if let Some(raw_body) = &case.request.raw_body {
            builder = builder.header("content-type", "application/json");
            Body::from(raw_body.clone())
        } else {
            Body::empty()
        };
        let response = build_router(state).oneshot(builder.body(body)?).await?;
        let status = response.status();
        let actual_headers = corpus
            .header_contract
            .iter()
            .filter_map(|name| {
                response
                    .headers()
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .map(|value| {
                        let value = if name == "x-wayfinder-router-request-id" {
                            normalizer.request_id(value)
                        } else {
                            value.to_owned()
                        };
                        (name.clone(), value)
                    })
            })
            .collect::<BTreeMap<_, _>>();
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        let actual_body = match &case.response.body {
            BodyFixture::Json(_) => match serde_json::from_slice(&bytes) {
                Ok(value) => BodyFixture::Json(normalizer.json_value(value, None)),
                Err(error) => {
                    mismatches.push(format!(
                        "{} JSON decode: {error}; raw Rust body {:?}",
                        case.name,
                        String::from_utf8_lossy(&bytes)
                    ));
                    BodyFixture::Json(Value::Null)
                }
            },
            BodyFixture::Text(_) => {
                let mut value = String::from_utf8(bytes.to_vec())?;
                if case.request.path == "/metrics" {
                    value = migration_metric_families(&value);
                }
                if case.name == "metrics_after_one_dry_run" {
                    value = latency_metrics(&value);
                }
                BodyFixture::Text(value)
            }
        };

        if status != StatusCode::from_u16(case.response.status)? {
            mismatches.push(format!(
                "{} status: fixture {}, actual {}",
                case.name, case.response.status, status
            ));
        }
        if actual_headers != case.response.headers {
            mismatches.push(format!(
                "{} headers:\n  fixture: {:?}\n  actual:  {:?}",
                case.name, case.response.headers, actual_headers
            ));
        }
        match (&case.response.body, actual_body) {
            (BodyFixture::Json(expected), BodyFixture::Json(actual)) if expected != &actual => {
                mismatches.push(format!(
                    "{} JSON body:\n  fixture: {}\n  actual:  {}",
                    case.name, expected, actual
                ));
            }
            (BodyFixture::Text(expected), BodyFixture::Text(actual)) if expected != &actual => {
                mismatches.push(format!(
                    "{} text body:\n  fixture: {:?}\n  actual:  {:?}",
                    case.name, expected, actual
                ));
            }
            (BodyFixture::Json(_), BodyFixture::Text(_))
            | (BodyFixture::Text(_), BodyFixture::Json(_)) => {
                mismatches.push(format!("{} body kind differs", case.name));
            }
            _ => {}
        }
    }

    assert!(
        mismatches.is_empty(),
        "gateway migration-contract mismatches:\n{}",
        mismatches.join("\n")
    );
    Ok(())
}
