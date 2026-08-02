//! Shared policy state for replicated gateway processes.
//!
//! `memory` remains the zero-configuration default. The opt-in Redis backend
//! provides fixed-window request/token counters using Lua and Redis server
//! time, while callers can fall back to the bounded local limiter on outage.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
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
use wayfinder_service::pricing::TurnCost;

use crate::rate_limit::{LimitKind, RateReservation, RateResult, RateSnapshot, ReservationResult};

const REDIS_KEY_ROOT: &str = "wayfinder:v1";
const SHARED_COST_SCALE: f64 = 1_000_000.0;
const SHARED_QUALITY_SCALE: f64 = 1_000_000.0;
type CanaryRedisSnapshot = (i64, i64, i64, i64, i64, i64, i64, i64, i64, String);
const SHARED_LEDGER_DAY_TTL_SECONDS: u64 = 400 * 24 * 60 * 60;
const SHARED_LEDGER_MONTH_TTL_SECONDS: u64 = 5 * 365 * 24 * 60 * 60;
const SHARED_LEDGER_IDEMPOTENCY_TTL_SECONDS: u64 = 5 * 365 * 24 * 60 * 60;
const SHARED_BUDGET_RESERVATION_TTL_MS: u64 = 6 * 60 * 60 * 1_000;

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

/// One prompt-free cost observation written to the shared ledger.
#[derive(Clone, Debug, PartialEq)]
pub struct SharedCostObservation {
    /// Request identity used to make retries idempotent.
    pub request_id: String,
    /// UTC date bucket in `YYYY-MM-DD` form.
    pub date: String,
    /// Dimensions that receive the observation, such as `global`, `key:<id>`,
    /// `workspace:<id>`, and `route:<model>`.
    pub scopes: Vec<String>,
    /// Realized provider cost in the configured accounting unit.
    pub realized: f64,
    /// Counterfactual baseline cost in the configured accounting unit.
    pub baseline: f64,
    /// Baseline minus realized cost.
    pub savings: f64,
    /// Prompt/input tokens.
    pub prompt_tokens: u64,
    /// Completion/output tokens.
    pub completion_tokens: u64,
    /// Whether one or both token counts were estimated.
    pub estimated: bool,
}

impl SharedCostObservation {
    /// Build a bounded observation from an already-accounted turn.
    #[must_use]
    pub fn from_turn(
        request_id: impl Into<String>,
        date: impl Into<String>,
        cost: &TurnCost,
        scopes: Vec<String>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            date: date.into(),
            scopes,
            realized: cost.realized,
            baseline: cost.baseline,
            savings: cost.savings,
            prompt_tokens: cost.prompt_tokens,
            completion_tokens: cost.completion_tokens,
            estimated: cost.estimated,
        }
    }
}

/// Shared spend returned by one configured budget dimension.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SharedSpend {
    /// Realized cost in the configured accounting unit.
    pub realized: f64,
}

/// One budget dimension participating in an atomic fleet reservation.
#[derive(Clone, Debug, PartialEq)]
pub struct SharedBudgetScope {
    /// Encoded accounting dimension, for example `global` or `key:team-a`.
    pub scope: String,
    /// Accounting window used by this cap.
    pub window: String,
    /// Spend ceiling in the configured accounting unit.
    pub limit: f64,
    /// Whether reaching the ceiling blocks online delivery.
    pub blocks: bool,
}

/// Conservative, idempotent spend reservation held until terminal accounting.
#[derive(Clone, Debug, PartialEq)]
pub struct SharedBudgetReservation {
    /// Request identity used for idempotent reserve/release/settlement.
    pub request_id: String,
    /// UTC date bucket used to select day and month ledgers.
    pub date: String,
    /// Conservative amount reserved in each participating scope.
    pub amount: f64,
    /// Budget dimensions covered by the reservation.
    pub scopes: Vec<SharedBudgetScope>,
}

/// One bounded exact-response cache insertion sent to the shared backend.
#[derive(Clone, Debug, PartialEq)]
pub struct SharedCachePut {
    /// Current cache/config generation.
    pub generation: String,
    /// SHA-256 cache key.
    pub key: String,
    /// Opaque serialized response value.
    pub value: Vec<u8>,
    /// Retained response-body bytes used for the operator byte bound.
    pub body_bytes: usize,
    /// Entry lifetime in seconds; zero means no expiry.
    pub ttl_seconds: f64,
    /// Maximum number of retained entries.
    pub max_entries: usize,
    /// Maximum aggregate retained response-body bytes.
    pub max_bytes: usize,
}

/// One prompt-free canary outcome written to fleet state.
#[derive(Clone, Debug, PartialEq)]
pub struct SharedCanaryObservation {
    /// Stable rollout identifier.
    pub rollout_id: String,
    /// Request identity used for idempotent observation writes.
    pub request_id: String,
    /// Whether the canary response completed successfully.
    pub succeeded: bool,
    /// End-to-end provider latency in milliseconds.
    pub latency_ms: u64,
    /// Realized canary cost, when priced usage was available.
    pub realized_cost: Option<f64>,
    /// Counterfactual baseline cost, when priced usage was available.
    pub baseline_cost: Option<f64>,
    /// Quality loss relative to the approved baseline, when labelled.
    pub quality_loss: Option<f64>,
}

/// Bounded fleet-wide canary counters for one fixed window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SharedCanarySnapshot {
    /// Number of unique canary observations in the active window.
    pub requests: u64,
    /// Number of failed observations in the active window.
    pub errors: u64,
    /// Sum of observed latency samples.
    pub latency_sum_ms: u64,
    /// Number of latency samples.
    pub latency_samples: u64,
    /// Sum of realized priced cost.
    pub realized_cost: f64,
    /// Sum of counterfactual baseline priced cost.
    pub baseline_cost: f64,
    /// Number of cost samples.
    pub cost_samples: u64,
    /// Sum of labelled quality loss.
    pub quality_loss: f64,
    /// Number of quality samples.
    pub quality_samples: u64,
    /// Rollout tripwire reason, if a replica has stopped the rollout.
    pub tripped_reason: Option<String>,
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

    /// Record one cost observation across all requested dimensions.
    ///
    /// Implementations must make the operation idempotent by `request_id`.
    /// The default backend deliberately returns unavailable so existing local
    /// ledger behaviour remains the fallback contract.
    fn record_cost<'a>(&'a self, _observation: SharedCostObservation) -> StateFuture<'a, bool> {
        Box::pin(async { Err(StateBackendError::Unavailable) })
    }

    /// Read realized spend for one shared ledger dimension and window.
    fn spend<'a>(
        &'a self,
        _window: &'a str,
        _scope: &'a str,
        _date: &'a str,
    ) -> StateFuture<'a, SharedSpend> {
        Box::pin(async { Err(StateBackendError::Unavailable) })
    }

    /// Atomically reserve conservative spend across all hard budget scopes.
    fn reserve_budget<'a>(
        &'a self,
        _reservation: SharedBudgetReservation,
    ) -> StateFuture<'a, bool> {
        Box::pin(async { Err(StateBackendError::Unavailable) })
    }

    /// Release a reservation after a failed or cancelled delivery.
    fn release_budget<'a>(&'a self, _reservation: SharedBudgetReservation) -> StateFuture<'a, ()> {
        Box::pin(async { Err(StateBackendError::Unavailable) })
    }

    /// Settle a completed request by releasing its reserved amount. The
    /// realized amount is recorded separately by `record_cost`.
    fn settle_budget<'a>(&'a self, reservation: SharedBudgetReservation) -> StateFuture<'a, ()> {
        self.release_budget(reservation)
    }

    /// Acquire one fleet-wide in-flight lease.
    fn acquire_admission<'a>(
        &'a self,
        _limit: u64,
        _lease_id: &'a str,
        _ttl_ms: u64,
    ) -> StateFuture<'a, bool> {
        Box::pin(async { Err(StateBackendError::Unavailable) })
    }

    /// Release one fleet-wide in-flight lease.
    fn release_admission<'a>(&'a self, _lease_id: &'a str) -> StateFuture<'a, ()> {
        Box::pin(async { Err(StateBackendError::Unavailable) })
    }

    /// Claim a provider health attempt, including the single half-open probe.
    fn begin_provider_health<'a>(
        &'a self,
        _target: &'a str,
        _threshold: u64,
        _cooldown_ms: u64,
        _probe_id: &'a str,
    ) -> StateFuture<'a, bool> {
        Box::pin(async { Err(StateBackendError::Unavailable) })
    }

    /// Record one provider health outcome.
    fn complete_provider_health<'a>(
        &'a self,
        _target: &'a str,
        _threshold: u64,
        _cooldown_ms: u64,
        _probe_id: &'a str,
        _succeeded: bool,
    ) -> StateFuture<'a, ()> {
        Box::pin(async { Err(StateBackendError::Unavailable) })
    }

    /// Release a half-open probe without counting it as a provider failure.
    fn release_provider_health<'a>(
        &'a self,
        _target: &'a str,
        _probe_id: &'a str,
    ) -> StateFuture<'a, ()> {
        Box::pin(async { Err(StateBackendError::Unavailable) })
    }

    /// Read one bounded, exact-response cache value from shared state.
    ///
    /// The value is opaque to the state backend. Callers own serialization and
    /// must ensure it contains no prompt-free metadata beyond the response
    /// contract they intentionally retain.
    fn shared_cache_get<'a>(
        &'a self,
        _generation: &'a str,
        _key: &'a str,
    ) -> StateFuture<'a, Option<Vec<u8>>> {
        Box::pin(async { Err(StateBackendError::Unavailable) })
    }

    /// Insert one exact-response cache value, enforcing shared entry and body
    /// bounds atomically when the backend supports it.
    fn shared_cache_put<'a>(&'a self, _put: SharedCachePut) -> StateFuture<'a, bool> {
        Box::pin(async { Err(StateBackendError::Unavailable) })
    }

    /// Purge all values in the current shared cache generation.
    fn shared_cache_clear<'a>(&'a self, _generation: &'a str) -> StateFuture<'a, ()> {
        Box::pin(async { Err(StateBackendError::Unavailable) })
    }

    /// Record one idempotent canary outcome in the active fleet window.
    fn record_canary<'a>(
        &'a self,
        _observation: SharedCanaryObservation,
        _window_seconds: f64,
        _now_seconds: f64,
    ) -> StateFuture<'a, bool> {
        Box::pin(async { Err(StateBackendError::Unavailable) })
    }

    /// Read the bounded canary counters and current trip state.
    fn canary_snapshot<'a>(
        &'a self,
        _rollout_id: &'a str,
        _window_seconds: f64,
        _now_seconds: f64,
    ) -> StateFuture<'a, SharedCanarySnapshot> {
        Box::pin(async { Err(StateBackendError::Unavailable) })
    }

    /// Stop a rollout for the bounded hold interval.
    fn trip_canary<'a>(
        &'a self,
        _rollout_id: &'a str,
        _reason: &'a str,
        _hold_seconds: f64,
        _now_seconds: f64,
    ) -> StateFuture<'a, ()> {
        Box::pin(async { Err(StateBackendError::Unavailable) })
    }

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
    canary: Arc<Mutex<BTreeMap<String, MemoryCanaryState>>>,
}

#[derive(Clone, Debug, Default)]
struct MemoryCanaryState {
    window: u64,
    seen: BTreeSet<String>,
    snapshot: SharedCanarySnapshot,
    tripped_until: f64,
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

    fn record_canary<'a>(
        &'a self,
        observation: SharedCanaryObservation,
        window_seconds: f64,
        now_seconds: f64,
    ) -> StateFuture<'a, bool> {
        Box::pin(async move {
            validate_canary_inputs(&observation, window_seconds)?;
            let window = canary_window(window_seconds, now_seconds)?;
            let mut states = self
                .canary
                .lock()
                .map_err(|_| StateBackendError::LockPoisoned)?;
            let state = states.entry(observation.rollout_id).or_default();
            rotate_memory_canary(state, window);
            if !state.seen.insert(observation.request_id) {
                return Ok(false);
            }
            state.snapshot.requests = state.snapshot.requests.saturating_add(1);
            if !observation.succeeded {
                state.snapshot.errors = state.snapshot.errors.saturating_add(1);
            }
            state.snapshot.latency_sum_ms = state
                .snapshot
                .latency_sum_ms
                .saturating_add(observation.latency_ms);
            state.snapshot.latency_samples = state.snapshot.latency_samples.saturating_add(1);
            if let (Some(realized), Some(baseline)) =
                (observation.realized_cost, observation.baseline_cost)
            {
                if realized.is_finite()
                    && baseline.is_finite()
                    && realized >= 0.0
                    && baseline >= 0.0
                {
                    state.snapshot.realized_cost += realized;
                    state.snapshot.baseline_cost += baseline;
                    state.snapshot.cost_samples = state.snapshot.cost_samples.saturating_add(1);
                }
            }
            if let Some(loss) = observation.quality_loss {
                if loss.is_finite() && (0.0..=1.0).contains(&loss) {
                    state.snapshot.quality_loss += loss;
                    state.snapshot.quality_samples =
                        state.snapshot.quality_samples.saturating_add(1);
                }
            }
            Ok(true)
        })
    }

    fn canary_snapshot<'a>(
        &'a self,
        rollout_id: &'a str,
        window_seconds: f64,
        now_seconds: f64,
    ) -> StateFuture<'a, SharedCanarySnapshot> {
        Box::pin(async move {
            if rollout_id.is_empty() || rollout_id.len() > 128 {
                return Err(StateBackendError::Unavailable);
            }
            let window = canary_window(window_seconds, now_seconds)?;
            let mut states = self
                .canary
                .lock()
                .map_err(|_| StateBackendError::LockPoisoned)?;
            let state = states.entry(rollout_id.to_owned()).or_default();
            rotate_memory_canary(state, window);
            if state.tripped_until <= now_seconds {
                state.tripped_until = 0.0;
                state.snapshot.tripped_reason = None;
            }
            Ok(state.snapshot.clone())
        })
    }

    fn trip_canary<'a>(
        &'a self,
        rollout_id: &'a str,
        reason: &'a str,
        hold_seconds: f64,
        now_seconds: f64,
    ) -> StateFuture<'a, ()> {
        Box::pin(async move {
            if rollout_id.is_empty()
                || rollout_id.len() > 128
                || reason.is_empty()
                || reason.len() > 128
                || !hold_seconds.is_finite()
                || hold_seconds <= 0.0
            {
                return Err(StateBackendError::Unavailable);
            }
            let mut states = self
                .canary
                .lock()
                .map_err(|_| StateBackendError::LockPoisoned)?;
            let state = states.entry(rollout_id.to_owned()).or_default();
            state.tripped_until = state.tripped_until.max(now_seconds + hold_seconds);
            state.snapshot.tripped_reason = Some(reason.to_owned());
            Ok(())
        })
    }

    fn degraded(&self) -> bool {
        false
    }
}

fn validate_canary_inputs(
    observation: &SharedCanaryObservation,
    window_seconds: f64,
) -> Result<(), StateBackendError> {
    if observation.rollout_id.is_empty()
        || observation.rollout_id.len() > 128
        || observation.request_id.is_empty()
        || observation.request_id.len() > 128
        || !window_seconds.is_finite()
        || window_seconds <= 0.0
        || observation.latency_ms > 86_400_000
    {
        return Err(StateBackendError::Unavailable);
    }
    Ok(())
}

fn non_negative_u64(value: i64) -> Result<u64, StateBackendError> {
    u64::try_from(value).map_err(|_| StateBackendError::Unavailable)
}

fn canary_window(window_seconds: f64, now_seconds: f64) -> Result<u64, StateBackendError> {
    if !window_seconds.is_finite() || window_seconds <= 0.0 || !now_seconds.is_finite() {
        return Err(StateBackendError::Unavailable);
    }
    Ok((now_seconds / window_seconds).floor().max(0.0) as u64)
}

fn rotate_memory_canary(state: &mut MemoryCanaryState, window: u64) {
    if state.window == window {
        return;
    }
    let tripped_reason = state.snapshot.tripped_reason.take();
    state.window = window;
    state.seen.clear();
    state.snapshot = SharedCanarySnapshot {
        tripped_reason,
        ..SharedCanarySnapshot::default()
    };
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

const RECORD_COST_SCRIPT: &str = r#"
-- KEYS[1] is the idempotency set. Remaining keys are the day/month/all
-- aggregate hashes for each requested accounting scope.
if redis.call('SADD', KEYS[1], ARGV[1]) == 0 then return 0 end
local realized = ARGV[2]
local baseline = ARGV[3]
local savings = ARGV[4]
local prompt = ARGV[5]
local completion = ARGV[6]
local estimated = ARGV[7]
for index = 2, #KEYS do
  redis.call('HINCRBY', KEYS[index], 'requests', 1)
  redis.call('HINCRBY', KEYS[index], 'estimated_requests', estimated)
  redis.call('HINCRBY', KEYS[index], 'prompt_tokens', prompt)
  redis.call('HINCRBY', KEYS[index], 'completion_tokens', completion)
  redis.call('HINCRBY', KEYS[index], 'realized_micros', realized)
  redis.call('HINCRBY', KEYS[index], 'baseline_micros', baseline)
  redis.call('HINCRBY', KEYS[index], 'savings_micros', savings)
  local period_offset = (index - 2) % 3
  if period_offset == 0 then
    redis.call('PEXPIRE', KEYS[index], tonumber(ARGV[8]) * 1000)
  elseif period_offset == 1 then
    redis.call('PEXPIRE', KEYS[index], tonumber(ARGV[9]) * 1000)
  end
end
redis.call('PEXPIRE', KEYS[1], tonumber(ARGV[10]) * 1000)
return 1
"#;

const RESERVE_BUDGET_SCRIPT: &str = r#"
local request_id = ARGV[1]
local amount = tonumber(ARGV[2])
local scope_count = tonumber(ARGV[3])
if request_id == nil or amount == nil or amount < 0 or scope_count == nil or scope_count < 1 then
  return redis.error_reply('invalid budget reservation')
end
local field = request_id
local scope_arg_stride = 4
local time = redis.call('TIME')
local now_ms = (tonumber(time[1]) * 1000) + math.floor(tonumber(time[2]) / 1000)
local ttl_ms = tonumber(ARGV[4 + scope_count * scope_arg_stride])
for scope_index = 1, scope_count do
  local key_base = (scope_index - 1) * 3
  local expiry = KEYS[key_base + 2]
  local amounts = KEYS[key_base + 3]
  local expired = redis.call('ZRANGEBYSCORE', expiry, '-inf', now_ms)
  for _, expired_id in ipairs(expired) do
    local expired_amount = tonumber(redis.call('HGET', amounts, expired_id) or '0')
    if expired_amount > 0 then
      redis.call('HINCRBY', KEYS[key_base + 1], 'reserved_micros', -expired_amount)
      redis.call('HDEL', amounts, expired_id)
    end
    redis.call('ZREM', expiry, expired_id)
  end
end
local existing_count = 0
for scope_index = 1, scope_count do
  local key_base = (scope_index - 1) * 3
  local amounts = KEYS[key_base + 3]
  if redis.call('HEXISTS', amounts, field) == 1 then existing_count = existing_count + 1 end
end
if existing_count == scope_count then return 1 end
if existing_count > 0 then return redis.error_reply('partial budget reservation') end
for scope_index = 1, scope_count do
  local key_base = (scope_index - 1) * 3
  local arg_base = 4 + (scope_index - 1) * scope_arg_stride
  local limit = tonumber(ARGV[arg_base])
  local blocks = tonumber(ARGV[arg_base + 1])
  if blocks == 1 then
    local realized = tonumber(redis.call('HGET', KEYS[key_base + 1], 'realized_micros') or '0')
    local reserved = tonumber(redis.call('HGET', KEYS[key_base + 1], 'reserved_micros') or '0')
    if realized + reserved + amount > limit then return 0 end
  end
end
for scope_index = 1, scope_count do
  local key_base = (scope_index - 1) * 3
  local arg_base = 4 + (scope_index - 1) * scope_arg_stride
  redis.call('HINCRBY', KEYS[key_base + 1], 'reserved_micros', amount)
  local ledger_ttl_ms = tonumber(ARGV[arg_base + 2])
  if redis.call('PTTL', KEYS[key_base + 1]) < 0 and ledger_ttl_ms ~= nil and ledger_ttl_ms > 0 then
    redis.call('PEXPIRE', KEYS[key_base + 1], ledger_ttl_ms)
  end
  redis.call('HSET', KEYS[key_base + 3], field, amount)
  redis.call('ZADD', KEYS[key_base + 2], now_ms + ttl_ms, field)
  redis.call('PEXPIRE', KEYS[key_base + 2], ttl_ms)
  redis.call('PEXPIRE', KEYS[key_base + 3], ttl_ms)
end
return 1
"#;

const RELEASE_BUDGET_SCRIPT: &str = r#"
local field = ARGV[1]
for scope_index = 1, #KEYS, 3 do
  local ledger = KEYS[scope_index]
  local expiry = KEYS[scope_index + 1]
  local amounts = KEYS[scope_index + 2]
  local reserved = tonumber(redis.call('HGET', amounts, field) or '0')
  if reserved > 0 then
    redis.call('HINCRBY', ledger, 'reserved_micros', -reserved)
    redis.call('HDEL', amounts, field)
  end
  redis.call('ZREM', expiry, field)
end
return 1
"#;

const ACQUIRE_ADMISSION_SCRIPT: &str = r#"
local limit = tonumber(ARGV[1])
local ttl_ms = tonumber(ARGV[2])
if limit == nil or limit < 1 or ttl_ms == nil or ttl_ms < 1 then
  return redis.error_reply('invalid shared admission bounds')
end
local time = redis.call('TIME')
local now_ms = (tonumber(time[1]) * 1000) + math.floor(tonumber(time[2]) / 1000)
redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
if redis.call('ZSCORE', KEYS[1], ARGV[3]) ~= false then return 1 end
if redis.call('ZCARD', KEYS[1]) >= limit then return 0 end
redis.call('ZADD', KEYS[1], now_ms + ttl_ms, ARGV[3])
redis.call('PEXPIRE', KEYS[1], ttl_ms)
return 1
"#;

const RELEASE_ADMISSION_SCRIPT: &str = r#"
redis.call('ZREM', KEYS[1], ARGV[1])
if redis.call('ZCARD', KEYS[1]) == 0 then redis.call('DEL', KEYS[1]) end
return 1
"#;

const BEGIN_HEALTH_SCRIPT: &str = r#"
local time = redis.call('TIME')
local now_ms = (tonumber(time[1]) * 1000) + math.floor(tonumber(time[2]) / 1000)
local open_until = tonumber(redis.call('HGET', KEYS[1], 'open_until') or '0')
local probe = redis.call('HGET', KEYS[1], 'probe')
if open_until > now_ms and probe ~= ARGV[3] then return 0 end
if probe ~= false and probe ~= ARGV[3] then return 0 end
redis.call('HSET', KEYS[1], 'probe', ARGV[3])
redis.call('PEXPIRE', KEYS[1], math.max(tonumber(ARGV[2]) * 2, 1000))
return 1
"#;

const COMPLETE_HEALTH_SCRIPT: &str = r#"
local time = redis.call('TIME')
local now_ms = (tonumber(time[1]) * 1000) + math.floor(tonumber(time[2]) / 1000)
local probe = redis.call('HGET', KEYS[1], 'probe')
if probe ~= false and probe ~= ARGV[3] then return 0 end
if ARGV[4] == '1' then
  redis.call('HSET', KEYS[1], 'failures', 0, 'open_until', 0)
  redis.call('HDEL', KEYS[1], 'probe')
  return 1
end
local failures = redis.call('HINCRBY', KEYS[1], 'failures', 1)
if failures >= tonumber(ARGV[1]) then
  redis.call('HSET', KEYS[1], 'open_until', now_ms + tonumber(ARGV[2]))
end
redis.call('HDEL', KEYS[1], 'probe')
return 1
"#;

const RELEASE_HEALTH_SCRIPT: &str = r#"
if redis.call('HGET', KEYS[1], 'probe') == ARGV[1] then
  redis.call('HDEL', KEYS[1], 'probe')
end
return 1
"#;

const SHARED_CACHE_GET_SCRIPT: &str = r#"
local generation = redis.call('HGET', KEYS[2], 'generation')
if generation ~= ARGV[2] then
  local stale = redis.call('ZRANGE', KEYS[4], 0, -1)
  for _, digest in ipairs(stale) do
    redis.call('DEL', ARGV[3] .. digest)
  end
  redis.call('DEL', KEYS[3], KEYS[4])
  redis.call('HSET', KEYS[2], 'generation', ARGV[2], 'bytes', 0)
end
local entry = redis.call('GET', KEYS[1])
if not entry then
  local prior = tonumber(redis.call('HGET', KEYS[3], ARGV[1]) or '0')
  if prior > 0 then
    redis.call('HINCRBY', KEYS[2], 'bytes', -prior)
    redis.call('HDEL', KEYS[3], ARGV[1])
    redis.call('ZREM', KEYS[4], ARGV[1])
  end
  return false
end
local time = redis.call('TIME')
local now_ms = (tonumber(time[1]) * 1000) + math.floor(tonumber(time[2]) / 1000)
redis.call('ZADD', KEYS[4], now_ms, ARGV[1])
return entry
"#;

const SHARED_CACHE_PUT_SCRIPT: &str = r#"
local generation = redis.call('HGET', KEYS[2], 'generation')
if generation ~= ARGV[2] then
  local stale = redis.call('ZRANGE', KEYS[4], 0, -1)
  for _, digest in ipairs(stale) do
    redis.call('DEL', ARGV[3] .. digest)
  end
  redis.call('DEL', KEYS[3], KEYS[4])
  redis.call('HSET', KEYS[2], 'generation', ARGV[2], 'bytes', 0)
end
local body_bytes = tonumber(ARGV[5])
local ttl_ms = tonumber(ARGV[6])
local max_entries = tonumber(ARGV[7])
local max_bytes = tonumber(ARGV[8])
if body_bytes == nil or body_bytes < 0 or ttl_ms == nil or ttl_ms < 0
    or max_entries == nil or max_entries < 1 or max_bytes == nil or max_bytes < 1 then
  return 0
end
if body_bytes > max_bytes then return 0 end
local previous = tonumber(redis.call('HGET', KEYS[3], ARGV[1]) or '0')
if previous > 0 then
  redis.call('HINCRBY', KEYS[2], 'bytes', -previous)
end
redis.call('SET', KEYS[1], ARGV[4])
if ttl_ms > 0 then redis.call('PEXPIRE', KEYS[1], ttl_ms) end
redis.call('HSET', KEYS[3], ARGV[1], body_bytes)
local time = redis.call('TIME')
local now_ms = (tonumber(time[1]) * 1000) + math.floor(tonumber(time[2]) / 1000)
redis.call('ZADD', KEYS[4], now_ms, ARGV[1])
redis.call('HINCRBY', KEYS[2], 'bytes', body_bytes - previous)
local bytes = tonumber(redis.call('HGET', KEYS[2], 'bytes') or '0')
while redis.call('ZCARD', KEYS[4]) > max_entries or bytes > max_bytes do
  local oldest = redis.call('ZRANGE', KEYS[4], 0, 0)
  if #oldest == 0 then break end
  local victim = oldest[1]
  local victim_bytes = tonumber(redis.call('HGET', KEYS[3], victim) or '0')
  redis.call('DEL', ARGV[3] .. victim)
  redis.call('HDEL', KEYS[3], victim)
  redis.call('ZREM', KEYS[4], victim)
  bytes = math.max(0, bytes - victim_bytes)
  redis.call('HSET', KEYS[2], 'bytes', bytes)
end
return redis.call('EXISTS', KEYS[1])
"#;

const SHARED_CACHE_CLEAR_SCRIPT: &str = r#"
local entries = redis.call('ZRANGE', KEYS[3], 0, -1)
for _, digest in ipairs(entries) do
  redis.call('DEL', ARGV[1] .. digest)
end
redis.call('DEL', KEYS[1], KEYS[2], KEYS[3])
return 1
"#;

const CANARY_RECORD_SCRIPT: &str = r#"
local time = redis.call('TIME')
local now_ms = (tonumber(time[1]) * 1000) + math.floor(tonumber(time[2]) / 1000)
local window_ms = tonumber(ARGV[2])
local window_id = math.floor(now_ms / window_ms)
local current = redis.call('HGET', KEYS[1], 'window')
if current == false or tonumber(current) ~= window_id then
  redis.call('HSET', KEYS[1], 'window', window_id, 'requests', 0, 'errors', 0,
    'latency_sum_ms', 0, 'latency_samples', 0, 'realized_micros', 0,
    'baseline_micros', 0, 'cost_samples', 0, 'quality_scaled', 0,
    'quality_samples', 0)
  redis.call('DEL', KEYS[2])
end
if redis.call('SADD', KEYS[2], ARGV[1]) == 0 then return 0 end
redis.call('PEXPIRE', KEYS[1], math.max(window_ms * 2, 1000))
redis.call('PEXPIRE', KEYS[2], math.max(window_ms * 2, 1000))
redis.call('HINCRBY', KEYS[1], 'requests', 1)
if ARGV[3] == '0' then redis.call('HINCRBY', KEYS[1], 'errors', 1) end
redis.call('HINCRBY', KEYS[1], 'latency_sum_ms', tonumber(ARGV[4]))
redis.call('HINCRBY', KEYS[1], 'latency_samples', 1)
if tonumber(ARGV[5]) >= 0 and tonumber(ARGV[6]) >= 0 then
  redis.call('HINCRBY', KEYS[1], 'realized_micros', tonumber(ARGV[5]))
  redis.call('HINCRBY', KEYS[1], 'baseline_micros', tonumber(ARGV[6]))
  redis.call('HINCRBY', KEYS[1], 'cost_samples', 1)
end
if tonumber(ARGV[7]) >= 0 then
  redis.call('HINCRBY', KEYS[1], 'quality_scaled', tonumber(ARGV[7]))
  redis.call('HINCRBY', KEYS[1], 'quality_samples', 1)
end
return 1
"#;

const CANARY_SNAPSHOT_SCRIPT: &str = r#"
local time = redis.call('TIME')
local now_ms = (tonumber(time[1]) * 1000) + math.floor(tonumber(time[2]) / 1000)
local window_ms = tonumber(ARGV[1])
local window_id = math.floor(now_ms / window_ms)
local current = redis.call('HGET', KEYS[1], 'window')
if current == false or tonumber(current) ~= window_id then
  redis.call('HSET', KEYS[1], 'window', window_id, 'requests', 0, 'errors', 0,
    'latency_sum_ms', 0, 'latency_samples', 0, 'realized_micros', 0,
    'baseline_micros', 0, 'cost_samples', 0, 'quality_scaled', 0,
    'quality_samples', 0)
  redis.call('DEL', KEYS[2])
end
return {
  tonumber(redis.call('HGET', KEYS[1], 'requests') or '0'),
  tonumber(redis.call('HGET', KEYS[1], 'errors') or '0'),
  tonumber(redis.call('HGET', KEYS[1], 'latency_sum_ms') or '0'),
  tonumber(redis.call('HGET', KEYS[1], 'latency_samples') or '0'),
  tonumber(redis.call('HGET', KEYS[1], 'realized_micros') or '0'),
  tonumber(redis.call('HGET', KEYS[1], 'baseline_micros') or '0'),
  tonumber(redis.call('HGET', KEYS[1], 'cost_samples') or '0'),
  tonumber(redis.call('HGET', KEYS[1], 'quality_scaled') or '0'),
  tonumber(redis.call('HGET', KEYS[1], 'quality_samples') or '0'),
  redis.call('HGET', KEYS[1], 'tripped_reason') or ''
}
"#;

const CANARY_TRIP_SCRIPT: &str = r#"
local hold_ms = tonumber(ARGV[3])
if hold_ms == nil or hold_ms <= 0 then return 0 end
redis.call('HSET', KEYS[1], 'tripped_reason', ARGV[2])
redis.call('PEXPIRE', KEYS[1], math.max(hold_ms, 1000))
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

    fn ledger(&self, period: &str, scope: &str) -> String {
        format!(
            "{REDIS_KEY_ROOT}:{}:ledger:{}:{}",
            self.encoded_namespace,
            encode_redis_component(period),
            encode_redis_component(scope)
        )
    }

    fn ledger_seen(&self) -> String {
        format!("{REDIS_KEY_ROOT}:{}:ledger-seen", self.encoded_namespace)
    }

    fn budget_reservations(&self, period: &str, scope: &str) -> String {
        format!(
            "{REDIS_KEY_ROOT}:{}:budget-reservations:{}:{}",
            self.encoded_namespace,
            encode_redis_component(period),
            encode_redis_component(scope)
        )
    }

    fn budget_amounts(&self, period: &str, scope: &str) -> String {
        format!(
            "{REDIS_KEY_ROOT}:{}:budget-amounts:{}:{}",
            self.encoded_namespace,
            encode_redis_component(period),
            encode_redis_component(scope)
        )
    }

    fn admission(&self) -> String {
        format!("{REDIS_KEY_ROOT}:{}:admission", self.encoded_namespace)
    }

    fn health(&self, target: &str) -> String {
        format!(
            "{REDIS_KEY_ROOT}:{}:health:{}",
            self.encoded_namespace,
            encode_redis_component(target)
        )
    }

    fn cache_entry_prefix(&self) -> String {
        format!("{REDIS_KEY_ROOT}:{}:cache:entry:", self.encoded_namespace)
    }

    fn cache_entry(&self, key: &str) -> String {
        format!("{}{}", self.cache_entry_prefix(), key)
    }

    fn cache_meta(&self) -> String {
        format!("{REDIS_KEY_ROOT}:{}:cache:meta", self.encoded_namespace)
    }

    fn cache_sizes(&self) -> String {
        format!("{REDIS_KEY_ROOT}:{}:cache:sizes", self.encoded_namespace)
    }

    fn cache_index(&self) -> String {
        format!("{REDIS_KEY_ROOT}:{}:cache:index", self.encoded_namespace)
    }

    fn canary(&self, rollout_id: &str) -> String {
        format!(
            "{REDIS_KEY_ROOT}:{}:canary:{}",
            self.encoded_namespace,
            encode_redis_component(rollout_id)
        )
    }

    fn canary_seen(&self, rollout_id: &str) -> String {
        format!(
            "{REDIS_KEY_ROOT}:{}:canary-seen:{}",
            self.encoded_namespace,
            encode_redis_component(rollout_id)
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

fn budget_period(date: &str, window: &str) -> Result<String, StateBackendError> {
    match window {
        "day" => Ok(format!("day:{date}")),
        "month" => Ok(format!("month:{}", &date[..7])),
        "all" => Ok("all".to_owned()),
        _ => Err(StateBackendError::Unavailable),
    }
}

fn shared_budget_keys(
    namespace: &RedisKeyNamespace,
    date: &str,
    scopes: &[SharedBudgetScope],
) -> Result<Vec<String>, StateBackendError> {
    let date = validate_ledger_date(date)?;
    let mut keys = Vec::with_capacity(scopes.len() * 3);
    for scope in scopes {
        if scope.scope.is_empty() || scope.scope.len() > 128 {
            return Err(StateBackendError::Unavailable);
        }
        let period = budget_period(&date, &scope.window)?;
        keys.push(namespace.ledger(&period, &scope.scope));
        keys.push(namespace.budget_reservations(&period, &scope.scope));
        keys.push(namespace.budget_amounts(&period, &scope.scope));
    }
    Ok(keys)
}

fn validate_shared_cache_inputs(generation: &str, key: &str) -> Result<(), StateBackendError> {
    if generation.is_empty() || generation.len() > 128 {
        return Err(StateBackendError::Unavailable);
    }
    if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StateBackendError::Unavailable);
    }
    Ok(())
}

fn cache_ttl_millis(ttl_seconds: f64) -> Result<u64, StateBackendError> {
    if !ttl_seconds.is_finite() || ttl_seconds < 0.0 {
        return Err(StateBackendError::Unavailable);
    }
    if ttl_seconds == 0.0 {
        return Ok(0);
    }
    let millis = (ttl_seconds * 1_000.0).ceil();
    if !millis.is_finite() || millis > u64::MAX as f64 {
        return Err(StateBackendError::Unavailable);
    }
    Ok(millis as u64)
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

    fn record_cost<'a>(&'a self, observation: SharedCostObservation) -> StateFuture<'a, bool> {
        Box::pin(async move {
            let date = validate_ledger_date(&observation.date)?;
            if observation.request_id.is_empty() || observation.request_id.len() > 128 {
                return Err(StateBackendError::Unavailable);
            }
            let mut scopes = BTreeSet::new();
            for scope in observation.scopes {
                if scope.is_empty() || scope.len() > 128 {
                    return Err(StateBackendError::Unavailable);
                }
                scopes.insert(scope);
            }
            if scopes.is_empty() || scopes.len() > 8 {
                return Err(StateBackendError::Unavailable);
            }
            let realized = scaled_cost(observation.realized)?;
            let baseline = scaled_cost(observation.baseline)?;
            let savings = scaled_signed_cost(observation.savings)?;
            let prompt = i64::try_from(observation.prompt_tokens)
                .map_err(|_| StateBackendError::Unavailable)?;
            let completion = i64::try_from(observation.completion_tokens)
                .map_err(|_| StateBackendError::Unavailable)?;
            let estimated = i64::from(observation.estimated);
            let mut keys = vec![self.keys.ledger_seen()];
            for scope in scopes {
                keys.push(self.keys.ledger(&format!("day:{date}"), &scope));
                keys.push(self.keys.ledger(&format!("month:{}", &date[..7]), &scope));
                keys.push(self.keys.ledger("all", &scope));
            }
            let script = Script::new(RECORD_COST_SCRIPT);
            let mut invocation = script.prepare_invoke();
            for key in keys {
                invocation.key(key);
            }
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = invocation
                .arg(observation.request_id)
                .arg(realized)
                .arg(baseline)
                .arg(savings)
                .arg(prompt)
                .arg(completion)
                .arg(estimated)
                .arg(SHARED_LEDGER_DAY_TTL_SECONDS)
                .arg(SHARED_LEDGER_MONTH_TTL_SECONDS)
                .arg(SHARED_LEDGER_IDEMPOTENCY_TTL_SECONDS)
                .invoke_async(&mut connection)
                .await;
            self.mark(
                result
                    .map(|value| value == 1)
                    .map_err(|_| StateBackendError::Unavailable),
            )
        })
    }

    fn record_canary<'a>(
        &'a self,
        observation: SharedCanaryObservation,
        window_seconds: f64,
        _now_seconds: f64,
    ) -> StateFuture<'a, bool> {
        Box::pin(async move {
            validate_canary_inputs(&observation, window_seconds)?;
            let window_ms = (window_seconds * 1_000.0).ceil().max(1.0) as u64;
            let realized = observation.realized_cost.map(scaled_cost).transpose()?;
            let baseline = observation.baseline_cost.map(scaled_cost).transpose()?;
            let quality = observation
                .quality_loss
                .map(|value| {
                    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                        return Err(StateBackendError::Unavailable);
                    }
                    Ok((value * SHARED_QUALITY_SCALE).round() as i64)
                })
                .transpose()?;
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = Script::new(CANARY_RECORD_SCRIPT)
                .key(self.keys.canary(&observation.rollout_id))
                .key(self.keys.canary_seen(&observation.rollout_id))
                .arg(observation.request_id)
                .arg(window_ms)
                .arg(i64::from(observation.succeeded))
                .arg(observation.latency_ms)
                .arg(realized.unwrap_or(-1))
                .arg(baseline.unwrap_or(-1))
                .arg(quality.unwrap_or(-1))
                .invoke_async(&mut connection)
                .await;
            self.mark(
                result
                    .map(|value| value == 1)
                    .map_err(|_| StateBackendError::Unavailable),
            )
        })
    }

    fn canary_snapshot<'a>(
        &'a self,
        rollout_id: &'a str,
        window_seconds: f64,
        _now_seconds: f64,
    ) -> StateFuture<'a, SharedCanarySnapshot> {
        Box::pin(async move {
            if rollout_id.is_empty() || rollout_id.len() > 128 {
                return Err(StateBackendError::Unavailable);
            }
            if !window_seconds.is_finite() || window_seconds <= 0.0 {
                return Err(StateBackendError::Unavailable);
            }
            let window_ms = (window_seconds * 1_000.0).ceil().max(1.0) as u64;
            let mut connection = self.connection.clone();
            let result: Result<CanaryRedisSnapshot, _> = Script::new(CANARY_SNAPSHOT_SCRIPT)
                .key(self.keys.canary(rollout_id))
                .key(self.keys.canary_seen(rollout_id))
                .arg(window_ms)
                .invoke_async(&mut connection)
                .await;
            let value = result.map_err(|_| StateBackendError::Unavailable)?;
            self.mark(Ok(SharedCanarySnapshot {
                requests: non_negative_u64(value.0)?,
                errors: non_negative_u64(value.1)?,
                latency_sum_ms: non_negative_u64(value.2)?,
                latency_samples: non_negative_u64(value.3)?,
                realized_cost: value.4.max(0) as f64 / SHARED_COST_SCALE,
                baseline_cost: value.5.max(0) as f64 / SHARED_COST_SCALE,
                cost_samples: non_negative_u64(value.6)?,
                quality_loss: value.7.max(0) as f64 / SHARED_QUALITY_SCALE,
                quality_samples: non_negative_u64(value.8)?,
                tripped_reason: (!value.9.is_empty()).then_some(value.9),
            }))
        })
    }

    fn trip_canary<'a>(
        &'a self,
        rollout_id: &'a str,
        reason: &'a str,
        hold_seconds: f64,
        _now_seconds: f64,
    ) -> StateFuture<'a, ()> {
        Box::pin(async move {
            if rollout_id.is_empty()
                || rollout_id.len() > 128
                || reason.is_empty()
                || reason.len() > 128
                || !hold_seconds.is_finite()
                || hold_seconds <= 0.0
            {
                return Err(StateBackendError::Unavailable);
            }
            let hold_ms = (hold_seconds * 1_000.0).ceil().max(1.0) as u64;
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = Script::new(CANARY_TRIP_SCRIPT)
                .key(self.keys.canary(rollout_id))
                .arg(rollout_id)
                .arg(reason)
                .arg(hold_ms)
                .invoke_async(&mut connection)
                .await;
            let result = result
                .map_err(|_| StateBackendError::Unavailable)
                .and_then(|value| {
                    if value == 1 {
                        Ok(())
                    } else {
                        Err(StateBackendError::Unavailable)
                    }
                });
            self.mark(result)
        })
    }

    fn spend<'a>(
        &'a self,
        window: &'a str,
        scope: &'a str,
        date: &'a str,
    ) -> StateFuture<'a, SharedSpend> {
        Box::pin(async move {
            let date = validate_ledger_date(date)?;
            if scope.is_empty() || scope.len() > 128 {
                return Err(StateBackendError::Unavailable);
            }
            let period = match window {
                "day" => format!("day:{date}"),
                "month" => format!("month:{}", &date[..7]),
                "all" => "all".to_owned(),
                _ => return Err(StateBackendError::Unavailable),
            };
            let mut connection = self.connection.clone();
            let value: Option<i64> = redis::cmd("HGET")
                .arg(self.keys.ledger(&period, scope))
                .arg("realized_micros")
                .query_async(&mut connection)
                .await
                .map_err(|_| StateBackendError::Unavailable)?;
            self.mark(Ok(SharedSpend {
                realized: value.map_or(0.0, |value| value as f64 / SHARED_COST_SCALE),
            }))
        })
    }

    fn reserve_budget<'a>(&'a self, reservation: SharedBudgetReservation) -> StateFuture<'a, bool> {
        Box::pin(async move {
            if reservation.request_id.is_empty()
                || reservation.request_id.len() > 128
                || reservation.scopes.is_empty()
                || reservation.scopes.len() > 8
            {
                return Err(StateBackendError::Unavailable);
            }
            let amount = scaled_cost(reservation.amount)?;
            let keys = shared_budget_keys(&self.keys, &reservation.date, &reservation.scopes)?;
            let script = Script::new(RESERVE_BUDGET_SCRIPT);
            let mut invocation = script.prepare_invoke();
            for key in keys {
                invocation.key(key);
            }
            invocation
                .arg(&reservation.request_id)
                .arg(amount)
                .arg(reservation.scopes.len());
            for scope in &reservation.scopes {
                let limit = scaled_cost(scope.limit)?;
                let blocks = if scope.blocks { 1_i64 } else { 0_i64 };
                let ledger_ttl_ms = match scope.window.as_str() {
                    "day" => SHARED_LEDGER_DAY_TTL_SECONDS.saturating_mul(1_000),
                    "month" | "all" => SHARED_LEDGER_MONTH_TTL_SECONDS.saturating_mul(1_000),
                    _ => return Err(StateBackendError::Unavailable),
                };
                invocation.arg(limit).arg(blocks).arg(ledger_ttl_ms);
                invocation.arg(SHARED_BUDGET_RESERVATION_TTL_MS);
            }
            invocation.arg(SHARED_BUDGET_RESERVATION_TTL_MS);
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = invocation.invoke_async(&mut connection).await;
            self.mark(
                result
                    .map(|value| value == 1)
                    .map_err(|_| StateBackendError::Unavailable),
            )
        })
    }

    fn release_budget<'a>(&'a self, reservation: SharedBudgetReservation) -> StateFuture<'a, ()> {
        Box::pin(async move {
            let keys = shared_budget_keys(&self.keys, &reservation.date, &reservation.scopes)?;
            let script = Script::new(RELEASE_BUDGET_SCRIPT);
            let mut invocation = script.prepare_invoke();
            for key in keys {
                invocation.key(key);
            }
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = invocation
                .arg(&reservation.request_id)
                .invoke_async(&mut connection)
                .await;
            self.mark(
                result
                    .map(|_| ())
                    .map_err(|_| StateBackendError::Unavailable),
            )
        })
    }

    fn settle_budget<'a>(&'a self, reservation: SharedBudgetReservation) -> StateFuture<'a, ()> {
        self.release_budget(reservation)
    }

    fn acquire_admission<'a>(
        &'a self,
        limit: u64,
        lease_id: &'a str,
        ttl_ms: u64,
    ) -> StateFuture<'a, bool> {
        Box::pin(async move {
            if limit == 0 || lease_id.is_empty() || lease_id.len() > 128 || ttl_ms == 0 {
                return Err(StateBackendError::Unavailable);
            }
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = Script::new(ACQUIRE_ADMISSION_SCRIPT)
                .key(self.keys.admission())
                .arg(limit)
                .arg(ttl_ms)
                .arg(lease_id)
                .invoke_async(&mut connection)
                .await;
            self.mark(
                result
                    .map(|value| value == 1)
                    .map_err(|_| StateBackendError::Unavailable),
            )
        })
    }

    fn release_admission<'a>(&'a self, lease_id: &'a str) -> StateFuture<'a, ()> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = Script::new(RELEASE_ADMISSION_SCRIPT)
                .key(self.keys.admission())
                .arg(lease_id)
                .invoke_async(&mut connection)
                .await;
            self.mark(
                result
                    .map(|_| ())
                    .map_err(|_| StateBackendError::Unavailable),
            )
        })
    }

    fn begin_provider_health<'a>(
        &'a self,
        target: &'a str,
        threshold: u64,
        cooldown_ms: u64,
        probe_id: &'a str,
    ) -> StateFuture<'a, bool> {
        Box::pin(async move {
            if threshold == 0 || cooldown_ms == 0 || target.is_empty() || probe_id.is_empty() {
                return Err(StateBackendError::Unavailable);
            }
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = Script::new(BEGIN_HEALTH_SCRIPT)
                .key(self.keys.health(target))
                .arg(threshold)
                .arg(cooldown_ms)
                .arg(probe_id)
                .invoke_async(&mut connection)
                .await;
            self.mark(
                result
                    .map(|value| value == 1)
                    .map_err(|_| StateBackendError::Unavailable),
            )
        })
    }

    fn complete_provider_health<'a>(
        &'a self,
        target: &'a str,
        threshold: u64,
        cooldown_ms: u64,
        probe_id: &'a str,
        succeeded: bool,
    ) -> StateFuture<'a, ()> {
        Box::pin(async move {
            if threshold == 0 || cooldown_ms == 0 || target.is_empty() || probe_id.is_empty() {
                return Err(StateBackendError::Unavailable);
            }
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = Script::new(COMPLETE_HEALTH_SCRIPT)
                .key(self.keys.health(target))
                .arg(threshold)
                .arg(cooldown_ms)
                .arg(probe_id)
                .arg(if succeeded { 1 } else { 0 })
                .invoke_async(&mut connection)
                .await;
            self.mark(match result {
                Ok(1) => Ok(()),
                Ok(_) | Err(_) => Err(StateBackendError::Unavailable),
            })
        })
    }

    fn release_provider_health<'a>(
        &'a self,
        target: &'a str,
        probe_id: &'a str,
    ) -> StateFuture<'a, ()> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = Script::new(RELEASE_HEALTH_SCRIPT)
                .key(self.keys.health(target))
                .arg(probe_id)
                .invoke_async(&mut connection)
                .await;
            self.mark(
                result
                    .map(|_| ())
                    .map_err(|_| StateBackendError::Unavailable),
            )
        })
    }

    fn shared_cache_get<'a>(
        &'a self,
        generation: &'a str,
        key: &'a str,
    ) -> StateFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            validate_shared_cache_inputs(generation, key)?;
            let mut connection = self.connection.clone();
            let value: Result<Option<Vec<u8>>, _> = Script::new(SHARED_CACHE_GET_SCRIPT)
                .key(self.keys.cache_entry(key))
                .key(self.keys.cache_meta())
                .key(self.keys.cache_sizes())
                .key(self.keys.cache_index())
                .arg(key)
                .arg(generation)
                .arg(self.keys.cache_entry_prefix())
                .invoke_async(&mut connection)
                .await;
            self.mark(value.map_err(|_| StateBackendError::Unavailable))
        })
    }

    fn shared_cache_put<'a>(&'a self, put: SharedCachePut) -> StateFuture<'a, bool> {
        Box::pin(async move {
            validate_shared_cache_inputs(&put.generation, &put.key)?;
            let body_bytes =
                u64::try_from(put.body_bytes).map_err(|_| StateBackendError::Unavailable)?;
            let max_entries =
                u64::try_from(put.max_entries).map_err(|_| StateBackendError::Unavailable)?;
            let max_bytes =
                u64::try_from(put.max_bytes).map_err(|_| StateBackendError::Unavailable)?;
            if max_entries == 0 || max_bytes == 0 || body_bytes > max_bytes {
                return Ok(false);
            }
            let ttl_ms = cache_ttl_millis(put.ttl_seconds)?;
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = Script::new(SHARED_CACHE_PUT_SCRIPT)
                .key(self.keys.cache_entry(&put.key))
                .key(self.keys.cache_meta())
                .key(self.keys.cache_sizes())
                .key(self.keys.cache_index())
                .arg(&put.key)
                .arg(&put.generation)
                .arg(self.keys.cache_entry_prefix())
                .arg(put.value)
                .arg(body_bytes)
                .arg(ttl_ms)
                .arg(max_entries)
                .arg(max_bytes)
                .invoke_async(&mut connection)
                .await;
            self.mark(
                result
                    .map(|value| value == 1)
                    .map_err(|_| StateBackendError::Unavailable),
            )
        })
    }

    fn shared_cache_clear<'a>(&'a self, generation: &'a str) -> StateFuture<'a, ()> {
        Box::pin(async move {
            if generation.is_empty() || generation.len() > 128 {
                return Err(StateBackendError::Unavailable);
            }
            let mut connection = self.connection.clone();
            let result: Result<i64, _> = Script::new(SHARED_CACHE_CLEAR_SCRIPT)
                .key(self.keys.cache_meta())
                .key(self.keys.cache_sizes())
                .key(self.keys.cache_index())
                .arg(self.keys.cache_entry_prefix())
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

fn validate_ledger_date(value: &str) -> Result<String, StateBackendError> {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return Err(StateBackendError::Unavailable);
    }
    Ok(value.to_owned())
}

fn scaled_cost(value: f64) -> Result<i64, StateBackendError> {
    if !value.is_finite() || value < 0.0 {
        return Err(StateBackendError::Unavailable);
    }
    let scaled = (value * SHARED_COST_SCALE).round();
    if !scaled.is_finite() || scaled > i64::MAX as f64 {
        return Err(StateBackendError::Unavailable);
    }
    Ok(scaled as i64)
}

fn scaled_signed_cost(value: f64) -> Result<i64, StateBackendError> {
    if !value.is_finite() {
        return Err(StateBackendError::Unavailable);
    }
    let scaled = (value * SHARED_COST_SCALE).round();
    if !scaled.is_finite() || scaled < i64::MIN as f64 || scaled > i64::MAX as f64 {
        return Err(StateBackendError::Unavailable);
    }
    Ok(scaled as i64)
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

    #[tokio::test]
    async fn memory_backend_canary_counters_are_idempotent_and_trippable()
    -> Result<(), StateBackendError> {
        let backend = MemoryStateBackend::default();
        let observation = SharedCanaryObservation {
            rollout_id: "release".to_owned(),
            request_id: "request-1".to_owned(),
            succeeded: false,
            latency_ms: 25,
            realized_cost: Some(2.0),
            baseline_cost: Some(1.0),
            quality_loss: Some(0.2),
        };
        assert!(
            backend
                .record_canary(observation.clone(), 60.0, 1.0)
                .await?
        );
        assert!(!backend.record_canary(observation, 60.0, 1.0).await?);
        let snapshot = backend.canary_snapshot("release", 60.0, 1.0).await?;
        assert_eq!(snapshot.requests, 1);
        assert_eq!(snapshot.errors, 1);
        backend
            .trip_canary("release", "error-rate", 300.0, 1.0)
            .await?;
        assert_eq!(
            backend
                .canary_snapshot("release", 60.0, 1.0)
                .await?
                .tripped_reason
                .as_deref(),
            Some("error-rate")
        );
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
    fn shared_ledger_inputs_are_strictly_validated() {
        assert_eq!(
            validate_ledger_date("2026-07-11").ok().as_deref(),
            Some("2026-07-11")
        );
        assert!(validate_ledger_date("2026-7-11").is_err());
        assert!(validate_ledger_date("2026-07-1x").is_err());
        assert_eq!(
            budget_period("2026-07-11", "day").ok().as_deref(),
            Some("day:2026-07-11")
        );
        assert_eq!(
            budget_period("2026-07-11", "month").ok().as_deref(),
            Some("month:2026-07")
        );
        assert_eq!(
            budget_period("2026-07-11", "all").ok().as_deref(),
            Some("all")
        );
        assert!(budget_period("2026-07-11", "week").is_err());

        assert_eq!(scaled_cost(1.234_567), Ok(1_234_567));
        assert!(scaled_cost(-0.1).is_err());
        assert!(scaled_cost(f64::NAN).is_err());
        assert_eq!(scaled_signed_cost(-1.25), Ok(-1_250_000));
        assert!(scaled_signed_cost(f64::INFINITY).is_err());
    }

    #[test]
    fn shared_budget_keys_reject_unbounded_dimensions() {
        let namespace = RedisKeyNamespace::new("prod");
        let valid = [SharedBudgetScope {
            scope: "workspace:production".to_owned(),
            window: "month".to_owned(),
            limit: 10.0,
            blocks: true,
        }];
        assert_eq!(
            shared_budget_keys(&namespace, "2026-07-11", &valid).map(|keys| keys.len()),
            Ok(3)
        );
        assert!(
            shared_budget_keys(
                &namespace,
                "2026-07-11",
                &[SharedBudgetScope {
                    scope: String::new(),
                    window: "month".to_owned(),
                    limit: 10.0,
                    blocks: true,
                }]
            )
            .is_err()
        );
        assert!(
            shared_budget_keys(
                &namespace,
                "2026-07-11",
                &[SharedBudgetScope {
                    scope: "global".to_owned(),
                    window: "week".to_owned(),
                    limit: 10.0,
                    blocks: true,
                }]
            )
            .is_err()
        );
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
