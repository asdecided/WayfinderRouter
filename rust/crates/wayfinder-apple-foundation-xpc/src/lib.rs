//! Bounded client for Wayfinder's Apple Foundation Models XPC service.
//!
//! Native XPC and Objective-C are confined to `xpc_bridge.m` and the small C
//! ABI module below. Public errors contain only stable classes and never retain
//! request or response content.

use std::borrow::Cow;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SERVICE_NAME: &str = "com.wayfinder.FoundationModelBroker";
pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_REQUEST_ID_BYTES: usize = 128;
pub const MAX_ENCODED_REQUEST_BYTES: usize = 1_048_576;
pub const MAX_INSTRUCTIONS_BYTES: usize = 16_384;
pub const MAX_MESSAGES: usize = 64;
pub const MAX_MESSAGE_BYTES: usize = 262_144;
pub const MAX_RESPONSE_BYTES: usize = 524_288;
pub const MAX_CHUNK_BYTES: usize = 65_536;
pub const MAX_QUEUED_CHUNKS: usize = 32;
pub const MAX_TIMEOUT: Duration = Duration::from_secs(120);
pub const DEFAULT_CONTROL_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Availability {
    Available,
    DeviceNotEligible,
    AppleIntelligenceNotEnabled,
    ModelNotReady,
    Unsupported,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerateRequest {
    pub request_id: String,
    pub instructions: Option<String>,
    pub messages: Vec<Message>,
    pub timeout: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerateResponse {
    pub request_id: String,
    pub content: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum StreamEventKind {
    Chunk,
    Terminal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StreamEvent {
    protocol_version: u32,
    #[serde(rename = "requestID")]
    pub request_id: String,
    pub sequence: usize,
    pub kind: StreamEventKind,
    pub content: Option<String>,
}

impl StreamEvent {
    /// Construct one current-protocol chunk for deterministic service adapters and tests.
    #[must_use]
    pub fn chunk(
        request_id: impl Into<String>,
        sequence: usize,
        content: impl Into<String>,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id.into(),
            sequence,
            kind: StreamEventKind::Chunk,
            content: Some(content.into()),
        }
    }

    /// Construct one current-protocol terminal marker.
    #[must_use]
    pub fn terminal(request_id: impl Into<String>, sequence: usize) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id.into(),
            sequence,
            kind: StreamEventKind::Terminal,
            content: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum FoundationModelsXpcError {
    #[error("Foundation Models XPC is unavailable on this platform")]
    UnsupportedPlatform,
    #[error("Foundation Models XPC request is invalid")]
    InvalidRequest,
    #[error("Foundation Models XPC request exceeded its bound")]
    RequestTooLarge,
    #[error("Foundation Models XPC response is malformed")]
    MalformedResponse,
    #[error("Foundation Models XPC response exceeded its bound")]
    ResponseTooLarge,
    #[error("Foundation Models XPC protocol version is unsupported")]
    UnsupportedVersion,
    #[error("Foundation Models are unavailable")]
    Unavailable,
    #[error("Foundation Models XPC request timed out")]
    TimedOut,
    #[error("Foundation Models XPC request was cancelled")]
    Cancelled,
    #[error("Foundation Models XPC caller was denied")]
    Denied,
    #[error("Foundation Models generation failed")]
    GenerationFailed,
    #[error("Foundation Models XPC stream was invalid")]
    InvalidStream,
}

pub trait FoundationModelsTransport: Send + Sync {
    fn availability(
        &self,
        request: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, FoundationModelsXpcError>;
    fn generate(
        &self,
        request: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, FoundationModelsXpcError>;
    fn stream(
        &self,
        request: &[u8],
        timeout: Duration,
        on_event: &mut dyn FnMut(&[u8]) -> Result<(), FoundationModelsXpcError>,
    ) -> Result<(), FoundationModelsXpcError>;
    fn cancel(&self, request_id: &str, timeout: Duration) -> Result<(), FoundationModelsXpcError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemFoundationModelsTransport;

impl FoundationModelsTransport for SystemFoundationModelsTransport {
    fn availability(
        &self,
        request: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, FoundationModelsXpcError> {
        platform::request_reply(platform::Operation::Availability, request, timeout)
    }

    fn generate(
        &self,
        request: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, FoundationModelsXpcError> {
        platform::request_reply(platform::Operation::Generate, request, timeout)
    }

    fn stream(
        &self,
        request: &[u8],
        timeout: Duration,
        on_event: &mut dyn FnMut(&[u8]) -> Result<(), FoundationModelsXpcError>,
    ) -> Result<(), FoundationModelsXpcError> {
        platform::stream(request, timeout, on_event)
    }

    fn cancel(&self, request_id: &str, timeout: Duration) -> Result<(), FoundationModelsXpcError> {
        platform::cancel(request_id, timeout)
    }
}

#[derive(Debug)]
pub struct FoundationModelsClient<T = SystemFoundationModelsTransport> {
    transport: T,
}

impl Default for FoundationModelsClient<SystemFoundationModelsTransport> {
    fn default() -> Self {
        Self {
            transport: SystemFoundationModelsTransport,
        }
    }
}

impl<T: FoundationModelsTransport> FoundationModelsClient<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    pub fn availability(&self, request_id: &str) -> Result<Availability, FoundationModelsXpcError> {
        validate_request_id(request_id)?;
        let request = encode(&AvailabilityRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id,
        })?;
        let reply = self
            .transport
            .availability(&request, DEFAULT_CONTROL_TIMEOUT)?;
        let response: AvailabilityResponse<'_> = decode(&reply)?;
        validate_response_envelope(response.protocol_version, &response.request_id, request_id)?;
        Ok(response.availability)
    }

    pub fn generate(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateResponse, FoundationModelsXpcError> {
        validate_generate_request(request)?;
        let timeout_milliseconds = duration_milliseconds(request.timeout)?;
        let wire = GenerateRequestWire {
            protocol_version: PROTOCOL_VERSION,
            request_id: &request.request_id,
            instructions: request.instructions.as_deref(),
            messages: &request.messages,
            timeout_milliseconds,
        };
        let encoded = encode(&wire)?;
        let reply = self.transport.generate(&encoded, request.timeout)?;
        let response: GenerateResponseWire<'_> = decode(&reply)?;
        validate_response_envelope(
            response.protocol_version,
            &response.request_id,
            &request.request_id,
        )?;
        if response.content.len() > MAX_RESPONSE_BYTES {
            return Err(FoundationModelsXpcError::ResponseTooLarge);
        }
        Ok(GenerateResponse {
            request_id: response.request_id.into_owned(),
            content: response.content.into_owned(),
        })
    }

    pub fn stream(
        &self,
        request: &GenerateRequest,
        mut on_event: impl FnMut(StreamEvent) -> Result<(), FoundationModelsXpcError>,
    ) -> Result<(), FoundationModelsXpcError> {
        validate_generate_request(request)?;
        let encoded = encode(&GenerateRequestWire {
            protocol_version: PROTOCOL_VERSION,
            request_id: &request.request_id,
            instructions: request.instructions.as_deref(),
            messages: &request.messages,
            timeout_milliseconds: duration_milliseconds(request.timeout)?,
        })?;
        let mut expected_sequence = 0_usize;
        let mut accumulated_bytes = 0_usize;
        let mut event_count = 0_usize;
        let mut terminal_seen = false;
        self.transport
            .stream(&encoded, request.timeout, &mut |bytes| {
                let event: StreamEvent = decode(bytes)?;
                validate_response_envelope(
                    event.protocol_version,
                    &event.request_id,
                    &request.request_id,
                )?;
                if terminal_seen || event.sequence != expected_sequence {
                    return Err(FoundationModelsXpcError::InvalidStream);
                }
                event_count += 1;
                if event_count > MAX_QUEUED_CHUNKS {
                    return Err(FoundationModelsXpcError::InvalidStream);
                }
                match event.kind {
                    StreamEventKind::Chunk => {
                        let content = event
                            .content
                            .as_deref()
                            .ok_or(FoundationModelsXpcError::InvalidStream)?;
                        if content.len() > MAX_CHUNK_BYTES {
                            return Err(FoundationModelsXpcError::ResponseTooLarge);
                        }
                        accumulated_bytes = accumulated_bytes
                            .checked_add(content.len())
                            .ok_or(FoundationModelsXpcError::ResponseTooLarge)?;
                        if accumulated_bytes > MAX_RESPONSE_BYTES {
                            return Err(FoundationModelsXpcError::ResponseTooLarge);
                        }
                    }
                    StreamEventKind::Terminal => {
                        if event.content.is_some() {
                            return Err(FoundationModelsXpcError::InvalidStream);
                        }
                        terminal_seen = true;
                    }
                }
                expected_sequence += 1;
                on_event(event)
            })?;
        if !terminal_seen {
            return Err(FoundationModelsXpcError::InvalidStream);
        }
        Ok(())
    }

    pub fn cancel(&self, request_id: &str) -> Result<(), FoundationModelsXpcError> {
        validate_request_id(request_id)?;
        self.transport.cancel(request_id, DEFAULT_CONTROL_TIMEOUT)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AvailabilityRequest<'a> {
    protocol_version: u32,
    #[serde(rename = "requestID")]
    request_id: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AvailabilityResponse<'a> {
    protocol_version: u32,
    // `Cow` rather than `&str`: a JSON string containing an escape sequence
    // cannot be borrowed from the input, and the broker's encoder escapes.
    #[serde(rename = "requestID", borrow)]
    request_id: Cow<'a, str>,
    availability: Availability,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerateRequestWire<'a> {
    protocol_version: u32,
    #[serde(rename = "requestID")]
    request_id: &'a str,
    instructions: Option<&'a str>,
    messages: &'a [Message],
    timeout_milliseconds: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateResponseWire<'a> {
    protocol_version: u32,
    #[serde(rename = "requestID", borrow)]
    request_id: Cow<'a, str>,
    // Model output routinely contains newlines and quotes, which arrive
    // escaped and therefore cannot be borrowed.
    #[serde(borrow)]
    content: Cow<'a, str>,
}

fn validate_request_id(request_id: &str) -> Result<(), FoundationModelsXpcError> {
    if request_id.is_empty() || request_id.len() > MAX_REQUEST_ID_BYTES || request_id.contains('\0')
    {
        return Err(FoundationModelsXpcError::InvalidRequest);
    }
    Ok(())
}

fn validate_generate_request(request: &GenerateRequest) -> Result<(), FoundationModelsXpcError> {
    validate_request_id(&request.request_id)?;
    if request
        .instructions
        .as_ref()
        .is_some_and(|value| value.len() > MAX_INSTRUCTIONS_BYTES)
        || request.messages.is_empty()
        || request.messages.len() > MAX_MESSAGES
        || request
            .messages
            .iter()
            .any(|message| message.content.is_empty() || message.content.len() > MAX_MESSAGE_BYTES)
    {
        return Err(FoundationModelsXpcError::InvalidRequest);
    }
    duration_milliseconds(request.timeout)?;
    Ok(())
}

fn duration_milliseconds(timeout: Duration) -> Result<u64, FoundationModelsXpcError> {
    if timeout.is_zero() || timeout > MAX_TIMEOUT || timeout.subsec_nanos() % 1_000_000 != 0 {
        return Err(FoundationModelsXpcError::InvalidRequest);
    }
    u64::try_from(timeout.as_millis()).map_err(|_| FoundationModelsXpcError::InvalidRequest)
}

fn validate_response_envelope(
    version: u32,
    actual_id: &str,
    expected_id: &str,
) -> Result<(), FoundationModelsXpcError> {
    if version != PROTOCOL_VERSION {
        return Err(FoundationModelsXpcError::UnsupportedVersion);
    }
    if actual_id != expected_id {
        return Err(FoundationModelsXpcError::MalformedResponse);
    }
    Ok(())
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, FoundationModelsXpcError> {
    let bytes = serde_json::to_vec(value).map_err(|_| FoundationModelsXpcError::InvalidRequest)?;
    if bytes.len() > MAX_ENCODED_REQUEST_BYTES {
        return Err(FoundationModelsXpcError::RequestTooLarge);
    }
    Ok(bytes)
}

fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, FoundationModelsXpcError> {
    if bytes.len() > MAX_ENCODED_REQUEST_BYTES {
        return Err(FoundationModelsXpcError::ResponseTooLarge);
    }
    serde_json::from_slice(bytes).map_err(|_| FoundationModelsXpcError::MalformedResponse)
}

#[cfg(any(target_os = "macos", test))]
fn map_native_status(status: i32) -> Result<(), FoundationModelsXpcError> {
    match status {
        0 => Ok(()),
        1 => Err(FoundationModelsXpcError::TimedOut),
        2 => Err(FoundationModelsXpcError::Unavailable),
        3 => Err(FoundationModelsXpcError::Denied),
        4 => Err(FoundationModelsXpcError::RequestTooLarge),
        5 => Err(FoundationModelsXpcError::ResponseTooLarge),
        6 => Err(FoundationModelsXpcError::UnsupportedVersion),
        7 => Err(FoundationModelsXpcError::Cancelled),
        8 => Err(FoundationModelsXpcError::GenerationFailed),
        _ => Err(FoundationModelsXpcError::Unavailable),
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{CString, c_char, c_double, c_int, c_void};
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::*;

    #[repr(i32)]
    pub(super) enum Operation {
        Availability = 0,
        Generate = 1,
    }

    type EventCallback = unsafe extern "C" fn(*mut c_void, *const u8, usize) -> c_int;

    unsafe extern "C" {
        fn wayfinder_foundation_xpc_request(
            operation: c_int,
            request: *const u8,
            request_length: usize,
            output: *mut u8,
            output_capacity: usize,
            output_length: *mut usize,
            timeout_seconds: c_double,
        ) -> c_int;
        fn wayfinder_foundation_xpc_stream(
            request: *const u8,
            request_length: usize,
            callback: EventCallback,
            context: *mut c_void,
            timeout_seconds: c_double,
        ) -> c_int;
        fn wayfinder_foundation_xpc_cancel(
            request_id: *const c_char,
            timeout_seconds: c_double,
        ) -> c_int;
    }

    pub(super) fn request_reply(
        operation: Operation,
        request: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, FoundationModelsXpcError> {
        let mut output = vec![0_u8; MAX_ENCODED_REQUEST_BYTES];
        let mut output_length = 0_usize;
        // SAFETY: all pointers reference allocations of the stated lengths for
        // the duration of the synchronous C call.
        let status = unsafe {
            wayfinder_foundation_xpc_request(
                operation as c_int,
                request.as_ptr(),
                request.len(),
                output.as_mut_ptr(),
                output.len(),
                &mut output_length,
                timeout.as_secs_f64(),
            )
        };
        map_native_status(status)?;
        if output_length > output.len() {
            return Err(FoundationModelsXpcError::ResponseTooLarge);
        }
        output.truncate(output_length);
        Ok(output)
    }

    pub(super) fn stream(
        request: &[u8],
        timeout: Duration,
        on_event: &mut dyn FnMut(&[u8]) -> Result<(), FoundationModelsXpcError>,
    ) -> Result<(), FoundationModelsXpcError> {
        struct Context<'a> {
            callback: &'a mut dyn FnMut(&[u8]) -> Result<(), FoundationModelsXpcError>,
            error: Option<FoundationModelsXpcError>,
        }
        unsafe extern "C" fn receive(
            context: *mut c_void,
            bytes: *const u8,
            length: usize,
        ) -> c_int {
            if context.is_null() || bytes.is_null() || length > MAX_ENCODED_REQUEST_BYTES {
                return 1;
            }
            // SAFETY: the bridge keeps the NSData backing bytes alive for this callback.
            let data = unsafe { std::slice::from_raw_parts(bytes, length) };
            // SAFETY: context points to the stack value below for the entire synchronous call.
            let context = unsafe { &mut *(context.cast::<Context<'_>>()) };
            match catch_unwind(AssertUnwindSafe(|| (context.callback)(data))) {
                Ok(Ok(())) => 0,
                Ok(Err(error)) => {
                    context.error = Some(error);
                    1
                }
                Err(_) => {
                    context.error = Some(FoundationModelsXpcError::InvalidStream);
                    1
                }
            }
        }
        let mut context = Context {
            callback: on_event,
            error: None,
        };
        // SAFETY: request and context remain alive until the bridge returns.
        let status = unsafe {
            wayfinder_foundation_xpc_stream(
                request.as_ptr(),
                request.len(),
                receive,
                (&mut context as *mut Context<'_>).cast(),
                timeout.as_secs_f64(),
            )
        };
        if let Some(error) = context.error {
            return Err(error);
        }
        map_native_status(status)
    }

    pub(super) fn cancel(
        request_id: &str,
        timeout: Duration,
    ) -> Result<(), FoundationModelsXpcError> {
        let request_id =
            CString::new(request_id).map_err(|_| FoundationModelsXpcError::InvalidRequest)?;
        // SAFETY: request_id is NUL-terminated and alive for the synchronous call.
        map_native_status(unsafe {
            wayfinder_foundation_xpc_cancel(request_id.as_ptr(), timeout.as_secs_f64())
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;
    pub(super) enum Operation {
        Availability,
        Generate,
    }
    pub(super) fn request_reply(
        _: Operation,
        _: &[u8],
        _: Duration,
    ) -> Result<Vec<u8>, FoundationModelsXpcError> {
        Err(FoundationModelsXpcError::UnsupportedPlatform)
    }
    pub(super) fn stream(
        _: &[u8],
        _: Duration,
        _: &mut dyn FnMut(&[u8]) -> Result<(), FoundationModelsXpcError>,
    ) -> Result<(), FoundationModelsXpcError> {
        Err(FoundationModelsXpcError::UnsupportedPlatform)
    }
    pub(super) fn cancel(_: &str, _: Duration) -> Result<(), FoundationModelsXpcError> {
        Err(FoundationModelsXpcError::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakeTransport {
        reply: Mutex<Vec<u8>>,
        events: Mutex<Vec<Vec<u8>>>,
    }
    impl FakeTransport {
        fn with_reply(reply: &str) -> Self {
            Self {
                reply: Mutex::new(reply.as_bytes().to_vec()),
                events: Mutex::default(),
            }
        }
        fn with_events(events: &[&str]) -> Self {
            Self {
                reply: Mutex::default(),
                events: Mutex::new(
                    events
                        .iter()
                        .map(|value| value.as_bytes().to_vec())
                        .collect(),
                ),
            }
        }
    }
    impl FoundationModelsTransport for FakeTransport {
        fn availability(&self, _: &[u8], _: Duration) -> Result<Vec<u8>, FoundationModelsXpcError> {
            self.reply
                .lock()
                .map_err(|_| FoundationModelsXpcError::Unavailable)
                .map(|v| v.clone())
        }
        fn generate(&self, _: &[u8], _: Duration) -> Result<Vec<u8>, FoundationModelsXpcError> {
            self.availability(&[], Duration::ZERO)
        }
        fn stream(
            &self,
            _: &[u8],
            _: Duration,
            callback: &mut dyn FnMut(&[u8]) -> Result<(), FoundationModelsXpcError>,
        ) -> Result<(), FoundationModelsXpcError> {
            for event in self
                .events
                .lock()
                .map_err(|_| FoundationModelsXpcError::Unavailable)?
                .iter()
            {
                callback(event)?;
            }
            Ok(())
        }
        fn cancel(&self, _: &str, _: Duration) -> Result<(), FoundationModelsXpcError> {
            Ok(())
        }
    }

    fn request() -> GenerateRequest {
        GenerateRequest {
            request_id: "opaque-1".into(),
            instructions: Some("Be brief".into()),
            messages: vec![Message {
                role: MessageRole::User,
                content: "hello".into(),
            }],
            timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn availability_and_generate_validate_response_envelopes() {
        let client = FoundationModelsClient::new(FakeTransport::with_reply(
            r#"{"protocolVersion":1,"requestID":"opaque-1","availability":"model-not-ready"}"#,
        ));
        assert_eq!(
            client.availability("opaque-1"),
            Ok(Availability::ModelNotReady)
        );
        let client = FoundationModelsClient::new(FakeTransport::with_reply(
            r#"{"protocolVersion":1,"requestID":"opaque-1","content":"answer"}"#,
        ));
        assert_eq!(
            client.generate(&request()).map(|response| response.content),
            Ok("answer".into())
        );
    }

    #[test]
    fn generate_decodes_escaped_content_from_the_broker() {
        // Foundation's JSONEncoder escapes newlines, quotes, backslashes and
        // forward slashes, so real model output never arrives escape-free.
        let client = FoundationModelsClient::new(FakeTransport::with_reply(
            r#"{"protocolVersion":1,"requestID":"opaque-1","content":"line one\nsaid \"hi\" a\/b \\ end"}"#,
        ));
        assert_eq!(
            client.generate(&request()).map(|response| response.content),
            Ok("line one\nsaid \"hi\" a/b \\ end".into())
        );
    }

    #[test]
    fn requests_use_swifts_stable_acronym_spelling() {
        let bytes = encode(&AvailabilityRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: "opaque-1",
        })
        .map_err(|error| error.to_string());
        let rendered =
            bytes.and_then(|bytes| String::from_utf8(bytes).map_err(|error| error.to_string()));
        assert_eq!(
            rendered,
            Ok(r#"{"protocolVersion":1,"requestID":"opaque-1"}"#.into())
        );
    }

    #[test]
    fn request_and_timeout_bounds_fail_before_transport() {
        let client = FoundationModelsClient::new(FakeTransport::default());
        for request_id in ["", &"x".repeat(MAX_REQUEST_ID_BYTES + 1)] {
            assert_eq!(
                client.availability(request_id),
                Err(FoundationModelsXpcError::InvalidRequest)
            );
        }
        let mut invalid = request();
        invalid.messages.clear();
        assert_eq!(
            client.generate(&invalid),
            Err(FoundationModelsXpcError::InvalidRequest)
        );
        invalid = request();
        invalid.timeout = MAX_TIMEOUT + Duration::from_millis(1);
        assert_eq!(
            client.generate(&invalid),
            Err(FoundationModelsXpcError::InvalidRequest)
        );
    }

    #[test]
    fn stream_requires_ordered_events_and_one_terminal() {
        let good = FakeTransport::with_events(&[
            r#"{"protocolVersion":1,"requestID":"opaque-1","sequence":0,"kind":"chunk","content":"a"}"#,
            r#"{"protocolVersion":1,"requestID":"opaque-1","sequence":1,"kind":"terminal"}"#,
        ]);
        let client = FoundationModelsClient::new(good);
        let mut kinds = Vec::new();
        assert_eq!(
            client.stream(&request(), |event| {
                kinds.push(event.kind);
                Ok(())
            }),
            Ok(())
        );
        assert_eq!(
            kinds,
            vec![StreamEventKind::Chunk, StreamEventKind::Terminal]
        );

        let missing = FoundationModelsClient::new(FakeTransport::with_events(&[
            r#"{"protocolVersion":1,"requestID":"opaque-1","sequence":0,"kind":"chunk","content":"a"}"#,
        ]));
        assert_eq!(
            missing.stream(&request(), |_| Ok(())),
            Err(FoundationModelsXpcError::InvalidStream)
        );
    }

    #[test]
    fn errors_do_not_retain_request_or_response_content() {
        let marker = "private-prompt-marker";
        for error in [
            FoundationModelsXpcError::InvalidRequest,
            FoundationModelsXpcError::MalformedResponse,
            FoundationModelsXpcError::GenerationFailed,
            FoundationModelsXpcError::Denied,
        ] {
            assert!(!error.to_string().contains(marker));
            assert!(!format!("{error:?}").contains(marker));
        }
    }

    #[test]
    fn native_statuses_are_stable_and_sanitized() {
        assert_eq!(
            map_native_status(1),
            Err(FoundationModelsXpcError::TimedOut)
        );
        assert_eq!(map_native_status(3), Err(FoundationModelsXpcError::Denied));
        assert_eq!(
            map_native_status(7),
            Err(FoundationModelsXpcError::Cancelled)
        );
        assert_eq!(
            map_native_status(99),
            Err(FoundationModelsXpcError::Unavailable)
        );
    }
}
