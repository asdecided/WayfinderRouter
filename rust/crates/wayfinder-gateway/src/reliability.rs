//! Shared buffered-delivery reliability state.
//!
//! Routing selects a logical model once. This module only controls how that
//! decision is delivered: bounded retries, per-target circuit state, and the
//! configured cross-tier fallback direction.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use wayfinder_config::gateway::GatewayConfig;
use wayfinder_providers::reliability::{
    CircuitAttempt, CircuitBreaker, FailoverPolicy, delivery_plan, retry_delays_with,
};

/// Python-compatible initial full-jitter backoff slot.
pub const RETRY_BASE_SECONDS: f64 = 0.2;
/// Python-compatible maximum full-jitter backoff slot.
pub const RETRY_CAP_SECONDS: f64 = 5.0;

type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
type Jitter = Arc<dyn Fn() -> f64 + Send + Sync>;

/// Invalid reliability configuration or synchronized state.
#[derive(Debug, Error, PartialEq)]
pub enum ReliabilityError {
    /// Retry or breaker bounds do not fit this platform.
    #[error("gateway reliability bounds exceed the supported platform size")]
    InvalidBounds,
    /// Failover policy was not one of the validated values.
    #[error("gateway failover policy is invalid")]
    InvalidPolicy,
    /// Monotonic time was not finite and non-negative.
    #[error("gateway reliability clock is invalid")]
    InvalidTime,
    /// Circuit state could not be synchronized.
    #[error("gateway circuit-breaker state is unavailable")]
    LockPoisoned,
}

/// Process-local reliability configuration plus shared circuit state.
pub struct ReliabilityPolicy {
    retries: usize,
    failover: FailoverPolicy,
    breaker: Arc<Mutex<CircuitBreaker>>,
    clock: Clock,
    jitter: Jitter,
}

/// Owned permission for one concrete target attempt sequence.
///
/// Dropping an unfinished half-open attempt conservatively fails the probe and
/// restarts its cooldown. Closed attempts retain the historical cancellation
/// behavior and do not count client abandonment as an upstream failure.
pub struct ReliabilityAttempt {
    breaker: Arc<Mutex<CircuitBreaker>>,
    clock: Clock,
    target: String,
    state: CircuitAttempt,
    completed: bool,
}

impl ReliabilityAttempt {
    /// Whether this lease owns the target's sole half-open probe.
    #[must_use]
    pub const fn is_half_open(&self) -> bool {
        matches!(self.state, CircuitAttempt::HalfOpen)
    }

    /// Finish the target-level attempt sequence and release circuit ownership.
    pub fn complete(mut self, succeeded: bool) -> Result<(), ReliabilityError> {
        let now = validated_now(&self.clock)?;
        let mut breaker = self
            .breaker
            .lock()
            .map_err(|_| ReliabilityError::LockPoisoned)?;
        breaker.record_at(&self.target, succeeded, now);
        self.completed = true;
        Ok(())
    }
}

impl Drop for ReliabilityAttempt {
    fn drop(&mut self) {
        if self.completed || !self.is_half_open() {
            return;
        }
        let Ok(now) = validated_now(&self.clock) else {
            return;
        };
        let Ok(mut breaker) = self.breaker.lock() else {
            return;
        };
        breaker.abandon_at(&self.target, self.state, now);
        self.completed = true;
    }
}

impl ReliabilityPolicy {
    /// Build from validated gateway configuration with process-local sources.
    pub fn from_gateway_config(config: &GatewayConfig) -> Result<Self, ReliabilityError> {
        let started = Instant::now();
        Self::from_gateway_config_with_sources(
            config,
            move || started.elapsed().as_secs_f64(),
            system_jitter,
        )
    }

    /// Build with injected monotonic time and jitter for deterministic tests.
    pub fn from_gateway_config_with_sources(
        config: &GatewayConfig,
        clock: impl Fn() -> f64 + Send + Sync + 'static,
        jitter: impl Fn() -> f64 + Send + Sync + 'static,
    ) -> Result<Self, ReliabilityError> {
        let retries =
            usize::try_from(config.retries).map_err(|_| ReliabilityError::InvalidBounds)?;
        let threshold = usize::try_from(config.breaker_threshold)
            .map_err(|_| ReliabilityError::InvalidBounds)?;
        let failover = parse_failover(&config.failover).ok_or(ReliabilityError::InvalidPolicy)?;
        Ok(Self {
            retries,
            failover,
            breaker: Arc::new(Mutex::new(CircuitBreaker::new(
                threshold,
                config.breaker_cooldown,
            ))),
            clock: Arc::new(clock),
            jitter: Arc::new(jitter),
        })
    }

    /// Number of retry attempts after the initial call.
    #[must_use]
    pub const fn retries(&self) -> usize {
        self.retries
    }

    /// Configured policy, overridden only by a recognized request header.
    #[must_use]
    pub fn effective_failover(&self, header: Option<&str>) -> FailoverPolicy {
        header.and_then(parse_failover).unwrap_or(self.failover)
    }

    /// Return one full-jitter delay per configured retry.
    #[must_use]
    pub fn retry_delays(&self) -> Vec<f64> {
        retry_delays_with(self.retries, RETRY_BASE_SECONDS, RETRY_CAP_SECONDS, || {
            (self.jitter)()
        })
    }

    /// Filter the primary and ordered candidates through current circuit state.
    pub fn delivery_plan(
        &self,
        primary: &str,
        candidates: &[String],
        allow: impl FnMut(&str) -> bool,
    ) -> Result<Vec<String>, ReliabilityError> {
        let breaker = self
            .breaker
            .lock()
            .map_err(|_| ReliabilityError::LockPoisoned)?;
        let now = self.now()?;
        Ok(delivery_plan(
            primary,
            candidates.iter().map(String::as_str),
            Some((&breaker, now)),
            allow,
        ))
    }

    /// Whether one concrete target is currently outside its circuit cooldown.
    pub fn target_available(&self, target: &str) -> Result<bool, ReliabilityError> {
        Ok(!self.delivery_plan(target, &[], |_| true)?.is_empty())
    }

    /// Acquire one target-level lease, including single-flight half-open state.
    pub fn begin_attempt(
        &self,
        target: &str,
    ) -> Result<Option<ReliabilityAttempt>, ReliabilityError> {
        let now = self.now()?;
        let mut breaker = self
            .breaker
            .lock()
            .map_err(|_| ReliabilityError::LockPoisoned)?;
        let Some(state) = breaker.begin_at(target, now) else {
            return Ok(None);
        };
        drop(breaker);
        Ok(Some(ReliabilityAttempt {
            breaker: Arc::clone(&self.breaker),
            clock: Arc::clone(&self.clock),
            target: target.to_owned(),
            state,
            completed: false,
        }))
    }

    /// Fold one target-level outcome into the shared breaker.
    pub fn record(&self, target: &str, succeeded: bool) -> Result<(), ReliabilityError> {
        let mut breaker = self
            .breaker
            .lock()
            .map_err(|_| ReliabilityError::LockPoisoned)?;
        let now = self.now()?;
        breaker.record_at(target, succeeded, now);
        Ok(())
    }

    fn now(&self) -> Result<f64, ReliabilityError> {
        validated_now(&self.clock)
    }
}

impl fmt::Debug for ReliabilityPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReliabilityPolicy")
            .field("retries", &self.retries)
            .field("failover", &self.failover)
            .field("breaker", &self.breaker)
            .field("clock", &"<monotonic>")
            .field("jitter", &"<bounded source>")
            .finish()
    }
}

impl Default for ReliabilityPolicy {
    fn default() -> Self {
        let started = Instant::now();
        Self {
            retries: 2,
            failover: FailoverPolicy::SameTier,
            breaker: Arc::new(Mutex::new(CircuitBreaker::default())),
            clock: Arc::new(move || started.elapsed().as_secs_f64()),
            jitter: Arc::new(system_jitter),
        }
    }
}

fn validated_now(clock: &Clock) -> Result<f64, ReliabilityError> {
    let now = clock();
    (now.is_finite() && now >= 0.0)
        .then_some(now)
        .ok_or(ReliabilityError::InvalidTime)
}

/// Sleep for a validated retry delay without blocking an executor worker.
pub async fn sleep_retry(seconds: f64) {
    let Ok(duration) = Duration::try_from_secs_f64(seconds) else {
        return;
    };
    if !duration.is_zero() {
        tokio::time::sleep(duration).await;
    }
}

/// Parse one configuration/header failover value.
#[must_use]
pub fn parse_failover(value: &str) -> Option<FailoverPolicy> {
    match value {
        "same-tier" => Some(FailoverPolicy::SameTier),
        "degrade" => Some(FailoverPolicy::Degrade),
        "escalate" => Some(FailoverPolicy::Escalate),
        _ => None,
    }
}

fn system_jitter() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| {
            f64::from(duration.subsec_nanos()) / 1_000_000_000.0
        })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use tokio::sync::Barrier;

    use super::*;

    #[test]
    fn configuration_header_and_shared_breaker_are_deterministic() -> Result<(), ReliabilityError> {
        let config = GatewayConfig {
            retries: 2,
            breaker_threshold: 1,
            breaker_cooldown: 30.0,
            failover: "degrade".to_owned(),
            ..GatewayConfig::default()
        };
        let policy = ReliabilityPolicy::from_gateway_config_with_sources(&config, || 10.0, || 0.5)?;
        assert_eq!(policy.retry_delays(), [0.1, 0.2]);
        assert_eq!(
            policy.effective_failover(Some("escalate")),
            FailoverPolicy::Escalate
        );
        assert_eq!(
            policy.effective_failover(Some("invalid")),
            FailoverPolicy::Degrade
        );
        assert_eq!(
            policy.delivery_plan("cloud", &["local".to_owned()], |_| true)?,
            ["cloud", "local"]
        );
        policy.record("cloud", false)?;
        assert!(!policy.target_available("cloud")?);
        assert_eq!(
            policy.delivery_plan("cloud", &["local".to_owned()], |_| true)?,
            ["local"]
        );
        Ok(())
    }

    #[test]
    fn malformed_clock_fails_closed() -> Result<(), ReliabilityError> {
        let policy = ReliabilityPolicy::from_gateway_config_with_sources(
            &GatewayConfig::default(),
            || f64::NAN,
            || 0.0,
        )?;
        assert_eq!(
            policy.delivery_plan("local", &[], |_| true),
            Err(ReliabilityError::InvalidTime)
        );
        assert_eq!(
            policy.record("local", true),
            Err(ReliabilityError::InvalidTime)
        );
        Ok(())
    }

    #[test]
    fn half_open_probe_success_failure_and_drop_are_conservative() -> Result<(), ReliabilityError> {
        let now = Arc::new(AtomicU64::new(0));
        let clock = Arc::clone(&now);
        let config = GatewayConfig {
            breaker_threshold: 1,
            breaker_cooldown: 10.0,
            ..GatewayConfig::default()
        };
        let policy = ReliabilityPolicy::from_gateway_config_with_sources(
            &config,
            move || clock.load(Ordering::SeqCst) as f64,
            || 0.0,
        )?;

        policy.record("cloud", false)?;
        now.store(10, Ordering::SeqCst);
        let probe = policy
            .begin_attempt("cloud")?
            .ok_or(ReliabilityError::InvalidTime)?;
        assert!(probe.is_half_open());
        assert!(policy.begin_attempt("cloud")?.is_none());
        probe.complete(true)?;
        let closed = policy
            .begin_attempt("cloud")?
            .ok_or(ReliabilityError::InvalidTime)?;
        assert!(!closed.is_half_open());
        drop(closed);

        policy.record("cloud", false)?;
        now.store(20, Ordering::SeqCst);
        policy
            .begin_attempt("cloud")?
            .ok_or(ReliabilityError::InvalidTime)?
            .complete(false)?;
        assert!(policy.begin_attempt("cloud")?.is_none());

        now.store(30, Ordering::SeqCst);
        let abandoned = policy
            .begin_attempt("cloud")?
            .ok_or(ReliabilityError::InvalidTime)?;
        drop(abandoned);
        assert!(policy.begin_attempt("cloud")?.is_none());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_half_open_callers_receive_one_probe() -> Result<(), ReliabilityError> {
        let config = GatewayConfig {
            breaker_threshold: 1,
            breaker_cooldown: 10.0,
            ..GatewayConfig::default()
        };
        let now = Arc::new(AtomicU64::new(0));
        let clock = Arc::clone(&now);
        let policy = Arc::new(ReliabilityPolicy::from_gateway_config_with_sources(
            &config,
            move || clock.load(Ordering::SeqCst) as f64,
            || 0.0,
        )?);
        policy.record("cloud", false)?;
        now.store(10, Ordering::SeqCst);
        let start = Arc::new(Barrier::new(3));
        let finish = Arc::new(Barrier::new(3));
        let admitted = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let policy = Arc::clone(&policy);
            let start = Arc::clone(&start);
            let finish = Arc::clone(&finish);
            let admitted = Arc::clone(&admitted);
            tasks.push(tokio::spawn(async move {
                start.wait().await;
                let lease = policy.begin_attempt("cloud");
                if lease.as_ref().is_ok_and(Option::is_some) {
                    admitted.fetch_add(1, Ordering::SeqCst);
                }
                finish.wait().await;
                lease
            }));
        }
        start.wait().await;
        finish.wait().await;
        for task in tasks {
            let _ = task.await.map_err(|_| ReliabilityError::LockPoisoned)??;
        }
        assert_eq!(admitted.load(Ordering::SeqCst), 1);
        Ok(())
    }
}
