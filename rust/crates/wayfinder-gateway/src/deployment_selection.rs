//! Bounded, prompt-free signals for interchangeable deployment selection.
//!
//! This module deliberately sits after route/tier eligibility. It cannot
//! change a scored model, privacy boundary, capability class, or named route;
//! it only orders concrete members already hidden behind one public alias.

use std::cmp::Ordering;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use wayfinder_config::gateway::{DeploymentSelectionPolicy, DeploymentSelectionStrategy};

use crate::ConfiguredModel;

const MAX_SIGNAL_KEYS: usize = 1_024;
const MAX_SAMPLES_PER_KEY: usize = 32;
const MAX_SIGNAL_ID_BYTES: usize = 128;
const OVERFLOW_SIGNAL_ID: &str = "__other__";

/// One prompt-free observation from a concrete deployment attempt.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DeploymentObservation {
    /// End-to-end delivery time for a buffered response or completed stream.
    pub latency_seconds: Option<f64>,
    /// Time from stream establishment to the first non-empty content event.
    pub ttft_seconds: Option<f64>,
    /// Completion characters per second, when content was measurable.
    pub throughput_chars_per_second: Option<f64>,
    /// Whether the provider returned a successful response.
    pub success: bool,
    /// Whether the provider explicitly rate-limited the attempt.
    pub rate_limited: bool,
    /// Normalized provider capacity pressure, where zero is healthy.
    pub capacity_pressure: Option<f64>,
}

impl DeploymentObservation {
    pub(crate) fn success(
        latency_seconds: f64,
        ttft_seconds: Option<f64>,
        throughput_chars_per_second: Option<f64>,
    ) -> Self {
        Self {
            latency_seconds: Some(latency_seconds),
            ttft_seconds,
            throughput_chars_per_second,
            success: true,
            rate_limited: false,
            capacity_pressure: Some(0.0),
        }
    }

    pub(crate) fn failure(latency_seconds: Option<f64>, status: Option<u16>) -> Self {
        let rate_limited = status == Some(429);
        let capacity_pressure = match status {
            Some(429 | 503) => Some(1.0),
            Some(500..=599) | None => Some(0.5),
            Some(_) => Some(0.25),
        };
        Self {
            latency_seconds,
            ttft_seconds: None,
            throughput_chars_per_second: None,
            success: false,
            rate_limited,
            capacity_pressure,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DeploymentSignalSummary {
    pub latency_seconds: f64,
    pub ttft_seconds: Option<f64>,
    pub throughput_chars_per_second: Option<f64>,
    pub availability: f64,
    pub rate_limit_rate: f64,
    pub capacity_pressure: f64,
}

#[derive(Clone, Copy, Debug)]
struct TimedObservation {
    observed_at: f64,
    observation: DeploymentObservation,
}

#[derive(Default)]
struct SignalState {
    samples: VecDeque<TimedObservation>,
}

/// Process-local bounded rolling observations.
pub(crate) struct DeploymentObservationStore {
    state: Mutex<BTreeMap<String, SignalState>>,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
}

impl std::fmt::Debug for DeploymentObservationStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeploymentObservationStore")
            .field("max_signal_keys", &MAX_SIGNAL_KEYS)
            .field("max_samples_per_key", &MAX_SAMPLES_PER_KEY)
            .finish_non_exhaustive()
    }
}

impl DeploymentObservationStore {
    pub(crate) fn new(clock: impl Fn() -> f64 + Send + Sync + 'static) -> Self {
        Self {
            state: Mutex::new(BTreeMap::new()),
            clock: Arc::new(clock),
        }
    }

    pub(crate) fn observe(&self, deployment_id: &str, observation: DeploymentObservation) {
        let Some(observation) = sanitize_observation(observation) else {
            return;
        };
        let key = bounded_signal_id(deployment_id);
        let now = bounded_clock((self.clock)());
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if !state.contains_key(&key) && state.len() >= MAX_SIGNAL_KEYS {
            return;
        }
        let samples = &mut state.entry(key).or_default().samples;
        samples.push_back(TimedObservation {
            observed_at: now,
            observation,
        });
        while samples.len() > MAX_SAMPLES_PER_KEY {
            samples.pop_front();
        }
    }

    pub(crate) fn summary(
        &self,
        deployment_id: &str,
        observation_ttl: f64,
    ) -> Option<DeploymentSignalSummary> {
        if !observation_ttl.is_finite() || observation_ttl <= 0.0 {
            return None;
        }
        let now = bounded_clock((self.clock)());
        let key = bounded_signal_id(deployment_id);
        let Ok(state) = self.state.lock() else {
            return None;
        };
        let samples = state.get(&key)?.samples.iter().filter(|sample| {
            now >= sample.observed_at && now - sample.observed_at <= observation_ttl
        });
        let mut count = 0_u64;
        let mut successes = 0_u64;
        let mut rate_limited = 0_u64;
        let mut latency_sum = 0.0;
        let mut latency_count = 0_u64;
        let mut ttft_sum = 0.0;
        let mut ttft_count = 0_u64;
        let mut throughput_sum = 0.0;
        let mut throughput_count = 0_u64;
        let mut capacity_sum = 0.0;
        let mut capacity_count = 0_u64;
        for sample in samples {
            count = count.saturating_add(1);
            successes = successes.saturating_add(u64::from(sample.observation.success));
            rate_limited = rate_limited.saturating_add(u64::from(sample.observation.rate_limited));
            if let Some(value) = sample.observation.latency_seconds {
                latency_sum += value;
                latency_count = latency_count.saturating_add(1);
            }
            if let Some(value) = sample.observation.ttft_seconds {
                ttft_sum += value;
                ttft_count = ttft_count.saturating_add(1);
            }
            if let Some(value) = sample.observation.throughput_chars_per_second {
                throughput_sum += value;
                throughput_count = throughput_count.saturating_add(1);
            }
            if let Some(value) = sample.observation.capacity_pressure {
                capacity_sum += value;
                capacity_count = capacity_count.saturating_add(1);
            }
        }
        if count == 0 || latency_count == 0 || capacity_count == 0 {
            return None;
        }
        Some(DeploymentSignalSummary {
            latency_seconds: latency_sum / latency_count as f64,
            ttft_seconds: (ttft_count > 0).then_some(ttft_sum / ttft_count as f64),
            throughput_chars_per_second: (throughput_count > 0)
                .then_some(throughput_sum / throughput_count as f64),
            availability: successes as f64 / count as f64,
            rate_limit_rate: rate_limited as f64 / count as f64,
            capacity_pressure: capacity_sum / capacity_count as f64,
        })
    }

    #[cfg(test)]
    fn key_count(&self) -> usize {
        self.state.lock().map_or(0, |state| state.len())
    }
}

/// Ordered concrete targets and the prompt-free reason for that ordering.
#[derive(Clone, Debug)]
pub(crate) struct DeploymentSelection {
    pub(crate) targets: Vec<ConfiguredModel>,
    pub(crate) reason: &'static str,
}

impl DeploymentObservationStore {
    pub(crate) fn select(
        &self,
        policy: &DeploymentSelectionPolicy,
        candidates: Vec<ConfiguredModel>,
    ) -> DeploymentSelection {
        let mut candidates = apply_price_ceiling(policy, candidates);
        if candidates.is_empty() {
            return DeploymentSelection {
                targets: Vec::new(),
                reason: "price-ceiling-blocked",
            };
        }
        if candidates.len() <= 1 || policy.strategy == DeploymentSelectionStrategy::Weighted {
            return DeploymentSelection {
                targets: candidates,
                reason: if policy.max_cost_per_1k.is_some() {
                    "weighted-price-ceiling"
                } else {
                    "weighted"
                },
            };
        }
        if policy.strategy == DeploymentSelectionStrategy::Cost {
            if candidates
                .iter()
                .all(|candidate| candidate.cost_per_1k().is_some())
            {
                candidates.sort_by(compare_cost);
                return DeploymentSelection {
                    targets: candidates,
                    reason: "cost",
                };
            }
            return DeploymentSelection {
                targets: candidates,
                reason: "weighted-cost-unknown",
            };
        }
        let summaries = candidates
            .iter()
            .map(|candidate| self.summary(candidate.delivery_id().as_str(), policy.observation_ttl))
            .collect::<Vec<_>>();
        let signals_ready = summaries.iter().all(|summary| {
            summary.is_some_and(|summary| match policy.strategy {
                DeploymentSelectionStrategy::Latency => summary.latency_seconds.is_finite(),
                DeploymentSelectionStrategy::Throughput => summary
                    .throughput_chars_per_second
                    .is_some_and(f64::is_finite),
                DeploymentSelectionStrategy::Availability => summary.availability.is_finite(),
                DeploymentSelectionStrategy::Capacity => summary.capacity_pressure.is_finite(),
                DeploymentSelectionStrategy::Weighted | DeploymentSelectionStrategy::Cost => true,
            })
        });
        if !signals_ready {
            return DeploymentSelection {
                targets: candidates,
                reason: "weighted-stale",
            };
        }
        candidates.sort_by(|left, right| {
            let Some(left_summary) =
                self.summary(left.delivery_id().as_str(), policy.observation_ttl)
            else {
                return left.delivery_id().cmp(&right.delivery_id());
            };
            let Some(right_summary) =
                self.summary(right.delivery_id().as_str(), policy.observation_ttl)
            else {
                return left.delivery_id().cmp(&right.delivery_id());
            };
            let ordering = match policy.strategy {
                DeploymentSelectionStrategy::Latency => left_summary
                    .latency_seconds
                    .total_cmp(&right_summary.latency_seconds)
                    .then_with(|| {
                        left_summary
                            .ttft_seconds
                            .unwrap_or(f64::MAX)
                            .total_cmp(&right_summary.ttft_seconds.unwrap_or(f64::MAX))
                    }),
                DeploymentSelectionStrategy::Throughput => right_summary
                    .throughput_chars_per_second
                    .unwrap_or(0.0)
                    .total_cmp(&left_summary.throughput_chars_per_second.unwrap_or(0.0)),
                DeploymentSelectionStrategy::Availability => right_summary
                    .availability
                    .total_cmp(&left_summary.availability)
                    .then_with(|| {
                        left_summary
                            .rate_limit_rate
                            .total_cmp(&right_summary.rate_limit_rate)
                    }),
                DeploymentSelectionStrategy::Capacity => left_summary
                    .capacity_pressure
                    .total_cmp(&right_summary.capacity_pressure),
                DeploymentSelectionStrategy::Weighted | DeploymentSelectionStrategy::Cost => {
                    Ordering::Equal
                }
            };
            ordering.then_with(|| left.delivery_id().cmp(&right.delivery_id()))
        });
        DeploymentSelection {
            targets: candidates,
            reason: policy.strategy.as_str(),
        }
    }
}

fn apply_price_ceiling(
    policy: &DeploymentSelectionPolicy,
    candidates: Vec<ConfiguredModel>,
) -> Vec<ConfiguredModel> {
    let Some(ceiling) = policy.max_cost_per_1k else {
        return candidates;
    };
    candidates
        .into_iter()
        .filter(|candidate| {
            candidate
                .cost_per_1k()
                .is_some_and(|cost| cost.is_finite() && cost <= ceiling)
        })
        .collect()
}

fn compare_cost(left: &ConfiguredModel, right: &ConfiguredModel) -> Ordering {
    left.cost_per_1k()
        .unwrap_or(f64::MAX)
        .total_cmp(&right.cost_per_1k().unwrap_or(f64::MAX))
        .then_with(|| left.delivery_id().cmp(&right.delivery_id()))
}

fn sanitize_observation(mut observation: DeploymentObservation) -> Option<DeploymentObservation> {
    for value in [
        observation.latency_seconds,
        observation.ttft_seconds,
        observation.throughput_chars_per_second,
        observation.capacity_pressure,
    ]
    .into_iter()
    .flatten()
    {
        if !value.is_finite() || value < 0.0 {
            return None;
        }
    }
    observation.capacity_pressure = observation.capacity_pressure.map(|value| value.min(1.0));
    Some(observation)
}

fn bounded_signal_id(value: &str) -> String {
    if value.len() <= MAX_SIGNAL_ID_BYTES
        && value
            .chars()
            .all(|character| !character.is_control() || character == '\n')
    {
        value.to_owned()
    } else {
        OVERFLOW_SIGNAL_ID.to_owned()
    }
}

fn bounded_clock(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    fn model(id: &str, cost: Option<f64>) -> ConfiguredModel {
        ConfiguredModel::new(id, format!("https://{id}.example/v1"), id, None, true)
            .with_cost_per_1k(cost)
            .with_deployments(Vec::new())
    }

    fn clock() -> (Arc<AtomicU64>, DeploymentObservationStore) {
        let now = Arc::new(AtomicU64::new(10));
        let source = Arc::clone(&now);
        let store =
            DeploymentObservationStore::new(move || source.load(AtomicOrdering::Relaxed) as f64);
        (now, store)
    }

    #[test]
    fn stale_or_sparse_signals_fall_back_to_weighted_order() {
        let (now, store) = clock();
        let left = model("a", Some(1.0));
        let right = model("b", Some(1.0));
        store.observe(
            &left.delivery_id(),
            DeploymentObservation::success(0.2, Some(0.1), Some(100.0)),
        );
        now.store(100, AtomicOrdering::Relaxed);
        let result = store.select(
            &DeploymentSelectionPolicy {
                strategy: DeploymentSelectionStrategy::Latency,
                ..DeploymentSelectionPolicy::default()
            },
            vec![right.clone(), left.clone()],
        );
        assert_eq!(result.reason, "weighted-stale");
        assert_eq!(result.targets[0].provider_model(), "b");
    }

    #[test]
    fn latency_selection_has_stable_identifier_ties() {
        let (_now, store) = clock();
        let left = model("a", Some(1.0));
        let right = model("b", Some(1.0));
        for candidate in [&left, &right] {
            store.observe(
                &candidate.delivery_id(),
                DeploymentObservation::success(0.2, Some(0.1), Some(100.0)),
            );
        }
        let result = store.select(
            &DeploymentSelectionPolicy {
                strategy: DeploymentSelectionStrategy::Latency,
                ..DeploymentSelectionPolicy::default()
            },
            vec![right, left],
        );
        assert_eq!(result.reason, "latency");
        assert_eq!(result.targets[0].provider_model(), "a");
    }

    #[test]
    fn price_ceiling_is_a_hard_member_filter() {
        let (_now, store) = clock();
        let cheap = model("cheap", Some(0.01));
        let expensive = model("expensive", Some(0.10));
        let result = store.select(
            &DeploymentSelectionPolicy {
                max_cost_per_1k: Some(0.02),
                ..DeploymentSelectionPolicy::default()
            },
            vec![expensive, cheap.clone()],
        );
        assert_eq!(result.reason, "weighted-price-ceiling");
        assert_eq!(result.targets.len(), 1);
        assert_eq!(result.targets[0].provider_model(), "cheap");
    }

    #[test]
    fn throughput_availability_and_capacity_strategies_use_their_signal() {
        let (_now, store) = clock();
        let fast = model("fast", Some(0.01));
        let slow = model("slow", Some(0.01));
        store.observe(
            &fast.delivery_id(),
            DeploymentObservation::success(0.2, Some(0.1), Some(200.0)),
        );
        store.observe(
            &slow.delivery_id(),
            DeploymentObservation::success(0.2, Some(0.1), Some(20.0)),
        );
        let throughput = store.select(
            &DeploymentSelectionPolicy {
                strategy: DeploymentSelectionStrategy::Throughput,
                ..DeploymentSelectionPolicy::default()
            },
            vec![slow.clone(), fast.clone()],
        );
        assert_eq!(throughput.targets[0].provider_model(), "fast");

        store.observe(
            &fast.delivery_id(),
            DeploymentObservation::failure(Some(0.2), Some(503)),
        );
        let availability = store.select(
            &DeploymentSelectionPolicy {
                strategy: DeploymentSelectionStrategy::Availability,
                ..DeploymentSelectionPolicy::default()
            },
            vec![fast.clone(), slow.clone()],
        );
        assert_eq!(availability.targets[0].provider_model(), "slow");

        let pressured = model("pressured", Some(0.01));
        store.observe(
            &pressured.delivery_id(),
            DeploymentObservation::failure(Some(0.2), Some(503)),
        );
        let capacity = store.select(
            &DeploymentSelectionPolicy {
                strategy: DeploymentSelectionStrategy::Capacity,
                ..DeploymentSelectionPolicy::default()
            },
            vec![pressured, slow],
        );
        assert_eq!(capacity.targets[0].provider_model(), "slow");
    }

    #[test]
    fn signal_key_cardinality_is_bounded() {
        let (_now, store) = clock();
        for index in 0..(MAX_SIGNAL_KEYS + 32) {
            store.observe(
                &format!("deployment-{index}"),
                DeploymentObservation::success(0.1, None, None),
            );
        }
        assert_eq!(store.key_count(), MAX_SIGNAL_KEYS);
    }
}
