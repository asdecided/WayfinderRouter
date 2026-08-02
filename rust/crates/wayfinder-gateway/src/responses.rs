//! Bounded OpenAI Responses API compatibility.
//!
//! The gateway keeps the routing and provider execution contract shared with
//! Chat Completions, but exposes a strict Responses-shaped boundary. Only the
//! fields translated below are accepted; silently dropping a new Responses
//! field would make a request appear to succeed with different semantics.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wayfinder_providers::openai_compat::DEFAULT_MAX_RESPONSE_BYTES;
use wayfinder_providers::sse::{SseDecoder, SseEvent};

use crate::{AppState, chat_completions, new_request_id};

/// Stable contract version for the compatibility surface.
pub const RESPONSES_CONTRACT_VERSION: &str = "wf-responses-v1";
/// Maximum number of input items retained in one request.
pub const MAX_INPUT_ITEMS: usize = 64;
/// Maximum UTF-8 characters across instructions and input history.
pub const MAX_INPUT_CHARS: usize = 1_000_000;
/// Maximum generated output characters relayed through one stream.
pub const MAX_OUTPUT_CHARS: usize = 1_000_000;
/// Maximum output-token bound accepted from a client.
pub const MAX_OUTPUT_TOKENS: u64 = 32_768;
/// Maximum upstream SSE events consumed for one Responses stream.
pub const MAX_STREAM_EVENTS: usize = 8_192;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponsesRequest {
    model: String,
    input: ResponsesInput,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    max_output_tokens: Option<u64>,
    #[serde(default)]
    temperature: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ResponsesInput {
    Text(String),
    Items(Vec<ResponsesInputItem>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponsesInputItem {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    role: Option<String>,
    content: ResponsesContent,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ResponsesContent {
    Text(String),
    Parts(Vec<ResponsesContentPart>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponsesContentPart {
    #[serde(rename = "type")]
    kind: String,
    text: String,
}

#[derive(Debug, Serialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
}

impl ResponsesRequest {
    fn validate_and_translate(self) -> Result<Value, String> {
        if self.model.trim().is_empty() || self.model.len() > 128 {
            return Err("Responses field 'model' must be 1-128 bytes".to_owned());
        }
        if let Some(instructions) = self.instructions.as_ref() {
            if instructions.chars().count() > MAX_INPUT_CHARS {
                return Err(format!(
                    "Responses field 'instructions' exceeds {MAX_INPUT_CHARS} characters"
                ));
            }
        }
        if let Some(max_output_tokens) = self.max_output_tokens {
            if max_output_tokens == 0 || max_output_tokens > MAX_OUTPUT_TOKENS {
                return Err(format!(
                    "Responses field 'max_output_tokens' must be between 1 and {MAX_OUTPUT_TOKENS}"
                ));
            }
        }
        if let Some(temperature) = self.temperature {
            if !temperature.is_finite() || !(0.0..=2.0).contains(&temperature) {
                return Err("Responses field 'temperature' must be between 0 and 2".to_owned());
            }
        }

        let mut messages = Vec::new();
        let mut input_chars = 0usize;
        if let Some(instructions) = self.instructions {
            input_chars = input_chars.saturating_add(instructions.chars().count());
            messages.push(json!({"role": "system", "content": instructions}));
        }
        match self.input {
            ResponsesInput::Text(text) => {
                input_chars = input_chars.saturating_add(text.chars().count());
                messages.push(json!({"role": "user", "content": text}));
            }
            ResponsesInput::Items(items) => {
                if items.is_empty() {
                    return Err("Responses field 'input' must contain at least one item".to_owned());
                }
                if items.len() > MAX_INPUT_ITEMS {
                    return Err(format!(
                        "Responses field 'input' exceeds {MAX_INPUT_ITEMS} items"
                    ));
                }
                for item in items {
                    let kind = item.kind.as_deref().unwrap_or("message");
                    if kind != "message" {
                        return Err(format!(
                            "unsupported Responses input item type '{kind}'; only 'message' is supported"
                        ));
                    }
                    let role = item.role.as_deref().ok_or_else(|| {
                        "Responses message input requires a 'role' field".to_owned()
                    })?;
                    if !matches!(role, "system" | "developer" | "user" | "assistant") {
                        return Err(format!("unsupported Responses message role '{role}'"));
                    }
                    let content = content_text(item.content)?;
                    input_chars = input_chars.saturating_add(content.chars().count());
                    messages.push(json!({"role": role, "content": content}));
                }
            }
        }
        if input_chars > MAX_INPUT_CHARS {
            return Err(format!(
                "Responses input history exceeds {MAX_INPUT_CHARS} characters"
            ));
        }
        let mut body = serde_json::Map::new();
        body.insert("model".to_owned(), Value::String(self.model));
        body.insert("messages".to_owned(), Value::Array(messages));
        body.insert("stream".to_owned(), Value::Bool(self.stream));
        if let Some(max_output_tokens) = self.max_output_tokens {
            body.insert("max_tokens".to_owned(), json!(max_output_tokens));
        }
        if let Some(temperature) = self.temperature {
            body.insert("temperature".to_owned(), json!(temperature));
        }
        Ok(Value::Object(body))
    }
}

fn content_text(content: ResponsesContent) -> Result<String, String> {
    match content {
        ResponsesContent::Text(text) => Ok(text),
        ResponsesContent::Parts(parts) => {
            if parts.is_empty() {
                return Err("Responses message content must contain at least one part".to_owned());
            }
            let mut text = String::new();
            for part in parts {
                if !matches!(part.kind.as_str(), "input_text" | "output_text" | "text") {
                    return Err(format!(
                        "unsupported Responses content type '{}'; only text parts are supported",
                        part.kind
                    ));
                }
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&part.text);
            }
            Ok(text)
        }
    }
}

/// Handle a bounded Responses API request by normalizing to the existing
/// authenticated Chat Completions execution path.
pub(crate) async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(rejection) => return crate::body_rejection_response(&state, rejection),
    };
    let request = match serde_json::from_slice::<ResponsesRequest>(&body) {
        Ok(request) => request,
        Err(error) => {
            let message = if error.to_string().contains("unknown field") {
                format!("unsupported Responses API field: {error}")
            } else {
                format!("invalid Responses API request: {error}")
            };
            return unsupported_request(message);
        }
    };
    let stream_requested = request.stream;
    let model = request.model.clone();
    let openai_body = match request.validate_and_translate() {
        Ok(body) => body,
        Err(message) => return unsupported_request(message),
    };
    let encoded = match serde_json::to_vec(&openai_body) {
        Ok(encoded) => encoded,
        Err(error) => return unsupported_request(error.to_string()),
    };
    let inner = chat_completions(State(state), headers, Ok(Bytes::from(encoded))).await;
    let status = inner.status();
    let upstream_stream = inner
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));
    if !status.is_success() || !stream_requested || !upstream_stream {
        return responses_buffered(inner, &model).await;
    }
    responses_streaming(inner, model).await
}

fn unsupported_request(message: String) -> Response {
    crate::error_response(
        StatusCode::BAD_REQUEST,
        "wayfinder_router_unsupported_request",
        message,
        HeaderMap::new(),
    )
}

async fn responses_buffered(inner: Response, requested_model: &str) -> Response {
    let status = inner.status();
    let headers = route_headers(inner.headers());
    let body = match to_bytes(inner.into_body(), DEFAULT_MAX_RESPONSE_BYTES).await {
        Ok(body) => body,
        Err(error) => {
            return crate::error_response(
                StatusCode::BAD_GATEWAY,
                "wayfinder_router_upstream_error",
                format!("Responses upstream body exceeded the response bound: {error}"),
                headers,
            );
        }
    };
    if !status.is_success() {
        return (status, headers, body).into_response();
    }
    let value = match serde_json::from_slice::<Value>(&body) {
        Ok(value) => value,
        Err(error) => {
            return crate::error_response(
                StatusCode::BAD_GATEWAY,
                "wayfinder_router_upstream_error",
                format!("Responses upstream returned invalid JSON: {error}"),
                headers,
            );
        }
    };
    if value.get("wayfinder").is_some() && value.get("choices").is_none() {
        let mut output_headers = headers;
        output_headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        return (status, output_headers, body).into_response();
    }
    let response_id = response_id(&headers);
    let model = headers
        .get("x-wayfinder-router-served-by")
        .and_then(|value| value.to_str().ok())
        .unwrap_or(requested_model);
    let text = extract_chat_text(&value);
    if text.chars().count() > MAX_OUTPUT_CHARS {
        return crate::error_response(
            StatusCode::BAD_GATEWAY,
            "wayfinder_router_upstream_error",
            format!("Responses output exceeds {MAX_OUTPUT_CHARS} characters"),
            headers,
        );
    }
    let usage = extract_usage(&value);
    let response = response_value(&response_id, model, &text, usage.as_ref(), "completed");
    let mut output_headers = headers;
    output_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    (status, output_headers, Json(response)).into_response()
}

async fn responses_streaming(inner: Response, requested_model: String) -> Response {
    let headers = route_headers(inner.headers());
    let response_id = response_id(&headers);
    let stream = inner.into_body().into_data_stream();
    let mut relay = ResponsesStreamRelay::new(stream, response_id, requested_model);
    relay.pending.push_back(relay.created_frame());
    let body_stream = stream::unfold(Some(relay), |relay| async move {
        let mut relay = relay?;
        if let Some(frame) = relay.pending.pop_front() {
            return Some((Ok::<Bytes, Infallible>(frame), Some(relay)));
        }
        if relay.terminal {
            return None;
        }
        loop {
            match relay.upstream.next().await {
                Some(Ok(chunk)) => match relay.decoder.push(&chunk) {
                    Ok(events) => {
                        relay.process_events(events);
                        if let Some(frame) = relay.pending.pop_front() {
                            return Some((Ok::<Bytes, Infallible>(frame), Some(relay)));
                        }
                    }
                    Err(error) => {
                        relay.fail(format!("Responses SSE decode failed: {error}"));
                        return Some((Ok::<Bytes, Infallible>(relay.take_pending()), Some(relay)));
                    }
                },
                Some(Err(error)) => {
                    relay.fail(format!("Responses upstream stream failed: {error}"));
                    return Some((Ok::<Bytes, Infallible>(relay.take_pending()), Some(relay)));
                }
                None => {
                    match relay.decoder.finish() {
                        Ok(events) => relay.process_events(events),
                        Err(error) => relay.fail(format!("Responses SSE decode failed: {error}")),
                    }
                    if !relay.terminal {
                        relay.complete();
                    }
                    return relay
                        .pending
                        .pop_front()
                        .map(|frame| (Ok::<Bytes, Infallible>(frame), Some(relay)));
                }
            }
        }
    });
    let mut output_headers = headers;
    output_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    (
        StatusCode::OK,
        output_headers,
        Body::from_stream(body_stream),
    )
        .into_response()
}

struct ResponsesStreamRelay<S> {
    upstream: S,
    decoder: SseDecoder,
    pending: VecDeque<Bytes>,
    response_id: String,
    model: String,
    output: String,
    usage: Option<Usage>,
    terminal: bool,
    event_count: usize,
}

impl<S> ResponsesStreamRelay<S>
where
    S: Stream<Item = Result<Bytes, axum::Error>> + Unpin,
{
    fn new(upstream: S, response_id: String, model: String) -> Self {
        Self {
            upstream,
            decoder: SseDecoder::default(),
            pending: VecDeque::new(),
            response_id,
            model,
            output: String::new(),
            usage: None,
            terminal: false,
            event_count: 0,
        }
    }

    fn created_frame(&self) -> Bytes {
        frame(
            "response.created",
            json!({
                "type": "response.created",
                "response": response_value(&self.response_id, &self.model, "", None, "in_progress")
            }),
        )
    }

    fn process_events(&mut self, events: Vec<SseEvent>) {
        for event in events {
            if self.terminal {
                break;
            }
            self.event_count = self.event_count.saturating_add(1);
            if self.event_count > MAX_STREAM_EVENTS {
                self.fail(format!(
                    "Responses stream exceeds {MAX_STREAM_EVENTS} upstream events"
                ));
                break;
            }
            if event.data == "[DONE]" {
                self.complete();
                break;
            }
            let value = match serde_json::from_str::<Value>(&event.data) {
                Ok(value) => value,
                Err(error) => {
                    self.fail(format!(
                        "Responses upstream returned invalid SSE JSON: {error}"
                    ));
                    break;
                }
            };
            if let Some(error) = value.get("error") {
                self.fail(
                    error["message"]
                        .as_str()
                        .unwrap_or("upstream error")
                        .to_owned(),
                );
                break;
            }
            if let Some(usage) = extract_usage(&value) {
                self.usage = Some(usage);
            }
            let Some(choices) = value.get("choices").and_then(Value::as_array) else {
                continue;
            };
            for choice in choices {
                let Some(delta) = choice.get("delta") else {
                    continue;
                };
                let Some(text) = delta.get("content").and_then(Value::as_str) else {
                    continue;
                };
                if text.is_empty() {
                    continue;
                }
                if self
                    .output
                    .chars()
                    .count()
                    .saturating_add(text.chars().count())
                    > MAX_OUTPUT_CHARS
                {
                    self.fail(format!(
                        "Responses stream exceeds {MAX_OUTPUT_CHARS} output characters"
                    ));
                    return;
                }
                self.output.push_str(text);
                self.pending.push_back(frame(
                    "response.output_text.delta",
                    json!({
                        "type": "response.output_text.delta",
                        "response_id": self.response_id,
                        "delta": text
                    }),
                ));
            }
        }
    }

    fn complete(&mut self) {
        if self.terminal {
            return;
        }
        self.terminal = true;
        self.pending.push_back(frame(
            "response.completed",
            json!({
                "type": "response.completed",
                "response": response_value(
                    &self.response_id,
                    &self.model,
                    &self.output,
                    self.usage.as_ref(),
                    "completed"
                )
            }),
        ));
    }

    fn take_pending(&mut self) -> Bytes {
        self.pending.pop_front().unwrap_or_else(|| {
            frame(
                "response.failed",
                json!({
                    "type": "response.failed",
                    "error": {
                        "type": "wayfinder_router_upstream_error",
                        "message": "Responses stream failed before a terminal event was available"
                    }
                }),
            )
        })
    }

    fn fail(&mut self, message: String) {
        if self.terminal {
            return;
        }
        self.terminal = true;
        self.pending.push_back(frame(
            "response.failed",
            json!({
                "type": "response.failed",
                "response": response_value(
                    &self.response_id,
                    &self.model,
                    &self.output,
                    self.usage.as_ref(),
                    "failed"
                ),
                "error": {"type": "wayfinder_router_upstream_error", "message": message}
            }),
        ));
    }
}

fn frame(event: &str, value: Value) -> Bytes {
    let payload = serde_json::to_string(&value).unwrap_or_else(|_| {
        "{\"type\":\"response.failed\",\"error\":{\"message\":\"serialization failure\"}}"
            .to_owned()
    });
    Bytes::from(format!("event: {event}\ndata: {payload}\n\n"))
}

fn response_value(
    response_id: &str,
    model: &str,
    text: &str,
    usage: Option<&Usage>,
    status: &str,
) -> Value {
    let mut response = json!({
        "id": format!("resp_{response_id}"),
        "object": "response",
        "created_at": SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs()),
        "status": status,
        "model": model,
        "output": [{
            "id": format!("msg_{response_id}"),
            "type": "message",
            "status": status,
            "role": "assistant",
            "content": [{"type": "output_text", "text": text, "annotations": []}]
        }]
    });
    if let Some(usage) = usage {
        response["usage"] = serde_json::to_value(usage).unwrap_or(Value::Null);
    }
    response
}

fn response_id(headers: &HeaderMap) -> String {
    headers
        .get("x-wayfinder-router-request-id")
        .and_then(|value| value.to_str().ok())
        .map_or_else(new_request_id, ToOwned::to_owned)
}

fn route_headers(headers: &HeaderMap) -> HeaderMap {
    headers
        .iter()
        .filter(|(name, _)| name.as_str().starts_with("x-wayfinder"))
        .fold(HeaderMap::new(), |mut output, (name, value)| {
            output.insert(name.clone(), value.clone());
            output
        })
}

fn extract_chat_text(value: &Value) -> String {
    let Some(choices) = value.get("choices").and_then(Value::as_array) else {
        return String::new();
    };
    let Some(message) = choices.first().and_then(|choice| choice.get("message")) else {
        return String::new();
    };
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn extract_usage(value: &Value) -> Option<Usage> {
    let usage = value.get("usage")?;
    let input_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64)?;
    let output_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64)?;
    Some(Usage {
        input_tokens,
        output_tokens,
        total_tokens: input_tokens.saturating_add(output_tokens),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_text_and_multiturn_input_without_dropping_context() -> Result<(), String> {
        let request = serde_json::from_value::<ResponsesRequest>(json!({
            "model": "auto",
            "instructions": "be concise",
            "input": [
                {"type": "message", "role": "user", "content": "hello"},
                {"type": "message", "role": "assistant", "content": [
                    {"type": "output_text", "text": "hi"}
                ]},
                {"type": "message", "role": "user", "content": "follow up"}
            ]
        }))
        .map_err(|error| error.to_string())?;
        let translated = request.validate_and_translate()?;
        assert_eq!(
            translated["messages"],
            json!([
                {"role":"system","content":"be concise"},
                {"role":"user","content":"hello"},
                {"role":"assistant","content":"hi"},
                {"role":"user","content":"follow up"}
            ])
        );
        Ok(())
    }

    #[test]
    fn rejects_unknown_fields_instead_of_silently_ignoring_them() -> Result<(), String> {
        let error = match serde_json::from_value::<ResponsesRequest>(json!({
            "model": "auto",
            "input": "hello",
            "tools": []
        })) {
            Ok(_) => return Err("tools are not supported".to_owned()),
            Err(error) => error,
        };
        assert!(error.to_string().contains("unknown field"));
        Ok(())
    }

    #[test]
    fn usage_is_normalized_to_responses_names() -> Result<(), String> {
        let usage = match extract_usage(&json!({
            "usage": {"prompt_tokens": 4, "completion_tokens": 3}
        })) {
            Some(usage) => usage,
            None => return Err("usage".to_owned()),
        };
        assert_eq!(usage.input_tokens, 4);
        assert_eq!(usage.output_tokens, 3);
        assert_eq!(usage.total_tokens, 7);
        Ok(())
    }
}
