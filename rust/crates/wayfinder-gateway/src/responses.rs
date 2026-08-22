//! Bounded OpenAI Responses API compatibility.
//!
//! The gateway keeps the routing and provider execution contract shared with
//! Chat Completions, but exposes a strict Responses-shaped boundary. Only the
//! fields translated below are accepted; silently dropping a new Responses
//! field would make a request appear to succeed with different semantics.

use std::collections::{BTreeMap, HashMap, VecDeque};
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
/// Maximum number of model-visible tools after namespace expansion.
pub const MAX_TOOLS: usize = 128;
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
    #[serde(default)]
    tools: Vec<Value>,
    #[serde(default = "default_tool_choice")]
    tool_choice: Value,
    #[serde(default)]
    parallel_tool_calls: bool,
    #[serde(default)]
    reasoning: Option<Value>,
    #[serde(default)]
    store: bool,
    #[serde(default)]
    stream_options: Option<Value>,
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    service_tier: Option<String>,
    #[serde(default)]
    prompt_cache_key: Option<String>,
    #[serde(default)]
    text: Option<Value>,
    #[serde(default)]
    client_metadata: Option<Value>,
}

fn default_tool_choice() -> Value {
    Value::String("auto".to_owned())
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ResponsesInput {
    Text(String),
    Items(Vec<Value>),
}

#[derive(Clone, Debug, Default)]
struct ResponsesToolMetadata {
    identities: HashMap<String, ResponsesToolIdentity>,
}

#[derive(Clone, Debug)]
struct ResponsesToolIdentity {
    name: String,
    namespace: Option<String>,
    custom: bool,
}

impl ResponsesToolMetadata {
    fn wire_name(&self, name: &str, namespace: Option<&str>, custom: bool) -> Option<&str> {
        self.identities.iter().find_map(|(wire_name, identity)| {
            (identity.name == name
                && identity.namespace.as_deref() == namespace
                && identity.custom == custom)
                .then_some(wire_name.as_str())
        })
    }

    fn identity<'a>(&'a self, wire_name: &'a str) -> ResponsesToolIdentity {
        self.identities
            .get(wire_name)
            .cloned()
            .unwrap_or_else(|| ResponsesToolIdentity {
                name: wire_name.to_owned(),
                namespace: None,
                custom: false,
            })
    }
}

#[derive(Clone, Debug, Default)]
struct ChatToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
}

impl ResponsesRequest {
    fn validate_and_translate(self) -> Result<(Value, ResponsesToolMetadata), String> {
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
        if self.store {
            return Err("Responses field 'store' must be false".to_owned());
        }
        validate_codex_controls(
            self.stream_options.as_ref(),
            &self.include,
            self.prompt_cache_key.as_deref(),
            self.text.as_ref(),
            self.client_metadata.as_ref(),
        )?;

        let (tools, tool_metadata) = translate_tools(self.tools)?;
        let tool_choice = translate_tool_choice(self.tool_choice, &tool_metadata)?;

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
                let mut pending_tool_calls = Vec::new();
                for item in items {
                    translate_input_item(
                        item,
                        &mut messages,
                        &mut pending_tool_calls,
                        &mut input_chars,
                        &tool_metadata,
                    )?;
                }
                flush_tool_calls(&mut messages, &mut pending_tool_calls);
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
        if !tools.is_empty() {
            body.insert("tools".to_owned(), Value::Array(tools));
            body.insert("tool_choice".to_owned(), tool_choice);
            body.insert(
                "parallel_tool_calls".to_owned(),
                Value::Bool(self.parallel_tool_calls),
            );
        }
        if let Some(max_output_tokens) = self.max_output_tokens {
            body.insert("max_tokens".to_owned(), json!(max_output_tokens));
        }
        if let Some(temperature) = self.temperature {
            body.insert("temperature".to_owned(), json!(temperature));
        }
        if let Some(reasoning) = self.reasoning {
            if let Some(effort) = validate_reasoning(reasoning)? {
                body.insert("reasoning_effort".to_owned(), Value::String(effort));
            }
        }
        if let Some(service_tier) = self.service_tier {
            if !matches!(
                service_tier.as_str(),
                "auto" | "default" | "flex" | "priority"
            ) {
                return Err(format!(
                    "unsupported Responses service_tier '{service_tier}'"
                ));
            }
            body.insert("service_tier".to_owned(), Value::String(service_tier));
        }
        Ok((Value::Object(body), tool_metadata))
    }
}

fn content_text(content: Value) -> Result<String, String> {
    match content {
        Value::String(text) => Ok(text),
        Value::Array(parts) => {
            if parts.is_empty() {
                return Err("Responses message content must contain at least one part".to_owned());
            }
            let mut text = String::new();
            for part in parts {
                let part = part
                    .as_object()
                    .ok_or_else(|| "Responses message content parts must be objects".to_owned())?;
                reject_unknown_fields(part, &["type", "text", "annotations", "logprobs"])?;
                let kind = required_string(part, "type")?;
                if !matches!(kind, "input_text" | "output_text" | "text") {
                    return Err(format!(
                        "unsupported Responses content type '{}'; only text parts are supported",
                        kind
                    ));
                }
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(required_string(part, "text")?);
            }
            Ok(text)
        }
        _ => Err("Responses message content must be text or text parts".to_owned()),
    }
}

fn reject_unknown_fields(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
) -> Result<(), String> {
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(format!("unsupported Responses API field '{field}'"));
    }
    Ok(())
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Responses field '{field}' must be a non-empty string"))
}

fn flush_tool_calls(messages: &mut Vec<Value>, pending: &mut Vec<Value>) {
    if !pending.is_empty() {
        messages.push(json!({
            "role": "assistant",
            "content": null,
            "tool_calls": std::mem::take(pending),
        }));
    }
}

fn translate_input_item(
    item: Value,
    messages: &mut Vec<Value>,
    pending_tool_calls: &mut Vec<Value>,
    input_chars: &mut usize,
    tool_metadata: &ResponsesToolMetadata,
) -> Result<(), String> {
    let item = item
        .as_object()
        .ok_or_else(|| "Responses input items must be objects".to_owned())?;
    let kind = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    match kind {
        "message" => {
            reject_unknown_fields(
                item,
                &[
                    "id",
                    "type",
                    "role",
                    "content",
                    "status",
                    "phase",
                    "internal_chat_message_metadata_passthrough",
                ],
            )?;
            flush_tool_calls(messages, pending_tool_calls);
            let role = required_string(item, "role")?;
            if !matches!(role, "system" | "developer" | "user" | "assistant") {
                return Err(format!("unsupported Responses message role '{role}'"));
            }
            let content = item
                .get("content")
                .cloned()
                .ok_or_else(|| "Responses message input requires 'content'".to_owned())?;
            let content = content_text(content)?;
            *input_chars = input_chars.saturating_add(content.chars().count());
            messages.push(json!({"role": role, "content": content}));
        }
        "function_call" => {
            reject_unknown_fields(
                item,
                &[
                    "id",
                    "type",
                    "name",
                    "namespace",
                    "arguments",
                    "encrypted_function_args",
                    "call_id",
                    "status",
                    "internal_chat_message_metadata_passthrough",
                ],
            )?;
            let name = required_string(item, "name")?;
            let namespace = item.get("namespace").and_then(Value::as_str);
            let wire_name = tool_metadata
                .wire_name(name, namespace, false)
                .unwrap_or(name);
            let arguments = required_string(item, "arguments")?;
            let call_id = required_string(item, "call_id")?;
            *input_chars = input_chars.saturating_add(arguments.chars().count());
            pending_tool_calls.push(json!({
                "id": call_id,
                "type": "function",
                "function": {"name": wire_name, "arguments": arguments},
            }));
        }
        "custom_tool_call" => {
            reject_unknown_fields(
                item,
                &[
                    "id",
                    "type",
                    "name",
                    "namespace",
                    "input",
                    "call_id",
                    "status",
                    "internal_chat_message_metadata_passthrough",
                ],
            )?;
            let name = required_string(item, "name")?;
            let namespace = item.get("namespace").and_then(Value::as_str);
            let wire_name = tool_metadata
                .wire_name(name, namespace, true)
                .unwrap_or(name);
            let input = required_string(item, "input")?;
            let call_id = required_string(item, "call_id")?;
            *input_chars = input_chars.saturating_add(input.chars().count());
            pending_tool_calls.push(json!({
                "id": call_id,
                "type": "function",
                "function": {
                    "name": wire_name,
                    "arguments": serde_json::to_string(&json!({"input": input}))
                        .map_err(|error| error.to_string())?,
                },
            }));
        }
        "function_call_output" | "custom_tool_call_output" => {
            reject_unknown_fields(
                item,
                &[
                    "id",
                    "type",
                    "name",
                    "call_id",
                    "output",
                    "internal_chat_message_metadata_passthrough",
                ],
            )?;
            flush_tool_calls(messages, pending_tool_calls);
            let call_id = required_string(item, "call_id")?;
            let output = item
                .get("output")
                .cloned()
                .ok_or_else(|| "Responses tool output requires 'output'".to_owned())?;
            let output = content_text(output)?;
            *input_chars = input_chars.saturating_add(output.chars().count());
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": output,
            }));
        }
        other => {
            return Err(format!("unsupported Responses input item type '{other}'"));
        }
    }
    Ok(())
}

fn translate_tools(tools: Vec<Value>) -> Result<(Vec<Value>, ResponsesToolMetadata), String> {
    if tools.len() > MAX_TOOLS {
        return Err(format!(
            "Responses field 'tools' exceeds {MAX_TOOLS} entries"
        ));
    }
    let mut translated = Vec::with_capacity(tools.len());
    let mut metadata = ResponsesToolMetadata::default();
    for tool in tools {
        let tool = tool
            .as_object()
            .ok_or_else(|| "Responses tools must be objects".to_owned())?;
        let kind = required_string(tool, "type")?;
        match kind {
            "function" => {
                translate_one_tool(tool, None, false, &mut translated, &mut metadata)?;
            }
            "custom" => {
                translate_one_tool(tool, None, true, &mut translated, &mut metadata)?;
            }
            "namespace" => {
                reject_unknown_fields(
                    tool,
                    &["type", "name", "description", "defer_loading", "tools"],
                )?;
                let namespace = required_string(tool, "name")?;
                let nested = tool
                    .get("tools")
                    .and_then(Value::as_array)
                    .ok_or_else(|| format!("Responses namespace '{namespace}' requires tools"))?;
                for nested_tool in nested {
                    if translated.len() >= MAX_TOOLS {
                        return Err(format!(
                            "Responses field 'tools' exceeds {MAX_TOOLS} entries after namespace expansion"
                        ));
                    }
                    let nested_tool = nested_tool.as_object().ok_or_else(|| {
                        format!("Responses namespace '{namespace}' tools must be objects")
                    })?;
                    match required_string(nested_tool, "type")? {
                        "function" => translate_one_tool(
                            nested_tool,
                            Some(namespace),
                            false,
                            &mut translated,
                            &mut metadata,
                        )?,
                        "custom" => translate_one_tool(
                            nested_tool,
                            Some(namespace),
                            true,
                            &mut translated,
                            &mut metadata,
                        )?,
                        other => {
                            return Err(format!(
                                "unsupported Responses namespace tool type '{other}'"
                            ));
                        }
                    }
                }
            }
            other => {
                return Err(format!(
                    "unsupported Responses tool type '{other}'; only function, custom, and namespace tools are supported"
                ));
            }
        }
    }
    Ok((translated, metadata))
}

fn translate_one_tool(
    tool: &serde_json::Map<String, Value>,
    namespace: Option<&str>,
    custom: bool,
    translated: &mut Vec<Value>,
    metadata: &mut ResponsesToolMetadata,
) -> Result<(), String> {
    if translated.len() >= MAX_TOOLS {
        return Err(format!(
            "Responses field 'tools' exceeds {MAX_TOOLS} entries after namespace expansion"
        ));
    }
    let name = required_string(tool, "name")?;
    if name.len() > 128 {
        return Err("Responses tool names must not exceed 128 bytes".to_owned());
    }
    if let Some(description) = tool.get("description") {
        if !description.is_string() {
            return Err(format!(
                "Responses tool '{name}' description must be a string"
            ));
        }
    }
    let wire_name = chat_tool_name(namespace, name, translated.len(), &metadata.identities);
    let description = tool
        .get("description")
        .cloned()
        .unwrap_or(Value::String(String::new()));
    let parameters = if custom {
        reject_unknown_fields(
            tool,
            &["type", "name", "description", "defer_loading", "format"],
        )?;
        let format = tool
            .get("format")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("Responses custom tool '{name}' requires format"))?;
        reject_unknown_fields(format, &["type", "syntax", "definition"])?;
        json!({
            "type": "object",
            "properties": {"input": {"type": "string"}},
            "required": ["input"],
            "additionalProperties": false,
        })
    } else {
        reject_unknown_fields(
            tool,
            &[
                "type",
                "name",
                "description",
                "strict",
                "defer_loading",
                "parameters",
            ],
        )?;
        let parameters = tool
            .get("parameters")
            .cloned()
            .ok_or_else(|| format!("Responses function tool '{name}' requires parameters"))?;
        if !parameters.is_object() {
            return Err(format!(
                "Responses function tool '{name}' parameters must be an object"
            ));
        }
        parameters
    };
    let mut function = json!({
        "name": wire_name.clone(),
        "description": description,
        "parameters": parameters,
    });
    if !custom {
        let strict = tool.get("strict").cloned().unwrap_or(Value::Bool(false));
        if !strict.is_boolean() {
            return Err(format!(
                "Responses function tool '{name}' strict must be boolean"
            ));
        }
        function["strict"] = strict;
    }
    translated.push(json!({"type": "function", "function": function}));
    metadata.identities.insert(
        wire_name,
        ResponsesToolIdentity {
            name: name.to_owned(),
            namespace: namespace.map(str::to_owned),
            custom,
        },
    );
    Ok(())
}

fn chat_tool_name(
    namespace: Option<&str>,
    name: &str,
    index: usize,
    existing: &HashMap<String, ResponsesToolIdentity>,
) -> String {
    let direct_is_valid = namespace.is_none()
        && name.len() <= 64
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        && !existing.contains_key(name);
    if direct_is_valid {
        return name.to_owned();
    }
    let suffix = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .take(48)
        .collect::<String>();
    format!("wf_{index}_{suffix}")
}

fn translate_tool_choice(choice: Value, metadata: &ResponsesToolMetadata) -> Result<Value, String> {
    match choice {
        Value::String(choice) if matches!(choice.as_str(), "auto" | "none" | "required") => {
            Ok(Value::String(choice))
        }
        Value::Object(choice) => {
            reject_unknown_fields(&choice, &["type", "name"])?;
            let kind = required_string(&choice, "type")?;
            if !matches!(kind, "function" | "custom") {
                return Err(format!("unsupported Responses tool_choice type '{kind}'"));
            }
            let name = required_string(&choice, "name")?;
            let identity = metadata
                .identities
                .values()
                .find(|identity| identity.name == name && identity.custom == (kind == "custom"));
            let Some(identity) = identity else {
                return Err(format!(
                    "Responses tool_choice references unknown {kind} tool '{name}'"
                ));
            };
            let wire_name = metadata
                .wire_name(
                    &identity.name,
                    identity.namespace.as_deref(),
                    identity.custom,
                )
                .unwrap_or(name);
            Ok(json!({"type": "function", "function": {"name": wire_name}}))
        }
        _ => Err("unsupported Responses field 'tool_choice'".to_owned()),
    }
}

fn validate_reasoning(reasoning: Value) -> Result<Option<String>, String> {
    let reasoning = reasoning
        .as_object()
        .ok_or_else(|| "Responses field 'reasoning' must be an object".to_owned())?;
    reject_unknown_fields(reasoning, &["effort", "summary", "context"])?;
    for field in ["summary", "context"] {
        if let Some(value) = reasoning.get(field) {
            if !value.is_string() && !value.is_null() {
                return Err(format!(
                    "Responses reasoning field '{field}' must be a string"
                ));
            }
        }
    }
    let effort = reasoning
        .get("effort")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if effort.as_deref().is_some_and(|effort| {
        !matches!(
            effort,
            "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
        )
    }) {
        return Err("unsupported Responses reasoning effort".to_owned());
    }
    Ok(effort)
}

fn validate_codex_controls(
    stream_options: Option<&Value>,
    include: &[String],
    prompt_cache_key: Option<&str>,
    text: Option<&Value>,
    client_metadata: Option<&Value>,
) -> Result<(), String> {
    if let Some(options) = stream_options {
        let options = options
            .as_object()
            .ok_or_else(|| "Responses field 'stream_options' must be an object".to_owned())?;
        reject_unknown_fields(options, &["reasoning_summary_delivery", "include_usage"])?;
    }
    if include
        .iter()
        .any(|value| value != "reasoning.encrypted_content")
    {
        return Err("unsupported Responses field 'include'".to_owned());
    }
    if prompt_cache_key.is_some_and(|value| value.len() > 512) {
        return Err("Responses field 'prompt_cache_key' exceeds 512 bytes".to_owned());
    }
    if let Some(text) = text {
        let text = text
            .as_object()
            .ok_or_else(|| "Responses field 'text' must be an object".to_owned())?;
        reject_unknown_fields(text, &["verbosity", "format"])?;
        if text.get("format").is_some_and(|value| !value.is_null()) {
            return Err("Responses structured text formats are not supported".to_owned());
        }
    }
    if client_metadata.is_some_and(|value| !value.is_object()) {
        return Err("Responses field 'client_metadata' must be an object".to_owned());
    }
    Ok(())
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
    let (openai_body, tool_metadata) = match request.validate_and_translate() {
        Ok(translated) => translated,
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
        return responses_buffered(inner, &model, &tool_metadata).await;
    }
    responses_streaming(inner, model, tool_metadata).await
}

fn unsupported_request(message: String) -> Response {
    crate::error_response(
        StatusCode::BAD_REQUEST,
        "wayfinder_router_unsupported_request",
        message,
        HeaderMap::new(),
    )
}

async fn responses_buffered(
    inner: Response,
    requested_model: &str,
    tool_metadata: &ResponsesToolMetadata,
) -> Response {
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
    let tool_calls = extract_buffered_tool_calls(&value);
    let output_chars = text
        .chars()
        .count()
        .saturating_add(tool_calls_chars(&tool_calls));
    if tool_calls.len() > MAX_TOOLS || output_chars > MAX_OUTPUT_CHARS {
        return crate::error_response(
            StatusCode::BAD_GATEWAY,
            "wayfinder_router_upstream_error",
            "Responses tool output exceeds the bounded contract".to_owned(),
            headers,
        );
    }
    let response = response_value(
        &response_id,
        model,
        &text,
        &tool_calls,
        tool_metadata,
        usage.as_ref(),
        "completed",
    );
    let mut output_headers = headers;
    output_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    (status, output_headers, Json(response)).into_response()
}

async fn responses_streaming(
    inner: Response,
    requested_model: String,
    tool_metadata: ResponsesToolMetadata,
) -> Response {
    let headers = route_headers(inner.headers());
    let response_id = response_id(&headers);
    let stream = inner.into_body().into_data_stream();
    let mut relay = ResponsesStreamRelay::new(stream, response_id, requested_model, tool_metadata);
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
    tool_calls: BTreeMap<u64, ChatToolCall>,
    tool_output_chars: usize,
    tool_metadata: ResponsesToolMetadata,
    usage: Option<Usage>,
    terminal: bool,
    event_count: usize,
}

impl<S> ResponsesStreamRelay<S>
where
    S: Stream<Item = Result<Bytes, axum::Error>> + Unpin,
{
    fn new(
        upstream: S,
        response_id: String,
        model: String,
        tool_metadata: ResponsesToolMetadata,
    ) -> Self {
        Self {
            upstream,
            decoder: SseDecoder::default(),
            pending: VecDeque::new(),
            response_id,
            model,
            output: String::new(),
            tool_calls: BTreeMap::new(),
            tool_output_chars: 0,
            tool_metadata,
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
                "response": response_value(
                    &self.response_id,
                    &self.model,
                    "",
                    &[],
                    &self.tool_metadata,
                    None,
                    "in_progress"
                )
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
                if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    if let Err(error) = self.accumulate_tool_calls(tool_calls) {
                        self.fail(error);
                        return;
                    }
                }
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
                    .saturating_add(self.tool_output_chars)
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

    fn accumulate_tool_calls(&mut self, calls: &[Value]) -> Result<(), String> {
        for call in calls {
            let call = call.as_object().ok_or_else(|| {
                "Responses upstream returned a non-object tool call delta".to_owned()
            })?;
            let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
            if !self.tool_calls.contains_key(&index) && self.tool_calls.len() >= MAX_TOOLS {
                return Err(format!("Responses stream exceeds {MAX_TOOLS} tool calls"));
            }
            let entry = self.tool_calls.entry(index).or_default();
            if let Some(id) = call.get("id").and_then(Value::as_str) {
                self.tool_output_chars = self.tool_output_chars.saturating_add(id.chars().count());
                entry.id.push_str(id);
            }
            if let Some(function) = call.get("function").and_then(Value::as_object) {
                if let Some(name) = function.get("name").and_then(Value::as_str) {
                    self.tool_output_chars =
                        self.tool_output_chars.saturating_add(name.chars().count());
                    entry.name.push_str(name);
                }
                if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                    self.tool_output_chars = self
                        .tool_output_chars
                        .saturating_add(arguments.chars().count());
                    entry.arguments.push_str(arguments);
                }
            }
            if self
                .output
                .chars()
                .count()
                .saturating_add(self.tool_output_chars)
                > MAX_OUTPUT_CHARS
            {
                return Err(format!(
                    "Responses stream exceeds {MAX_OUTPUT_CHARS} output characters"
                ));
            }
        }
        Ok(())
    }

    fn complete(&mut self) {
        if self.terminal {
            return;
        }
        self.terminal = true;
        if !self.output.is_empty() {
            self.pending.push_back(frame(
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "item": message_output(&self.response_id, &self.output, "completed"),
                }),
            ));
        }
        for (index, tool_call) in &self.tool_calls {
            self.pending.push_back(frame(
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "item": tool_call_output(
                        &self.response_id,
                        *index,
                        tool_call,
                        &self.tool_metadata,
                    ),
                }),
            ));
        }
        let tool_calls = self.tool_calls.values().cloned().collect::<Vec<_>>();
        self.pending.push_back(frame(
            "response.completed",
            json!({
                "type": "response.completed",
                "response": response_value(
                    &self.response_id,
                    &self.model,
                    &self.output,
                    &tool_calls,
                    &self.tool_metadata,
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
                    &self.tool_calls.values().cloned().collect::<Vec<_>>(),
                    &self.tool_metadata,
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
    tool_calls: &[ChatToolCall],
    tool_metadata: &ResponsesToolMetadata,
    usage: Option<&Usage>,
    status: &str,
) -> Value {
    let mut output = Vec::new();
    if !text.is_empty() {
        output.push(message_output(response_id, text, status));
    }
    output.extend(tool_calls.iter().enumerate().map(|(index, tool_call)| {
        tool_call_output(
            response_id,
            u64::try_from(index).unwrap_or(u64::MAX),
            tool_call,
            tool_metadata,
        )
    }));
    let mut response = json!({
        "id": format!("resp_{response_id}"),
        "object": "response",
        "created_at": SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs()),
        "status": status,
        "model": model,
        "output": output,
    });
    if let Some(usage) = usage {
        response["usage"] = serde_json::to_value(usage).unwrap_or(Value::Null);
    }
    response
}

fn message_output(response_id: &str, text: &str, status: &str) -> Value {
    json!({
        "id": format!("msg_{response_id}"),
        "type": "message",
        "status": status,
        "role": "assistant",
        "content": [{"type": "output_text", "text": text, "annotations": []}],
    })
}

fn tool_call_output(
    response_id: &str,
    index: u64,
    tool_call: &ChatToolCall,
    metadata: &ResponsesToolMetadata,
) -> Value {
    let identity = metadata.identity(&tool_call.name);
    let call_id = if tool_call.id.is_empty() {
        format!("call_{response_id}_{index}")
    } else {
        tool_call.id.clone()
    };
    let mut output = if identity.custom {
        let input = serde_json::from_str::<Value>(&tool_call.arguments)
            .ok()
            .and_then(|value| {
                value
                    .get("input")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| tool_call.arguments.clone());
        json!({
            "id": format!("ctc_{response_id}_{index}"),
            "type": "custom_tool_call",
            "status": "completed",
            "name": identity.name,
            "input": input,
            "call_id": call_id,
        })
    } else {
        json!({
            "id": format!("fc_{response_id}_{index}"),
            "type": "function_call",
            "status": "completed",
            "name": identity.name,
            "arguments": tool_call.arguments,
            "call_id": call_id,
        })
    };
    if let Some(namespace) = identity.namespace {
        output["namespace"] = Value::String(namespace);
    }
    output
}

fn extract_buffered_tool_calls(value: &Value) -> Vec<ChatToolCall> {
    value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .filter_map(|call| {
                    let function = call.get("function")?;
                    Some(ChatToolCall {
                        id: call
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        name: function.get("name")?.as_str()?.to_owned(),
                        arguments: function
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}")
                            .to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn tool_calls_chars(tool_calls: &[ChatToolCall]) -> usize {
    tool_calls.iter().fold(0usize, |total, call| {
        total
            .saturating_add(call.id.chars().count())
            .saturating_add(call.name.chars().count())
            .saturating_add(call.arguments.chars().count())
    })
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
        let (translated, _) = request.validate_and_translate()?;
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
            "background": true
        })) {
            Ok(_) => return Err("background must not be silently ignored".to_owned()),
            Err(error) => error,
        };
        assert!(error.to_string().contains("unknown field"));
        Ok(())
    }

    #[test]
    fn translates_codex_0_149_function_and_custom_tool_contract() -> Result<(), String> {
        // Mirrors ResponsesApiRequest and ToolSpec as serialized by OpenAI Codex
        // rust-v0.149.0 (commit 758ef40f50c1a458425c7cfbf1eb12cbc07af0b0).
        let request = serde_json::from_value::<ResponsesRequest>(json!({
            "model": "auto",
            "instructions": "Use tools to inspect the repository.",
            "input": [
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "Fix the failing test"}
                ]},
                {
                    "type": "function_call",
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"cargo test\"}",
                    "call_id": "call_exec"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_exec",
                    "output": "one test failed"
                }
            ],
            "tools": [
                {
                    "type": "function",
                    "name": "exec_command",
                    "description": "Run a command",
                    "strict": false,
                    "parameters": {"type": "object", "properties": {"cmd": {"type": "string"}}}
                },
                {
                    "type": "custom",
                    "name": "apply_patch",
                    "description": "Apply a patch",
                    "format": {"type": "grammar", "syntax": "lark", "definition": "start: /.+/"}
                }
            ],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "reasoning": {"effort": "medium", "summary": "auto"},
            "store": false,
            "stream": true,
            "include": ["reasoning.encrypted_content"],
            "prompt_cache_key": "codex-session",
            "client_metadata": {"originator": "codex_cli_rs"}
        }))
        .map_err(|error| error.to_string())?;
        let (translated, metadata) = request.validate_and_translate()?;
        assert_eq!(translated["tool_choice"], "auto");
        assert_eq!(translated["parallel_tool_calls"], true);
        assert_eq!(translated["reasoning_effort"], "medium");
        assert_eq!(translated["tools"][0]["function"]["name"], "exec_command");
        assert_eq!(translated["tools"][1]["function"]["name"], "apply_patch");
        assert_eq!(
            translated["messages"][2]["tool_calls"][0]["id"],
            "call_exec"
        );
        assert_eq!(translated["messages"][3]["role"], "tool");
        assert!(
            metadata
                .identities
                .get("apply_patch")
                .is_some_and(|identity| identity.custom)
        );
        Ok(())
    }

    #[test]
    fn restores_custom_tool_output_shape_for_codex() {
        let metadata = ResponsesToolMetadata {
            identities: HashMap::from([(
                "apply_patch".to_owned(),
                ResponsesToolIdentity {
                    name: "apply_patch".to_owned(),
                    namespace: None,
                    custom: true,
                },
            )]),
        };
        let output = tool_call_output(
            "request",
            0,
            &ChatToolCall {
                id: "call_patch".to_owned(),
                name: "apply_patch".to_owned(),
                arguments: "{\"input\":\"*** Begin Patch\"}".to_owned(),
            },
            &metadata,
        );
        assert_eq!(output["type"], "custom_tool_call");
        assert_eq!(output["input"], "*** Begin Patch");
        assert_eq!(output["call_id"], "call_patch");
    }

    #[test]
    fn flattens_and_restores_codex_namespace_tools() -> Result<(), String> {
        let (translated, metadata) = translate_tools(vec![json!({
            "type": "namespace",
            "name": "mcp__github",
            "description": "GitHub tools",
            "tools": [{
                "type": "function",
                "name": "get_issue",
                "description": "Get an issue",
                "strict": false,
                "parameters": {"type": "object", "properties": {}}
            }]
        })])?;
        let wire_name = translated[0]["function"]["name"]
            .as_str()
            .ok_or_else(|| "wire name".to_owned())?;
        let output = tool_call_output(
            "request",
            0,
            &ChatToolCall {
                id: "call_issue".to_owned(),
                name: wire_name.to_owned(),
                arguments: "{}".to_owned(),
            },
            &metadata,
        );
        assert_eq!(output["type"], "function_call");
        assert_eq!(output["name"], "get_issue");
        assert_eq!(output["namespace"], "mcp__github");
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
