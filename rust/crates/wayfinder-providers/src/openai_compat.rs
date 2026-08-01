//! Bounded OpenAI-compatible buffered provider transport.
//!
//! Configured provider origins may be local or hosted. Redirects and ambient
//! proxies are disabled so an authorization header cannot silently move to an
//! unconfigured destination. Streaming uses a separate explicit state machine.

use std::fmt;
use std::pin::Pin;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::{Stream, StreamExt};
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderMap, HeaderValue, StatusCode};
use reqwest::redirect::Policy;
use reqwest::{Client, Url};
use serde_json::Value;
use thiserror::Error;

/// Default upper bound for one buffered provider response (8 MiB).
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 8 * 1_024 * 1_024;
/// Default total provider request timeout.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Default provider TCP/TLS connection timeout.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Validated configured OpenAI-compatible endpoint.
#[derive(Clone, PartialEq, Eq)]
pub struct OpenAiEndpoint {
    chat_completions: Url,
}

impl OpenAiEndpoint {
    /// Validate a configured base URL and append Python's `/chat/completions` suffix.
    ///
    /// Userinfo, query strings, and fragments are rejected so secrets cannot be
    /// embedded in endpoint state and request credentials have one explicit origin.
    pub fn parse(base_url: &str) -> Result<Self, ProviderError> {
        if base_url.is_empty() || base_url.trim() != base_url {
            return Err(ProviderError::InvalidEndpoint);
        }
        let complete = format!("{}/chat/completions", base_url.trim_end_matches('/'));
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
        Ok(Self {
            chat_completions: url,
        })
    }

    /// Fully resolved chat-completions URL.
    #[must_use]
    pub fn chat_completions_url(&self) -> &Url {
        &self.chat_completions
    }
}

impl fmt::Debug for OpenAiEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiEndpoint")
            .field("scheme", &self.chat_completions.scheme())
            .field("host", &self.chat_completions.host_str())
            .field("port", &self.chat_completions.port())
            .finish_non_exhaustive()
    }
}

/// Ephemeral provider credential with redacted formatting and zeroed owned bytes.
///
/// This type intentionally implements neither `Clone` nor `Serialize`.
pub struct SecretValue {
    bytes: Vec<u8>,
}

impl SecretValue {
    /// Own a credential supplied by an authenticated process boundary.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            bytes: value.into().into_bytes(),
        }
    }

    /// Own opaque credential bytes returned by a secret broker or command seam.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Whether the supplied credential is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Credential byte length, safe for bounds tests and never the value.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Make one explicit owned copy for a single provider request.
    ///
    /// This type deliberately does not implement [`Clone`]: long-lived secret
    /// stores must opt in at the call site instead of acquiring accidental
    /// copies through generic derives or container operations.
    #[must_use]
    pub fn duplicate_for_request(&self) -> Self {
        Self {
            bytes: self.bytes.clone(),
        }
    }

    fn bearer_header(&self) -> Result<HeaderValue, ProviderError> {
        let mut bytes = Vec::with_capacity("Bearer ".len().saturating_add(self.bytes.len()));
        bytes.extend_from_slice(b"Bearer ");
        bytes.extend_from_slice(&self.bytes);
        let parsed = HeaderValue::from_bytes(&bytes);
        bytes.fill(0);
        let mut value = parsed.map_err(|_| ProviderError::InvalidCredential)?;
        value.set_sensitive(true);
        Ok(value)
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

/// Explicit transport bounds and deadlines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderClientConfig {
    /// Total request deadline, including body transfer.
    pub request_timeout: Duration,
    /// TCP/TLS connection deadline.
    pub connect_timeout: Duration,
    /// Maximum decoded buffered response bytes.
    pub max_response_bytes: usize,
}

impl Default for ProviderClientConfig {
    fn default() -> Self {
        Self {
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }
}

/// Bounded provider-client construction or delivery failure.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ProviderError {
    /// Endpoint is not an absolute HTTP(S) URL under the configured-origin policy.
    #[error(
        "provider endpoint must be an absolute HTTP(S) URL without userinfo, query, or fragment"
    )]
    InvalidEndpoint,
    /// A timeout or response bound was zero.
    #[error("provider timeouts and response bound must be positive")]
    InvalidClientConfig,
    /// Credential cannot be represented safely in an HTTP authorization header.
    #[error("provider credential cannot be represented as an HTTP header")]
    InvalidCredential,
    /// Reqwest could not construct the policy-constrained client.
    #[error("provider HTTP client could not be constructed")]
    ClientBuild,
    /// Connection, timeout, TLS, cancellation-adjacent, or body transport failure.
    #[error("provider transport failed")]
    Transport,
    /// Buffered response exceeded the configured upper bound.
    #[error("provider response exceeds {limit} bytes")]
    ResponseTooLarge {
        /// Active response bound.
        limit: usize,
    },
}

/// Provider response retained without request or credential metadata.
pub struct BufferedProviderResponse {
    status: StatusCode,
    content_type: String,
    body: Bytes,
}

/// Cancellable provider response body yielded incrementally.
pub type ProviderByteStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, ProviderError>> + Send + 'static>>;

/// One upstream streaming response whose body is never aggregated.
pub struct StreamingProviderResponse {
    status: StatusCode,
    content_type: String,
    stream: ProviderByteStream,
}

impl StreamingProviderResponse {
    /// Upstream status, retained for diagnostics and future policy checks.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    /// Upstream media type, defaulting to `text/event-stream` when absent.
    #[must_use]
    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    /// Consume the response and transfer its cancellable byte stream.
    #[must_use]
    pub fn into_stream(self) -> ProviderByteStream {
        self.stream
    }
}

impl fmt::Debug for StreamingProviderResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamingProviderResponse")
            .field("status", &self.status)
            .field("content_type", &self.content_type)
            .field("stream", &"<cancellable bytes>")
            .finish()
    }
}

impl BufferedProviderResponse {
    /// Upstream HTTP status, including ordinary provider 4xx/5xx values.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    /// Upstream content type, defaulting to `application/json` when absent/invalid.
    #[must_use]
    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    /// Raw bounded provider body for compatible relay/translation.
    #[must_use]
    pub fn body(&self) -> &Bytes {
        &self.body
    }

    /// Consume the response and transfer ownership of its body.
    #[must_use]
    pub fn into_body(self) -> Bytes {
        self.body
    }
}

impl fmt::Debug for BufferedProviderResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BufferedProviderResponse")
            .field("status", &self.status)
            .field("content_type", &self.content_type)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

/// Reusable provider client whose policies are fixed at construction.
#[derive(Clone)]
pub struct OpenAiProviderClient {
    client: Client,
    max_response_bytes: usize,
}

impl OpenAiProviderClient {
    /// Construct a client with redirects and ambient proxy discovery disabled.
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

    /// POST a buffered JSON request, preserving ordinary upstream HTTP statuses.
    ///
    /// Dropping this future drops the Reqwest request/response stream, providing
    /// cancellation at the async task boundary. No retry is performed here;
    /// delivery policy owns attempts so duplicate calls remain explicit.
    pub async fn send_buffered(
        &self,
        endpoint: &OpenAiEndpoint,
        body: &Value,
        credential: Option<&SecretValue>,
    ) -> Result<BufferedProviderResponse, ProviderError> {
        self.send_buffered_with_headers(endpoint, body, credential, None)
            .await
    }

    /// POST a buffered JSON request with bounded W3C propagation headers.
    ///
    /// Only `traceparent` and `tracestate` are copied from `headers`; callers
    /// cannot override authorization or content negotiation through this seam.
    pub async fn send_buffered_with_headers(
        &self,
        endpoint: &OpenAiEndpoint,
        body: &Value,
        credential: Option<&SecretValue>,
        headers: Option<&HeaderMap>,
    ) -> Result<BufferedProviderResponse, ProviderError> {
        let mut request = self
            .client
            .post(endpoint.chat_completions_url().clone())
            .header(ACCEPT, "application/json")
            .json(body);
        if let Some(headers) = headers {
            for name in ["traceparent", "tracestate"] {
                if let Some(value) = headers.get(name) {
                    request = request.header(name, value.clone());
                }
            }
        }
        if let Some(credential) = credential {
            request = request.header(AUTHORIZATION, credential.bearer_header()?);
        }
        let response = request.send().await.map_err(|_| ProviderError::Transport)?;
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
        Ok(BufferedProviderResponse {
            status,
            content_type,
            body: body.freeze(),
        })
    }

    /// POST a streaming JSON request and return the response byte stream.
    ///
    /// The returned stream owns Reqwest's response. Dropping it cancels the
    /// transport. Aggregate size is intentionally not bounded here; callers
    /// must apply incremental line/event bounds appropriate to their protocol.
    pub async fn send_stream(
        &self,
        endpoint: &OpenAiEndpoint,
        body: &Value,
        credential: Option<&SecretValue>,
    ) -> Result<StreamingProviderResponse, ProviderError> {
        self.send_stream_with_headers(endpoint, body, credential, None)
            .await
    }

    /// POST a streaming JSON request with bounded W3C propagation headers.
    pub async fn send_stream_with_headers(
        &self,
        endpoint: &OpenAiEndpoint,
        body: &Value,
        credential: Option<&SecretValue>,
        headers: Option<&HeaderMap>,
    ) -> Result<StreamingProviderResponse, ProviderError> {
        let mut request = self
            .client
            .post(endpoint.chat_completions_url().clone())
            .header(ACCEPT, "text/event-stream")
            .json(body);
        if let Some(headers) = headers {
            for name in ["traceparent", "tracestate"] {
                if let Some(value) = headers.get(name) {
                    request = request.header(name, value.clone());
                }
            }
        }
        if let Some(credential) = credential {
            request = request.header(AUTHORIZATION, credential.bearer_header()?);
        }
        let response = request.send().await.map_err(|_| ProviderError::Transport)?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("text/event-stream")
            .to_owned();
        let stream = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|_| ProviderError::Transport));
        Ok(StreamingProviderResponse {
            status,
            content_type,
            stream: Box::pin(stream),
        })
    }
}

impl fmt::Debug for OpenAiProviderClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiProviderClient")
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener};
    use std::thread::{self, JoinHandle};

    use serde_json::json;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type CapturedRequest = JoinHandle<std::io::Result<Vec<u8>>>;

    fn test_config(max_response_bytes: usize) -> ProviderClientConfig {
        ProviderClientConfig {
            request_timeout: Duration::from_secs(2),
            connect_timeout: Duration::from_secs(1),
            max_response_bytes,
        }
    }

    fn find_header_end(bytes: &[u8]) -> Option<usize> {
        bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
    }

    fn request_content_length(headers: &[u8]) -> usize {
        String::from_utf8_lossy(headers)
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0)
    }

    fn spawn_one_response(response: Vec<u8>) -> Result<(String, CapturedRequest), std::io::Error> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let task = thread::spawn(move || {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4_096];
            loop {
                let read = stream.read(&mut chunk)?;
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.len() > 1024 * 1024 {
                    return Err(std::io::Error::other("test request exceeded bound"));
                }
                if let Some(header_end) = find_header_end(&request) {
                    let content_length = request_content_length(&request[..header_end]);
                    if request.len() >= header_end.saturating_add(content_length) {
                        break;
                    }
                }
            }
            stream.write_all(&response)?;
            Ok(request)
        });
        Ok((format!("http://{address}/v1"), task))
    }

    fn join_request(task: CapturedRequest) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        task.join()
            .map_err(|_| std::io::Error::other("test provider thread panicked"))?
            .map_err(Into::into)
    }

    #[test]
    fn endpoints_are_exact_and_reject_credential_bearing_urls() -> TestResult {
        let endpoint = OpenAiEndpoint::parse("http://127.0.0.1:11434/v1/")?;
        assert_eq!(
            endpoint.chat_completions_url().as_str(),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        for invalid in [
            "",
            " localhost:11434/v1",
            "ftp://example.com/v1",
            "https://key@example.com/v1",
            "https://example.com/v1?key=value",
            "https://example.com/v1#fragment",
        ] {
            assert_eq!(
                OpenAiEndpoint::parse(invalid),
                Err(ProviderError::InvalidEndpoint)
            );
        }
        let rendered = format!("{endpoint:?}");
        assert!(!rendered.contains("/v1"));
        Ok(())
    }

    #[test]
    fn secret_formatting_is_redacted_and_bad_headers_fail() {
        let secret = SecretValue::new("provider-test-value");
        assert!(!secret.is_empty());
        assert_eq!(format!("{secret}"), "[REDACTED]");
        assert_eq!(format!("{secret:?}"), "SecretValue([REDACTED])");
        let invalid = SecretValue::new("line\nbreak");
        assert_eq!(
            invalid.bearer_header(),
            Err(ProviderError::InvalidCredential)
        );
        let duplicate = secret.duplicate_for_request();
        assert_eq!(duplicate.len(), secret.len());
        assert_eq!(format!("{duplicate:?}"), "SecretValue([REDACTED])");
    }

    #[test]
    fn client_config_rejects_unbounded_values() {
        let mut config = test_config(1);
        config.request_timeout = Duration::ZERO;
        assert!(matches!(
            OpenAiProviderClient::new(config),
            Err(ProviderError::InvalidClientConfig)
        ));
        assert!(matches!(
            OpenAiProviderClient::new(test_config(0)),
            Err(ProviderError::InvalidClientConfig)
        ));
    }

    #[tokio::test]
    async fn buffered_call_replays_status_content_type_and_sensitive_auth() -> TestResult {
        let response = b"HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: 16\r\nConnection: close\r\n\r\n{\"error\":\"busy\"}".to_vec();
        let (base_url, task) = spawn_one_response(response)?;
        let endpoint = OpenAiEndpoint::parse(&base_url)?;
        let client = OpenAiProviderClient::new(test_config(1_024))?;
        let secret = SecretValue::new("provider-test-value");
        let response = client
            .send_buffered(&endpoint, &json!({"model": "small"}), Some(&secret))
            .await?;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.content_type(), "application/json");
        assert_eq!(
            response.body(),
            &Bytes::from_static(b"{\"error\":\"busy\"}")
        );
        assert!(!format!("{response:?}").contains("busy"));

        let request = String::from_utf8(join_request(task)?)?;
        assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        assert!(request.contains("authorization: Bearer provider-test-value\r\n"));
        assert!(request.contains("{\"model\":\"small\"}"));
        Ok(())
    }

    #[tokio::test]
    async fn propagation_headers_are_bounded_to_w3c_context() -> TestResult {
        let response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_vec();
        let (base_url, task) = spawn_one_response(response)?;
        let endpoint = OpenAiEndpoint::parse(&base_url)?;
        let client = OpenAiProviderClient::new(test_config(1_024))?;
        let mut headers = HeaderMap::new();
        headers.insert(
            "traceparent",
            HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        );
        headers.insert("tracestate", HeaderValue::from_static("vendor=value"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer must-not-forward"),
        );
        let _ = client
            .send_buffered_with_headers(&endpoint, &json!({}), None, Some(&headers))
            .await?;
        let request = String::from_utf8(join_request(task)?)?;
        assert!(
            request.contains(
                "traceparent: 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01\r\n"
            )
        );
        assert!(request.contains("tracestate: vendor=value\r\n"));
        assert!(!request.contains("must-not-forward"));
        Ok(())
    }

    #[tokio::test]
    async fn declared_response_bound_is_enforced_before_body_collection() -> TestResult {
        let response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 999\r\nConnection: close\r\n\r\n".to_vec();
        let (base_url, task) = spawn_one_response(response)?;
        let endpoint = OpenAiEndpoint::parse(&base_url)?;
        let client = OpenAiProviderClient::new(test_config(10))?;
        assert!(matches!(
            client.send_buffered(&endpoint, &json!({}), None).await,
            Err(ProviderError::ResponseTooLarge { limit: 10 })
        ));
        let _ = join_request(task)?;
        Ok(())
    }

    #[tokio::test]
    async fn redirects_are_returned_not_followed() -> TestResult {
        let response = b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest/meta-data\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let (base_url, task) = spawn_one_response(response)?;
        let endpoint = OpenAiEndpoint::parse(&base_url)?;
        let client = OpenAiProviderClient::new(test_config(10))?;
        let response = client.send_buffered(&endpoint, &json!({}), None).await?;
        assert_eq!(response.status(), StatusCode::FOUND);
        assert!(response.body().is_empty());
        let _ = join_request(task)?;
        Ok(())
    }
}
