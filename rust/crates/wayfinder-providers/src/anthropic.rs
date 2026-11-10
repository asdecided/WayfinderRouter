//! Pure Anthropic Messages to OpenAI Chat Completions translation.
//!
//! This compatibility boundary performs no routing, I/O, or authentication and
//! intentionally accepts tolerant `serde_json::Value` inputs. Streaming is an
//! explicit, bounded state machine over the shared incremental SSE decoder.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::sse::{
    DEFAULT_MAX_EVENT_BYTES, DEFAULT_MAX_LINE_BYTES, SseDecodeError, SseDecoder, SseEvent,
};

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|number| number != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn flatten_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| match block {
                Value::String(text) => Some(text.as_str()),
                Value::Object(block) => block.get("text").and_then(Value::as_str),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

// Python's json.dumps defaults to ensure_ascii=True. OpenAI function arguments
// are strings, so this observable detail matters even though both forms are JSON.
fn python_compact_json(value: &Value) -> String {
    let encoded = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_owned());
    let mut output = String::with_capacity(encoded.len());
    for character in encoded.chars() {
        let code = u32::from(character);
        if code <= 0x7e {
            output.push(character);
        } else if code <= 0xffff {
            output.push_str(&format!("\\u{code:04x}"));
        } else {
            let code = code - 0x1_0000;
            let high = 0xd800 + (code >> 10);
            let low = 0xdc00 + (code & 0x3ff);
            output.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
        }
    }
    pad_python_exponents(&output)
}

// CPython pads a one-digit JSON exponent (`1e-06`) while serde_json emits
// `1e-6`. Only inspect JSON number tokens: string contents must remain exact.
fn pad_python_exponents(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut output = String::with_capacity(encoded.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            output.push(char::from(byte));
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            output.push('"');
            index += 1;
            continue;
        }
        if byte != b'e' && byte != b'E' {
            output.push(char::from(byte));
            index += 1;
            continue;
        }

        output.push('e');
        index += 1;
        if index < bytes.len() && matches!(bytes[index], b'+' | b'-') {
            output.push(char::from(bytes[index]));
            index += 1;
        }
        let digits_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index.saturating_sub(digits_start) == 1 {
            output.push('0');
        }
        for digit in &bytes[digits_start..index] {
            output.push(char::from(*digit));
        }
    }
    output
}

fn translate_tool(tool: &Map<String, Value>) -> Value {
    let mut function = Map::new();
    function.insert(
        "name".to_owned(),
        tool.get("name")
            .cloned()
            .unwrap_or_else(|| Value::String(String::new())),
    );
    let parameters = tool
        .get("input_schema")
        .filter(|value| truthy(value))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    function.insert("parameters".to_owned(), parameters);
    if let Some(description) = tool.get("description").and_then(Value::as_str) {
        if !description.is_empty() {
            function.insert(
                "description".to_owned(),
                Value::String(description.to_owned()),
            );
        }
    }
    json!({"type": "function", "function": function})
}

fn translate_tool_choice(choice: &Value) -> Value {
    if choice.is_string() {
        return choice.clone();
    }
    let Some(choice) = choice.as_object() else {
        return Value::String("auto".to_owned());
    };
    match choice.get("type").and_then(Value::as_str) {
        Some("auto") => Value::String("auto".to_owned()),
        Some("any") => Value::String("required".to_owned()),
        Some("none") => Value::String("none".to_owned()),
        Some("tool") if choice.get("name").is_some_and(truthy) => json!({
            "type": "function", "function": {"name": choice.get("name").cloned()}
        }),
        _ => Value::String("auto".to_owned()),
    }
}

fn translate_message(message: &Map<String, Value>) -> Vec<Value> {
    let role = message.get("role").cloned().unwrap_or(Value::Null);
    let Some(content) = message.get("content") else {
        return Vec::new();
    };
    if let Value::String(text) = content {
        return vec![json!({"role": role, "content": text})];
    }
    let Some(blocks) = content.as_array() else {
        return Vec::new();
    };
    let mut text = Vec::new();
    let mut tool_calls = Vec::new();
    let mut output = Vec::new();
    for block in blocks.iter().filter_map(Value::as_object) {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(part) = block.get("text").and_then(Value::as_str) {
                    text.push(part.to_owned());
                }
            }
            Some("tool_use") => {
                let input = block
                    .get("input")
                    .filter(|value| truthy(value))
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Map::new()));
                tool_calls.push(json!({
                    "id": block.get("id").cloned().unwrap_or_else(|| Value::String(String::new())),
                    "type": "function",
                    "function": {
                        "name": block.get("name").cloned().unwrap_or_else(|| Value::String(String::new())),
                        "arguments": python_compact_json(&input),
                    }
                }));
            }
            Some("tool_result") => output.push(json!({
                "role": "tool",
                "tool_call_id": block.get("tool_use_id").cloned().unwrap_or_else(|| Value::String(String::new())),
                "content": flatten_text(block.get("content")),
            })),
            _ => {}
        }
    }
    if role.as_str() == Some("assistant") {
        if !text.is_empty() || !tool_calls.is_empty() {
            let mut assistant = Map::new();
            assistant.insert("role".to_owned(), Value::String("assistant".to_owned()));
            assistant.insert(
                "content".to_owned(),
                if text.is_empty() {
                    Value::Null
                } else {
                    Value::String(text.join("\n"))
                },
            );
            if !tool_calls.is_empty() {
                assistant.insert("tool_calls".to_owned(), Value::Array(tool_calls));
            }
            output.push(Value::Object(assistant));
        }
    } else if !text.is_empty() {
        output.push(json!({"role": role, "content": text.join("\n")}));
    }
    output
}

/// Translate an Anthropic `/v1/messages` request into an OpenAI chat request.
#[must_use]
pub fn anthropic_to_openai_request(body: &Value) -> Value {
    let empty = Map::new();
    let body = body.as_object().unwrap_or(&empty);
    let mut output = Map::new();
    output.insert(
        "model".to_owned(),
        body.get("model")
            .cloned()
            .unwrap_or_else(|| Value::String("auto".to_owned())),
    );
    let mut messages = Vec::new();
    let system = flatten_text(body.get("system"));
    if !system.is_empty() {
        messages.push(json!({"role": "system", "content": system}));
    }
    if let Some(input) = body.get("messages").and_then(Value::as_array) {
        for message in input.iter().filter_map(Value::as_object) {
            messages.extend(translate_message(message));
        }
    }
    output.insert("messages".to_owned(), Value::Array(messages));
    for key in ["max_tokens", "temperature", "top_p"] {
        if let Some(value) = body.get(key) {
            output.insert(key.to_owned(), value.clone());
        }
    }
    if let Some(value) = body.get("stop_sequences").filter(|value| truthy(value)) {
        output.insert("stop".to_owned(), value.clone());
    }
    if body.get("stream").is_some_and(truthy) {
        output.insert("stream".to_owned(), Value::Bool(true));
    }
    if let Some(tools) = body
        .get("tools")
        .filter(|value| truthy(value))
        .and_then(Value::as_array)
    {
        output.insert(
            "tools".to_owned(),
            Value::Array(
                tools
                    .iter()
                    .filter_map(Value::as_object)
                    .map(translate_tool)
                    .collect(),
            ),
        );
    }
    if let Some(choice) = body.get("tool_choice").filter(|value| !value.is_null()) {
        output.insert("tool_choice".to_owned(), translate_tool_choice(choice));
    }
    Value::Object(output)
}

fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        0
    } else {
        u64::try_from(text.chars().count() / 4)
            .unwrap_or(u64::MAX)
            .max(1)
    }
}

fn stop_reason(reason: Option<&Value>) -> &'static str {
    match reason.and_then(Value::as_str) {
        Some("length") => "max_tokens",
        Some("tool_calls" | "function_call") => "tool_use",
        _ => "end_turn",
    }
}

fn usage(response: &Map<String, Value>, prompt: &str, completion: &str) -> (u64, u64) {
    if let Some(usage) = response.get("usage").and_then(Value::as_object) {
        if let (Some(input), Some(output)) = (
            usage.get("prompt_tokens").and_then(Value::as_u64),
            usage.get("completion_tokens").and_then(Value::as_u64),
        ) {
            return (input, output);
        }
    }
    (estimate_tokens(prompt), estimate_tokens(completion))
}

/// Translate a buffered OpenAI chat completion into an Anthropic message.
#[must_use]
pub fn openai_to_anthropic_response(
    response: &Value,
    model: &str,
    message_id: &str,
    prompt_text: &str,
) -> Value {
    let empty = Map::new();
    let response = response.as_object().unwrap_or(&empty);
    let choice = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(Value::as_object);
    let message = choice
        .and_then(|choice| choice.get("message"))
        .and_then(Value::as_object);
    let mut content = Vec::new();
    let mut completion = String::new();
    if let Some(text) = message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
    {
        completion.push_str(text);
        if !text.is_empty() {
            content.push(json!({"type": "text", "text": text}));
        }
    }
    if let Some(calls) = message
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for (index, call) in calls.iter().filter_map(Value::as_object).enumerate() {
            let function = call.get("function").and_then(Value::as_object);
            let arguments = function
                .and_then(|function| function.get("arguments"))
                .and_then(Value::as_str)
                .unwrap_or("");
            completion.push_str(arguments);
            content.push(json!({
                "type": "tool_use",
                "id": call.get("id").filter(|value| truthy(value)).cloned()
                    .unwrap_or_else(|| Value::String(format!("toolu_{index}"))),
                "name": function.and_then(|function| function.get("name")).cloned()
                    .unwrap_or_else(|| Value::String(String::new())),
                "input": serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({})),
            }));
        }
    }
    if content.is_empty() {
        content.push(json!({"type": "text", "text": ""}));
    }
    let (input_tokens, output_tokens) = usage(response, prompt_text, &completion);
    json!({
        "id": message_id, "type": "message", "role": "assistant", "model": model,
        "content": content,
        "stop_reason": stop_reason(choice.and_then(|choice| choice.get("finish_reason"))),
        "stop_sequence": Value::Null,
        "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens},
    })
}

/// Build an Anthropic error envelope for an HTTP status.
#[must_use]
pub fn anthropic_error(status: u16, message: &str) -> Value {
    let error_type = match status {
        400 | 402 | 422 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        413 => "request_too_large",
        429 => "rate_limit_error",
        503 => "overloaded_error",
        _ => "api_error",
    };
    json!({"type": "error", "error": {"type": error_type, "message": message}})
}

/// Default maximum number of interleaved OpenAI tool calls retained per stream.
pub const DEFAULT_MAX_STREAM_TOOL_CALLS: usize = 128;
/// Default aggregate bytes retained for tool ids, names, and argument fragments.
pub const DEFAULT_MAX_STREAM_TOOL_BYTES: usize = 4 * 1_024 * 1_024;

/// Memory limits for [`MessagesStreamTranslator`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnthropicStreamLimits {
    /// Maximum bytes in one upstream SSE field line.
    pub max_line_bytes: usize,
    /// Maximum aggregate `data:` bytes in one upstream SSE event.
    pub max_event_bytes: usize,
    /// Maximum distinct OpenAI tool-call indices buffered until stream end.
    pub max_tool_calls: usize,
    /// Maximum aggregate retained bytes across buffered tool calls.
    pub max_tool_bytes: usize,
}

impl Default for AnthropicStreamLimits {
    fn default() -> Self {
        Self {
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
            max_tool_calls: DEFAULT_MAX_STREAM_TOOL_CALLS,
            max_tool_bytes: DEFAULT_MAX_STREAM_TOOL_BYTES,
        }
    }
}

/// Malformed or over-limit input to the Anthropic streaming adapter.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AnthropicStreamError {
    /// One or more configured bounds were zero.
    #[error("Anthropic stream limits must be positive")]
    InvalidLimits,
    /// The shared incremental SSE decoder rejected the transport bytes.
    #[error(transparent)]
    Decode(#[from] SseDecodeError),
    /// Too many distinct parallel tool calls were retained.
    #[error("Anthropic stream exceeds {limit} buffered tool calls")]
    TooManyToolCalls { limit: usize },
    /// Buffered tool-call metadata and arguments exceeded the aggregate bound.
    #[error("Anthropic stream exceeds {limit} buffered tool bytes")]
    ToolDataTooLarge { limit: usize },
}

#[derive(Clone, Debug, Default)]
struct BufferedToolCall {
    id: Option<String>,
    name: String,
    arguments: String,
}

/// Incrementally translate fragmented OpenAI SSE into Anthropic Messages SSE.
///
/// Text deltas are emitted immediately. OpenAI may interleave parallel tool
/// calls, so tool-call fragments are retained by upstream index and emitted as
/// complete, sequential Anthropic blocks in first-seen order at termination.
/// Call [`Self::start`] before consuming transport bytes; it is idempotent, and
/// [`Self::push`] / [`Self::finish`] also emit it if the caller has not.
#[derive(Debug)]
pub struct MessagesStreamTranslator {
    decoder: SseDecoder,
    limits: AnthropicStreamLimits,
    model: String,
    message_id: String,
    input_tokens: u64,
    output_tokens: u64,
    usage_seen: bool,
    completion_chars: u64,
    finish_reason: Option<String>,
    next_index: usize,
    current_text_index: Option<usize>,
    tool_positions: BTreeMap<u64, usize>,
    tools: Vec<BufferedToolCall>,
    buffered_tool_bytes: usize,
    started: bool,
    terminal: bool,
}

impl MessagesStreamTranslator {
    /// Construct a translator with production defaults.
    #[must_use]
    pub fn new(model: impl Into<String>, message_id: impl Into<String>, input_tokens: u64) -> Self {
        Self {
            decoder: SseDecoder::default(),
            limits: AnthropicStreamLimits::default(),
            model: model.into(),
            message_id: message_id.into(),
            input_tokens,
            output_tokens: 0,
            usage_seen: false,
            completion_chars: 0,
            finish_reason: None,
            next_index: 0,
            current_text_index: None,
            tool_positions: BTreeMap::new(),
            tools: Vec::new(),
            buffered_tool_bytes: 0,
            started: false,
            terminal: false,
        }
    }

    /// Construct a translator with explicit parser and accumulator bounds.
    pub fn with_limits(
        model: impl Into<String>,
        message_id: impl Into<String>,
        input_tokens: u64,
        limits: AnthropicStreamLimits,
    ) -> Result<Self, AnthropicStreamError> {
        if limits.max_line_bytes == 0
            || limits.max_event_bytes == 0
            || limits.max_tool_calls == 0
            || limits.max_tool_bytes == 0
        {
            return Err(AnthropicStreamError::InvalidLimits);
        }
        let decoder = SseDecoder::new(limits.max_line_bytes, limits.max_event_bytes)?;
        Ok(Self {
            decoder,
            limits,
            model: model.into(),
            message_id: message_id.into(),
            input_tokens,
            output_tokens: 0,
            usage_seen: false,
            completion_chars: 0,
            finish_reason: None,
            next_index: 0,
            current_text_index: None,
            tool_positions: BTreeMap::new(),
            tools: Vec::new(),
            buffered_tool_bytes: 0,
            started: false,
            terminal: false,
        })
    }

    /// Emit the single mandatory `message_start` frame.
    pub fn start(&mut self) -> Vec<Vec<u8>> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        vec![message_start_frame(
            &self.message_id,
            &self.model,
            self.input_tokens,
        )]
    }

    /// Feed one arbitrary transport fragment and return completed output frames.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, AnthropicStreamError> {
        let mut output = self.start();
        if self.terminal {
            return Ok(output);
        }
        let events = self.decoder.push(chunk)?;
        self.process_events(events, &mut output)?;
        Ok(output)
    }

    /// Finish a transport, including an unterminated final SSE field/event.
    pub fn finish(&mut self) -> Result<Vec<Vec<u8>>, AnthropicStreamError> {
        let mut output = self.start();
        if self.terminal {
            return Ok(output);
        }
        let events = self.decoder.finish()?;
        self.process_events(events, &mut output)?;
        if !self.terminal {
            self.finish_message(&mut output);
        }
        Ok(output)
    }

    /// Whether `[DONE]`, an upstream error, or transport completion terminated the stream.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.terminal
    }

    fn process_events(
        &mut self,
        events: Vec<SseEvent>,
        output: &mut Vec<Vec<u8>>,
    ) -> Result<(), AnthropicStreamError> {
        for event in events {
            if self.terminal {
                break;
            }
            let data = event.data.trim();
            if data == "[DONE]" {
                self.finish_message(output);
                break;
            }
            let Ok(payload) = serde_json::from_str::<Value>(data) else {
                if event.event == "error" {
                    self.finish_error("api_error", data, output);
                }
                continue;
            };
            if let Some((error_type, message)) = stream_error(&event.event, &payload, data) {
                self.finish_error(&error_type, &message, output);
                break;
            }
            self.feed_payload(&payload, output)?;
        }
        Ok(())
    }

    fn feed_payload(
        &mut self,
        payload: &Value,
        output: &mut Vec<Vec<u8>>,
    ) -> Result<(), AnthropicStreamError> {
        let Some(payload) = payload.as_object() else {
            return Ok(());
        };
        if let Some(usage) = payload.get("usage").and_then(Value::as_object) {
            if let Some(tokens) = usage.get("completion_tokens").and_then(Value::as_u64) {
                self.output_tokens = tokens;
                self.usage_seen = true;
            }
            if let Some(tokens) = usage.get("prompt_tokens").and_then(Value::as_u64) {
                self.input_tokens = tokens;
            }
        }

        let Some(choices) = payload.get("choices").and_then(Value::as_array) else {
            return Ok(());
        };
        for choice in choices.iter().filter_map(Value::as_object) {
            if let Some(delta) = choice.get("delta").and_then(Value::as_object) {
                if let Some(text) = delta.get("content").and_then(Value::as_str) {
                    if !text.is_empty() {
                        let index = self.open_text(output);
                        self.completion_chars = self.completion_chars.saturating_add(
                            u64::try_from(text.chars().count()).unwrap_or(u64::MAX),
                        );
                        output.push(text_delta_frame(index, text));
                    }
                }
                if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    for tool_call in tool_calls.iter().filter_map(Value::as_object) {
                        self.feed_tool_call(tool_call)?;
                    }
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                if !reason.is_empty() {
                    self.finish_reason = Some(reason.to_owned());
                }
            }
        }
        Ok(())
    }

    fn open_text(&mut self, output: &mut Vec<Vec<u8>>) -> usize {
        if let Some(index) = self.current_text_index {
            return index;
        }
        let index = self.next_index;
        self.next_index = self.next_index.saturating_add(1);
        self.current_text_index = Some(index);
        output.push(text_start_frame(index));
        index
    }

    fn feed_tool_call(
        &mut self,
        tool_call: &Map<String, Value>,
    ) -> Result<(), AnthropicStreamError> {
        let upstream_index = tool_call.get("index").and_then(Value::as_u64).unwrap_or(0);
        let position = if let Some(position) = self.tool_positions.get(&upstream_index) {
            *position
        } else {
            if self.tools.len() >= self.limits.max_tool_calls {
                return Err(AnthropicStreamError::TooManyToolCalls {
                    limit: self.limits.max_tool_calls,
                });
            }
            let position = self.tools.len();
            self.tools.push(BufferedToolCall::default());
            self.tool_positions.insert(upstream_index, position);
            position
        };

        if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
            if !id.is_empty() {
                let old_len = self.tools[position].id.as_ref().map_or(0, String::len);
                self.replace_buffered_bytes(old_len, id.len())?;
                self.tools[position].id = Some(id.to_owned());
            }
        }
        let function = tool_call.get("function").and_then(Value::as_object);
        if let Some(name) = function
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
        {
            if !name.is_empty() {
                let old_len = self.tools[position].name.len();
                self.replace_buffered_bytes(old_len, name.len())?;
                self.tools[position].name = name.to_owned();
            }
        }
        if let Some(arguments) = function
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
        {
            if !arguments.is_empty() {
                self.add_buffered_bytes(arguments.len())?;
                self.tools[position].arguments.push_str(arguments);
                self.completion_chars = self
                    .completion_chars
                    .saturating_add(u64::try_from(arguments.chars().count()).unwrap_or(u64::MAX));
            }
        }
        Ok(())
    }

    fn replace_buffered_bytes(
        &mut self,
        old_len: usize,
        new_len: usize,
    ) -> Result<(), AnthropicStreamError> {
        let candidate = self
            .buffered_tool_bytes
            .saturating_sub(old_len)
            .saturating_add(new_len);
        if candidate > self.limits.max_tool_bytes {
            return Err(AnthropicStreamError::ToolDataTooLarge {
                limit: self.limits.max_tool_bytes,
            });
        }
        self.buffered_tool_bytes = candidate;
        Ok(())
    }

    fn add_buffered_bytes(&mut self, amount: usize) -> Result<(), AnthropicStreamError> {
        let candidate = self.buffered_tool_bytes.saturating_add(amount);
        if candidate > self.limits.max_tool_bytes {
            return Err(AnthropicStreamError::ToolDataTooLarge {
                limit: self.limits.max_tool_bytes,
            });
        }
        self.buffered_tool_bytes = candidate;
        Ok(())
    }

    fn finish_message(&mut self, output: &mut Vec<Vec<u8>>) {
        if self.terminal {
            return;
        }
        if let Some(index) = self.current_text_index.take() {
            output.push(block_stop_frame(index));
        }
        for tool_position in 0..self.tools.len() {
            let index = self.next_index;
            self.next_index = self.next_index.saturating_add(1);
            let fallback_id = format!("toolu_{index}");
            let tool = &self.tools[tool_position];
            output.push(tool_start_frame(
                index,
                tool.id.as_deref().unwrap_or(&fallback_id),
                &tool.name,
            ));
            if !tool.arguments.is_empty() {
                output.push(tool_delta_frame(index, &tool.arguments));
            }
            output.push(block_stop_frame(index));
        }
        if self.next_index == 0 {
            output.push(text_start_frame(0));
            output.push(block_stop_frame(0));
        }
        let output_tokens = if self.usage_seen {
            self.output_tokens
        } else if self.completion_chars == 0 {
            0
        } else {
            (self.completion_chars / 4).max(1)
        };
        output.push(message_delta_frame(
            stream_stop_reason(self.finish_reason.as_deref()),
            output_tokens,
        ));
        output.push(message_stop_frame());
        self.terminal = true;
        self.clear_buffered_tools();
    }

    fn finish_error(&mut self, error_type: &str, message: &str, output: &mut Vec<Vec<u8>>) {
        if self.terminal {
            return;
        }
        output.push(error_frame(error_type, message));
        self.terminal = true;
        self.current_text_index = None;
        self.clear_buffered_tools();
    }

    fn clear_buffered_tools(&mut self) {
        self.tool_positions.clear();
        self.tools.clear();
        self.buffered_tool_bytes = 0;
    }
}

/// Translate a finite iterable of arbitrarily fragmented OpenAI SSE bytes.
pub fn translate_openai_sse_fragments<I, B>(
    fragments: I,
    model: &str,
    message_id: &str,
    input_tokens: u64,
) -> Result<Vec<Vec<u8>>, AnthropicStreamError>
where
    I: IntoIterator<Item = B>,
    B: AsRef<[u8]>,
{
    let mut translator = MessagesStreamTranslator::new(model, message_id, input_tokens);
    let mut output = translator.start();
    for fragment in fragments {
        output.extend(translator.push(fragment.as_ref())?);
        if translator.is_finished() {
            break;
        }
    }
    output.extend(translator.finish()?);
    Ok(output)
}

fn stream_stop_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("length") => "max_tokens",
        Some("tool_calls" | "function_call") => "tool_use",
        _ => "end_turn",
    }
}

fn stream_error(event_type: &str, payload: &Value, raw: &str) -> Option<(String, String)> {
    let object = payload.as_object();
    // An explicit `"error": null` on an ordinary chunk is not an error.
    let nested = object
        .and_then(|object| object.get("error"))
        .filter(|error| !error.is_null());
    let is_error = nested.is_some()
        || event_type == "error"
        || object
            .and_then(|object| object.get("type"))
            .and_then(Value::as_str)
            == Some("error");
    if !is_error {
        return None;
    }
    let detail = nested.unwrap_or(payload);
    let detail_object = detail.as_object();
    let source_type = detail_object
        .and_then(|detail| detail.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("api_error");
    let error_type = match source_type {
        "invalid_request_error"
        | "authentication_error"
        | "permission_error"
        | "not_found_error"
        | "request_too_large"
        | "rate_limit_error"
        | "overloaded_error"
        | "api_error" => source_type,
        _ => "api_error",
    };
    let message = detail_object
        .and_then(|detail| detail.get("message"))
        .and_then(Value::as_str)
        .or_else(|| detail.as_str())
        .or_else(|| {
            object
                .and_then(|object| object.get("message"))
                .and_then(Value::as_str)
        })
        .unwrap_or(raw);
    Some((error_type.to_owned(), message.to_owned()))
}

fn quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

fn sse_frame(event: &str, data: String) -> Vec<u8> {
    let mut frame = String::with_capacity(event.len() + data.len() + 16);
    frame.push_str("event: ");
    frame.push_str(event);
    frame.push_str("\ndata: ");
    frame.push_str(&data);
    frame.push_str("\n\n");
    frame.into_bytes()
}

fn message_start_frame(message_id: &str, model: &str, input_tokens: u64) -> Vec<u8> {
    sse_frame(
        "message_start",
        format!(
            "{{\"type\":\"message_start\",\"message\":{{\"id\":{},\"type\":\"message\",\"role\":\"assistant\",\"model\":{},\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{{\"input_tokens\":{input_tokens},\"output_tokens\":0}}}}}}",
            quoted(message_id),
            quoted(model),
        ),
    )
}

fn text_start_frame(index: usize) -> Vec<u8> {
    sse_frame(
        "content_block_start",
        format!(
            "{{\"type\":\"content_block_start\",\"index\":{index},\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}"
        ),
    )
}

fn text_delta_frame(index: usize, text: &str) -> Vec<u8> {
    sse_frame(
        "content_block_delta",
        format!(
            "{{\"type\":\"content_block_delta\",\"index\":{index},\"delta\":{{\"type\":\"text_delta\",\"text\":{}}}}}",
            quoted(text),
        ),
    )
}

fn tool_start_frame(index: usize, id: &str, name: &str) -> Vec<u8> {
    sse_frame(
        "content_block_start",
        format!(
            "{{\"type\":\"content_block_start\",\"index\":{index},\"content_block\":{{\"type\":\"tool_use\",\"id\":{},\"name\":{},\"input\":{{}}}}}}",
            quoted(id),
            quoted(name),
        ),
    )
}

fn tool_delta_frame(index: usize, arguments: &str) -> Vec<u8> {
    sse_frame(
        "content_block_delta",
        format!(
            "{{\"type\":\"content_block_delta\",\"index\":{index},\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":{}}}}}",
            quoted(arguments),
        ),
    )
}

fn block_stop_frame(index: usize) -> Vec<u8> {
    sse_frame(
        "content_block_stop",
        format!("{{\"type\":\"content_block_stop\",\"index\":{index}}}"),
    )
}

fn message_delta_frame(reason: &str, output_tokens: u64) -> Vec<u8> {
    sse_frame(
        "message_delta",
        format!(
            "{{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":{},\"stop_sequence\":null}},\"usage\":{{\"output_tokens\":{output_tokens}}}}}",
            quoted(reason),
        ),
    )
}

fn message_stop_frame() -> Vec<u8> {
    sse_frame("message_stop", "{\"type\":\"message_stop\"}".to_owned())
}

fn error_frame(error_type: &str, message: &str) -> Vec<u8> {
    sse_frame(
        "error",
        format!(
            "{{\"type\":\"error\",\"error\":{{\"type\":{},\"message\":{}}}}}",
            quoted(error_type),
            quoted(message),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed_events(frames: &[Vec<u8>]) -> Result<Vec<(String, Value)>, String> {
        frames
            .iter()
            .map(|frame| {
                let text = std::str::from_utf8(frame).map_err(|error| error.to_string())?;
                let (event_line, data_line) = text
                    .split_once('\n')
                    .ok_or_else(|| "missing SSE data line".to_owned())?;
                let event = event_line
                    .strip_prefix("event: ")
                    .ok_or_else(|| "missing SSE event field".to_owned())?
                    .to_owned();
                let data = data_line
                    .strip_prefix("data: ")
                    .and_then(|line| line.strip_suffix("\n\n"))
                    .ok_or_else(|| "malformed SSE data field".to_owned())?;
                let value = serde_json::from_str(data).map_err(|error| error.to_string())?;
                Ok((event, value))
            })
            .collect()
    }

    fn event_names(events: &[(String, Value)]) -> Vec<&str> {
        events.iter().map(|(event, _)| event.as_str()).collect()
    }

    #[test]
    fn request_translation_covers_blocks_tools_and_sampling() {
        let output = anthropic_to_openai_request(&json!({
            "model": "claude-x", "system": [{"text": "a"}, {"text": "b"}],
            "messages": [
                {"role": "assistant", "content": [{"type": "tool_use", "id": "tu", "name": "search", "input": {"q": "café"}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu", "content": [{"text": "ok"}]}, {"type": "text", "text": "thanks"}]}
            ],
            "max_tokens": 64, "temperature": 0.5, "top_p": 0.9, "top_k": 10,
            "stop_sequences": ["STOP"], "stream": true,
            "tools": [{"name": "search", "description": "d", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "tool", "name": "search"}
        }));
        assert_eq!(
            output["messages"][0],
            json!({"role": "system", "content": "a\nb"})
        );
        assert_eq!(
            output["messages"][1]["tool_calls"][0]["function"]["arguments"],
            "{\"q\":\"caf\\u00e9\"}"
        );
        assert_eq!(
            output["messages"][2],
            json!({"role": "tool", "tool_call_id": "tu", "content": "ok"})
        );
        assert_eq!(
            output["messages"][3],
            json!({"role": "user", "content": "thanks"})
        );
        assert_eq!(
            output["tool_choice"],
            json!({"type": "function", "function": {"name": "search"}})
        );
        assert_eq!(output["stop"], json!(["STOP"]));
        assert_eq!(output["stream"], true);
        assert!(output.get("top_k").is_none());
    }

    #[test]
    fn response_translation_covers_text_tool_usage_and_errors() {
        let text = openai_to_anthropic_response(
            &json!({"choices": [{"message": {"content": "hello"}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 7, "completion_tokens": 2}}),
            "m",
            "i",
            "",
        );
        assert_eq!(text["content"], json!([{"type": "text", "text": "hello"}]));
        assert_eq!(
            text["usage"],
            json!({"input_tokens": 7, "output_tokens": 2})
        );
        let tool = openai_to_anthropic_response(
            &json!({"choices": [{"message": {"tool_calls": [{"id": "c", "function": {"name": "f", "arguments": "not json"}}]}, "finish_reason": "tool_calls"}]}),
            "m",
            "i",
            "yyyyyyyy",
        );
        assert_eq!(
            tool["content"],
            json!([{"type": "tool_use", "id": "c", "name": "f", "input": {}}])
        );
        assert_eq!(tool["stop_reason"], "tool_use");
        assert_eq!(tool["usage"]["input_tokens"], 2);
    }

    #[test]
    fn empty_response_has_required_block_and_error_map_matches() {
        let output = openai_to_anthropic_response(
            &json!({"choices": [{"message": {"content": ""}, "finish_reason": "length"}]}),
            "m",
            "i",
            "",
        );
        assert_eq!(output["content"], json!([{"type": "text", "text": ""}]));
        assert_eq!(output["stop_reason"], "max_tokens");
        assert_eq!(
            anthropic_error(429, "slow")["error"]["type"],
            "rate_limit_error"
        );
        assert_eq!(
            anthropic_error(402, "broke")["error"]["type"],
            "invalid_request_error"
        );
        assert_eq!(anthropic_error(500, "boom")["error"]["type"], "api_error");
    }

    #[test]
    fn request_tool_arguments_match_python_order_escaping_and_exponents() -> Result<(), String> {
        let body: Value = serde_json::from_str(
            r#"{"messages":[{"role":"assistant","content":[{"type":"tool_use","id":"x","name":"f","input":{"z":1,"a":2,"f":1e-6,"d":"\u007f"}}]}]}"#,
        )
        .map_err(|error| error.to_string())?;
        let output = anthropic_to_openai_request(&body);
        assert_eq!(
            output["messages"][0]["tool_calls"][0]["function"]["arguments"],
            r#"{"z":1,"a":2,"f":1e-06,"d":"\u007f"}"#
        );
        Ok(())
    }

    #[test]
    fn buffered_tool_arguments_preserve_large_integers_and_malformed_fallback() -> Result<(), String>
    {
        let response: Value = serde_json::from_str(
            r#"{"choices":[{"message":{"tool_calls":[{"function":{"arguments":"{\"id\":123456789012345678901234567890}"}},{"function":{"arguments":"not json"}}]}}]}"#,
        )
        .map_err(|error| error.to_string())?;
        let output = openai_to_anthropic_response(&response, "m", "i", "");
        assert_eq!(
            output["content"][0]["input"]["id"].to_string(),
            "123456789012345678901234567890"
        );
        assert_eq!(output["content"][1]["input"], json!({}));
        Ok(())
    }

    #[test]
    fn fragmented_text_stream_has_exact_anthropic_sequence_and_usage() -> Result<(), String> {
        let frames = translate_openai_sse_fragments(
            [
                &b"data: {\"choices\":[{\"delta\":{\"content\":\"He"[..],
                &b"l\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n"[..],
                &b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"completion_tokens\":1}}\n\ndata: [DONE]\n\n"[..],
            ],
            "m",
            "msg_1",
            3,
        )
        .map_err(|error| error.to_string())?;
        let events = parsed_events(&frames)?;
        assert_eq!(
            event_names(&events),
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(events[0].1["message"]["id"], "msg_1");
        assert_eq!(
            events[2].1["delta"],
            json!({"type": "text_delta", "text": "Hel"})
        );
        assert_eq!(events[3].1["delta"]["text"], "lo");
        assert_eq!(events[5].1["delta"]["stop_reason"], "end_turn");
        assert_eq!(events[5].1["usage"]["output_tokens"], 1);
        Ok(())
    }

    #[test]
    fn parallel_tool_calls_flush_complete_in_first_seen_order() -> Result<(), String> {
        let frames = translate_openai_sse_fragments(
            [
                &b"data: {\"choices\":[{\"delta\":{\"content\":\"thinking\"}}]}\n\n"[..],
                &b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":8,\"id\":\"call_a\",\"function\":{\"name\":\"alpha\",\"arguments\":\"{\\\"x\\\":\"}}]}}]}\n\n"[..],
                &b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":3,\"id\":\"call_b\",\"function\":{\"name\":\"beta\",\"arguments\":\"{\\\"y\\\":\"}}]}}]}\n\n"[..],
                &b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":8,\"function\":{\"arguments\":\"1}\"}},{\"index\":3,\"function\":{\"arguments\":\"2}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n"[..],
            ],
            "m",
            "i",
            0,
        )
        .map_err(|error| error.to_string())?;
        let events = parsed_events(&frames)?;
        let starts: Vec<&Value> = events
            .iter()
            .filter(|(event, data)| {
                event == "content_block_start" && data["content_block"]["type"] == "tool_use"
            })
            .map(|(_, data)| data)
            .collect();
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0]["content_block"]["id"], "call_a");
        assert_eq!(starts[1]["content_block"]["id"], "call_b");
        let tool_deltas: Vec<&Value> = events
            .iter()
            .filter(|(_, data)| data["delta"]["type"] == "input_json_delta")
            .map(|(_, data)| data)
            .collect();
        assert_eq!(tool_deltas[0]["delta"]["partial_json"], r#"{"x":1}"#);
        assert_eq!(tool_deltas[1]["delta"]["partial_json"], r#"{"y":2}"#);
        let text_stop = events
            .iter()
            .position(|(event, _)| event == "content_block_stop")
            .ok_or_else(|| "missing text stop".to_owned())?;
        let first_tool = events
            .iter()
            .position(|(_, data)| data["content_block"]["type"] == "tool_use")
            .ok_or_else(|| "missing tool start".to_owned())?;
        assert!(text_stop < first_tool);
        Ok(())
    }

    #[test]
    fn empty_done_and_missing_done_both_close_normally() -> Result<(), String> {
        let empty = translate_openai_sse_fragments([&b"data: [DONE]\n\n"[..]], "m", "i", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            event_names(&parsed_events(&empty)?),
            [
                "message_start",
                "content_block_start",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );

        let missing_done = translate_openai_sse_fragments(
            [&b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n"[..]],
            "m",
            "i",
            0,
        )
        .map_err(|error| error.to_string())?;
        let events = parsed_events(&missing_done)?;
        assert_eq!(
            event_names(&events)[events.len() - 2..],
            ["message_delta", "message_stop"]
        );
        Ok(())
    }

    #[test]
    fn upstream_error_is_a_terminal_anthropic_error_event() -> Result<(), String> {
        let frames = translate_openai_sse_fragments(
            [
                &b"data: {\"error\":{\"message\":\"connection refused\",\"type\":\"wayfinder_router_upstream_error\"}}\n\n"[..],
                &b"data: [DONE]\n\n"[..],
            ],
            "m",
            "i",
            0,
        )
        .map_err(|error| error.to_string())?;
        let events = parsed_events(&frames)?;
        assert_eq!(event_names(&events), ["message_start", "error"]);
        assert_eq!(events[1].1["error"]["type"], "api_error");
        assert_eq!(events[1].1["error"]["message"], "connection refused");
        Ok(())
    }

    #[test]
    fn null_error_field_on_a_content_chunk_is_not_an_error() -> Result<(), String> {
        let frames = translate_openai_sse_fragments(
            [
                &b"data: {\"error\":null,\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n"[..],
                &b"data: {\"error\":null,\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n"[..],
                &b"data: [DONE]\n\n"[..],
            ],
            "m",
            "i",
            0,
        )
        .map_err(|error| error.to_string())?;
        let events = parsed_events(&frames)?;
        let names = event_names(&events);
        assert!(
            !names.contains(&"error"),
            "unexpected error event: {names:?}"
        );
        assert_eq!(names.last().copied(), Some("message_stop"));
        Ok(())
    }

    #[test]
    fn stream_tool_accumulator_bounds_are_enforced() -> Result<(), String> {
        let limits = AnthropicStreamLimits {
            max_line_bytes: 1_024,
            max_event_bytes: 1_024,
            max_tool_calls: 1,
            max_tool_bytes: 8,
        };
        let mut translator = MessagesStreamTranslator::with_limits("m", "i", 0, limits)
            .map_err(|error| error.to_string())?;
        let error = translator
            .push(b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0},{\"index\":1}]}}]}\n\n")
            .err();
        assert_eq!(
            error,
            Some(AnthropicStreamError::TooManyToolCalls { limit: 1 })
        );
        Ok(())
    }
}
