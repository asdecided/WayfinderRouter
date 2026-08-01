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

use redis::Script;
use redis::aio::ConnectionManager;
use thiserror::Error;

use crate::rate_limit::{LimitKind, RateResult, RateSnapshot};

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
    fn append_audit<'a>(&'a self, _key: &'a str, _event: &'a str) -> StateFuture<'a, ()> {
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

/// Redis-backed fixed-window counters using server time and Lua atomicity.
#[derive(Clone)]
pub struct RedisStateBackend {
    connection: ConnectionManager,
    namespace: Arc<str>,
    degraded: Arc<AtomicBool>,
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
        Ok(Self {
            connection,
            namespace: namespace.into(),
            degraded: Arc::new(AtomicBool::new(false)),
        })
    }

    fn key(&self, key: &str) -> String {
        format!("{}:ratelimit:{key}", self.namespace)
    }

    fn audit_key(&self, key: &str) -> String {
        format!("{}:audit:{key}", self.namespace)
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
            let value: (i64, i64, i64) = Script::new(ADMIT_SCRIPT)
                .key(self.key(key))
                .arg(window_ms)
                .arg(config.rpm.unwrap_or(0))
                .arg(config.tpm.unwrap_or(0))
                .invoke_async(&mut connection)
                .await
                .map_err(|_| StateBackendError::Unavailable)?;
            self.mark(redis_result(value))
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
            let value: (i64, i64) = Script::new(SNAPSHOT_SCRIPT)
                .key(self.key(key))
                .arg(window_ms)
                .invoke_async(&mut connection)
                .await
                .map_err(|_| StateBackendError::Unavailable)?;
            self.mark(
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
                    .ok_or(StateBackendError::Unavailable),
            )
        })
    }

    fn append_audit<'a>(&'a self, key: &'a str, event: &'a str) -> StateFuture<'a, ()> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = redis::cmd("RPUSH")
                .arg(self.audit_key(key))
                .arg(event)
                .query_async(&mut connection)
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

    #[test]
    fn redis_result_rejects_malformed_script_values() {
        assert!(redis_result((1, 1, 0)).is_err());
        assert!(redis_result((0, 9, 0)).is_err());
        assert!(redis_result((0, 1, -1)).is_err());
    }
}
