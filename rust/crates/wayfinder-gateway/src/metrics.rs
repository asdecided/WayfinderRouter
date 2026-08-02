//! Thread-safe, prompt-free Prometheus gateway metrics.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Mutex;

use thiserror::Error;
use wayfinder_routing_core::python_round;

/// Default maximum unique values retained by any dynamic label family.
pub const DEFAULT_MAX_LABEL_SERIES: usize = 256;
/// Maximum bytes retained from one label value.
pub const MAX_LABEL_BYTES: usize = 128;
const OVERFLOW_LABEL: &str = "__other__";
const DECISION_BUCKETS: [f64; 9] = [
    0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05,
];
const UPSTREAM_BUCKETS: [f64; 10] = [0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0];
const TTFT_BUCKETS: [f64; 10] = [0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0];
const THROUGHPUT_BUCKETS: [f64; 8] = [1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 500.0, 1_000.0];
const CAPACITY_BUCKETS: [f64; 6] = [0.01, 0.1, 0.25, 0.5, 0.75, 1.0];
const QUEUE_WAIT_BUCKETS: [f64; 8] = [0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 2.0];

/// Metrics synchronization or invalid-observation failure.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum MetricsError {
    /// Internal metrics state could not be synchronized.
    #[error("gateway metrics lock is unavailable")]
    LockPoisoned,
    /// A duration or cost observation was negative or non-finite.
    #[error("gateway metric observations must be finite and non-negative")]
    InvalidObservation,
}

#[derive(Clone, Debug)]
struct Histogram<const N: usize> {
    bounds: [f64; N],
    counts: [u64; N],
    sum: f64,
    count: u64,
}

impl<const N: usize> Histogram<N> {
    const fn new(bounds: [f64; N]) -> Self {
        Self {
            bounds,
            counts: [0; N],
            sum: 0.0,
            count: 0,
        }
    }

    fn observe(&mut self, value: f64) -> Result<(), MetricsError> {
        validate_observation(value)?;
        self.sum = (self.sum + value).min(f64::MAX);
        self.count = self.count.saturating_add(1);
        for (index, bound) in self.bounds.iter().enumerate() {
            if value <= *bound {
                self.counts[index] = self.counts[index].saturating_add(1);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct MetricsState {
    requests: BTreeMap<(String, String), u64>,
    upstream_errors: BTreeMap<String, u64>,
    reload_failures: u64,
    decision: Histogram<9>,
    upstream: BTreeMap<String, Histogram<10>>,
    deployment_ttft: BTreeMap<String, Histogram<10>>,
    deployment_throughput: BTreeMap<String, Histogram<8>>,
    deployment_capacity: BTreeMap<String, Histogram<6>>,
    deployment_selections: BTreeMap<(String, String, String), u64>,
    shadow_events: BTreeMap<(String, String), u64>,
    shadow_provider_cost: f64,
    model_costs: BTreeMap<String, f64>,
    realized_cost: f64,
    baseline_cost: f64,
    cache_hits: u64,
    cache_misses: u64,
    cache_avoided_cost: f64,
    rate_limited: BTreeMap<String, u64>,
    key_requests: BTreeMap<String, u64>,
    admission_in_flight: u64,
    admission_peak_in_flight: u64,
    admission_queue_wait: Histogram<8>,
    admission_rejected: BTreeMap<String, u64>,
    state_degraded: bool,
}

impl Default for MetricsState {
    fn default() -> Self {
        Self {
            requests: BTreeMap::new(),
            upstream_errors: BTreeMap::new(),
            reload_failures: 0,
            decision: Histogram::new(DECISION_BUCKETS),
            upstream: BTreeMap::new(),
            deployment_ttft: BTreeMap::new(),
            deployment_throughput: BTreeMap::new(),
            deployment_capacity: BTreeMap::new(),
            deployment_selections: BTreeMap::new(),
            shadow_events: BTreeMap::new(),
            shadow_provider_cost: 0.0,
            model_costs: BTreeMap::new(),
            realized_cost: 0.0,
            baseline_cost: 0.0,
            cache_hits: 0,
            cache_misses: 0,
            cache_avoided_cost: 0.0,
            rate_limited: BTreeMap::new(),
            key_requests: BTreeMap::new(),
            admission_in_flight: 0,
            admission_peak_in_flight: 0,
            admission_queue_wait: Histogram::new(QUEUE_WAIT_BUCKETS),
            admission_rejected: BTreeMap::new(),
            state_degraded: false,
        }
    }
}

/// In-memory gateway metrics with bounded dynamic label cardinality.
#[derive(Debug)]
pub struct GatewayMetrics {
    version: String,
    max_label_series: usize,
    state: Mutex<MetricsState>,
}

impl GatewayMetrics {
    /// Construct an empty collector. Zero series capacity retains only the overflow bucket.
    #[must_use]
    pub fn new(version: impl Into<String>, max_label_series: usize) -> Self {
        let version = bounded_label(&version.into());
        Self {
            version,
            max_label_series,
            state: Mutex::new(MetricsState::default()),
        }
    }

    /// Replace informational per-model prices, retaining sorted bounded metadata only.
    pub fn set_model_costs(&self, costs: &BTreeMap<String, f64>) -> Result<(), MetricsError> {
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        state.model_costs.clear();
        for (model, cost) in costs {
            validate_observation(*cost)?;
            if state.model_costs.len() >= self.max_label_series {
                break;
            }
            state.model_costs.insert(bounded_label(model), *cost);
        }
        Ok(())
    }

    /// Accumulate realized and always-frontier baseline cost.
    pub fn observe_cost(&self, realized: f64, baseline: f64) -> Result<(), MetricsError> {
        validate_observation(realized)?;
        validate_observation(baseline)?;
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        state.realized_cost = python_round((state.realized_cost + realized).min(f64::MAX), 6);
        state.baseline_cost = python_round((state.baseline_cost + baseline).min(f64::MAX), 6);
        Ok(())
    }

    /// Record one deterministic routing decision.
    pub fn observe_decision(
        &self,
        model: &str,
        mode: &str,
        seconds: f64,
    ) -> Result<(), MetricsError> {
        validate_observation(seconds)?;
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        let key = (bounded_label(model), bounded_label(mode));
        let key = bounded_pair_key(&state.requests, key, self.max_label_series);
        saturating_increment(state.requests.entry(key).or_default());
        state.decision.observe(seconds)
    }

    /// Record one successful upstream round-trip.
    pub fn observe_upstream(&self, model: &str, seconds: f64) -> Result<(), MetricsError> {
        validate_observation(seconds)?;
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        let key = bounded_map_key(&state.upstream, model, self.max_label_series);
        state
            .upstream
            .entry(key)
            .or_insert_with(|| Histogram::new(UPSTREAM_BUCKETS))
            .observe(seconds)
    }

    /// Record one upstream transport failure.
    pub fn observe_upstream_error(&self, model: &str) -> Result<(), MetricsError> {
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        let key = bounded_map_key(&state.upstream_errors, model, self.max_label_series);
        saturating_increment(state.upstream_errors.entry(key).or_default());
        Ok(())
    }

    /// Record a prompt-free deployment time-to-first-token observation.
    pub fn observe_deployment_ttft(
        &self,
        deployment: &str,
        seconds: f64,
    ) -> Result<(), MetricsError> {
        validate_observation(seconds)?;
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        let key = bounded_map_key(&state.deployment_ttft, deployment, self.max_label_series);
        state
            .deployment_ttft
            .entry(key)
            .or_insert_with(|| Histogram::new(TTFT_BUCKETS))
            .observe(seconds)
    }

    /// Record a prompt-free deployment completion-throughput observation.
    pub fn observe_deployment_throughput(
        &self,
        deployment: &str,
        chars_per_second: f64,
    ) -> Result<(), MetricsError> {
        validate_observation(chars_per_second)?;
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        let key = bounded_map_key(
            &state.deployment_throughput,
            deployment,
            self.max_label_series,
        );
        state
            .deployment_throughput
            .entry(key)
            .or_insert_with(|| Histogram::new(THROUGHPUT_BUCKETS))
            .observe(chars_per_second)
    }

    /// Record a prompt-free provider capacity-pressure observation.
    pub fn observe_deployment_capacity(
        &self,
        deployment: &str,
        pressure: f64,
    ) -> Result<(), MetricsError> {
        validate_observation(pressure)?;
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        let key = bounded_map_key(
            &state.deployment_capacity,
            deployment,
            self.max_label_series,
        );
        state
            .deployment_capacity
            .entry(key)
            .or_insert_with(|| Histogram::new(CAPACITY_BUCKETS))
            .observe(pressure.min(1.0))
    }

    /// Count one bounded deployment-selection reason without request content.
    pub fn observe_deployment_selection(
        &self,
        model: &str,
        deployment: &str,
        reason: &str,
    ) -> Result<(), MetricsError> {
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        let key = bounded_triple_key(
            &state.deployment_selections,
            (
                bounded_label(model),
                bounded_label(deployment),
                bounded_label(reason),
            ),
            self.max_label_series,
        );
        saturating_increment(state.deployment_selections.entry(key).or_default());
        Ok(())
    }

    /// Count one bounded prompt-free shadow event.
    pub fn observe_shadow_event(&self, event: &str, candidate: &str) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let key = bounded_pair_key(
            &state.shadow_events,
            (bounded_label(event), bounded_label(candidate)),
            self.max_label_series,
        );
        saturating_increment(state.shadow_events.entry(key).or_default());
    }

    /// Accumulate the estimated cost of explicitly authorized provider shadows.
    pub fn observe_shadow_cost(&self, cost: f64) {
        if !cost.is_finite() || cost < 0.0 {
            return;
        }
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.shadow_provider_cost =
            python_round((state.shadow_provider_cost + cost).min(f64::MAX), 6);
    }

    /// Record one exact-match cache hit and its avoided upstream cost.
    pub fn observe_cache_hit(&self, avoided_cost: f64) -> Result<(), MetricsError> {
        validate_observation(avoided_cost)?;
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        state.cache_hits = state.cache_hits.saturating_add(1);
        state.cache_avoided_cost =
            python_round((state.cache_avoided_cost + avoided_cost).min(f64::MAX), 6);
        Ok(())
    }

    /// Record one cacheable miss.
    pub fn observe_cache_miss(&self) -> Result<(), MetricsError> {
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        state.cache_misses = state.cache_misses.saturating_add(1);
        Ok(())
    }

    /// Record one `rpm` or `tpm` rejection.
    pub fn observe_rate_limited(&self, limit: &str) -> Result<(), MetricsError> {
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        let key = bounded_map_key(&state.rate_limited, limit, self.max_label_series);
        saturating_increment(state.rate_limited.entry(key).or_default());
        Ok(())
    }

    /// Attribute one authenticated request by virtual-key identifier, never token.
    pub fn observe_key_request(&self, key_id: &str) -> Result<(), MetricsError> {
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        let key = bounded_map_key(&state.key_requests, key_id, self.max_label_series);
        saturating_increment(state.key_requests.entry(key).or_default());
        Ok(())
    }

    /// Record one admitted upstream delivery and its bounded queue wait.
    pub fn observe_admission(&self, waited_seconds: f64) -> Result<(), MetricsError> {
        validate_observation(waited_seconds)?;
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        state.admission_in_flight = state.admission_in_flight.saturating_add(1);
        state.admission_peak_in_flight = state
            .admission_peak_in_flight
            .max(state.admission_in_flight);
        state.admission_queue_wait.observe(waited_seconds)
    }

    /// Record release of one admitted upstream delivery.
    pub fn observe_admission_release(&self) -> Result<(), MetricsError> {
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        state.admission_in_flight = state.admission_in_flight.saturating_sub(1);
        Ok(())
    }

    /// Record one request rejected by the bounded admission queue.
    pub fn observe_admission_rejected(&self, reason: &str) -> Result<(), MetricsError> {
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        let key = bounded_map_key(&state.admission_rejected, reason, self.max_label_series);
        saturating_increment(state.admission_rejected.entry(key).or_default());
        Ok(())
    }

    /// Record a failed hot reload that retained last-good state.
    pub fn record_reload_failure(&self) -> Result<(), MetricsError> {
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        state.reload_failures = state.reload_failures.saturating_add(1);
        Ok(())
    }

    /// Set the shared-state degradation gauge without retaining backend details.
    pub fn set_state_degraded(&self, degraded: bool) -> Result<(), MetricsError> {
        let mut state = self.state.lock().map_err(|_| MetricsError::LockPoisoned)?;
        state.state_degraded = degraded;
        Ok(())
    }

    /// Render deterministic Prometheus text exposition compatible with Python.
    pub fn render(&self) -> Result<String, MetricsError> {
        let state = self
            .state
            .lock()
            .map_err(|_| MetricsError::LockPoisoned)?
            .clone();
        Ok(render_state(&self.version, &state))
    }
}

impl Default for GatewayMetrics {
    fn default() -> Self {
        Self::new(
            option_env!("WAYFINDER_PRODUCT_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")),
            DEFAULT_MAX_LABEL_SERIES,
        )
    }
}

fn validate_observation(value: f64) -> Result<(), MetricsError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(MetricsError::InvalidObservation)
    }
}

fn saturating_increment(value: &mut u64) {
    *value = value.saturating_add(1);
}

fn bounded_label(value: &str) -> String {
    if value.len() <= MAX_LABEL_BYTES
        && value
            .chars()
            .all(|character| !character.is_control() || character == '\n')
    {
        value.to_owned()
    } else {
        OVERFLOW_LABEL.to_owned()
    }
}

fn bounded_map_key<V>(map: &BTreeMap<String, V>, value: &str, maximum: usize) -> String {
    let value = bounded_label(value);
    if map.contains_key(&value) || map.len() < maximum {
        value
    } else {
        OVERFLOW_LABEL.to_owned()
    }
}

fn bounded_pair_key<V>(
    map: &BTreeMap<(String, String), V>,
    value: (String, String),
    maximum: usize,
) -> (String, String) {
    if map.contains_key(&value) || map.len() < maximum {
        value
    } else {
        (OVERFLOW_LABEL.to_owned(), OVERFLOW_LABEL.to_owned())
    }
}

fn bounded_triple_key<V>(
    map: &BTreeMap<(String, String, String), V>,
    value: (String, String, String),
    maximum: usize,
) -> (String, String, String) {
    if map.contains_key(&value) || map.len() < maximum {
        value
    } else {
        (
            OVERFLOW_LABEL.to_owned(),
            OVERFLOW_LABEL.to_owned(),
            OVERFLOW_LABEL.to_owned(),
        )
    }
}

fn label_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

fn python_general(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0".to_owned()
        } else {
            "0".to_owned()
        };
    }
    let exponent = value.abs().log10().floor() as i32;
    if !(-4..6).contains(&exponent) {
        let rendered = format!("{value:.5e}");
        let (mantissa, raw_exponent) = rendered.split_once('e').unwrap_or((&rendered, "+0"));
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        let exponent_value = raw_exponent.parse::<i32>().unwrap_or(0);
        let sign = if exponent_value < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exponent_value.unsigned_abs())
    } else {
        let decimals = usize::try_from(5_i32.saturating_sub(exponent)).unwrap_or(0);
        let rendered = format!("{value:.decimals$}");
        if rendered.contains('.') {
            rendered
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_owned()
        } else {
            rendered
        }
    }
}

fn render_histogram<const N: usize>(
    output: &mut String,
    name: &str,
    histogram: &Histogram<N>,
    label_pairs: &str,
) {
    let separator = if label_pairs.is_empty() { "" } else { "," };
    for (bound, count) in histogram.bounds.iter().zip(histogram.counts) {
        let _ = writeln!(
            output,
            "{name}_bucket{{{label_pairs}{separator}le=\"{}\"}} {count}",
            python_general(*bound)
        );
    }
    let _ = writeln!(
        output,
        "{name}_bucket{{{label_pairs}{separator}le=\"+Inf\"}} {}",
        histogram.count
    );
    let braces = if label_pairs.is_empty() {
        String::new()
    } else {
        format!("{{{label_pairs}}}")
    };
    let _ = writeln!(
        output,
        "{name}_sum{braces} {}",
        python_general(histogram.sum)
    );
    let _ = writeln!(output, "{name}_count{braces} {}", histogram.count);
}

#[allow(clippy::too_many_lines)]
fn render_state(version: &str, state: &MetricsState) -> String {
    let mut output = String::new();
    output.push_str("# HELP wayfinder_router_build_info Build information.\n");
    output.push_str("# TYPE wayfinder_router_build_info gauge\n");
    let _ = writeln!(
        output,
        "wayfinder_router_build_info{{version=\"{}\"}} 1",
        label_escape(version)
    );

    output.push_str("# HELP wayfinder_router_requests_total Routed requests by model and mode.\n");
    output.push_str("# TYPE wayfinder_router_requests_total counter\n");
    for ((model, mode), count) in &state.requests {
        let _ = writeln!(
            output,
            "wayfinder_router_requests_total{{model=\"{}\",mode=\"{}\"}} {count}",
            label_escape(model),
            label_escape(mode)
        );
    }

    output.push_str(
        "# HELP wayfinder_router_upstream_errors_total Upstream transport failures by model.\n",
    );
    output.push_str("# TYPE wayfinder_router_upstream_errors_total counter\n");
    for (model, count) in &state.upstream_errors {
        let _ = writeln!(
            output,
            "wayfinder_router_upstream_errors_total{{model=\"{}\"}} {count}",
            label_escape(model)
        );
    }

    output.push_str(
        "# HELP wayfinder_router_cache_hits_total Exact-match response cache hits (WF-ADR-0033).\n",
    );
    output.push_str("# TYPE wayfinder_router_cache_hits_total counter\n");
    let _ = writeln!(
        output,
        "wayfinder_router_cache_hits_total {}",
        state.cache_hits
    );
    output.push_str(
        "# HELP wayfinder_router_cache_misses_total Cacheable requests that missed the cache.\n",
    );
    output.push_str("# TYPE wayfinder_router_cache_misses_total counter\n");
    let _ = writeln!(
        output,
        "wayfinder_router_cache_misses_total {}",
        state.cache_misses
    );
    output.push_str("# HELP wayfinder_router_cache_avoided_cost_total Upstream cost avoided by cache hits (chosen-tier cost; distinct from routing savings vs always-frontier).\n");
    output.push_str("# TYPE wayfinder_router_cache_avoided_cost_total counter\n");
    let _ = writeln!(
        output,
        "wayfinder_router_cache_avoided_cost_total {}",
        python_general(state.cache_avoided_cost)
    );

    output.push_str("# HELP wayfinder_router_rate_limited_total Requests rejected with 429 by limit (WF-ADR-0034).\n");
    output.push_str("# TYPE wayfinder_router_rate_limited_total counter\n");
    for (limit, count) in &state.rate_limited {
        let _ = writeln!(
            output,
            "wayfinder_router_rate_limited_total{{limit=\"{}\"}} {count}",
            label_escape(limit)
        );
    }

    if !state.key_requests.is_empty() {
        output.push_str("# HELP wayfinder_router_key_requests_total Requests by virtual-key id (WF-ADR-0035).\n");
        output.push_str("# TYPE wayfinder_router_key_requests_total counter\n");
        for (key, count) in &state.key_requests {
            let _ = writeln!(
                output,
                "wayfinder_router_key_requests_total{{key=\"{}\"}} {count}",
                label_escape(key)
            );
        }
    }

    output.push_str(
        "# HELP wayfinder_router_in_flight_deliveries Current admitted upstream deliveries.\n",
    );
    output.push_str("# TYPE wayfinder_router_in_flight_deliveries gauge\n");
    let _ = writeln!(
        output,
        "wayfinder_router_in_flight_deliveries {}",
        state.admission_in_flight
    );
    output.push_str("# HELP wayfinder_router_peak_in_flight_deliveries Highest simultaneous admitted deliveries since process start.\n");
    output.push_str("# TYPE wayfinder_router_peak_in_flight_deliveries gauge\n");
    let _ = writeln!(
        output,
        "wayfinder_router_peak_in_flight_deliveries {}",
        state.admission_peak_in_flight
    );
    output.push_str("# HELP wayfinder_router_admission_queue_wait_seconds Time admitted deliveries waited for capacity.\n");
    output.push_str("# TYPE wayfinder_router_admission_queue_wait_seconds histogram\n");
    render_histogram(
        &mut output,
        "wayfinder_router_admission_queue_wait_seconds",
        &state.admission_queue_wait,
        "",
    );
    output.push_str("# HELP wayfinder_router_admission_rejected_total Requests rejected by bounded delivery admission.\n");
    output.push_str("# TYPE wayfinder_router_admission_rejected_total counter\n");
    for (reason, count) in &state.admission_rejected {
        let _ = writeln!(
            output,
            "wayfinder_router_admission_rejected_total{{reason=\"{}\"}} {count}",
            label_escape(reason)
        );
    }

    output.push_str("# HELP wayfinder_router_config_reload_failures_total Config reloads that failed and kept the last-good config.\n");
    output.push_str("# TYPE wayfinder_router_config_reload_failures_total counter\n");
    let _ = writeln!(
        output,
        "wayfinder_router_config_reload_failures_total {}",
        state.reload_failures
    );

    output.push_str("# HELP wayfinder_state_degraded Shared-state backend unavailable; process-local fallback is active.\n");
    output.push_str("# TYPE wayfinder_state_degraded gauge\n");
    let _ = writeln!(
        output,
        "wayfinder_state_degraded {}",
        u8::from(state.state_degraded)
    );

    if !state.model_costs.is_empty() {
        output.push_str("# HELP wayfinder_router_model_cost_per_1k Configured per-1k-token cost by model (informational, WF-ADR-0017).\n");
        output.push_str("# TYPE wayfinder_router_model_cost_per_1k gauge\n");
        for (model, cost) in &state.model_costs {
            let _ = writeln!(
                output,
                "wayfinder_router_model_cost_per_1k{{model=\"{}\"}} {}",
                label_escape(model),
                python_general(*cost)
            );
        }
    }

    output.push_str("# HELP wayfinder_router_realized_cost_total Cumulative realized spend on the chosen tier (USD, or relative units when no cost_per_1k is configured; WF-DESIGN-0007).\n");
    output.push_str("# TYPE wayfinder_router_realized_cost_total counter\n");
    let _ = writeln!(
        output,
        "wayfinder_router_realized_cost_total {}",
        python_general(state.realized_cost)
    );
    output.push_str("# HELP wayfinder_router_baseline_cost_total Cumulative cost had every request gone to the dearest tier (the always-frontier counterfactual).\n");
    output.push_str("# TYPE wayfinder_router_baseline_cost_total counter\n");
    let _ = writeln!(
        output,
        "wayfinder_router_baseline_cost_total {}",
        python_general(state.baseline_cost)
    );
    output.push_str("# HELP wayfinder_router_savings_cost_total Cumulative savings vs always-frontier (baseline minus realized).\n");
    output.push_str("# TYPE wayfinder_router_savings_cost_total counter\n");
    let savings = python_round(state.baseline_cost - state.realized_cost, 6);
    let _ = writeln!(
        output,
        "wayfinder_router_savings_cost_total {}",
        python_general(savings)
    );

    output.push_str("# HELP wayfinder_router_decision_latency_seconds Time to score a prompt and pick a model (no model call).\n");
    output.push_str("# TYPE wayfinder_router_decision_latency_seconds histogram\n");
    render_histogram(
        &mut output,
        "wayfinder_router_decision_latency_seconds",
        &state.decision,
        "",
    );

    output.push_str("# HELP wayfinder_router_upstream_latency_seconds Upstream model round-trip time by model.\n");
    output.push_str("# TYPE wayfinder_router_upstream_latency_seconds histogram\n");
    for (model, histogram) in &state.upstream {
        render_histogram(
            &mut output,
            "wayfinder_router_upstream_latency_seconds",
            histogram,
            &format!("model=\"{}\"", label_escape(model)),
        );
    }
    if !state.deployment_selections.is_empty() {
        output.push_str("# HELP wayfinder_router_deployment_selection_total Interchangeable deployment selections by bounded strategy reason (WF-ADR-0062).\n");
        output.push_str("# TYPE wayfinder_router_deployment_selection_total counter\n");
        for ((model, deployment, reason), count) in &state.deployment_selections {
            let _ = writeln!(
                output,
                "wayfinder_router_deployment_selection_total{{model=\"{}\",deployment=\"{}\",reason=\"{}\"}} {count}",
                label_escape(model),
                label_escape(deployment),
                label_escape(reason)
            );
        }
    }
    if !state.shadow_events.is_empty() {
        output.push_str(
            "# HELP wayfinder_router_shadow_events_total Bounded prompt-free shadow-routing events (WF-ADR-0063).\n",
        );
        output.push_str("# TYPE wayfinder_router_shadow_events_total counter\n");
        for ((event, candidate), count) in &state.shadow_events {
            let _ = writeln!(
                output,
                "wayfinder_router_shadow_events_total{{event=\"{}\",candidate=\"{}\"}} {count}",
                label_escape(event),
                label_escape(candidate)
            );
        }
    }
    if state.shadow_provider_cost > 0.0 {
        output.push_str(
            "# HELP wayfinder_router_shadow_provider_cost_total Estimated cost of explicitly authorized provider comparisons (WF-ADR-0063).\n",
        );
        output.push_str("# TYPE wayfinder_router_shadow_provider_cost_total counter\n");
        let _ = writeln!(
            output,
            "wayfinder_router_shadow_provider_cost_total {}",
            python_general(state.shadow_provider_cost)
        );
    }
    if !state.deployment_ttft.is_empty() {
        output.push_str("# HELP wayfinder_router_deployment_time_to_first_token_seconds Deployment time to first token by concrete target.\n");
        output
            .push_str("# TYPE wayfinder_router_deployment_time_to_first_token_seconds histogram\n");
        for (deployment, histogram) in &state.deployment_ttft {
            render_histogram(
                &mut output,
                "wayfinder_router_deployment_time_to_first_token_seconds",
                histogram,
                &format!("deployment=\"{}\"", label_escape(deployment)),
            );
        }
    }
    if !state.deployment_throughput.is_empty() {
        output.push_str("# HELP wayfinder_router_deployment_throughput_chars_per_second Completion throughput by concrete target.\n");
        output
            .push_str("# TYPE wayfinder_router_deployment_throughput_chars_per_second histogram\n");
        for (deployment, histogram) in &state.deployment_throughput {
            render_histogram(
                &mut output,
                "wayfinder_router_deployment_throughput_chars_per_second",
                histogram,
                &format!("deployment=\"{}\"", label_escape(deployment)),
            );
        }
    }
    if !state.deployment_capacity.is_empty() {
        output.push_str("# HELP wayfinder_router_deployment_capacity_pressure Provider-reported capacity pressure by concrete target.\n");
        output.push_str("# TYPE wayfinder_router_deployment_capacity_pressure histogram\n");
        for (deployment, histogram) in &state.deployment_capacity {
            render_histogram(
                &mut output,
                "wayfinder_router_deployment_capacity_pressure",
                histogram,
                &format!("deployment=\"{}\"", label_escape(deployment)),
            );
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn empty_render_matches_python_family_order_and_build_info() -> TestResult {
        let metrics = GatewayMetrics::new("2026.7.0", DEFAULT_MAX_LABEL_SERIES);
        let text = metrics.render()?;
        assert!(text.starts_with("# HELP wayfinder_router_build_info"));
        assert!(text.contains("wayfinder_router_build_info{version=\"2026.7.0\"} 1"));
        assert!(text.contains("# TYPE wayfinder_router_requests_total counter"));
        assert!(text.ends_with("# TYPE wayfinder_router_upstream_latency_seconds histogram\n"));
        Ok(())
    }

    #[test]
    fn counters_histograms_costs_and_escaping_render_deterministically() -> TestResult {
        let metrics = GatewayMetrics::new("v\\\"\n", DEFAULT_MAX_LABEL_SERIES);
        metrics.observe_decision("local", "scored", 0.0005)?;
        metrics.observe_upstream("local", 0.25)?;
        metrics.observe_upstream_error("cloud")?;
        metrics.observe_deployment_ttft("cloud#eu", 0.05)?;
        metrics.observe_deployment_throughput("cloud#eu", 100.0)?;
        metrics.observe_deployment_capacity("cloud#eu", 0.25)?;
        metrics.observe_deployment_selection("cloud", "cloud#eu", "latency")?;
        metrics.observe_shadow_event("divergence", "candidate");
        metrics.observe_shadow_cost(0.02);
        metrics.observe_cache_hit(0.01)?;
        metrics.observe_cache_miss()?;
        metrics.observe_rate_limited("rpm")?;
        metrics.observe_key_request("team-a")?;
        metrics.observe_admission(0.05)?;
        metrics.observe_admission_rejected("queue-full")?;
        metrics.observe_admission_release()?;
        metrics.record_reload_failure()?;
        metrics.observe_cost(0.01, 0.02)?;
        metrics.set_model_costs(&BTreeMap::from([
            ("cloud".to_owned(), 10.0),
            ("local".to_owned(), 0.0),
        ]))?;
        let text = metrics.render()?;
        assert!(text.contains("version=\"v\\\\\\\"\\n\""));
        assert!(
            text.contains("wayfinder_router_requests_total{model=\"local\",mode=\"scored\"} 1")
        );
        assert!(text.contains("wayfinder_router_decision_latency_seconds_bucket{le=\"0.0005\"} 1"));
        assert!(
            text.contains("wayfinder_router_upstream_latency_seconds_count{model=\"local\"} 1")
        );
        assert!(text.contains("wayfinder_router_cache_avoided_cost_total 0.01"));
        assert!(text.contains("wayfinder_router_model_cost_per_1k{model=\"cloud\"} 10"));
        assert!(text.contains("wayfinder_router_savings_cost_total 0.01"));
        assert!(text.contains(
            "wayfinder_router_deployment_selection_total{model=\"cloud\",deployment=\"cloud#eu\",reason=\"latency\"} 1"
        ));
        assert!(text.contains(
            "wayfinder_router_deployment_time_to_first_token_seconds_count{deployment=\"cloud#eu\"} 1"
        ));
        assert!(text.contains(
            "wayfinder_router_shadow_events_total{event=\"divergence\",candidate=\"candidate\"} 1"
        ));
        assert!(text.contains("wayfinder_router_shadow_provider_cost_total 0.02"));
        assert!(text.contains("wayfinder_router_peak_in_flight_deliveries 1"));
        assert!(text.contains("wayfinder_router_in_flight_deliveries 0"));
        assert!(text.contains("wayfinder_router_admission_queue_wait_seconds_count 1"));
        assert!(
            text.contains("wayfinder_router_admission_rejected_total{reason=\"queue-full\"} 1")
        );
        Ok(())
    }

    #[test]
    fn nonfinite_values_are_rejected_without_mutation() -> TestResult {
        let metrics = GatewayMetrics::default();
        assert_eq!(
            metrics.observe_decision("local", "scored", f64::NAN),
            Err(MetricsError::InvalidObservation)
        );
        assert_eq!(
            metrics.observe_cost(-1.0, 0.0),
            Err(MetricsError::InvalidObservation)
        );
        assert!(
            metrics
                .render()?
                .contains("wayfinder_router_decision_latency_seconds_count 0")
        );
        Ok(())
    }

    #[test]
    fn label_cardinality_and_length_are_bounded() -> TestResult {
        let metrics = GatewayMetrics::new("v", 1);
        metrics.observe_decision("first", "scored", 0.0)?;
        metrics.observe_decision("second", "pinned", 0.0)?;
        metrics.observe_upstream_error(&"secret-like-value".repeat(20))?;
        let text = metrics.render()?;
        assert!(text.contains("model=\"first\",mode=\"scored\""));
        assert!(text.contains("model=\"__other__\",mode=\"__other__\""));
        assert!(text.contains("upstream_errors_total{model=\"__other__\"}"));
        assert!(!text.contains("secret-like-value"));
        Ok(())
    }

    #[test]
    fn python_general_number_format_covers_fixed_and_exponent_boundaries() {
        assert_eq!(python_general(0.0001), "0.0001");
        assert_eq!(python_general(0.00001), "1e-05");
        assert_eq!(python_general(10.0), "10");
        assert_eq!(python_general(1_000_000.0), "1e+06");
        assert_eq!(python_general(0.300_000_000_000_000_04), "0.3");
    }
}
