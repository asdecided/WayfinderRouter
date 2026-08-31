//! Explicit, fixed-loopback local runtime discovery and first-inference proof.

use std::io::{Read, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::Url;
use reqwest::blocking::{Client, Response};
use reqwest::redirect::Policy;
use serde_json::{Value, json};

use crate::{EXIT_CONFIG, EXIT_OK, EXIT_USAGE, write_error, write_output};

const LOCAL_HELP: &str = concat!(
    "usage: wayfinder-router local discover --json\n",
    "       wayfinder-router local probe [--endpoint URL] --model ROUTE_ID --json"
);
const DEFAULT_ROUTER_ENDPOINT: &str = "http://127.0.0.1:8088";
const MAX_CATALOG_BYTES: usize = 1024 * 1024;
const MAX_DISCOVERED_CANDIDATES: usize = 256;
const MAX_PROBE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy)]
struct Catalog {
    runtime: &'static str,
    endpoint: &'static str,
    url: &'static str,
    shape: CatalogShape,
}

#[derive(Clone, Copy)]
enum CatalogShape {
    Ollama,
    OpenAi,
}

const CATALOGS: &[Catalog] = &[
    Catalog {
        runtime: "ollama",
        endpoint: "http://127.0.0.1:11434/v1",
        url: "http://127.0.0.1:11434/api/tags",
        shape: CatalogShape::Ollama,
    },
    Catalog {
        runtime: "lm-studio",
        endpoint: "http://127.0.0.1:1234/v1",
        url: "http://127.0.0.1:1234/v1/models",
        shape: CatalogShape::OpenAi,
    },
    Catalog {
        runtime: "llama.cpp",
        endpoint: "http://127.0.0.1:8080/v1",
        url: "http://127.0.0.1:8080/v1/models",
        shape: CatalogShape::OpenAi,
    },
    Catalog {
        runtime: "vllm",
        endpoint: "http://127.0.0.1:8000/v1",
        url: "http://127.0.0.1:8000/v1/models",
        shape: CatalogShape::OpenAi,
    },
];

pub(crate) fn run_local(
    arguments: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        write_output(stdout, LOCAL_HELP);
        return EXIT_OK;
    }
    match arguments.first().map(String::as_str) {
        Some("discover") => run_discover(&arguments[1..], stdout, stderr),
        Some("probe") => run_probe(&arguments[1..], stdout, stderr),
        Some(action) => usage_error(stderr, &format!("unsupported local action: {action}")),
        None => usage_error(stderr, "local needs discover or probe"),
    }
}

fn run_discover(arguments: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if arguments != ["--json"] {
        return usage_error(stderr, "local discover requires exactly --json");
    }
    let client = match Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_millis(250))
        .timeout(Duration::from_millis(900))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return config_error(stderr, &format!("cannot build discovery client: {error}"));
        }
    };
    let mut candidates = Vec::new();
    for catalog in CATALOGS {
        let Ok(response) = client.get(catalog.url).send() else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        let Ok(value) = bounded_json(response, MAX_CATALOG_BYTES) else {
            continue;
        };
        for model in parse_catalog(&value, catalog.shape) {
            candidates.push(json!({
                "runtime": catalog.runtime,
                "endpoint": catalog.endpoint,
                "route_id": "local",
                "model": model,
                "source": "fixed-loopback-catalog"
            }));
        }
    }
    candidates.sort_by(|left, right| {
        let left_key = (
            left["runtime"].as_str().unwrap_or(""),
            left["model"].as_str().unwrap_or(""),
        );
        let right_key = (
            right["runtime"].as_str().unwrap_or(""),
            right["model"].as_str().unwrap_or(""),
        );
        left_key.cmp(&right_key)
    });
    let candidates_truncated = cap_candidates(&mut candidates);
    let mut limitations = vec![
        "Only fixed loopback runtime catalogs were queried.",
        "No runtime or model was installed, pulled, or selected.",
    ];
    if candidates_truncated {
        limitations.push("The candidate list was capped at 256 models.");
    }
    let payload = json!({
        "schema_version": "wf-local-discovery-v1",
        "checked_at_ts": unix_timestamp(),
        "network_scope": "fixed-loopback-only",
        "checked_catalogs": CATALOGS.len(),
        "candidates": candidates,
        "limitations": limitations
    });
    write_json(stdout, stderr, &payload)
}

fn run_probe(arguments: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let options = match parse_probe_options(arguments) {
        Ok(options) => options,
        Err(message) => return usage_error(stderr, &message),
    };
    let client = match Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(45))
        .build()
    {
        Ok(client) => client,
        Err(error) => return config_error(stderr, &format!("cannot build probe client: {error}")),
    };
    let url = format!(
        "{}/v1/chat/completions",
        options.endpoint.trim_end_matches('/')
    );
    let response = match client
        .post(url)
        .header("content-type", "application/json")
        .json(&json!({
            "model": options.model,
            "messages": [{"role": "user", "content": "Reply with the single word OK."}],
            "max_tokens": 8,
            "temperature": 0,
            "stream": false
        }))
        .send()
    {
        Ok(response) => response,
        Err(error) => {
            return config_error(
                stderr,
                &format!("local inference probe could not reach the Router: {error}"),
            );
        }
    };
    let status = response.status();
    let served_by = response
        .headers()
        .get("x-wayfinder-router-served-by")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let request_id = response
        .headers()
        .get("x-wayfinder-router-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if !status.is_success() {
        return config_error(
            stderr,
            &format!(
                "local inference probe failed with Router HTTP {}",
                status.as_u16()
            ),
        );
    }
    let value = match bounded_json(response, MAX_PROBE_BYTES) {
        Ok(value) => value,
        Err(message) => return config_error(stderr, &format!("local inference probe: {message}")),
    };
    let has_text = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty());
    let Some(served_by) = served_by.filter(|value| !value.is_empty()) else {
        return config_error(
            stderr,
            "local inference probe response omitted the served route",
        );
    };
    if !has_text {
        return config_error(
            stderr,
            "local inference probe response contained no text result",
        );
    }
    let Some(request_id) = request_id.filter(|value| !value.is_empty()) else {
        return config_error(
            stderr,
            "local inference probe response omitted its receipt id",
        );
    };
    let execution_boundary =
        match probe_receipt(&client, &options.endpoint, &request_id, &served_by) {
            Ok(boundary) => boundary,
            Err(message) => {
                return config_error(stderr, &format!("local inference probe: {message}"));
            }
        };
    let payload = json!({
        "schema_version": "wf-local-probe-v1",
        "status": "passed",
        "checked_at_ts": unix_timestamp(),
        "endpoint": options.endpoint,
        "route_id": options.model,
        "served_by": served_by,
        "execution_boundary": execution_boundary,
        "request_count": 1,
        "prompt_content": "fixed-public-probe-only",
        "automatic_changed": false
    });
    write_json(stdout, stderr, &payload)
}

fn probe_receipt(
    client: &Client,
    endpoint: &str,
    request_id: &str,
    response_route: &str,
) -> Result<String, String> {
    let response = client
        .get(format!(
            "{}/router/recent?limit=20",
            endpoint.trim_end_matches('/')
        ))
        .send()
        .map_err(|error| format!("cannot read bounded delivery receipt: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "bounded delivery receipt returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let report = bounded_json(response, MAX_PROBE_BYTES)?;
    let receipt = report
        .get("recent")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry.get("request_id").and_then(Value::as_str) == Some(request_id))
        })
        .ok_or_else(|| "bounded delivery receipt is unavailable".to_owned())?;
    if !matches!(
        receipt.get("outcome").and_then(Value::as_str),
        Some("succeeded" | "cache-hit")
    ) {
        return Err("bounded delivery receipt is not a successful terminal result".to_owned());
    }
    if receipt.get("served_by").and_then(Value::as_str) != Some(response_route) {
        return Err("bounded delivery receipt did not match the served route".to_owned());
    }
    let boundary = receipt
        .get("execution_boundary")
        .and_then(Value::as_str)
        .ok_or_else(|| "bounded delivery receipt omitted its execution boundary".to_owned())?;
    if !matches!(boundary, "on-device" | "local-network") {
        return Err("bounded delivery receipt did not prove local execution".to_owned());
    }
    Ok(boundary.to_owned())
}

#[derive(Debug, PartialEq, Eq)]
struct ProbeOptions {
    endpoint: String,
    model: String,
}

fn parse_probe_options(arguments: &[String]) -> Result<ProbeOptions, String> {
    let mut endpoint = DEFAULT_ROUTER_ENDPOINT.to_owned();
    let mut model = None;
    let mut json_output = false;
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "--json" if !json_output => json_output = true,
            "--json" => return Err("--json may only be supplied once".to_owned()),
            "--endpoint" => {
                let value = arguments
                    .get(index + 1)
                    .ok_or_else(|| "--endpoint needs a value".to_owned())?;
                endpoint.clone_from(value);
                index += 1;
            }
            "--model" if model.is_none() => {
                model = Some(
                    arguments
                        .get(index + 1)
                        .ok_or_else(|| "--model needs a value".to_owned())?
                        .clone(),
                );
                index += 1;
            }
            "--model" => return Err("--model may only be supplied once".to_owned()),
            value => return Err(format!("unrecognized local probe argument: {value}")),
        }
        index += 1;
    }
    if !json_output {
        return Err("local probe requires --json".to_owned());
    }
    validate_loopback_endpoint(&endpoint)?;
    let endpoint_url = Url::parse(&endpoint).map_err(|_| "endpoint must be a valid URL")?;
    if endpoint_url.path() != "/" {
        return Err("local probe endpoint must be a loopback Router origin".to_owned());
    }
    let model = model.ok_or_else(|| "local probe needs --model".to_owned())?;
    validate_identifier(&model, "route id")?;
    Ok(ProbeOptions { endpoint, model })
}

pub(crate) fn validate_loopback_endpoint(endpoint: &str) -> Result<(), String> {
    let parsed = Url::parse(endpoint).map_err(|_| "endpoint must be a valid URL".to_owned())?;
    if parsed.scheme() != "http"
        || !matches!(
            parsed.host_str(),
            Some("127.0.0.1" | "localhost" | "::1" | "[::1]")
        )
        || parsed.port().is_none_or(|port| port == 0)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("endpoint must be an explicit loopback HTTP URL with a port".to_owned());
    }
    Ok(())
}

pub(crate) fn validate_identifier(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || value.chars().any(char::is_control)
        || value.contains('\"')
        || value.contains('\\')
    {
        return Err(format!(
            "{label} must be 1-256 characters without controls, quotes, or backslashes"
        ));
    }
    Ok(())
}

fn bounded_json(mut response: Response, maximum: usize) -> Result<Value, String> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err("response exceeded the evidence bound".to_owned());
    }
    let mut bytes = Vec::with_capacity(maximum.min(16 * 1024));
    response
        .by_ref()
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read response: {error}"))?;
    if bytes.len() > maximum {
        return Err("response exceeded the evidence bound".to_owned());
    }
    serde_json::from_slice(&bytes).map_err(|_| "response was not valid JSON".to_owned())
}

fn parse_catalog(value: &Value, shape: CatalogShape) -> Vec<String> {
    let entries = match shape {
        CatalogShape::Ollama => value.get("models"),
        CatalogShape::OpenAi => value.get("data"),
    }
    .and_then(Value::as_array);
    let Some(entries) = entries else {
        return Vec::new();
    };
    let key = match shape {
        CatalogShape::Ollama => "name",
        CatalogShape::OpenAi => "id",
    };
    let mut models = entries
        .iter()
        .filter_map(|entry| entry.get(key).and_then(Value::as_str))
        .filter(|model| validate_identifier(model, "model id").is_ok())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    models
}

fn cap_candidates(candidates: &mut Vec<Value>) -> bool {
    let truncated = candidates.len() > MAX_DISCOVERED_CANDIDATES;
    candidates.truncate(MAX_DISCOVERED_CANDIDATES);
    truncated
}

fn unix_timestamp() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}

fn write_json(stdout: &mut dyn Write, stderr: &mut dyn Write, payload: &Value) -> i32 {
    if serde_json::to_writer_pretty(&mut *stdout, payload).is_err() || writeln!(stdout).is_err() {
        return config_error(stderr, "cannot write local evidence output");
    }
    EXIT_OK
}

fn usage_error(stderr: &mut dyn Write, message: &str) -> i32 {
    write_error(stderr, &format!("wayfinder-router: {message}"));
    EXIT_USAGE
}

fn config_error(stderr: &mut dyn Write, message: &str) -> i32 {
    write_error(stderr, &format!("wayfinder-router: {message}"));
    EXIT_CONFIG
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn catalog_parsers_emit_only_bounded_model_ids() {
        assert_eq!(
            parse_catalog(
                &json!({"models": [{"name": "qwen2.5-coder:7b"}, {"name": "bad\nmodel"}]}),
                CatalogShape::Ollama,
            ),
            ["qwen2.5-coder:7b"]
        );
        assert_eq!(
            parse_catalog(
                &json!({"data": [{"id": "local/model"}, {"id": "local/model"}]}),
                CatalogShape::OpenAi,
            ),
            ["local/model"]
        );
    }

    #[test]
    fn discovery_candidate_count_has_a_fixed_upper_bound() {
        let mut candidates = (0..(MAX_DISCOVERED_CANDIDATES + 1))
            .map(|index| json!({"model": format!("model-{index}")}))
            .collect::<Vec<_>>();
        assert!(cap_candidates(&mut candidates));
        assert_eq!(candidates.len(), 256);
    }

    #[test]
    fn endpoint_validation_is_literal_loopback_only() {
        for endpoint in [
            "http://127.0.0.1:8088",
            "http://localhost:11434/v1",
            "http://[::1]:1234/v1",
        ] {
            assert!(validate_loopback_endpoint(endpoint).is_ok(), "{endpoint}");
        }
        for endpoint in [
            "https://127.0.0.1:8088",
            "http://0.0.0.0:8088",
            "http://localhost:8088@evil.example",
            "http://localhost",
            "http://localhost:0/v1",
            "http://localhost:8088/?token=secret",
        ] {
            assert!(validate_loopback_endpoint(endpoint).is_err(), "{endpoint}");
        }
    }

    #[test]
    fn probe_parser_requires_explicit_model_and_json() {
        assert_eq!(
            parse_probe_options(&[
                "--model".to_owned(),
                "local".to_owned(),
                "--json".to_owned(),
            ]),
            Ok(ProbeOptions {
                endpoint: DEFAULT_ROUTER_ENDPOINT.to_owned(),
                model: "local".to_owned(),
            })
        );
        assert!(parse_probe_options(&["--json".to_owned()]).is_err());
        assert!(parse_probe_options(&["--model".to_owned(), "local".to_owned()]).is_err());
    }

    #[test]
    fn probe_requires_a_successful_local_receipt_and_emits_no_response_text()
    -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let server = thread::spawn(move || -> std::io::Result<()> {
            for index in 0..2 {
                let (mut stream, _) = listener.accept()?;
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request)?;
                let body = if index == 0 {
                    r#"{"choices":[{"message":{"content":"OK"}}]}"#
                } else {
                    r#"{"recent":[{"request_id":"probe-1","outcome":"succeeded","served_by":"local","execution_boundary":"on-device"}]}"#
                };
                let extra_headers = if index == 0 {
                    "x-wayfinder-router-request-id: probe-1\r\nx-wayfinder-router-served-by: local\r\n"
                } else {
                    ""
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n{extra_headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )?;
            }
            Ok(())
        });
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_probe(
            &[
                "--endpoint".to_owned(),
                format!("http://{address}"),
                "--model".to_owned(),
                "local".to_owned(),
                "--json".to_owned(),
            ],
            &mut stdout,
            &mut stderr,
        );
        server.join().map_err(|_| "probe server panicked")??;
        assert_eq!(code, EXIT_OK, "{}", String::from_utf8_lossy(&stderr));
        let payload: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(payload["schema_version"], "wf-local-probe-v1");
        assert_eq!(payload["status"], "passed");
        assert_eq!(payload["served_by"], "local");
        assert_eq!(payload["execution_boundary"], "on-device");
        assert_eq!(payload["prompt_content"], "fixed-public-probe-only");
        assert!(!String::from_utf8_lossy(&stdout).contains("\"OK\""));
        Ok(())
    }
}
