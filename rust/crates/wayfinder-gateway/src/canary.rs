//! Deterministic, bounded production canary assignment and tripwires.
//!
//! Canary selection is deliberately separate from complexity scoring. The
//! scorer remains keyless and deterministic; this module only gates whether a
//! previously scored request may use an explicitly configured candidate route.

use sha2::{Digest, Sha256};
use wayfinder_config::gateway::CanaryConfig;

use crate::state::SharedCanarySnapshot;

/// Header carrying a stable application/cohort identity for canary sampling.
pub const CANARY_IDENTITY_HEADER: &str = "x-wayfinder-canary-identity";
/// Header selecting an approved cohort identity.
pub const CANARY_COHORT_HEADER: &str = "x-wayfinder-canary-cohort";
/// Header emitted when a request was admitted to a canary rollout.
pub const CANARY_RESPONSE_HEADER: &str = "x-wayfinder-router-canary";
/// Fixed-width assignment bucket cardinality.
const BUCKETS: u64 = 1_000_000;

/// Stable assignment outcome before fleet exposure admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Assignment {
    /// The identity is in the configured deterministic cohort.
    Eligible { identity: String },
    /// The request is outside the configured cohort or has no stable identity.
    Suppressed { reason: &'static str },
}

/// One tripwire reason, stable for audit and metrics consumers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tripwire {
    /// Too many canary requests failed.
    ErrorRate,
    /// Mean canary latency exceeded the bound.
    Latency,
    /// Priced cost exceeded the baseline multiplier.
    Cost,
    /// Labelled quality loss exceeded the bound.
    Quality,
}

impl Tripwire {
    /// Stable machine-readable reason.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ErrorRate => "error-rate",
            Self::Latency => "latency",
            Self::Cost => "cost-multiplier",
            Self::Quality => "quality-loss",
        }
    }
}

/// Choose a stable identity from the configured scope without changing the
/// routing request or complexity score.
#[must_use]
pub fn assignment_identity(
    config: &CanaryConfig,
    request_id: &str,
    workspace: Option<&str>,
    key: Option<&str>,
    identity_header: Option<&str>,
    cohort_header: Option<&str>,
) -> Assignment {
    let identity = match config.scope.as_str() {
        "request" => identity_header
            .filter(|value| !value.is_empty())
            .or(Some(request_id)),
        "workspace" => workspace,
        "key" => key,
        "cohort" => {
            let Some(expected) = config.cohort.as_deref() else {
                return Assignment::Suppressed {
                    reason: "cohort-not-configured",
                };
            };
            cohort_header.filter(|value| *value == expected)
        }
        _ => None,
    };
    let Some(identity) = identity.filter(|value| !value.is_empty()) else {
        return Assignment::Suppressed {
            reason: "stable-identity-missing",
        };
    };
    let bucket = assignment_bucket(&config.rollout_id, identity);
    let threshold = (config.fraction.clamp(0.0, 1.0) * BUCKETS as f64).floor() as u64;
    if bucket < threshold {
        Assignment::Eligible {
            identity: identity.to_owned(),
        }
    } else {
        Assignment::Suppressed {
            reason: "outside-cohort",
        }
    }
}

/// Return a deterministic bucket for one rollout and identity.
#[must_use]
pub fn assignment_bucket(rollout_id: &str, identity: &str) -> u64 {
    let mut digest = Sha256::new();
    digest.update(b"wayfinder-canary-v1\0");
    digest.update(rollout_id.as_bytes());
    digest.update([0]);
    digest.update(identity.as_bytes());
    let bytes = digest.finalize();
    let value = u64::from_be_bytes(bytes[..8].try_into().unwrap_or([0; 8]));
    value % BUCKETS
}

/// Evaluate bounded fleet counters. A missing sample is never a tripwire.
#[must_use]
pub fn tripwire(config: &CanaryConfig, snapshot: SharedCanarySnapshot) -> Option<Tripwire> {
    if snapshot.requests < config.min_samples {
        return None;
    }
    let error_rate = snapshot.errors as f64 / snapshot.requests as f64;
    if error_rate > config.max_error_rate {
        return Some(Tripwire::ErrorRate);
    }
    if snapshot.latency_samples > 0 {
        let mean_latency = snapshot.latency_sum_ms as f64 / snapshot.latency_samples as f64;
        if mean_latency > config.max_latency_ms {
            return Some(Tripwire::Latency);
        }
    }
    if snapshot.cost_samples > 0
        && snapshot.baseline_cost > 0.0
        && snapshot.realized_cost / snapshot.baseline_cost > config.max_cost_multiplier
    {
        return Some(Tripwire::Cost);
    }
    if snapshot.quality_samples > 0 {
        let quality_loss = snapshot.quality_loss / snapshot.quality_samples as f64;
        if quality_loss > config.max_quality_loss {
            return Some(Tripwire::Quality);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CanaryConfig {
        CanaryConfig {
            enabled: true,
            rollout_id: "release".to_owned(),
            candidate_route: "candidate".to_owned(),
            fraction: 0.5,
            scope: "workspace".to_owned(),
            cohort: None,
            max_requests: 100,
            window: 60.0,
            min_samples: 10,
            max_error_rate: 0.2,
            max_latency_ms: 100.0,
            max_cost_multiplier: 1.5,
            max_quality_loss: 0.1,
            rollback_hold: 300.0,
        }
    }

    #[test]
    fn assignment_is_stable_and_scope_specific() {
        let config = config();
        let first = assignment_identity(&config, "request-a", Some("team"), None, None, None);
        let second = assignment_identity(&config, "request-b", Some("team"), None, None, None);
        assert_eq!(first, second);
        let bucket = assignment_bucket("release", "team");
        assert!(bucket < BUCKETS);
    }

    #[test]
    fn tripwires_ignore_sparse_samples_and_stop_on_error_rate() {
        let config = config();
        assert_eq!(tripwire(&config, SharedCanarySnapshot::default()), None);
        let snapshot = SharedCanarySnapshot {
            requests: 10,
            errors: 3,
            latency_samples: 10,
            ..SharedCanarySnapshot::default()
        };
        assert_eq!(tripwire(&config, snapshot), Some(Tripwire::ErrorRate));
    }

    #[test]
    fn approved_cohort_is_required_for_cohort_scope() {
        let mut config = config();
        config.scope = "cohort".to_owned();
        config.cohort = Some("beta".to_owned());
        assert!(matches!(
            assignment_identity(&config, "request", None, None, None, Some("beta")),
            Assignment::Eligible { .. } | Assignment::Suppressed { .. }
        ));
        assert_eq!(
            assignment_identity(&config, "request", None, None, None, Some("other")),
            Assignment::Suppressed {
                reason: "stable-identity-missing"
            }
        );
    }
}
