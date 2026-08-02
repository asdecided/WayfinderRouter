//! Shared policy state for replicated gateway processes.
//!
//! `memory` remains the zero-configuration default. The opt-in Redis backend
//! provides fixed-window request/token counters using Lua and Redis server
//! time, while callers can fall back to the bounded local limiter on outage.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use redis::Script;
use redis::aio::ConnectionManager;
use thiserror::Error;

use crate::rate_limit::{LimitKind, RateReservation, RateResult, RateSnapshot, ReservationResult};

const REDIS_KEY_ROOT: &str = "wayfinder:v1";

fn encode_redis_component(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(value.as_bytes())
}

/// Logical key for the gateway-wide shared limit.
pub(crate) fn global_limit_key() -> Arc<str> {
    Arc::from("global")
}

/// Logical key for one workspace's shared limit.
pub(crate) fn workspace_limit_key(id: &str) -> Arc<str> {
    format!("workspace:{}", encode_redis_component(id)).into()
}

/// Logical key for one virtual key's shared limit.
pub(crate) fn virtual_key_limit_key(id: &str) -> Arc<str> {
    format!("virtual-key:{}", encode_redis_component(id)).into()
}

/// One fixed-window policy copied into a backend operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StateLimitConfig {
    /// Optional requests-per-window ceiling.
    pub rpm: Option<u64>,
    /// Optional tokens-per-window ceiling.
    pub tpm: Option<u64>,
    /// Fixed-window duration in seconds.
    pub window_seconds: f64,
}

impl StateLimitConfig {
    fn validate(self) -> Result<Self, StateBackendError> {
        if !self.window_seconds.is_finite() || self.window_seconds <= 0.0 {
            return Err(StateBackendError::InvalidWindow);
        }
        Ok(self)
    }
}

/// Sanitized shared-state failures.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum StateBackendError {
    /// The configured window is invalid.
    #[error("shared state window is invalid")]
    InvalidWindow,
    /// The backend operation could not be completed.
    #[error("shared state backend unavailable")]
    Unavailable,
    /// The process-local state lock is poisoned.
    #[error("shared state lock is unavailable")]
    LockPoisoned,
}

type StateFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, StateBackendError>> + Send + 'a>>;

/// Atomic fixed-window operations required by gateway policy state.
pub trait StateBackend: Send + Sync {
    /// Stable implementation label for diagnostics.
    fn backend_name(&self) -> &'static str;

    /// Reserve one request in a single policy dimension.
    fn admit<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        now_seconds: f64,
    ) -> StateFuture<'a, RateResult>;

    /// Atomically reserve one request and a conservative token budget.
    fn reserve<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        tokens: u64,
        now_seconds: f64,
    ) -> StateFuture<'a, ReservationResult>;

    /// Reserve only tokens after the request dimension was admitted earlier.
    fn reserve_tokens<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        tokens: u64,
        now_seconds: f64,
    ) -> StateFuture<'a, ReservationResult>;

    /// Reconcile observed tokens only in the reservation's original window.
    fn reconcile<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        reservation: RateReservation,
        actual_tokens: u64,
        now_seconds: f64,
    ) -> StateFuture<'a, bool>;

    /// Roll back one previously accepted scope after a later scope rejects.
    fn rollback<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        reservation: RateReservation,
        now_seconds: f64,
    ) -> StateFuture<'a, bool>;

    /// Add observed upstream tokens to one policy dimension.
    fn add_tokens<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        tokens: u64,
        now_seconds: f64,
    ) -> StateFuture<'a, ()>;

    /// Read the current request-window snapshot.
    fn snapshot<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        now_seconds: f64,
    ) -> StateFuture<'a, Option<RateSnapshot>>;

    /// Append one prompt-free operator audit event when the backend supports
    /// shared event storage. The memory backend deliberately returns an
    /// unavailable result so callers can retain their local JSONL sink.
    fn append_audit<'a>(
        &'a self,
        _key: &'a str,
        _event: &'a str,
        _max_events: usize,
    ) -> StateFuture<'a, ()> {
        Box::pin(async { Err(StateBackendError::Unavailable) })
    }

    /// Whether the backend has recently failed.
    fn degraded(&self) -> bool;
}

/// Process-local backend used by the default configuration and tests.
#[derive(Clone, Debug, Default)]
pub struct MemoryStateBackend {
    limiters: Arc<Mutex<BTreeMap<String, crate::rate_limit::RateLimiter>>>,
}

impl StateBackend for MemoryStateBackend {
    fn backend_name(&self) -> &'static str {
        "memory"
    }

    fn admit<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        now_seconds: f64,
    ) -> StateFuture<'a, RateResult> {
        Box::pin(async move {
            let config = config.validate()?;
            let mut limiters = self
                .limiters
                .lock()
                .map_err(|_| StateBackendError::LockPoisoned)?;
            let limiter = limiter_for(&mut limiters, key, config)?;
            limiter
                .admit_at(now_seconds)
                .map_err(|_| StateBackendError::Unavailable)
        })
    }

    fn reserve<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        tokens: u64,
        now_seconds: f64,
    ) -> StateFuture<'a, ReservationResult> {
        Box::pin(async move {
            let config = config.validate()?;
            let mut limiters = self
                .limiters
                .lock()
                .map_err(|_| StateBackendError::LockPoisoned)?;
            limiter_for(&mut limiters, key, config)?
                .reserve_at(tokens, now_seconds)
                .map_err(|_| StateBackendError::Unavailable)
        })
    }

    fn reserve_tokens<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        tokens: u64,
        now_seconds: f64,
    ) -> StateFuture<'a, ReservationResult> {
        Box::pin(async move {
            let config = config.validate()?;
            let mut limiters = self
                .limiters
                .lock()
                .map_err(|_| StateBackendError::LockPoisoned)?;
            limiter_for(&mut limiters, key, config)?
                .reserve_tokens_at(tokens, now_seconds)
                .map_err(|_| StateBackendError::Unavailable)
        })
    }

    fn reconcile<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        reservation: RateReservation,
        actual_tokens: u64,
        now_seconds: f64,
    ) -> StateFuture<'a, bool> {
        Box::pin(async move {
            let config = config.validate()?;
            let mut limiters = self
                .limiters
                .lock()
                .map_err(|_| StateBackendError::LockPoisoned)?;
            limiter_for_mut(&mut limiters, key, config)?
                .reconcile_at(reservation, actual_tokens, now_seconds)
                .map_err(|_| StateBackendError::Unavailable)
        })
    }

    fn rollback<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        reservation: RateReservation,
        now_seconds: f64,
    ) -> StateFuture<'a, bool> {
        Box::pin(async move {
            let config = config.validate()?;
            let mut limiters = self
                .limiters
                .lock()
                .map_err(|_| StateBackendError::LockPoisoned)?;
            limiter_for_mut(&mut limiters, key, config)?
                .rollback_at(reservation, now_seconds)
                .map_err(|_| StateBackendError::Unavailable)
        })
    }

    fn add_tokens<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        tokens: u64,
        now_seconds: f64,
    ) -> StateFuture<'a, ()> {
        Box::pin(async move {
            let config = config.validate()?;
            let mut limiters = self
                .limiters
                .lock()
                .map_err(|_| StateBackendError::LockPoisoned)?;
            limiter_for_mut(&mut limiters, key, config)?
                .add_tokens_at(tokens, now_seconds)
                .map_err(|_| StateBackendError::Unavailable)
        })
    }

    fn snapshot<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        now_seconds: f64,
    ) -> StateFuture<'a, Option<RateSnapshot>> {
        Box::pin(async move {
            let config = config.validate()?;
            if config.rpm.is_none() {
                return Ok(None);
            }
            let mut limiters = self
                .limiters
                .lock()
                .map_err(|_| StateBackendError::LockPoisoned)?;
            limiter_for(&mut limiters, key, config)?
                .snapshot_at(now_seconds)
                .map_err(|_| StateBackendError::Unavailable)
        })
    }

    fn degraded(&self) -> bool {
        false
    }
}

fn limiter_for<'a>(
    limiters: &'a mut BTreeMap<String, crate::rate_limit::RateLimiter>,
    key: &str,
    config: StateLimitConfig,
) -> Result<&'a crate::rate_limit::RateLimiter, StateBackendError> {
    if !limiters.contains_key(key) {
        limiters.insert(
            key.to_owned(),
            crate::rate_limit::RateLimiter::new(config.rpm, config.tpm, config.window_seconds)
                .map_err(|_| StateBackendError::InvalidWindow)?,
        );
    }
    limiters.get(key).ok_or(StateBackendError::Unavailable)
}

fn limiter_for_mut<'a>(
    limiters: &'a mut BTreeMap<String, crate::rate_limit::RateLimiter>,
    key: &str,
    config: StateLimitConfig,
) -> Result<&'a mut crate::rate_limit::RateLimiter, StateBackendError> {
    if !limiters.contains_key(key) {
        limiters.insert(
            key.to_owned(),
            crate::rate_limit::RateLimiter::new(config.rpm, config.tpm, config.window_seconds)
                .map_err(|_| StateBackendError::InvalidWindow)?,
        );
    }
    limiters.get_mut(key).ok_or(StateBackendError::Unavailable)
}

const ADMIT_SCRIPT: &str = r#"
local time = redis.call('TIME')
local now_ms = (tonumber(time[1]) * 1000) + math.floor(tonumber(time[2]) / 1000)
local window_ms = tonumber(ARGV[1])
local window_id = math.floor(now_ms / window_ms)
local current = redis.call('HGET', KEYS[1], 'window')
if current == false or tonumber(current) ~= window_id then
  redis.call('HSET', KEYS[1], 'window', window_id, 'requests', 0, 'tokens', 0)
  redis.call('PEXPIRE', KEYS[1], math.max(window_ms * 2, 1000))
end
local requests = tonumber(redis.call('HGET', KEYS[1], 'requests') or '0')
local tokens = tonumber(redis.call('HGET', KEYS[1], 'tokens') or '0')
local rpm = tonumber(ARGV[2])
local tpm = tonumber(ARGV[3])
local retry = math.max(0, math.ceil((((window_id + 1) * window_ms) - now_ms) / 1000))
if rpm > 0 and requests >= rpm then return {0, 1, retry} end
if tpm > 0 and tokens >= tpm then return {0, 2, retry} end
redis.call('HINCRBY', KEYS[1], 'requests', 1)
return {1, 0, 0}
"#;

const RESERVE_SCRIPT: &str = r#"
local time = redis.call('TIME')
local now_ms = (tonumber(time[1]) * 1000) + math.floor(tonumber(time[2]) / 1000)
local window_ms = tonumber(ARGV[1])
local window_id = math.floor(now_ms / window_ms)
local current = redis.call('HGET', KEYS[1], 'window')
if current == false or tonumber(current) ~= window_id then
  redis.call('HSET', KEYS[1], 'window', window_id, 'requests', 0, 'tokens', 0)
  redis.call('PEXPIRE', KEYS[1], math.max(window_ms * 2, 1000))
end
local requests = tonumber(redis.call('HGET', KEYS[1], 'requests') or '0')
local tokens = tonumber(redis.call('HGET', KEYS[1], 'tokens') or '0')
local rpm = tonumber(ARGV[2])
local tpm = tonumber(ARGV[3])
local reservation = tonumber(ARGV[4])
local reserve_request = tonumber(ARGV[5])
local retry = math.max(0, math.ceil((((window_id + 1) * window_ms) - now_ms) / 1000))
if reserve_request == 1 and rpm > 0 and requests >= rpm then return {0, 1, retry, window_id} end
if tpm > 0 and tokens + reservation > tpm then return {0, 2, retry, window_id} end
if reserve_request == 1 then redis.call('HINCRBY', KEYS[1], 'requests', 1) end
redis.call('HINCRBY', KEYS[1], 'tokens', reservation)
return {1, 0, 0, window_id}
"#;

const RECONCILE_SCRIPT: &str = r#"
local current = redis.call('HGET', KEYS[1], 'window')
if current == false or tonumber(current) ~= tonumber(ARGV[1]) then return 0 end
local reserved = tonumber(ARGV[2])
local actual = tonumber(ARGV[3])
redis.call('HINCRBY', KEYS[1], 'tokens', actual - reserved)
return 1
"#;

const ROLLBACK_SCRIPT: &str = r#"
local current = redis.call('HGET', KEYS[1], 'window')
if current == false or tonumber(current) ~= tonumber(ARGV[1]) then return 0 end
local requests = tonumber(redis.call('HGET', KEYS[1], 'requests') or '0')
local tokens = tonumber(redis.call('HGET', KEYS[1], 'tokens') or '0')
if tonumber(ARGV[3]) == 1 then
  redis.call('HSET', KEYS[1], 'requests', math.max(0, requests - 1))
end
redis.call('HSET', KEYS[1], 'tokens', math.max(0, tokens - tonumber(ARGV[2])))
return 1
"#;

const ADD_TOKENS_SCRIPT: &str = r#"
local time = redis.call('TIME')
local now_ms = (tonumber(time[1]) * 1000) + math.floor(tonumber(time[2]) / 1000)
local window_ms = tonumber(ARGV[1])
local window_id = math.floor(now_ms / window_ms)
local current = redis.call('HGET', KEYS[1], 'window')
if current == false or tonumber(current) ~= window_id then
  redis.call('HSET', KEYS[1], 'window', window_id, 'requests', 0, 'tokens', 0)
  redis.call('PEXPIRE', KEYS[1], math.max(window_ms * 2, 1000))
end
redis.call('HINCRBY', KEYS[1], 'tokens', ARGV[2])
return 1
"#;

const SNAPSHOT_SCRIPT: &str = r#"
local time = redis.call('TIME')
local now_ms = (tonumber(time[1]) * 1000) + math.floor(tonumber(time[2]) / 1000)
local window_ms = tonumber(ARGV[1])
local window_id = math.floor(now_ms / window_ms)
local current = redis.call('HGET', KEYS[1], 'window')
if current == false or tonumber(current) ~= window_id then
  redis.call('HSET', KEYS[1], 'window', window_id, 'requests', 0, 'tokens', 0)
  redis.call('PEXPIRE', KEYS[1], math.max(window_ms * 2, 1000))
end
local requests = tonumber(redis.call('HGET', KEYS[1], 'requests') or '0')
local retry = math.max(0, math.ceil((((window_id + 1) * window_ms) - now_ms) / 1000))
return {requests, retry}
"#;

const APPEND_AUDIT_SCRIPT: &str = r#"
local max_events = tonumber(ARGV[2])
if max_events == nil or max_events < 1 then
  return redis.error_reply('audit retention bound must be positive')
end
redis.call('RPUSH', KEYS[1], ARGV[1])
redis.call('LTRIM', KEYS[1], -max_events, -1)
return 1
"#;

/// Redis-backed fixed-window counters using server time and Lua atomicity.
#[derive(Clone)]
pub struct RedisStateBackend {
    connection: ConnectionManager,
    keys: RedisKeyNamespace,
    degraded: Arc<AtomicBool>,
}

/// Versioned Redis key construction with one base64url component per
/// operator-controlled value. The `limits` and `audit-log` domains are also
/// deliberately distinct from the legacy `ratelimit` and `audit` markers, so
/// a v1 key cannot alias a legacy key during a rolling cutover.
#[derive(Clone)]
struct RedisKeyNamespace {
    encoded_namespace: Arc<str>,
}

impl RedisKeyNamespace {
    fn new(namespace: &str) -> Self {
        Self {
            encoded_namespace: encode_redis_component(namespace).into(),
        }
    }

    fn limit(&self, key: &str) -> String {
        format!("{REDIS_KEY_ROOT}:{}:limits:{key}", self.encoded_namespace)
    }

    fn audit(&self, key: &str) -> String {
        format!(
            "{REDIS_KEY_ROOT}:{}:audit-log:{}",
            self.encoded_namespace,
            encode_redis_component(key)
        )
    }
}

impl RedisStateBackend {
    /// Connect and perform a readiness ping before serving traffic.
    pub async fn connect(
        url: &str,
        namespace: impl Into<Arc<str>>,
    ) -> Result<Self, StateBackendError> {
        let client = redis::Client::open(url).map_err(|_| StateBackendError::Unavailable)?;
        let mut connection = client
            .get_connection_manager()
            .await
            .map_err(|_| StateBackendError::Unavailable)?;
        redis::cmd("PING")
            .query_async::<String>(&mut connection)
            .await
            .map_err(|_| StateBackendError::Unavailable)?;
        let namespace = namespace.into();
        Ok(Self {
            connection,
            keys: RedisKeyNamespace::new(&namespace),
            degraded: Arc::new(AtomicBool::new(false)),
        })
    }

    fn key(&self, key: &str) -> String {
        self.keys.limit(key)
    }

    fn audit_key(&self, key: &str) -> String {
        self.keys.audit(key)
    }

    fn mark<T>(&self, result: Result<T, StateBackendError>) -> Result<T, StateBackendError> {
        match result {
            Ok(value) => {
                self.degraded.store(false, Ordering::Relaxed);
                Ok(value)
            }
            Err(error) => {
                self.degraded.store(true, Ordering::Relaxed);
                Err(error)
            }
        }
    }
}

impl StateBackend for RedisStateBackend {
    fn backend_name(&self) -> &'static str {
        "redis"
    }

    fn admit<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        _now_seconds: f64,
    ) -> StateFuture<'a, RateResult> {
        Box::pin(async move {
            let config = config.validate()?;
            let window_ms = (config.window_seconds * 1_000.0).ceil().max(1.0) as u64;
            let mut connection = self.connection.clone();
            let value: Result<(i64, i64, i64), _> = Script::new(ADMIT_SCRIPT)
                .key(self.key(key))
                .arg(window_ms)
                .arg(config.rpm.unwrap_or(0))
                .arg(config.tpm.unwrap_or(0))
                .invoke_async(&mut connection)
                .await;
            self.mark(
                value
                    .map_err(|_| StateBackendError::Unavailable)
                    .and_then(redis_result),
            )
        })
    }

    fn reserve<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        tokens: u64,
        _now_seconds: f64,
    ) -> StateFuture<'a, ReservationResult> {
        Box::pin(async move {
            let config = config.validate()?;
            let window_ms = (config.window_seconds * 1_000.0).ceil().max(1.0) as u64;
            let mut connection = self.connection.clone();
            let value: Result<(i64, i64, i64, i64), _> = Script::new(RESERVE_SCRIPT)
                .key(self.key(key))
                .arg(window_ms)
                .arg(config.rpm.unwrap_or(0))
                .arg(config.tpm.unwrap_or(0))
                .arg(tokens)
                .arg(1)
                .invoke_async(&mut connection)
                .await;
            self.mark(
                value
                    .map_err(|_| StateBackendError::Unavailable)
                    .and_then(|value| redis_reservation_result(value, tokens, true)),
            )
        })
    }

    fn reserve_tokens<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        tokens: u64,
        _now_seconds: f64,
    ) -> StateFuture<'a, ReservationResult> {
        Box::pin(async move {
            let config = config.validate()?;
            if config.tpm.is_none() {
                return Ok(ReservationResult {
                    rate: RateResult {
                        limited_by: None,
                        retry_after_seconds: 0,
                    },
                    reservation: None,
                });
            }
            let window_ms = (config.window_seconds * 1_000.0).ceil().max(1.0) as u64;
            let mut connection = self.connection.clone();
            let value: Result<(i64, i64, i64, i64), _> = Script::new(RESERVE_SCRIPT)
                .key(self.key(key))
                .arg(window_ms)
                .arg(config.rpm.unwrap_or(0))
                .arg(config.tpm.unwrap_or(0))
                .arg(tokens)
                .arg(0)
                .invoke_async(&mut connection)
                .await;
            self.mark(
                value
                    .map_err(|_| StateBackendError::Unavailable)
                    .and_then(|value| redis_reservation_result(value, tokens, false)),
            )
        })
    }

    fn reconcile<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        reservation: RateReservation,
        actual_tokens: u64,
        _now_seconds: f64,
    ) -> StateFuture<'a, bool> {
        Box::pin(async move {
            config.validate()?;
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = Script::new(RECONCILE_SCRIPT)
                .key(self.key(key))
                .arg(reservation.window_id())
                .arg(reservation.reserved_tokens())
                .arg(actual_tokens)
                .invoke_async(&mut connection)
                .await;
            self.mark(
                result
                    .map(|value| value == 1)
                    .map_err(|_| StateBackendError::Unavailable),
            )
        })
    }

    fn rollback<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        reservation: RateReservation,
        _now_seconds: f64,
    ) -> StateFuture<'a, bool> {
        Box::pin(async move {
            config.validate()?;
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = Script::new(ROLLBACK_SCRIPT)
                .key(self.key(key))
                .arg(reservation.window_id())
                .arg(reservation.reserved_tokens())
                .arg(if reservation.reserved_request() { 1 } else { 0 })
                .invoke_async(&mut connection)
                .await;
            self.mark(
                result
                    .map(|value| value == 1)
                    .map_err(|_| StateBackendError::Unavailable),
            )
        })
    }

    fn add_tokens<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        tokens: u64,
        _now_seconds: f64,
    ) -> StateFuture<'a, ()> {
        Box::pin(async move {
            let config = config.validate()?;
            let window_ms = (config.window_seconds * 1_000.0).ceil().max(1.0) as u64;
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = Script::new(ADD_TOKENS_SCRIPT)
                .key(self.key(key))
                .arg(window_ms)
                .arg(tokens)
                .invoke_async(&mut connection)
                .await;
            self.mark(
                result
                    .map(|_| ())
                    .map_err(|_| StateBackendError::Unavailable),
            )
        })
    }

    fn snapshot<'a>(
        &'a self,
        key: &'a str,
        config: StateLimitConfig,
        _now_seconds: f64,
    ) -> StateFuture<'a, Option<RateSnapshot>> {
        Box::pin(async move {
            let config = config.validate()?;
            let Some(limit) = config.rpm else {
                return Ok(None);
            };
            let window_ms = (config.window_seconds * 1_000.0).ceil().max(1.0) as u64;
            let mut connection = self.connection.clone();
            let value: Result<(i64, i64), _> = Script::new(SNAPSHOT_SCRIPT)
                .key(self.key(key))
                .arg(window_ms)
                .invoke_async(&mut connection)
                .await;
            self.mark(
                value
                    .map_err(|_| StateBackendError::Unavailable)
                    .and_then(|value| {
                        u64::try_from(value.0)
                            .ok()
                            .zip(u64::try_from(value.1).ok())
                            .map(|(requests, reset_seconds)| {
                                Some(RateSnapshot {
                                    limit,
                                    remaining: limit.saturating_sub(requests),
                                    reset_seconds,
                                })
                            })
                            .ok_or(StateBackendError::Unavailable)
                    }),
            )
        })
    }

    fn append_audit<'a>(
        &'a self,
        key: &'a str,
        event: &'a str,
        max_events: usize,
    ) -> StateFuture<'a, ()> {
        Box::pin(async move {
            if max_events == 0 {
                return Err(StateBackendError::Unavailable);
            }
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = Script::new(APPEND_AUDIT_SCRIPT)
                .key(self.audit_key(key))
                .arg(event)
                .arg(max_events)
                .invoke_async(&mut connection)
                .await;
            self.mark(
                result
                    .map(|_| ())
                    .map_err(|_| StateBackendError::Unavailable),
            )
        })
    }

    fn degraded(&self) -> bool {
        self.degraded.load(Ordering::Relaxed)
    }
}

fn redis_result(value: (i64, i64, i64)) -> Result<RateResult, StateBackendError> {
    let allowed = value.0 == 1;
    let limited_by = match value.1 {
        0 => None,
        1 => Some(LimitKind::Requests),
        2 => Some(LimitKind::Tokens),
        _ => return Err(StateBackendError::Unavailable),
    };
    let retry_after_seconds = u64::try_from(value.2).map_err(|_| StateBackendError::Unavailable)?;
    if allowed != limited_by.is_none() {
        return Err(StateBackendError::Unavailable);
    }
    Ok(RateResult {
        limited_by,
        retry_after_seconds,
    })
}

fn redis_reservation_result(
    value: (i64, i64, i64, i64),
    reserved_tokens: u64,
    reserved_request: bool,
) -> Result<ReservationResult, StateBackendError> {
    let rate = redis_result((value.0, value.1, value.2))?;
    let reservation = if rate.allowed() {
        Some(RateReservation {
            window_id: u64::try_from(value.3).map_err(|_| StateBackendError::Unavailable)?,
            reserved_tokens,
            reserved_request,
        })
    } else {
        None
    };
    Ok(ReservationResult { rate, reservation })
}

/// Build the configured backend before the listener starts.
pub async fn from_config(
    config: &wayfinder_config::gateway::StateConfig,
) -> Result<Arc<dyn StateBackend>, StateBackendError> {
    match config.backend.as_str() {
        "memory" => Ok(Arc::new(MemoryStateBackend::default())),
        "redis" => {
            let url = config
                .url
                .as_deref()
                .ok_or(StateBackendError::Unavailable)?;
            Ok(Arc::new(
                RedisStateBackend::connect(url, Arc::<str>::from(config.namespace.as_str()))
                    .await?,
            ))
        }
        _ => Err(StateBackendError::Unavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limit(rpm: u64) -> StateLimitConfig {
        StateLimitConfig {
            rpm: Some(rpm),
            tpm: None,
            window_seconds: 60.0,
        }
    }

    #[tokio::test]
    async fn memory_backend_shares_a_window_between_policy_handles() -> Result<(), StateBackendError>
    {
        let backend = MemoryStateBackend::default();
        assert!(backend.admit("prod", limit(2), 1.0).await?.allowed());
        assert!(backend.admit("prod", limit(2), 1.0).await?.allowed());
        assert_eq!(
            backend.admit("prod", limit(2), 1.0).await?.limited_by,
            Some(LimitKind::Requests)
        );
        assert!(backend.admit("prod", limit(2), 61.0).await?.allowed());
        Ok(())
    }

    #[tokio::test]
    async fn memory_backend_accounts_tokens_in_the_same_window() -> Result<(), StateBackendError> {
        let backend = MemoryStateBackend::default();
        let config = StateLimitConfig {
            rpm: None,
            tpm: Some(10),
            window_seconds: 60.0,
        };
        assert!(backend.admit("prod", config, 1.0).await?.allowed());
        backend.add_tokens("prod", config, 10, 1.0).await?;
        assert_eq!(
            backend.admit("prod", config, 1.0).await?.limited_by,
            Some(LimitKind::Tokens)
        );
        Ok(())
    }

    #[tokio::test]
    async fn memory_backend_reserves_reconciles_and_ignores_late_completion()
    -> Result<(), StateBackendError> {
        let backend = MemoryStateBackend::default();
        let config = StateLimitConfig {
            rpm: Some(2),
            tpm: Some(100),
            window_seconds: 60.0,
        };
        let reservation = backend
            .reserve("prod", config, 80, 1.0)
            .await?
            .reservation
            .ok_or(StateBackendError::Unavailable)?;
        assert_eq!(
            backend
                .reserve("prod", config, 21, 2.0)
                .await?
                .rate
                .limited_by,
            Some(LimitKind::Tokens)
        );
        assert!(
            backend
                .reconcile("prod", config, reservation, 20, 3.0)
                .await?
        );
        let next = backend
            .reserve("prod", config, 80, 60.0)
            .await?
            .reservation
            .ok_or(StateBackendError::Unavailable)?;
        assert!(
            !backend
                .reconcile("prod", config, reservation, 90, 61.0)
                .await?
        );
        assert!(backend.rollback("prod", config, next, 61.0).await?);
        Ok(())
    }

    #[test]
    fn redis_result_rejects_malformed_script_values() {
        assert!(redis_result((1, 1, 0)).is_err());
        assert!(redis_result((0, 9, 0)).is_err());
        assert!(redis_result((0, 1, -1)).is_err());
        assert!(redis_reservation_result((1, 0, 0, -1), 10, true).is_err());
        assert_eq!(
            redis_reservation_result((1, 0, 0, 7), 10, true)
                .ok()
                .and_then(|result| result.reservation)
                .map(RateReservation::window_id),
            Some(7)
        );
    }

    #[test]
    fn redis_v1_keys_encode_every_operator_controlled_component() {
        let keys = RedisKeyNamespace::new("prod:europe");

        assert_eq!(
            keys.limit(&virtual_key_limit_key("team:a")),
            "wayfinder:v1:cHJvZDpldXJvcGU:limits:virtual-key:dGVhbTph"
        );
        assert_eq!(
            keys.limit(&workspace_limit_key("research/west")),
            "wayfinder:v1:cHJvZDpldXJvcGU:limits:workspace:cmVzZWFyY2gvd2VzdA"
        );
        assert_eq!(
            keys.limit(&global_limit_key()),
            "wayfinder:v1:cHJvZDpldXJvcGU:limits:global"
        );
        assert_eq!(
            keys.audit("prod:europe"),
            "wayfinder:v1:cHJvZDpldXJvcGU:audit-log:cHJvZDpldXJvcGU"
        );
        assert_eq!(
            RedisKeyNamespace::new("???").limit(&workspace_limit_key(">>>")),
            "wayfinder:v1:Pz8_:limits:workspace:Pj4-"
        );
        assert_eq!(virtual_key_limit_key("?").as_ref(), "virtual-key:Pw");
    }

    #[test]
    fn redis_v1_limit_keys_do_not_repeat_legacy_delimiter_collisions() {
        fn legacy_virtual_limit_key(namespace: &str, id: &str) -> String {
            format!("{namespace}:ratelimit:{namespace}:key:{id}")
        }

        let first_namespace = "a";
        let first_id = "x:ratelimit:a:ratelimit:a:key:x:key:y";
        let second_namespace = "a:ratelimit:a:key:x";
        let second_id = "y";

        assert_eq!(
            legacy_virtual_limit_key(first_namespace, first_id),
            legacy_virtual_limit_key(second_namespace, second_id),
            "the regression pair must collide under the legacy concatenation"
        );

        let first = RedisKeyNamespace::new(first_namespace).limit(&virtual_key_limit_key(first_id));
        let second =
            RedisKeyNamespace::new(second_namespace).limit(&virtual_key_limit_key(second_id));

        assert_ne!(first, second);
        assert!(first.starts_with("wayfinder:v1:"));
        assert!(second.starts_with("wayfinder:v1:"));
        assert!(!first.contains(":ratelimit:"));
        assert!(!second.contains(":ratelimit:"));
    }

    #[test]
    fn redis_v1_audit_keys_do_not_repeat_legacy_delimiter_collisions() {
        fn legacy_audit_key(namespace: &str, stream: &str) -> String {
            format!("{namespace}:audit:{stream}")
        }

        let first_namespace = "tenant";
        let first_stream = "west:audit:events";
        let second_namespace = "tenant:audit:west";
        let second_stream = "events";

        assert_eq!(
            legacy_audit_key(first_namespace, first_stream),
            legacy_audit_key(second_namespace, second_stream),
            "the regression pair must collide under the legacy concatenation"
        );

        let first = RedisKeyNamespace::new(first_namespace).audit(first_stream);
        let second = RedisKeyNamespace::new(second_namespace).audit(second_stream);

        assert_ne!(first, second);
        assert!(!first.contains(":audit:"));
        assert!(!second.contains(":audit:"));
    }

    #[test]
    fn shared_audit_script_appends_then_trims_atomically() {
        assert!(matches!(
            (
                APPEND_AUDIT_SCRIPT.find("RPUSH"),
                APPEND_AUDIT_SCRIPT.find("LTRIM")
            ),
            (Some(append), Some(trim)) if append < trim
        ));
        assert!(APPEND_AUDIT_SCRIPT.contains("-max_events, -1"));
    }
}
