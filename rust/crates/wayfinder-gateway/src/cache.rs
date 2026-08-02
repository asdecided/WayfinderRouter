//! Opt-in, bounded, in-memory exact-match response cache.
//!
//! Keys contain only a SHA-256 digest of the canonical request projection;
//! prompt text is never retained in the index. Response bodies are retained
//! only while caching is enabled and are purged immediately when disabled.

use std::fmt;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Default cache TTL in seconds.
pub const DEFAULT_TTL_SECONDS: f64 = 300.0;
/// Default maximum number of retained responses.
pub const DEFAULT_MAX_ENTRIES: usize = 1_024;
/// Default retained response bytes (64 MiB).
pub const DEFAULT_MAX_BYTES: usize = 64 * 1_024 * 1_024;
/// Cache-key schema version. Bump when request or provider-contract semantics
/// change so shared entries cannot cross a prompt-contract boundary.
pub const CACHE_KEY_SCHEMA_VERSION: &str = "v2";

/// A complete buffered response eligible for exact replay.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedResponse {
    /// Original upstream status.
    pub status: u16,
    /// Original upstream media type.
    pub content_type: String,
    /// Original response body, replayed verbatim.
    pub body: Vec<u8>,
    /// Prompt tokens used for avoided-cost reporting.
    pub prompt_tokens: u64,
    /// Completion tokens used for avoided-cost reporting.
    pub completion_tokens: u64,
    /// Whether token usage was estimated.
    pub estimated: bool,
}

impl fmt::Debug for CachedResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CachedResponse")
            .field("status", &self.status)
            .field("content_type", &self.content_type)
            .field("body", &format_args!("<{} bytes>", self.body.len()))
            .field("prompt_tokens", &self.prompt_tokens)
            .field("completion_tokens", &self.completion_tokens)
            .field("estimated", &self.estimated)
            .finish()
    }
}

/// Cache configuration applied atomically with state mutation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CacheSettings {
    /// Whether retention and replay are enabled.
    pub enabled: bool,
    /// Zero means entries do not expire by age.
    pub ttl_seconds: f64,
    /// LRU entry ceiling.
    pub max_entries: usize,
    /// Aggregate response-body byte ceiling.
    pub max_bytes: usize,
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_seconds: DEFAULT_TTL_SECONDS,
            max_entries: DEFAULT_MAX_ENTRIES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// Prompt-free cache metrics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheStats {
    /// Retained responses.
    pub entries: usize,
    /// Aggregate retained response body bytes.
    pub bytes: usize,
    /// Successful replays.
    pub hits: u64,
    /// Enabled lookups without a fresh entry.
    pub misses: u64,
}

/// Invalid inputs or poisoned synchronization state.
#[derive(Debug, Error, PartialEq)]
pub enum CacheError {
    /// Cache keys require a JSON request object.
    #[error("cache key body must be a JSON object")]
    InvalidBody,
    /// TTL must be finite and non-negative.
    #[error("cache TTL must be finite and non-negative")]
    InvalidTtl,
    /// Configured entry or byte bounds do not fit this platform.
    #[error("cache bounds exceed the supported platform size")]
    InvalidBounds,
    /// Monotonic time must be finite and non-negative.
    #[error("cache time must be finite and non-negative")]
    InvalidTime,
    /// Canonical request serialization failed.
    #[error("cannot serialize cache key: {0}")]
    Json(String),
    /// Internal state could not be synchronized.
    #[error("cache state lock is unavailable")]
    LockPoisoned,
    /// A shared cache requires the configured Redis state backend.
    #[error("shared response cache requires the Redis state backend")]
    SharedBackendRequired,
}

#[derive(Debug)]
struct State {
    settings: CacheSettings,
    store: IndexMap<String, StoredResponse>,
    bytes: usize,
    hits: u64,
    misses: u64,
}

#[derive(Clone, Debug)]
struct StoredResponse {
    response: Arc<CachedResponse>,
    stored_at: f64,
}

/// Thread-safe exact-match LRU cache.
pub struct ResponseCache {
    state: Mutex<State>,
}

impl fmt::Debug for ResponseCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.state.lock() {
            Ok(state) => formatter
                .debug_struct("ResponseCache")
                .field("settings", &state.settings)
                .field("entries", &state.store.len())
                .field("bytes", &state.bytes)
                .field("hits", &state.hits)
                .field("misses", &state.misses)
                .finish(),
            Err(_) => formatter
                .debug_struct("ResponseCache")
                .field("state", &"<unavailable>")
                .finish(),
        }
    }
}

impl ResponseCache {
    /// Construct an empty cache with validated settings.
    pub fn new(settings: CacheSettings) -> Result<Self, CacheError> {
        validate_settings(settings)?;
        Ok(Self {
            state: Mutex::new(State {
                settings,
                store: IndexMap::new(),
                bytes: 0,
                hits: 0,
                misses: 0,
            }),
        })
    }

    /// Whether lookups and retention are currently enabled.
    pub fn enabled(&self) -> Result<bool, CacheError> {
        self.state
            .lock()
            .map(|state| state.settings.enabled)
            .map_err(|_| CacheError::LockPoisoned)
    }

    /// Return a fresh entry and move it to the MRU position.
    ///
    /// `now_seconds` must use the same monotonic clock origin passed to
    /// [`Self::put_at`]. Hits clone an [`Arc`], never the retained body bytes.
    pub fn get_at(
        &self,
        key: &str,
        now_seconds: f64,
    ) -> Result<Option<Arc<CachedResponse>>, CacheError> {
        validate_time(now_seconds)?;
        let mut state = self.state.lock().map_err(|_| CacheError::LockPoisoned)?;
        if !state.settings.enabled {
            return Ok(None);
        }
        let Some(entry) = state.store.shift_remove(key) else {
            state.misses = state.misses.saturating_add(1);
            return Ok(None);
        };
        state.bytes = state.bytes.saturating_sub(entry.response.body.len());
        if state.settings.ttl_seconds > 0.0
            && now_seconds - entry.stored_at >= state.settings.ttl_seconds
        {
            state.misses = state.misses.saturating_add(1);
            return Ok(None);
        }
        state.bytes = state.bytes.saturating_add(entry.response.body.len());
        let response = Arc::clone(&entry.response);
        state.store.insert(key.to_owned(), entry);
        state.hits = state.hits.saturating_add(1);
        Ok(Some(response))
    }

    /// Insert or refresh an entry and evict LRU responses to both bounds.
    ///
    /// `now_seconds` must use the same monotonic clock origin as [`Self::get_at`].
    /// Stamping inside this method prevents callers from constructing an entry
    /// whose age is unrelated to its insertion.
    pub fn put_at(
        &self,
        key: String,
        entry: CachedResponse,
        now_seconds: f64,
    ) -> Result<(), CacheError> {
        validate_time(now_seconds)?;
        let mut state = self.state.lock().map_err(|_| CacheError::LockPoisoned)?;
        if !state.settings.enabled
            || state.settings.max_entries == 0
            || state.settings.max_bytes == 0
            || entry.body.len() > state.settings.max_bytes
        {
            return Ok(());
        }
        if let Some(previous) = state.store.shift_remove(&key) {
            state.bytes = state.bytes.saturating_sub(previous.response.body.len());
        }
        state.bytes = state.bytes.saturating_add(entry.body.len());
        state.store.insert(
            key,
            StoredResponse {
                response: Arc::new(entry),
                stored_at: now_seconds,
            },
        );
        evict(&mut state);
        Ok(())
    }

    /// Purge every retained response without resetting cumulative counters.
    pub fn clear(&self) -> Result<(), CacheError> {
        let mut state = self.state.lock().map_err(|_| CacheError::LockPoisoned)?;
        state.store.clear();
        state.bytes = 0;
        Ok(())
    }

    /// Apply hot-reloaded settings; disabling purges bodies immediately.
    pub fn reconfigure(&self, settings: CacheSettings) -> Result<(), CacheError> {
        validate_settings(settings)?;
        let mut state = self.state.lock().map_err(|_| CacheError::LockPoisoned)?;
        state.settings = settings;
        if !settings.enabled {
            state.store.clear();
            state.bytes = 0;
        } else {
            evict(&mut state);
        }
        Ok(())
    }

    /// Return bounded metadata only.
    pub fn stats(&self) -> Result<CacheStats, CacheError> {
        let state = self.state.lock().map_err(|_| CacheError::LockPoisoned)?;
        Ok(CacheStats {
            entries: state.store.len(),
            bytes: state.bytes,
            hits: state.hits,
            misses: state.misses,
        })
    }
}

impl Default for ResponseCache {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                settings: CacheSettings::default(),
                store: IndexMap::new(),
                bytes: 0,
                hits: 0,
                misses: 0,
            }),
        }
    }
}

/// SHA-256 of the authenticated cache partition, public route, served upstream
/// model, and canonical request fields.
///
/// `partition` must identify the effective caller policy boundary. Managed
/// surfaces use the virtual-key id so an exact request from one tenant can
/// never replay content produced for another tenant.
pub fn cache_key(
    partition: &str,
    route: &str,
    served_model: &str,
    body: &Value,
) -> Result<String, CacheError> {
    cache_key_with_version(
        CACHE_KEY_SCHEMA_VERSION,
        partition,
        route,
        served_model,
        body,
    )
}

/// SHA-256 cache key with an explicit configuration/prompt-contract version.
pub fn cache_key_with_version(
    version: &str,
    partition: &str,
    route: &str,
    served_model: &str,
    body: &Value,
) -> Result<String, CacheError> {
    let object = body.as_object().ok_or(CacheError::InvalidBody)?;
    let projected = object
        .iter()
        .filter(|(name, _)| !matches!(name.as_str(), "model" | "stream"))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<Map<_, _>>();
    let envelope = canonical_json(serde_json::json!({
        "version": version,
        "partition": partition,
        "route": route,
        "model": served_model,
        "body": projected,
    }));
    let serialized =
        serde_json::to_vec(&envelope).map_err(|error| CacheError::Json(error.to_string()))?;
    let digest = Sha256::digest(serialized);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        encoded.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    Ok(encoded)
}

/// Serialize a response for the shared cache without exposing its contents in
/// logs or route metadata.
pub fn encode_shared_response(response: &CachedResponse) -> Result<Vec<u8>, CacheError> {
    serde_json::to_vec(response).map_err(|error| CacheError::Json(error.to_string()))
}

/// Decode one shared-cache value and reject malformed or truncated entries.
pub fn decode_shared_response(value: &[u8]) -> Result<CachedResponse, CacheError> {
    serde_json::from_slice(value).map_err(|error| CacheError::Json(error.to_string()))
}

fn canonical_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_json).collect()),
        Value::Object(values) => {
            let sorted = values
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>();
            Value::Object(
                sorted
                    .into_iter()
                    .map(|(name, value)| (name, canonical_json(value)))
                    .collect(),
            )
        }
        scalar => scalar,
    }
}

/// Whether a request is contractually deterministic enough for replay.
#[must_use]
pub fn is_cacheable(body: &Value) -> bool {
    let Some(body) = body.as_object() else {
        return false;
    };
    if body.get("stream") == Some(&Value::Bool(true)) {
        return false;
    }
    if body
        .get("temperature")
        .is_some_and(|value| !value.is_null() && !python_numeric_equal(value, 0.0))
    {
        return false;
    }
    if body
        .get("top_p")
        .is_some_and(|value| !value.is_null() && !python_numeric_equal(value, 1.0))
    {
        return false;
    }
    if body
        .get("n")
        .is_some_and(|value| !value.is_null() && !python_numeric_equal(value, 1.0))
    {
        return false;
    }
    if body.get("seed").is_some_and(|value| !value.is_null()) {
        return false;
    }
    if ["tools", "tool_choice", "logit_bias"]
        .iter()
        .any(|name| body.get(*name).is_some_and(python_truthy))
    {
        return false;
    }
    body.get("messages")
        .and_then(Value::as_array)
        .filter(|messages| !messages.is_empty())
        .is_some_and(|messages| {
            messages.iter().all(|message| {
                message
                    .as_object()
                    .and_then(|message| message.get("content"))
                    .is_some_and(Value::is_string)
            })
        })
}

/// Whether a buffered upstream response is a complete success worth storing.
#[must_use]
pub fn is_storable(status: u16, content_type: &str, response: &Value) -> bool {
    if status != 200 || !content_type.contains("json") {
        return false;
    }
    let Some(response) = response.as_object() else {
        return false;
    };
    if response.get("error").is_some_and(|error| !error.is_null()) {
        return false;
    }
    let Some(choice) = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(Value::as_object)
    else {
        return false;
    };
    let Some(message) = choice.get("message").and_then(Value::as_object) else {
        return false;
    };
    if message.get("tool_calls").is_some_and(python_truthy) {
        return false;
    }
    message
        .get("content")
        .and_then(Value::as_str)
        .is_some_and(|content| !content.is_empty())
}

fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn python_numeric_equal(value: &Value, expected: f64) -> bool {
    match value {
        Value::Bool(value) => f64::from(u8::from(*value)) == expected,
        Value::Number(value) => value.as_f64() == Some(expected),
        Value::Null | Value::String(_) | Value::Array(_) | Value::Object(_) => false,
    }
}

fn validate_settings(settings: CacheSettings) -> Result<(), CacheError> {
    if !settings.ttl_seconds.is_finite() || settings.ttl_seconds < 0.0 {
        return Err(CacheError::InvalidTtl);
    }
    Ok(())
}

fn validate_time(now_seconds: f64) -> Result<(), CacheError> {
    if !now_seconds.is_finite() || now_seconds < 0.0 {
        return Err(CacheError::InvalidTime);
    }
    Ok(())
}

fn evict(state: &mut State) {
    while !state.store.is_empty()
        && (state.store.len() > state.settings.max_entries
            || state.bytes > state.settings.max_bytes)
    {
        if let Some((_, entry)) = state.store.shift_remove_index(0) {
            state.bytes = state.bytes.saturating_sub(entry.response.body.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn entry(body: &[u8]) -> CachedResponse {
        CachedResponse {
            status: 200,
            content_type: "application/json".to_owned(),
            body: body.to_vec(),
            prompt_tokens: 3,
            completion_tokens: 4,
            estimated: false,
        }
    }

    #[test]
    fn key_is_deterministic_and_partitioned_by_caller_route_and_model() -> Result<(), CacheError> {
        let body = json!({
            "model": "auto",
            "stream": false,
            "messages": [{"role": "user", "content": "héllo 😀"}],
            "temperature": 0,
        });
        let key = cache_key("local", "cloud", "llama3.2", &body)?;
        assert_eq!(key.len(), 64);
        let changed = json!({
            "model": "prefer-hosted",
            "stream": true,
            "messages": [{"content": "héllo 😀", "role": "user"}],
            "temperature": 0,
        });
        assert_eq!(key, cache_key("local", "cloud", "llama3.2", &changed)?);
        assert_ne!(key, cache_key("other-key", "cloud", "llama3.2", &body)?);
        assert_ne!(key, cache_key("local", "other-route", "llama3.2", &body)?);
        assert_ne!(key, cache_key("local", "cloud", "other-model", &body)?);
        Ok(())
    }

    #[test]
    fn shared_key_version_and_response_encoding_are_explicit() -> Result<(), CacheError> {
        let body = json!({
            "messages": [{"role": "user", "content": "hello"}],
            "temperature": 0
        });
        let current = cache_key("tenant", "auto", "local", &body)?;
        let prior = cache_key_with_version("v1", "tenant", "auto", "local", &body)?;
        assert_ne!(current, prior);

        let response = entry(b"exact bytes");
        let encoded = encode_shared_response(&response)?;
        assert!(!encoded.is_empty());
        assert_eq!(decode_shared_response(&encoded)?, response);
        assert!(decode_shared_response(b"not-json").is_err());
        Ok(())
    }

    #[test]
    fn cacheable_policy_is_conservative() {
        let base = json!({"messages": [{"role": "user", "content": "hi"}]});
        assert!(is_cacheable(&base));
        for extra in [
            json!({"stream": true}),
            json!({"temperature": 0.1}),
            json!({"top_p": 0.9}),
            json!({"n": 2}),
            json!({"seed": 1}),
            json!({"tools": [{"type": "function"}]}),
            json!({"tool_choice": "auto"}),
            json!({"logit_bias": {"1": 2}}),
        ] {
            let mut candidate = base.clone();
            if let (Some(candidate), Some(extra)) = (candidate.as_object_mut(), extra.as_object()) {
                candidate.extend(extra.clone());
            }
            assert!(
                !is_cacheable(&candidate),
                "unexpectedly cacheable: {candidate}"
            );
        }
        assert!(!is_cacheable(&json!({"messages": []})));
        assert!(!is_cacheable(
            &json!({"messages": [{"content": [{"type": "text"}]}]})
        ));
        // Python bool is a numeric subtype and JSON 1.0 compares equal to 1.
        assert!(is_cacheable(&json!({
            "messages": [{"content": "hi"}],
            "temperature": false,
            "top_p": true,
            "n": 1.0
        })));
    }

    #[test]
    fn storable_requires_nonempty_plain_text_completion() {
        let good = json!({"choices": [{"message": {"content": "hello"}}]});
        assert!(is_storable(200, "application/json", &good));
        assert!(!is_storable(500, "application/json", &good));
        assert!(!is_storable(200, "text/plain", &good));
        assert!(!is_storable(
            200,
            "application/json",
            &json!({"error": {"message": "x"}})
        ));
        assert!(!is_storable(
            200,
            "application/json",
            &json!({"choices": [{"message": {"content": ""}}]})
        ));
        assert!(!is_storable(
            200,
            "application/json",
            &json!({"choices": [{"message": {"content": "x", "tool_calls": [{}]}}]})
        ));
    }

    #[test]
    fn disabled_cache_does_not_retain_or_count_misses() -> Result<(), CacheError> {
        let cache = ResponseCache::default();
        cache.put_at("a".to_owned(), entry(b"one"), 0.0)?;
        assert!(cache.get_at("a", 0.0)?.is_none());
        assert_eq!(
            cache.stats()?,
            CacheStats {
                entries: 0,
                bytes: 0,
                hits: 0,
                misses: 0,
            }
        );
        Ok(())
    }

    #[test]
    fn hit_refreshes_lru_and_entry_bound_evicts_oldest() -> Result<(), CacheError> {
        let cache = ResponseCache::new(CacheSettings {
            enabled: true,
            max_entries: 2,
            max_bytes: 100,
            ttl_seconds: 0.0,
        })?;
        cache.put_at("a".to_owned(), entry(b"a"), 0.0)?;
        cache.put_at("b".to_owned(), entry(b"b"), 0.0)?;
        let first_a = cache.get_at("a", 1.0)?.ok_or(CacheError::InvalidBody)?;
        cache.put_at("c".to_owned(), entry(b"c"), 1.0)?;
        assert!(cache.get_at("b", 1.0)?.is_none());
        let second_a = cache.get_at("a", 1.0)?.ok_or(CacheError::InvalidBody)?;
        assert!(Arc::ptr_eq(&first_a, &second_a));
        assert!(cache.get_at("c", 1.0)?.is_some());
        Ok(())
    }

    #[test]
    fn byte_bound_and_oversized_entries_are_enforced() -> Result<(), CacheError> {
        let cache = ResponseCache::new(CacheSettings {
            enabled: true,
            max_entries: 10,
            max_bytes: 4,
            ttl_seconds: 0.0,
        })?;
        cache.put_at("a".to_owned(), entry(b"aaa"), 0.0)?;
        cache.put_at("b".to_owned(), entry(b"bb"), 0.0)?;
        assert!(cache.get_at("a", 0.0)?.is_none());
        assert_eq!(cache.stats()?.bytes, 2);
        cache.put_at("huge".to_owned(), entry(b"12345"), 0.0)?;
        assert_eq!(cache.stats()?.entries, 1);
        Ok(())
    }

    #[test]
    fn ttl_expires_inclusively_and_counts_a_miss() -> Result<(), CacheError> {
        let cache = ResponseCache::new(CacheSettings {
            enabled: true,
            max_entries: 10,
            max_bytes: 100,
            ttl_seconds: 5.0,
        })?;
        cache.put_at("a".to_owned(), entry(b"a"), 10.0)?;
        assert!(cache.get_at("a", 14.999)?.is_some());
        assert!(cache.get_at("a", 15.0)?.is_none());
        assert_eq!(cache.stats()?.misses, 1);
        Ok(())
    }

    #[test]
    fn disabling_purges_bodies_but_retains_counters() -> Result<(), CacheError> {
        let cache = ResponseCache::new(CacheSettings {
            enabled: true,
            max_entries: 10,
            max_bytes: 100,
            ttl_seconds: 0.0,
        })?;
        cache.put_at("a".to_owned(), entry(b"secret response"), 0.0)?;
        assert!(cache.get_at("a", 0.0)?.is_some());
        cache.reconfigure(CacheSettings::default())?;
        let stats = cache.stats()?;
        assert_eq!(stats.entries, 0);
        assert_eq!(stats.bytes, 0);
        assert_eq!(stats.hits, 1);
        Ok(())
    }

    #[test]
    fn debug_never_renders_response_body() {
        let response = entry(b"sensitive completion");
        let rendered = format!("{response:?}");
        assert!(!rendered.contains("sensitive completion"));
        assert!(rendered.contains("20 bytes"));
    }
}
