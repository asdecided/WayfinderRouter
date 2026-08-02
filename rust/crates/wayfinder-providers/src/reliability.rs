//! Deterministic provider-delivery policy.
//!
//! These functions change how an already-selected model is delivered.  They
//! must not change the routing decision itself.  Time and jitter are passed in
//! explicitly so compatibility tests never need a network or wall clock.

use std::collections::{HashMap, HashSet};

use wayfinder_routing_core::python_round;

/// HTTP statuses that may succeed when attempted again.
pub const RETRYABLE_STATUS: [u16; 5] = [429, 500, 502, 503, 504];

/// HTTP statuses indicating an unusable upstream credential or proxy auth.
pub const AUTH_FAILURE_STATUS: [u16; 3] = [401, 403, 407];

/// Whether a provider attempt should be retried.
///
/// `None` represents a transport failure. Ordinary client errors fail fast.
#[must_use]
pub fn is_retryable(status: Option<u16>) -> bool {
    status.is_none_or(|value| RETRYABLE_STATUS.contains(&value))
}

/// Whether the provider target should count this response as an auth failure.
#[must_use]
pub fn is_auth_failure(status: Option<u16>) -> bool {
    status.is_some_and(|value| AUTH_FAILURE_STATUS.contains(&value))
}

/// Produce one full-jitter delay per retry.
///
/// The initial attempt has no delay. `rng` is called exactly once for every
/// retry and is expected to return a value in the inclusive range `0.0..=1.0`.
/// Values outside that range are clamped so a faulty jitter source cannot
/// create a negative or over-cap sleep.
#[must_use]
pub fn retry_delays_with(
    retries: usize,
    base_seconds: f64,
    cap_seconds: f64,
    mut rng: impl FnMut() -> f64,
) -> Vec<f64> {
    let base = if base_seconds.is_finite() {
        base_seconds.max(0.0)
    } else {
        0.0
    };
    let cap = if cap_seconds.is_finite() {
        cap_seconds.max(0.0)
    } else {
        0.0
    };

    (0..retries)
        .map(|index| {
            let exponent = i32::try_from(index).unwrap_or(i32::MAX);
            let slot = (base * 2_f64.powi(exponent)).min(cap);
            let sample = rng();
            let jitter = if sample.is_finite() {
                sample.clamp(0.0, 1.0)
            } else {
                0.0
            };
            python_round(slot * jitter, 6)
        })
        .collect()
}

/// A per-target circuit breaker whose clock is supplied by the caller.
///
/// Passing time explicitly avoids wall-clock behavior in tests and lets the
/// async gateway use Tokio's monotonic clock at its boundary. Timestamps are
/// opaque monotonic seconds; they must come from the same clock domain.
#[derive(Clone, Debug)]
pub struct CircuitBreaker {
    threshold: usize,
    cooldown_seconds: f64,
    failures: HashMap<String, usize>,
    opened_at: HashMap<String, f64>,
    half_open: HashSet<String>,
}

/// State owned by one admitted target attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircuitAttempt {
    /// The target was closed when the attempt began.
    Closed,
    /// This is the target's sole probe after cooldown.
    HalfOpen,
}

impl CircuitBreaker {
    /// Construct a breaker. A zero threshold behaves as one failure, and an
    /// invalid or negative cooldown behaves as no cooldown.
    #[must_use]
    pub fn new(threshold: usize, cooldown_seconds: f64) -> Self {
        Self {
            threshold: threshold.max(1),
            cooldown_seconds: if cooldown_seconds.is_finite() {
                cooldown_seconds.max(0.0)
            } else {
                0.0
            },
            failures: HashMap::new(),
            opened_at: HashMap::new(),
            half_open: HashSet::new(),
        }
    }

    /// Whether `target` may be tried now (closed or ready for a half-open probe).
    #[must_use]
    pub fn allow_at(&self, target: &str, now_seconds: f64) -> bool {
        if self.half_open.contains(target) {
            return false;
        }
        self.opened_at
            .get(target)
            .is_none_or(|opened| now_seconds - *opened >= self.cooldown_seconds)
    }

    /// Begin one target attempt, granting at most one half-open probe.
    pub fn begin_at(&mut self, target: &str, now_seconds: f64) -> Option<CircuitAttempt> {
        let Some(opened) = self.opened_at.get(target) else {
            return Some(CircuitAttempt::Closed);
        };
        if now_seconds - *opened < self.cooldown_seconds
            || !self.half_open.insert(target.to_owned())
        {
            return None;
        }
        Some(CircuitAttempt::HalfOpen)
    }

    /// Whether the breaker is still cooling down for `target`.
    #[must_use]
    pub fn is_open_at(&self, target: &str, now_seconds: f64) -> bool {
        !self.allow_at(target, now_seconds)
    }

    /// Record the result of one attempt.
    ///
    /// Success closes the breaker. Every failure at or above the threshold
    /// restarts the cooldown, matching the Python oracle's failed-probe rule.
    pub fn record_at(&mut self, target: &str, succeeded: bool, now_seconds: f64) {
        self.half_open.remove(target);
        if succeeded {
            self.failures.remove(target);
            self.opened_at.remove(target);
            return;
        }

        let count = self.failures.entry(target.to_owned()).or_default();
        *count = count.saturating_add(1);
        if *count >= self.threshold {
            self.opened_at.insert(target.to_owned(), now_seconds);
        }
    }

    /// Conservatively fail an aborted half-open probe and restart cooldown.
    pub fn abandon_at(&mut self, target: &str, attempt: CircuitAttempt, now_seconds: f64) {
        if attempt == CircuitAttempt::HalfOpen {
            self.record_at(target, false, now_seconds);
        }
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(5, 30.0)
    }
}

/// Build an ordered, de-duplicated same-tier delivery plan.
///
/// Open targets and targets rejected by `allow` are removed. The result may be
/// empty, in which case the caller should fail before making a request.
pub fn delivery_plan<'a>(
    primary: &'a str,
    fallbacks: impl IntoIterator<Item = &'a str>,
    breaker: Option<(&CircuitBreaker, f64)>,
    mut allow: impl FnMut(&str) -> bool,
) -> Vec<String> {
    let mut seen = HashSet::new();
    std::iter::once(primary)
        .chain(fallbacks)
        .filter(|target| seen.insert((*target).to_owned()))
        .filter(|target| breaker.is_none_or(|(state, now)| state.allow_at(target, now)))
        .filter(|target| allow(target))
        .map(ToOwned::to_owned)
        .collect()
}

/// Cross-tier failover behavior after same-tier endpoints are exhausted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FailoverPolicy {
    /// Never move to a different tier.
    #[default]
    SameTier,
    /// Try cheaper tiers, nearest first.
    Degrade,
    /// Try more expensive tiers, nearest first.
    Escalate,
}

/// Return cross-tier candidates in compatibility order.
#[must_use]
pub fn failover_candidates(chosen: &str, ladder: &[String], policy: FailoverPolicy) -> Vec<String> {
    let Some(index) = ladder.iter().position(|model| model == chosen) else {
        return Vec::new();
    };

    match policy {
        FailoverPolicy::SameTier => Vec::new(),
        FailoverPolicy::Degrade => ladder[..index].iter().rev().cloned().collect(),
        FailoverPolicy::Escalate => ladder[index.saturating_add(1)..].to_vec(),
    }
}

/// Whether a prompt estimate fits the target's configured context window.
#[must_use]
pub fn precheck_ok(estimated_tokens: u64, context_window: Option<u64>) -> bool {
    context_window.is_none_or(|window| estimated_tokens <= window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_only_transport_and_transient_statuses() {
        assert!(is_retryable(None));
        for status in RETRYABLE_STATUS {
            assert!(is_retryable(Some(status)));
        }
        for status in [200, 400, 401, 403, 404, 422] {
            assert!(!is_retryable(Some(status)));
        }
    }

    #[test]
    fn auth_failure_set_matches_python() {
        for status in AUTH_FAILURE_STATUS {
            assert!(is_auth_failure(Some(status)));
        }
        assert!(!is_auth_failure(None));
        assert!(!is_auth_failure(Some(429)));
    }

    #[test]
    fn jitter_is_exponential_capped_and_rounded() {
        assert_eq!(retry_delays_with(4, 0.2, 1.0, || 1.0), [0.2, 0.4, 0.8, 1.0]);
        assert_eq!(retry_delays_with(3, 0.2, 5.0, || 0.0), [0.0, 0.0, 0.0]);
        assert!(retry_delays_with(0, 0.2, 5.0, || 1.0).is_empty());
    }

    #[test]
    fn jitter_source_is_contained() {
        assert_eq!(retry_delays_with(1, 1.0, 5.0, || 2.0), [1.0]);
        assert_eq!(retry_delays_with(1, 1.0, 5.0, || -1.0), [0.0]);
        assert_eq!(retry_delays_with(1, 1.0, 5.0, || f64::NAN), [0.0]);
    }

    #[test]
    fn breaker_opens_then_allows_probe_at_cooldown() {
        let mut breaker = CircuitBreaker::new(3, 30.0);
        breaker.record_at("cloud", false, 0.0);
        breaker.record_at("cloud", false, 0.0);
        assert!(breaker.allow_at("cloud", 0.0));
        breaker.record_at("cloud", false, 0.0);
        assert!(breaker.is_open_at("cloud", 29.0));
        assert!(breaker.allow_at("cloud", 30.0));
    }

    #[test]
    fn failed_probe_reopens_and_success_closes() {
        let mut breaker = CircuitBreaker::new(2, 10.0);
        breaker.record_at("x", false, 0.0);
        breaker.record_at("x", false, 0.0);
        assert!(breaker.allow_at("x", 10.0));
        breaker.record_at("x", false, 10.0);
        assert!(breaker.is_open_at("x", 19.0));
        assert!(breaker.allow_at("x", 20.0));
        breaker.record_at("x", true, 20.0);
        assert!(breaker.allow_at("x", 20.0));
    }

    #[test]
    fn half_open_probe_is_single_flight_and_abandon_restarts_cooldown() {
        let mut breaker = CircuitBreaker::new(1, 10.0);
        breaker.record_at("x", false, 0.0);
        assert_eq!(breaker.begin_at("x", 10.0), Some(CircuitAttempt::HalfOpen));
        assert_eq!(breaker.begin_at("x", 10.0), None);
        assert!(!breaker.allow_at("x", 10.0));

        breaker.abandon_at("x", CircuitAttempt::HalfOpen, 10.0);
        assert!(breaker.is_open_at("x", 19.0));
        assert_eq!(breaker.begin_at("x", 20.0), Some(CircuitAttempt::HalfOpen));
        breaker.record_at("x", true, 20.0);
        assert_eq!(breaker.begin_at("x", 20.0), Some(CircuitAttempt::Closed));
    }

    #[test]
    fn breaker_is_per_target() {
        let mut breaker = CircuitBreaker::new(1, 999.0);
        breaker.record_at("a", false, 0.0);
        assert!(breaker.is_open_at("a", 0.0));
        assert!(breaker.allow_at("b", 0.0));
    }

    #[test]
    fn plan_orders_deduplicates_and_filters() {
        let mut breaker = CircuitBreaker::new(1, 999.0);
        breaker.record_at("cloud", false, 0.0);
        let plan = delivery_plan(
            "cloud",
            ["cloud", "cloud-2", "local"],
            Some((&breaker, 0.0)),
            |name| name != "local",
        );
        assert_eq!(plan, ["cloud-2"]);
    }

    #[test]
    fn plan_can_be_empty() {
        let mut breaker = CircuitBreaker::new(1, 999.0);
        breaker.record_at("cloud", false, 0.0);
        breaker.record_at("cloud-2", false, 0.0);
        let plan = delivery_plan("cloud", ["cloud-2"], Some((&breaker, 0.0)), |_| true);
        assert!(plan.is_empty());
    }

    #[test]
    fn failover_walks_nearest_tier_first() {
        let ladder = ["local", "mid", "cloud"].map(str::to_owned);
        assert_eq!(
            failover_candidates("mid", &ladder, FailoverPolicy::Degrade),
            ["local"]
        );
        assert_eq!(
            failover_candidates("local", &ladder, FailoverPolicy::Escalate),
            ["mid", "cloud"]
        );
        assert_eq!(
            failover_candidates("cloud", &ladder, FailoverPolicy::Degrade),
            ["mid", "local"]
        );
        assert!(failover_candidates("ghost", &ladder, FailoverPolicy::Degrade).is_empty());
    }

    #[test]
    fn precheck_honors_inclusive_context_limit() {
        assert!(precheck_ok(500, None));
        assert!(precheck_ok(500, Some(500)));
        assert!(!precheck_ok(501, Some(500)));
    }
}
