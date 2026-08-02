//! Bounded off-path shadow routing and provider comparison.
//!
//! Shadow work is deliberately an observer of the production request. It is
//! sampled and admitted without awaiting it, never participates in the
//! production reliability/budget/admission path, and stores only bounded
//! prompt-free decision metadata.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::OwnedSemaphorePermit;
use wayfinder_config::gateway::{RoutePreset, SHADOW_PROVIDER_CONSENT_HEADER, ShadowConfig};
use wayfinder_routing_core::{RUNTIME_CONTRACT_VERSION, RoutingRequest, score_complexity};

use crate::delivery::BufferedDeliveryResponse;
use crate::{AppState, eligible_deployment_targets};

/// Stable identifier for the shadow record schema.
pub(crate) const SHADOW_SCHEMA_VERSION: &str = "wf-shadow-v1";
/// Stable reason used when a candidate route has no eligible model.
pub(crate) const SHADOW_REASON_NO_ELIGIBLE_MODEL: &str = "no-eligible-model";

/// Prompt-free metadata for one counterfactual candidate evaluation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ShadowRecord {
    /// Production request identity; never a prompt digest.
    pub request_id: String,
    /// Candidate route evaluated off-path.
    pub candidate_route: String,
    /// Model selected by production routing.
    pub production_model: String,
    /// First eligible model in the candidate route, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow_model: Option<String>,
    /// Production deterministic score.
    pub production_score: f64,
    /// Counterfactual deterministic score.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow_score: Option<f64>,
    /// Whether the candidate would choose a different model.
    pub divergent: bool,
    /// Terminal decision state.
    pub status: String,
    /// Stable, bounded reason codes.
    pub reason_codes: Vec<String>,
    /// Whether a provider comparison was attempted.
    pub provider_compared: bool,
    /// Provider HTTP status when a comparison returned an ordinary response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_status: Option<u16>,
    /// Provider comparison latency, in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_latency_ms: Option<u64>,
    /// Version of the authoritative routing contract.
    pub scorer_version: u32,
    /// Fingerprint of the active routing configuration.
    pub config_fingerprint: String,
    /// Fingerprint of the candidate route inventory.
    pub candidate_fingerprint: String,
}

/// Bounded report over retained shadow records and prompt-free counters.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ShadowSnapshot {
    /// Retained records, newest first.
    pub records: Vec<ShadowRecord>,
    /// Number of sampled evaluations admitted.
    pub sampled: u64,
    /// Number rejected by the process-local concurrency bound.
    pub dropped_in_flight: u64,
    /// Number of provider comparisons rejected by their budget.
    pub dropped_provider_budget: u64,
    /// Number of completed decision evaluations.
    pub completed: u64,
    /// Number of candidate divergences.
    pub divergent: u64,
    /// Number of candidate evaluations that failed before a decision.
    pub errors: u64,
    /// Number of provider comparisons attempted.
    pub provider_comparisons: u64,
    /// Number of provider comparison errors.
    pub provider_errors: u64,
}

#[derive(Default)]
struct ShadowState {
    records: VecDeque<ShadowRecord>,
    sampled: u64,
    dropped_in_flight: u64,
    dropped_provider_budget: u64,
    completed: u64,
    divergent: u64,
    errors: u64,
    provider_comparisons: u64,
    provider_errors: u64,
    provider_window_start: f64,
    provider_window_used: u64,
}

/// Process-local bounded shadow record store.
pub(crate) struct ShadowStore {
    state: Mutex<ShadowState>,
    max_records: usize,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    active: AtomicBool,
}

impl std::fmt::Debug for ShadowStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShadowStore")
            .field("max_records", &self.max_records)
            .field("active", &self.active.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl ShadowStore {
    /// Construct a bounded store with an injected monotonic clock.
    pub(crate) fn new(max_records: u64, clock: impl Fn() -> f64 + Send + Sync + 'static) -> Self {
        Self {
            state: Mutex::new(ShadowState::default()),
            max_records: usize::try_from(max_records).unwrap_or(usize::MAX).max(1),
            clock: Arc::new(clock),
            active: AtomicBool::new(true),
        }
    }

    /// Stop or resume new and in-flight shadow work at a configuration boundary.
    pub(crate) fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Release);
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub(crate) fn sampled(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.sampled = state.sampled.saturating_add(1);
        }
    }

    pub(crate) fn dropped_in_flight(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.dropped_in_flight = state.dropped_in_flight.saturating_add(1);
        }
    }

    pub(crate) fn provider_slot(&self, config: &ShadowConfig) -> bool {
        let now = (self.clock)();
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if now.is_finite()
            && (state.provider_window_start <= 0.0
                || now - state.provider_window_start >= config.provider_window)
        {
            state.provider_window_start = now.max(0.0);
            state.provider_window_used = 0;
        }
        if state.provider_window_used >= config.provider_max_requests {
            state.dropped_provider_budget = state.dropped_provider_budget.saturating_add(1);
            return false;
        }
        state.provider_window_used = state.provider_window_used.saturating_add(1);
        state.provider_comparisons = state.provider_comparisons.saturating_add(1);
        true
    }

    pub(crate) fn provider_error(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.provider_errors = state.provider_errors.saturating_add(1);
        }
    }

    pub(crate) fn record(&self, record: ShadowRecord) {
        if let Ok(mut state) = self.state.lock() {
            state.completed = state.completed.saturating_add(1);
            if record.divergent {
                state.divergent = state.divergent.saturating_add(1);
            }
            state.records.push_front(record);
            while state.records.len() > self.max_records {
                state.records.pop_back();
            }
        }
    }

    pub(crate) fn error(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.errors = state.errors.saturating_add(1);
        }
    }

    pub(crate) fn snapshot(&self, limit: usize) -> ShadowSnapshot {
        let Ok(state) = self.state.lock() else {
            return ShadowSnapshot::default();
        };
        let limit = limit.min(self.max_records);
        ShadowSnapshot {
            records: state.records.iter().take(limit).cloned().collect(),
            sampled: state.sampled,
            dropped_in_flight: state.dropped_in_flight,
            dropped_provider_budget: state.dropped_provider_budget,
            completed: state.completed,
            divergent: state.divergent,
            errors: state.errors,
            provider_comparisons: state.provider_comparisons,
            provider_errors: state.provider_errors,
        }
    }
}

/// One request admitted to off-path evaluation.
#[derive(Clone)]
pub(crate) struct ShadowJob {
    pub request_id: String,
    pub prompt: String,
    pub routing_request: RoutingRequest,
    pub production_model: String,
    pub production_score: f64,
    pub request_body: Value,
    pub provider_consent: bool,
}

/// Schedule an admitted shadow job and release its concurrency permit.
pub(crate) async fn evaluate(
    state: AppState,
    config: ShadowConfig,
    store: Arc<ShadowStore>,
    job: ShadowJob,
    permit: OwnedSemaphorePermit,
) {
    let _permit = permit;
    if !store.is_active() {
        return;
    }
    let candidate_fingerprint = candidate_fingerprint(&config.candidate_routes);
    let config_fingerprint = config_fingerprint(state.routing(), &config);
    let score = match score_complexity(&job.prompt, state.routing()) {
        Ok(score) => score,
        Err(_) => {
            store.error();
            state.metrics().observe_shadow_event("error", "score");
            return;
        }
    };
    for candidate_route in &config.candidate_routes {
        if !store.is_active() {
            return;
        }
        let Some(route) = state.route_preset(candidate_route) else {
            store.error();
            state.metrics().observe_shadow_event("error", "route");
            continue;
        };
        let (shadow_model, reason_codes) = select_candidate(&state, &job.routing_request, route);
        let divergent = shadow_model
            .as_deref()
            .is_some_and(|model| model != job.production_model);
        let mut provider_compared = false;
        let mut provider_status = None;
        let mut provider_latency_ms = None;
        let mut provider_reason = None;
        if config.provider_comparisons
            && config.provider_sample_rate > 0.0
            && job.provider_consent
            && job.routing_request.privacy_posture
                == wayfinder_routing_core::PrivacyPosture::HostedAllowed
            && shadow_model.is_some()
            && deterministic_sample(
                &format!("{}:{candidate_route}:provider", job.request_id),
                &config_fingerprint,
                config.provider_sample_rate,
            )
            && store.provider_slot(&config)
        {
            provider_compared = true;
            let started = Instant::now();
            let model_name = shadow_model.as_deref().unwrap_or_default();
            let targets = state.deployment_candidates(model_name);
            let targets = eligible_deployment_targets(targets, &job.routing_request, false, 0);
            let targets = state.select_deployments(model_name, targets).targets;
            if let (Some(delivery), Some(target)) = (state.delivery_arc(), targets.first()) {
                match delivery.send(target, job.request_body.clone()).await {
                    Ok(response) => {
                        provider_status = Some(response.status().as_u16());
                        provider_latency_ms =
                            Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
                        if response.status().is_success() {
                            state
                                .metrics()
                                .observe_shadow_event("provider-success", candidate_route);
                            let cost = shadow_cost(&state, model_name, &job, &response);
                            if cost > 0.0 {
                                state.metrics().observe_shadow_cost(cost);
                            }
                        } else {
                            store.provider_error();
                            provider_reason = Some("provider-status".to_owned());
                            state
                                .metrics()
                                .observe_shadow_event("provider-error", candidate_route);
                        }
                    }
                    Err(_) => {
                        store.provider_error();
                        provider_reason = Some("provider-error".to_owned());
                        state
                            .metrics()
                            .observe_shadow_event("provider-error", candidate_route);
                    }
                }
            } else {
                store.provider_error();
                provider_reason = Some("provider-unavailable".to_owned());
                state
                    .metrics()
                    .observe_shadow_event("provider-error", candidate_route);
            }
        } else if config.provider_comparisons && job.provider_consent {
            provider_reason = Some("provider-comparison-not-admitted".to_owned());
        }
        let mut reasons = reason_codes;
        if let Some(reason) = provider_reason {
            reasons.push(reason);
        }
        if divergent {
            state
                .metrics()
                .observe_shadow_event("divergence", candidate_route);
        } else {
            state
                .metrics()
                .observe_shadow_event("match", candidate_route);
        }
        store.record(ShadowRecord {
            request_id: job.request_id.clone(),
            candidate_route: candidate_route.clone(),
            production_model: job.production_model.clone(),
            shadow_model,
            production_score: job.production_score,
            shadow_score: Some(score.score),
            divergent,
            status: "completed".to_owned(),
            reason_codes: reasons,
            provider_compared,
            provider_status,
            provider_latency_ms,
            scorer_version: RUNTIME_CONTRACT_VERSION,
            config_fingerprint: config_fingerprint.clone(),
            candidate_fingerprint: candidate_fingerprint.clone(),
        });
    }
}

fn select_candidate(
    state: &AppState,
    request: &RoutingRequest,
    route: &RoutePreset,
) -> (Option<String>, Vec<String>) {
    let mut reasons = Vec::new();
    for model_name in &route.models {
        let Some(model) = state
            .models()
            .iter()
            .find(|model| model.name() == model_name)
        else {
            reasons.push(format!("{model_name}:unknown-model"));
            continue;
        };
        let assessment = crate::assess_model(request, model);
        if assessment.is_eligible() {
            return (Some(model_name.clone()), reasons);
        }
        reasons.push(format!(
            "{model_name}:{}",
            assessment
                .exclusions
                .iter()
                .map(|reason| crate::exclusion_reason_name(*reason))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    reasons.push(SHADOW_REASON_NO_ELIGIBLE_MODEL.to_owned());
    (None, reasons)
}

fn shadow_cost(
    state: &AppState,
    model: &str,
    job: &ShadowJob,
    response: &BufferedDeliveryResponse,
) -> f64 {
    if !response.content_type().contains("json") {
        return 0.0;
    }
    let Ok(value) = serde_json::from_slice::<Value>(response.body()) else {
        return 0.0;
    };
    let prompt = crate::extract_prompt(
        job.request_body.get("messages").unwrap_or(&Value::Null),
        crate::RouteOn::All,
    );
    let usage =
        wayfinder_service::pricing::usage_tokens(&value, &prompt, crate::first_choice_text(&value));
    wayfinder_service::pricing::turn_cost(
        model,
        usage.prompt,
        usage.completion,
        &state.price_table().costs,
        usage.estimated,
        None,
    )
    .map_or(0.0, |cost| cost.realized)
}

pub(crate) fn deterministic_sample(request_id: &str, salt: &str, rate: f64) -> bool {
    if rate <= 0.0 {
        return false;
    }
    if rate >= 1.0 {
        return true;
    }
    let mut digest = Sha256::new();
    digest.update(request_id.as_bytes());
    digest.update([0]);
    digest.update(salt.as_bytes());
    let bytes = digest.finalize();
    let value = u64::from_be_bytes(bytes[..8].try_into().unwrap_or([0; 8]));
    (value as f64 / u64::MAX as f64) < rate
}

pub(crate) fn candidate_fingerprint(routes: &[String]) -> String {
    let mut digest = Sha256::new();
    digest.update(SHADOW_SCHEMA_VERSION.as_bytes());
    for route in routes {
        digest.update([0]);
        digest.update(route.as_bytes());
    }
    short_fingerprint(&digest.finalize())
}

pub(crate) fn config_fingerprint(
    routing: &wayfinder_routing_core::RoutingConfig,
    config: &ShadowConfig,
) -> String {
    let mut digest = Sha256::new();
    digest.update(wayfinder_config::dump_routing_toml(routing).as_bytes());
    digest.update([0]);
    digest.update(config.enabled.to_string().as_bytes());
    digest.update([0]);
    digest.update(config.sample_rate.to_string().as_bytes());
    digest.update([0]);
    for route in &config.candidate_routes {
        digest.update(route.as_bytes());
        digest.update([0]);
    }
    digest.update(config.max_in_flight.to_string().as_bytes());
    digest.update([0]);
    digest.update(config.max_records.to_string().as_bytes());
    digest.update([0]);
    digest.update(config.provider_comparisons.to_string().as_bytes());
    digest.update([0]);
    digest.update(config.provider_sample_rate.to_string().as_bytes());
    digest.update([0]);
    digest.update(config.provider_max_requests.to_string().as_bytes());
    digest.update([0]);
    digest.update(config.provider_window.to_string().as_bytes());
    short_fingerprint(&digest.finalize())
}

fn short_fingerprint(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether the explicit provider-comparison consent header is affirmative.
#[must_use]
pub(crate) fn provider_consent(headers: &http::HeaderMap) -> bool {
    headers
        .get(SHADOW_PROVIDER_CONSENT_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_sampling_is_stable_and_bounded() {
        assert!(!deterministic_sample("request", "salt", 0.0));
        assert!(deterministic_sample("request", "salt", 1.0));
        assert_eq!(
            deterministic_sample("request", "salt", 0.5),
            deterministic_sample("request", "salt", 0.5)
        );
    }

    #[test]
    fn candidate_fingerprint_is_ordered_and_versioned() {
        assert_ne!(
            candidate_fingerprint(&["a".to_owned()]),
            candidate_fingerprint(&["b".to_owned()])
        );
        assert_eq!(candidate_fingerprint(&[]), candidate_fingerprint(&[]));
    }

    #[test]
    fn store_can_stop_and_resume_in_flight_work() {
        let store = ShadowStore::new(2, || 1.0);
        assert!(store.is_active());
        store.set_active(false);
        assert!(!store.is_active());
        store.set_active(true);
        assert!(store.is_active());
    }
}
