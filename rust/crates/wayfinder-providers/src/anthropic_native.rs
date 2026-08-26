//! Native Anthropic Messages delivery primitives and OpenAI-shape translation.

use std::collections::BTreeMap;
use std::fmt;
use std::pin::Pin;

use bytes::{Bytes, BytesMut};
use futures_util::{Stream, StreamExt};
use http::header::{ACCEPT, CONTENT_TYPE};
use http::{HeaderMap, StatusCode};
use reqwest::redirect::Policy;
use reqwest::{Client, Url};
use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::openai_compat::{ProviderClientConfig, ProviderError, SecretValue};
use crate::sse::{SseDecodeError, SseDecoder, SseEvent};

/// Stable Anthropic Messages API version used by the native destination.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Bounded default when an OpenAI-shaped request omits `max_tokens`.
pub const DEFAULT_MAX_TOKENS: u64 = 4_096;
const MAX_TOOL_CALLS: usize = 64;

/// A validated Anthropic Messages base URL.
#[derive(Clone, PartialEq, Eq)]
pub struct AnthropicEndpoint {
    messages: Url,
}

impl AnthropicEndpoint {
    /// Validate a configured origin and append `/v1/messages`.
    pub fn parse(base_url: &str) -> Result<Self, ProviderError> {
        if base_url.is_empty() || base_url.trim() != base_url {
            return Err(ProviderError::InvalidEndpoint);
        }
        let complete = format!("{}/v1/messages", base_url.trim_end_matches('/'));
        let url = Url::parse(&complete).map_err(|_| ProviderError::InvalidEndpoint)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ProviderError::InvalidEndpoint);
        }
        Ok(Self { messages: url })
    }

    /// Fully resolved Messages endpoint.
    #[must_use]
    pub fn messages_url(&self) -> &Url {
        &self.messages
    }
}

impl fmt::Debug for AnthropicEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicEndpoint")
            .field("scheme", &self.messages.scheme())
            .field("host", &self.messages.host_str())
            .field("port", &self.messages.port())
            .finish_non_exhaustive()
    }
}

/// Request/response translation failure without retained request content.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AnthropicNativeError {
    /// The OpenAI-shaped request cannot be represented by Messages.
    #[error("request is unsupported by the Anthropic provider")]
    InvalidRequest,
    /// The provider returned an invalid buffered or streaming contract.
    #[error("Anthropic returned an invalid response")]
    InvalidResponse,
    /// Bounded SSE decoding failed.
    #[error(transparent)]
    Stream(#[from] SseDecodeError),
}

fn text_content(content: Option<&Value>) -> Result<String, AnthropicNativeError> {
    match content {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(text)) => Ok(text.clone()),
        Some(Value::Array(parts)) => {
            let mut text = Vec::new();
            for part in parts {
                let Some(part) = part.as_object() else {
                    return Err(AnthropicNativeError::InvalidRequest);
                };
                if part.get("type").and_then(Value::as_str) != Some("text") {
                    return Err(AnthropicNativeError::InvalidRequest);
                }
                text.push(
                    part.get("text")
                        .and_then(Value::as_str)
                        .ok_or(AnthropicNativeError::InvalidRequest)?
                        .to_owned(),
                );
            }
            Ok(text.join("\n"))
        }
        _ => Err(AnthropicNativeError::InvalidRequest),
    }
}

fn push_message(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = messages.last_mut().and_then(Value::as_object_mut) {
        if last.get("role").and_then(Value::as_str) == Some(role) {
            if let Some(content) = last.get_mut("content").and_then(Value::as_array_mut) {
                content.extend(blocks);
                return;
            }
        }
    }
    messages.push(json!({"role": role, "content": blocks}));
}

fn translate_openai_tool(tool: &Value) -> Result<Value, AnthropicNativeError> {
    let tool = tool
        .as_object()
        .ok_or(AnthropicNativeError::InvalidRequest)?;
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return Err(AnthropicNativeError::InvalidRequest);
    }
    let function = tool
        .get("function")
        .and_then(Value::as_object)
        .ok_or(AnthropicNativeError::InvalidRequest)?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or(AnthropicNativeError::InvalidRequest)?;
    let input_schema = function
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    if !input_schema.is_object() {
        return Err(AnthropicNativeError::InvalidRequest);
    }
    let mut translated = Map::new();
    translated.insert("name".to_owned(), Value::String(name.to_owned()));
    translated.insert("input_schema".to_owned(), input_schema);
    if let Some(description) = function.get("description").and_then(Value::as_str) {
        translated.insert(
            "description".to_owned(),
            Value::String(description.to_owned()),
        );
    }
    Ok(Value::Object(translated))
}

fn translate_tool_choice(value: &Value) -> Result<Value, AnthropicNativeError> {
    match value {
        Value::String(choice) => match choice.as_str() {
            "auto" => Ok(json!({"type": "auto"})),
            "required" => Ok(json!({"type": "any"})),
            "none" => Ok(json!({"type": "none"})),
            _ => Err(AnthropicNativeError::InvalidRequest),
        },
        Value::Object(choice) if choice.get("type").and_then(Value::as_str) == Some("function") => {
            let name = choice
                .get("function")
                .and_then(Value::as_object)
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or(AnthropicNativeError::InvalidRequest)?;
            Ok(json!({"type": "tool", "name": name}))
        }
        _ => Err(AnthropicNativeError::InvalidRequest),
    }
}

fn translate_assistant_message(
    message: &Map<String, Value>,
) -> Result<Vec<Value>, AnthropicNativeError> {
    let mut blocks = Vec::new();
    let text = text_content(message.get("content"))?;
    if !text.is_empty() {
        blocks.push(json!({"type": "text", "text": text}));
    }
    if let Some(calls) = message.get("tool_calls") {
        let calls = calls
            .as_array()
            .ok_or(AnthropicNativeError::InvalidRequest)?;
        if calls.len() > MAX_TOOL_CALLS {
            return Err(AnthropicNativeError::InvalidRequest);
        }
        for call in calls {
            let call = call
                .as_object()
                .ok_or(AnthropicNativeError::InvalidRequest)?;
            if call.get("type").and_then(Value::as_str) != Some("function") {
                return Err(AnthropicNativeError::InvalidRequest);
            }
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or(AnthropicNativeError::InvalidRequest)?;
            let function = call
                .get("function")
                .and_then(Value::as_object)
                .ok_or(AnthropicNativeError::InvalidRequest)?;
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or(AnthropicNativeError::InvalidRequest)?;
            let arguments = function
                .get("arguments")
                .and_then(Value::as_str)
                .ok_or(AnthropicNativeError::InvalidRequest)?;
            let input: Value = serde_json::from_str(arguments)
                .map_err(|_| AnthropicNativeError::InvalidRequest)?;
            if !input.is_object() {
                return Err(AnthropicNativeError::InvalidRequest);
            }
            blocks.push(json!({"type": "tool_use", "id": id, "name": name, "input": input}));
        }
    }
    Ok(blocks)
}

/// Translate one OpenAI Chat Completions request into Anthropic Messages.
pub fn openai_to_anthropic_request(
    body: &Value,
    provider_model: &str,
    streaming: bool,
) -> Result<Value, AnthropicNativeError> {
    let body = body
        .as_object()
        .ok_or(AnthropicNativeError::InvalidRequest)?;
    if [
        "audio",
        "frequency_penalty",
        "logprobs",
        "modalities",
        "prediction",
        "presence_penalty",
        "response_format",
        "seed",
        "service_tier",
        "top_logprobs",
    ]
    .iter()
    .any(|field| body.contains_key(*field))
        || body
            .get("n")
            .and_then(Value::as_u64)
            .is_some_and(|count| count != 1)
    {
        return Err(AnthropicNativeError::InvalidRequest);
    }
    let max_tokens = body
        .get("max_completion_tokens")
        .or_else(|| body.get("max_tokens"))
        .map_or(Ok(DEFAULT_MAX_TOKENS), |value| {
            value
                .as_u64()
                .filter(|value| *value > 0)
                .ok_or(AnthropicNativeError::InvalidRequest)
        })?;
    let raw_messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or(AnthropicNativeError::InvalidRequest)?;
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for message in raw_messages {
        let message = message
            .as_object()
            .ok_or(AnthropicNativeError::InvalidRequest)?;
        match message.get("role").and_then(Value::as_str) {
            Some("system" | "developer") => {
                let text = text_content(message.get("content"))?;
                if !text.is_empty() {
                    system.push(json!({"type": "text", "text": text}));
                }
            }
            Some("user") => {
                let text = text_content(message.get("content"))?;
                if !text.is_empty() {
                    push_message(
                        &mut messages,
                        "user",
                        vec![json!({"type": "text", "text": text})],
                    );
                }
            }
            Some("assistant") => {
                let blocks = translate_assistant_message(message)?;
                push_message(&mut messages, "assistant", blocks);
            }
            Some("tool") => {
                let tool_use_id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or(AnthropicNativeError::InvalidRequest)?;
                let content = text_content(message.get("content"))?;
                push_message(
                    &mut messages,
                    "user",
                    vec![json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content,
                    })],
                );
            }
            _ => return Err(AnthropicNativeError::InvalidRequest),
        }
    }
    if messages.is_empty() {
        return Err(AnthropicNativeError::InvalidRequest);
    }
    let mut output = Map::new();
    output.insert("model".to_owned(), Value::String(provider_model.to_owned()));
    output.insert("max_tokens".to_owned(), Value::Number(max_tokens.into()));
    output.insert("messages".to_owned(), Value::Array(messages));
    output.insert("stream".to_owned(), Value::Bool(streaming));
    if !system.is_empty() {
        output.insert("system".to_owned(), Value::Array(system));
    }
    for field in ["temperature", "top_p", "top_k"] {
        if let Some(value) = body.get(field) {
            output.insert(field.to_owned(), value.clone());
        }
    }
    if let Some(stop) = body.get("stop") {
        let sequences = match stop {
            Value::String(value) if !value.is_empty() => vec![Value::String(value.clone())],
            Value::Array(values)
                if !values.is_empty()
                    && values
                        .iter()
                        .all(|value| value.as_str().is_some_and(|value| !value.is_empty())) =>
            {
                values.clone()
            }
            _ => return Err(AnthropicNativeError::InvalidRequest),
        };
        output.insert("stop_sequences".to_owned(), Value::Array(sequences));
    }
    if let Some(tools) = body.get("tools") {
        let tools = tools
            .as_array()
            .ok_or(AnthropicNativeError::InvalidRequest)?;
        if tools.len() > MAX_TOOL_CALLS {
            return Err(AnthropicNativeError::InvalidRequest);
        }
        output.insert(
            "tools".to_owned(),
            Value::Array(
                tools
                    .iter()
                    .map(translate_openai_tool)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        );
    }
    if let Some(choice) = body.get("tool_choice") {
        let mut choice = translate_tool_choice(choice)?;
        if body.get("parallel_tool_calls").and_then(Value::as_bool) == Some(false) {
            choice
                .as_object_mut()
                .ok_or(AnthropicNativeError::InvalidRequest)?
                .insert("disable_parallel_tool_use".to_owned(), Value::Bool(true));
        }
        output.insert("tool_choice".to_owned(), choice);
    } else if body.get("parallel_tool_calls").and_then(Value::as_bool) == Some(false) {
        output.insert(
            "tool_choice".to_owned(),
            json!({"type": "auto", "disable_parallel_tool_use": true}),
        );
    }
    Ok(Value::Object(output))
}

fn openai_finish_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("max_tokens") => "length",
        Some("tool_use") => "tool_calls",
        _ => "stop",
    }
}

/// Translate a successful buffered Anthropic message into OpenAI Chat shape.
pub fn anthropic_to_openai_response(body: &Value) -> Result<Value, AnthropicNativeError> {
    let body = body
        .as_object()
        .ok_or(AnthropicNativeError::InvalidResponse)?;
    if body.get("type").and_then(Value::as_str) != Some("message") {
        return Err(AnthropicNativeError::InvalidResponse);
    }
    let content = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or(AnthropicNativeError::InvalidResponse)?;
    let mut text = Vec::new();
    let mut tool_calls = Vec::new();
    for block in content {
        let block = block
            .as_object()
            .ok_or(AnthropicNativeError::InvalidResponse)?;
        match block.get("type").and_then(Value::as_str) {
            Some("text") => text.push(
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or(AnthropicNativeError::InvalidResponse)?
                    .to_owned(),
            ),
            Some("tool_use") => {
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or(AnthropicNativeError::InvalidResponse)?;
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or(AnthropicNativeError::InvalidResponse)?;
                let input = block
                    .get("input")
                    .ok_or(AnthropicNativeError::InvalidResponse)?;
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(input)
                            .map_err(|_| AnthropicNativeError::InvalidResponse)?,
                    },
                }));
            }
            _ => return Err(AnthropicNativeError::InvalidResponse),
        }
    }
    let usage = body.get("usage").and_then(Value::as_object);
    let prompt_tokens = usage
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion_tokens = usage
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut message = Map::new();
    message.insert("role".to_owned(), Value::String("assistant".to_owned()));
    message.insert(
        "content".to_owned(),
        if text.is_empty() {
            Value::Null
        } else {
            Value::String(text.join(""))
        },
    );
    if !tool_calls.is_empty() {
        message.insert("tool_calls".to_owned(), Value::Array(tool_calls));
    }
    Ok(json!({
        "id": body.get("id").cloned().unwrap_or_else(|| Value::String("msg_anthropic".to_owned())),
        "object": "chat.completion",
        "created": 0,
        "model": body.get("model").cloned().unwrap_or_else(|| Value::String(String::new())),
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": openai_finish_reason(body.get("stop_reason").and_then(Value::as_str)),
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens.saturating_add(completion_tokens),
        },
    }))
}

/// Translate an Anthropic error envelope into OpenAI error shape.
#[must_use]
pub fn anthropic_to_openai_error(body: &Value) -> Value {
    let detail = body.get("error").unwrap_or(body);
    json!({
        "error": {
            "message": detail
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Anthropic upstream error"),
            "type": detail.get("type").and_then(Value::as_str).unwrap_or("api_error"),
        }
    })
}

fn openai_sse_frame(value: &Value) -> Result<Bytes, AnthropicNativeError> {
    let data = serde_json::to_string(value).map_err(|_| AnthropicNativeError::InvalidResponse)?;
    Ok(Bytes::from(format!("data: {data}\n\n")))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveBlock {
    Text,
    Tool {
        tool_index: usize,
        saw_arguments: bool,
    },
}

/// Incremental Anthropic Messages SSE to OpenAI Chat Completions SSE adapter.
#[derive(Debug)]
pub struct AnthropicToOpenAiStream {
    decoder: SseDecoder,
    id: String,
    model: String,
    input_tokens: u64,
    active: BTreeMap<u64, ActiveBlock>,
    tool_count: usize,
    finish_sent: bool,
    terminal: bool,
}

impl Default for AnthropicToOpenAiStream {
    fn default() -> Self {
        Self {
            decoder: SseDecoder::default(),
            id: "msg_anthropic".to_owned(),
            model: String::new(),
            input_tokens: 0,
            active: BTreeMap::new(),
            tool_count: 0,
            finish_sent: false,
            terminal: false,
        }
    }
}

impl AnthropicToOpenAiStream {
    /// Feed one arbitrarily fragmented provider chunk.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Bytes>, AnthropicNativeError> {
        let events = self.decoder.push(chunk)?;
        self.translate_events(events)
    }

    /// Finish the provider stream, requiring a terminal Messages event.
    pub fn finish(&mut self) -> Result<Vec<Bytes>, AnthropicNativeError> {
        let events = self.decoder.finish()?;
        let output = self.translate_events(events)?;
        if !self.terminal {
            return Err(AnthropicNativeError::InvalidResponse);
        }
        Ok(output)
    }

    fn translate_events(
        &mut self,
        events: Vec<SseEvent>,
    ) -> Result<Vec<Bytes>, AnthropicNativeError> {
        let mut output = Vec::new();
        for event in events {
            if self.terminal || event.event == "ping" {
                continue;
            }
            let payload: Value = serde_json::from_str(&event.data)
                .map_err(|_| AnthropicNativeError::InvalidResponse)?;
            let event_type = if event.event == "message" {
                payload.get("type").and_then(Value::as_str).unwrap_or("")
            } else {
                event.event.as_str()
            };
            match event_type {
                "message_start" => {
                    let message = payload
                        .get("message")
                        .and_then(Value::as_object)
                        .ok_or(AnthropicNativeError::InvalidResponse)?;
                    self.id = message
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("msg_anthropic")
                        .to_owned();
                    self.model = message
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    self.input_tokens = message
                        .get("usage")
                        .and_then(Value::as_object)
                        .and_then(|usage| usage.get("input_tokens"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    output.push(self.role_frame()?);
                }
                "content_block_start" => {
                    let index = payload
                        .get("index")
                        .and_then(Value::as_u64)
                        .ok_or(AnthropicNativeError::InvalidResponse)?;
                    let block = payload
                        .get("content_block")
                        .and_then(Value::as_object)
                        .ok_or(AnthropicNativeError::InvalidResponse)?;
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            self.active.insert(index, ActiveBlock::Text);
                        }
                        Some("tool_use") => {
                            if self.tool_count >= MAX_TOOL_CALLS {
                                return Err(AnthropicNativeError::InvalidResponse);
                            }
                            let tool_index = self.tool_count;
                            self.tool_count += 1;
                            self.active.insert(
                                index,
                                ActiveBlock::Tool {
                                    tool_index,
                                    saw_arguments: false,
                                },
                            );
                            output.push(openai_sse_frame(&json!({
                                "id": self.id,
                                "object": "chat.completion.chunk",
                                "created": 0,
                                "model": self.model,
                                "choices": [{
                                    "index": 0,
                                    "delta": {"tool_calls": [{
                                        "index": tool_index,
                                        "id": block.get("id").cloned().unwrap_or(Value::Null),
                                        "type": "function",
                                        "function": {
                                            "name": block
                                                .get("name")
                                                .cloned()
                                                .unwrap_or(Value::Null),
                                            "arguments": "",
                                        },
                                    }]},
                                    "finish_reason": Value::Null,
                                }],
                            }))?);
                        }
                        _ => return Err(AnthropicNativeError::InvalidResponse),
                    }
                }
                "content_block_delta" => {
                    let index = payload
                        .get("index")
                        .and_then(Value::as_u64)
                        .ok_or(AnthropicNativeError::InvalidResponse)?;
                    let delta = payload
                        .get("delta")
                        .and_then(Value::as_object)
                        .ok_or(AnthropicNativeError::InvalidResponse)?;
                    let translated_delta = match (
                        self.active.get_mut(&index),
                        delta.get("type").and_then(Value::as_str),
                    ) {
                        (Some(ActiveBlock::Text), Some("text_delta")) => {
                            let text = delta
                                .get("text")
                                .and_then(Value::as_str)
                                .ok_or(AnthropicNativeError::InvalidResponse)?;
                            json!({"content": text})
                        }
                        (
                            Some(ActiveBlock::Tool {
                                tool_index,
                                saw_arguments,
                            }),
                            Some("input_json_delta"),
                        ) => {
                            let partial = delta
                                .get("partial_json")
                                .and_then(Value::as_str)
                                .ok_or(AnthropicNativeError::InvalidResponse)?;
                            *saw_arguments = true;
                            json!({"tool_calls": [{
                                "index": tool_index,
                                "function": {"arguments": partial},
                            }]})
                        }
                        _ => return Err(AnthropicNativeError::InvalidResponse),
                    };
                    output.push(self.delta_frame(translated_delta)?);
                }
                "content_block_stop" => {
                    let index = payload
                        .get("index")
                        .and_then(Value::as_u64)
                        .ok_or(AnthropicNativeError::InvalidResponse)?;
                    match self.active.remove(&index) {
                        Some(ActiveBlock::Tool {
                            tool_index,
                            saw_arguments: false,
                        }) => output.push(self.delta_frame(json!({"tool_calls": [{
                            "index": tool_index,
                            "function": {"arguments": "{}"},
                        }]}))?),
                        Some(_) => {}
                        None => return Err(AnthropicNativeError::InvalidResponse),
                    }
                }
                "message_delta" => {
                    let reason = payload
                        .get("delta")
                        .and_then(Value::as_object)
                        .and_then(|delta| delta.get("stop_reason"))
                        .and_then(Value::as_str);
                    let output_tokens = payload
                        .get("usage")
                        .and_then(Value::as_object)
                        .and_then(|usage| usage.get("output_tokens"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    output.push(openai_sse_frame(&json!({
                        "id": self.id,
                        "object": "chat.completion.chunk",
                        "created": 0,
                        "model": self.model,
                        "choices": [{
                            "index": 0,
                            "delta": {},
                            "finish_reason": openai_finish_reason(reason),
                        }],
                        "usage": {
                            "prompt_tokens": self.input_tokens,
                            "completion_tokens": output_tokens,
                            "total_tokens": self.input_tokens.saturating_add(output_tokens),
                        },
                    }))?);
                    self.finish_sent = true;
                }
                "message_stop" => {
                    if !self.active.is_empty() {
                        return Err(AnthropicNativeError::InvalidResponse);
                    }
                    if !self.finish_sent {
                        output.push(openai_sse_frame(&json!({
                            "id": self.id,
                            "object": "chat.completion.chunk",
                            "created": 0,
                            "model": self.model,
                            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                        }))?);
                    }
                    output.push(Bytes::from_static(b"data: [DONE]\n\n"));
                    self.terminal = true;
                }
                "error" => {
                    output.push(openai_sse_frame(&anthropic_to_openai_error(&payload))?);
                    output.push(Bytes::from_static(b"data: [DONE]\n\n"));
                    self.terminal = true;
                }
                _ => return Err(AnthropicNativeError::InvalidResponse),
            }
        }
        Ok(output)
    }

    fn role_frame(&self) -> Result<Bytes, AnthropicNativeError> {
        self.delta_frame(json!({"role": "assistant"}))
    }

    fn delta_frame(&self, delta: Value) -> Result<Bytes, AnthropicNativeError> {
        openai_sse_frame(&json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": 0,
            "model": self.model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": Value::Null}],
        }))
    }
}

/// Bounded buffered Anthropic provider response.
pub struct BufferedAnthropicResponse {
    status: StatusCode,
    content_type: String,
    body: Bytes,
}

impl BufferedAnthropicResponse {
    /// Upstream status.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    /// Upstream media type.
    #[must_use]
    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    /// Consume the bounded body.
    #[must_use]
    pub fn into_body(self) -> Bytes {
        self.body
    }
}

/// Cancellable Anthropic response bytes.
pub type AnthropicByteStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, ProviderError>> + Send + 'static>>;

/// Streaming Anthropic provider response.
pub struct StreamingAnthropicResponse {
    status: StatusCode,
    content_type: String,
    stream: AnthropicByteStream,
}

impl StreamingAnthropicResponse {
    /// Upstream status.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    /// Upstream media type.
    #[must_use]
    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    /// Consume the cancellable stream.
    #[must_use]
    pub fn into_stream(self) -> AnthropicByteStream {
        self.stream
    }
}

/// Reqwest-backed native Anthropic client with fixed transport policy.
#[derive(Clone)]
pub struct AnthropicProviderClient {
    client: Client,
    max_response_bytes: usize,
}

impl AnthropicProviderClient {
    /// Construct a client with redirects and ambient proxies disabled.
    pub fn new(config: ProviderClientConfig) -> Result<Self, ProviderError> {
        if config.request_timeout.is_zero()
            || config.connect_timeout.is_zero()
            || config.max_response_bytes == 0
        {
            return Err(ProviderError::InvalidClientConfig);
        }
        let client = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            .tcp_nodelay(true)
            .user_agent(format!(
                "wayfinder-router/{}",
                option_env!("WAYFINDER_PRODUCT_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
            ))
            .build()
            .map_err(|_| ProviderError::ClientBuild)?;
        Ok(Self {
            client,
            max_response_bytes: config.max_response_bytes,
        })
    }

    fn request(
        &self,
        endpoint: &AnthropicEndpoint,
        body: &Value,
        credential: &SecretValue,
        accept: &'static str,
        headers: Option<&HeaderMap>,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        let mut request = self
            .client
            .post(endpoint.messages_url().clone())
            .header(ACCEPT, accept)
            .header("x-api-key", credential.api_key_header()?)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(body);
        if let Some(headers) = headers {
            for name in ["traceparent", "tracestate"] {
                if let Some(value) = headers.get(name) {
                    request = request.header(name, value.clone());
                }
            }
        }
        Ok(request)
    }

    /// Send one bounded buffered Messages request.
    pub async fn send_buffered_with_headers(
        &self,
        endpoint: &AnthropicEndpoint,
        body: &Value,
        credential: &SecretValue,
        headers: Option<&HeaderMap>,
    ) -> Result<BufferedAnthropicResponse, ProviderError> {
        let response = self
            .request(endpoint, body, credential, "application/json", headers)?
            .send()
            .await
            .map_err(|_| ProviderError::Transport)?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/json")
            .to_owned();
        if response
            .content_length()
            .is_some_and(|length| length > self.max_response_bytes as u64)
        {
            return Err(ProviderError::ResponseTooLarge {
                limit: self.max_response_bytes,
            });
        }
        let mut body = BytesMut::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ProviderError::Transport)?;
            let new_length =
                body.len()
                    .checked_add(chunk.len())
                    .ok_or(ProviderError::ResponseTooLarge {
                        limit: self.max_response_bytes,
                    })?;
            if new_length > self.max_response_bytes {
                return Err(ProviderError::ResponseTooLarge {
                    limit: self.max_response_bytes,
                });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(BufferedAnthropicResponse {
            status,
            content_type,
            body: body.freeze(),
        })
    }

    /// Establish one cancellable Messages SSE response.
    pub async fn send_stream_with_headers(
        &self,
        endpoint: &AnthropicEndpoint,
        body: &Value,
        credential: &SecretValue,
        headers: Option<&HeaderMap>,
    ) -> Result<StreamingAnthropicResponse, ProviderError> {
        let response = self
            .request(endpoint, body, credential, "text/event-stream", headers)?
            .send()
            .await
            .map_err(|_| ProviderError::Transport)?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("text/event-stream")
            .to_owned();
        if !status.is_success() {
            if response
                .content_length()
                .is_some_and(|length| length > self.max_response_bytes as u64)
            {
                return Err(ProviderError::ResponseTooLarge {
                    limit: self.max_response_bytes,
                });
            }
            let mut body = BytesMut::new();
            let mut upstream = response.bytes_stream();
            while let Some(chunk) = upstream.next().await {
                let chunk = chunk.map_err(|_| ProviderError::Transport)?;
                let new_length =
                    body.len()
                        .checked_add(chunk.len())
                        .ok_or(ProviderError::ResponseTooLarge {
                            limit: self.max_response_bytes,
                        })?;
                if new_length > self.max_response_bytes {
                    return Err(ProviderError::ResponseTooLarge {
                        limit: self.max_response_bytes,
                    });
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(StreamingAnthropicResponse {
                status,
                content_type,
                stream: Box::pin(futures_util::stream::once(async move { Ok(body.freeze()) })),
            });
        }
        let stream = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|_| ProviderError::Transport));
        Ok(StreamingAnthropicResponse {
            status,
            content_type,
            stream: Box::pin(stream),
        })
    }
}

impl fmt::Debug for AnthropicProviderClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicProviderClient")
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_translation_preserves_tools_and_never_copies_model()
    -> Result<(), AnthropicNativeError> {
        let translated = openai_to_anthropic_request(
            &json!({
                "model": "auto",
                "messages": [
                    {"role": "system", "content": "Be concise"},
                    {"role": "user", "content": "weather"},
                    {"role": "assistant", "content": null, "tool_calls": [{
                        "id": "call_1", "type": "function",
                        "function": {"name": "weather", "arguments": "{\"city\":\"Bath\"}"}
                    }]},
                    {"role": "tool", "tool_call_id": "call_1", "content": "rain"}
                ],
                "tools": [{"type": "function", "function": {
                    "name": "weather", "description": "forecast",
                    "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
                }}],
                "tool_choice": "required"
            }),
            "claude-provider-model",
            false,
        )?;
        assert_eq!(translated["model"], "claude-provider-model");
        assert_eq!(translated["max_tokens"], DEFAULT_MAX_TOKENS);
        assert_eq!(translated["system"][0]["text"], "Be concise");
        assert_eq!(translated["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(
            translated["messages"][2]["content"][0]["type"],
            "tool_result"
        );
        assert_eq!(translated["tools"][0]["name"], "weather");
        assert_eq!(translated["tool_choice"]["type"], "any");
        Ok(())
    }

    #[test]
    fn buffered_response_translation_preserves_usage_and_tool_calls()
    -> Result<(), AnthropicNativeError> {
        let translated = anthropic_to_openai_response(&json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "claude",
            "content": [
                {"type": "text", "text": "checking"},
                {"type": "tool_use", "id": "toolu_1", "name": "bash", "input": {"command": "pwd"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 9, "output_tokens": 4}
        }))?;
        assert_eq!(translated["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(translated["choices"][0]["message"]["content"], "checking");
        assert_eq!(
            translated["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"command\":\"pwd\"}"
        );
        assert_eq!(translated["usage"]["total_tokens"], 13);
        Ok(())
    }

    #[test]
    fn fragmented_stream_translates_text_tool_usage_and_done() -> Result<(), AnthropicNativeError> {
        let input = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude\",\"usage\":{\"input_tokens\":5}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"bash\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\":\\\"pwd\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":7}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
        let mut translator = AnthropicToOpenAiStream::default();
        let mut output = Vec::new();
        for fragment in input.as_bytes().chunks(13) {
            output.extend(translator.push(fragment)?);
        }
        output.extend(translator.finish()?);
        let joined = output
            .iter()
            .map(|chunk| String::from_utf8_lossy(chunk))
            .collect::<String>();
        assert!(joined.contains(r#""content":"hi""#));
        assert!(joined.contains(r#""name":"bash""#));
        assert!(joined.contains(r#""finish_reason":"tool_calls""#));
        assert!(joined.contains(r#""total_tokens":12"#));
        assert!(joined.ends_with("data: [DONE]\n\n"));
        Ok(())
    }

    #[test]
    fn endpoint_is_exact_and_rejects_credential_bearing_urls() -> Result<(), ProviderError> {
        let endpoint = AnthropicEndpoint::parse("https://api.anthropic.com")?;
        assert_eq!(
            endpoint.messages_url().as_str(),
            "https://api.anthropic.com/v1/messages"
        );
        assert!(AnthropicEndpoint::parse("https://key@example.com").is_err());
        assert!(AnthropicEndpoint::parse("https://example.com?next=evil").is_err());
        Ok(())
    }
}
