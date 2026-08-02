//! Bounded, reproducible evidence reports over prompt-free shadow records.
//!
//! Reports deliberately distinguish observed provider outcomes from quality
//! labels. A missing label is not a tie, and an insufficient sample never gets
//! promoted to a quality claim.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::shadow::{ShadowRecord, ShadowSnapshot};

/// Stable report schema version.
pub const EVIDENCE_SCHEMA_VERSION: &str = "wf-evidence-v1";
/// Minimum number of decisive quality labels required for a verdict.
pub const EVIDENCE_MIN_SAMPLES: u64 = 30;
/// Confidence level used by the deterministic Wilson intervals.
pub const EVIDENCE_CONFIDENCE: f64 = 0.95;
/// Maximum number of labels retained by one process.
pub const MAX_EVIDENCE_LABELS: usize = 2_048;
/// Maximum length of a label identity or evaluator version.
pub const MAX_EVIDENCE_LABEL_BYTES: usize = 128;

/// A quality comparison label for one prompt-free shadow record.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceLabelValue {
    /// The candidate was good enough compared with production.
    Win,
    /// Production was preferred over the candidate.
    Loss,
    /// Neither answer was preferred.
    Tie,
    /// The evaluator could not make a reliable comparison.
    Abstain,
}

/// The provenance of a quality label.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum EvidenceEvaluator {
    /// A bounded human-labelled result.
    Human,
    /// A deterministic evaluator with explicit version provenance.
    Automated { version: String },
}

impl EvidenceEvaluator {
    fn key(&self) -> String {
        match self {
            Self::Human => "human".to_owned(),
            Self::Automated { version } => format!("automated:{version}"),
        }
    }
}

/// A label payload accepted by the operator evidence surface.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceLabel {
    /// Request ID from a retained shadow record, never a prompt digest.
    pub request_id: String,
    /// Named candidate route from a retained shadow record.
    pub candidate_route: String,
    /// Human or deterministic automated outcome.
    pub label: EvidenceLabelValue,
    /// Explicit evaluator provenance.
    pub evaluator: EvidenceEvaluator,
}

/// Bounded label-store failure.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum EvidenceError {
    /// A label was malformed or exceeded a fixed field bound.
    #[error("invalid evidence label: {0}")]
    Invalid(String),
    /// The process-local retention bound would be exceeded.
    #[error("evidence label retention bound is {MAX_EVIDENCE_LABELS}")]
    Capacity,
    /// The label store could not be synchronized.
    #[error("evidence label store is unavailable")]
    LockPoisoned,
}

/// Process-local, bounded, deterministic label retention.
#[derive(Debug)]
pub struct EvidenceStore {
    labels: Mutex<BTreeMap<String, EvidenceLabel>>,
    max_labels: usize,
}

impl Default for EvidenceStore {
    fn default() -> Self {
        Self::new(MAX_EVIDENCE_LABELS)
    }
}

impl EvidenceStore {
    /// Construct a bounded label store.
    #[must_use]
    pub fn new(max_labels: usize) -> Self {
        Self {
            labels: Mutex::new(BTreeMap::new()),
            max_labels: max_labels.clamp(1, MAX_EVIDENCE_LABELS),
        }
    }

    /// Insert or replace labels by request, candidate, and evaluator identity.
    pub fn insert<I>(&self, labels: I) -> Result<usize, EvidenceError>
    where
        I: IntoIterator<Item = EvidenceLabel>,
    {
        let labels = labels.into_iter().collect::<Vec<_>>();
        if labels.len() > self.max_labels {
            return Err(EvidenceError::Capacity);
        }
        for label in &labels {
            validate_label(label)?;
        }
        let mut state = self
            .labels
            .lock()
            .map_err(|_| EvidenceError::LockPoisoned)?;
        // Check the entire batch before mutating the store. A rejected batch
        // must not leave a partially accepted prefix behind.
        let mut new_keys = BTreeSet::new();
        for label in &labels {
            let key = label_key(label);
            if !state.contains_key(&key) {
                new_keys.insert(key);
            }
        }
        if state.len() + new_keys.len() > self.max_labels {
            return Err(EvidenceError::Capacity);
        }
        for label in labels {
            state.insert(label_key(&label), label);
        }
        Ok(new_keys.len())
    }

    /// Return labels in stable key order.
    #[must_use]
    pub fn snapshot(&self) -> Vec<EvidenceLabel> {
        self.labels
            .lock()
            .map(|state| state.values().cloned().collect())
            .unwrap_or_default()
    }
}

fn validate_label(label: &EvidenceLabel) -> Result<(), EvidenceError> {
    for (name, value) in [
        ("request_id", label.request_id.as_str()),
        ("candidate_route", label.candidate_route.as_str()),
    ] {
        if value.is_empty() || value.len() > MAX_EVIDENCE_LABEL_BYTES {
            return Err(EvidenceError::Invalid(format!(
                "{name} must be between 1 and {MAX_EVIDENCE_LABEL_BYTES} bytes"
            )));
        }
        if value.bytes().any(|byte| byte == 0 || byte < 0x20) {
            return Err(EvidenceError::Invalid(format!(
                "{name} contains a control character"
            )));
        }
    }
    if let EvidenceEvaluator::Automated { version } = &label.evaluator {
        if version.is_empty() || version.len() > MAX_EVIDENCE_LABEL_BYTES {
            return Err(EvidenceError::Invalid(format!(
                "automated evaluator version must be between 1 and {MAX_EVIDENCE_LABEL_BYTES} bytes"
            )));
        }
        if version.bytes().any(|byte| byte == 0 || byte < 0x20) {
            return Err(EvidenceError::Invalid(
                "automated evaluator version contains a control character".to_owned(),
            ));
        }
    }
    Ok(())
}

fn label_key(label: &EvidenceLabel) -> String {
    format!(
        "{}\u{0}{}\u{0}{}",
        label.request_id,
        label.candidate_route,
        label.evaluator.key()
    )
}

/// Report verdict, deliberately tri-state rather than a binary quality claim.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceOutcome {
    /// The evidence clears the configured minimum and its lower bound is safe.
    Enforce,
    /// More representative evidence is required before changing production.
    KeepShadowing,
    /// Evidence is sufficient to reject the candidate.
    DoNotEnforce,
}

impl EvidenceOutcome {
    fn rank(self) -> u8 {
        match self {
            Self::Enforce => 0,
            Self::KeepShadowing => 1,
            Self::DoNotEnforce => 2,
        }
    }
}

/// A deterministic confidence interval for one bounded proportion.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct EvidenceInterval {
    pub confidence: f64,
    pub lower: f64,
    pub upper: f64,
}

/// Quality label counts and agreement provenance.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct EvidenceQuality {
    pub labels: u64,
    pub decisive: u64,
    pub wins: u64,
    pub losses: u64,
    pub ties: u64,
    pub abstentions: u64,
    pub coverage: f64,
    pub win_rate: Option<f64>,
    pub loss_rate: Option<f64>,
    pub delta: Option<f64>,
    pub delta_interval: Option<EvidenceInterval>,
    pub evaluator_counts: BTreeMap<String, u64>,
    pub judge_agreement: BTreeMap<String, EvidenceAgreement>,
}

/// Agreement between human labels and one automated evaluator version.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct EvidenceAgreement {
    pub samples: u64,
    pub agreement: f64,
    pub kappa: Option<f64>,
}

/// Provider-side observations kept separate from quality labels.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct EvidenceProvider {
    pub comparisons: u64,
    pub successes: u64,
    pub errors: u64,
    pub error_rate: Option<f64>,
    pub mean_latency_ms: Option<f64>,
    pub latency_samples: u64,
    pub cost_samples: u64,
    pub estimated_cost: Option<f64>,
    pub cost_class: String,
}

/// One candidate route's evidence summary.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EvidenceCandidate {
    pub candidate_route: String,
    pub samples: u64,
    pub completed: u64,
    pub missing_candidate: u64,
    pub divergent: u64,
    pub errors: u64,
    pub provider: EvidenceProvider,
    pub quality: EvidenceQuality,
    pub outcome: EvidenceOutcome,
    pub reasons: Vec<String>,
}

/// Stable machine-readable evidence artifact.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EvidenceReport {
    pub schema_version: &'static str,
    pub scorer_versions: Vec<u32>,
    pub config_fingerprints: Vec<String>,
    pub candidate_fingerprints: Vec<String>,
    pub sampled: u64,
    pub completed: u64,
    pub shadow_errors: u64,
    pub dropped_in_flight: u64,
    pub candidates: Vec<EvidenceCandidate>,
    pub outcome: EvidenceOutcome,
    pub reasons: Vec<String>,
}

impl EvidenceReport {
    /// Build a deterministic report from bounded records and labels.
    #[must_use]
    pub fn from_snapshot(snapshot: &ShadowSnapshot, labels: &[EvidenceLabel]) -> Self {
        let mut route_records = BTreeMap::<String, Vec<&ShadowRecord>>::new();
        let mut scorer_versions = BTreeSet::new();
        let mut config_fingerprints = BTreeSet::new();
        let mut candidate_fingerprints = BTreeSet::new();
        for record in &snapshot.records {
            route_records
                .entry(record.candidate_route.clone())
                .or_default()
                .push(record);
            scorer_versions.insert(record.scorer_version);
            config_fingerprints.insert(record.config_fingerprint.clone());
            candidate_fingerprints.insert(record.candidate_fingerprint.clone());
        }
        let mut candidates = route_records
            .into_iter()
            .map(|(route, records)| candidate_report(route, records, labels))
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.candidate_route.cmp(&right.candidate_route));

        let mut reasons = Vec::new();
        if snapshot.sampled == 0 {
            reasons.push("no-shadow-samples".to_owned());
        }
        if snapshot.completed < EVIDENCE_MIN_SAMPLES {
            reasons.push(format!(
                "insufficient-shadow-samples:{}/{}",
                snapshot.completed, EVIDENCE_MIN_SAMPLES
            ));
        }
        if config_fingerprints.len() > 1 {
            reasons.push("mixed-config-fingerprint".to_owned());
        }
        if candidate_fingerprints.len() > 1 {
            reasons.push("mixed-candidate-fingerprint".to_owned());
        }
        if candidates.iter().any(|candidate| {
            candidate
                .reasons
                .iter()
                .any(|reason| reason.starts_with("insufficient-quality-labels"))
        }) {
            reasons.push("quality-labels-incomplete".to_owned());
        }
        if candidates.is_empty() {
            reasons.push("no-candidate-records".to_owned());
        }
        let outcome = if !reasons.is_empty() {
            if reasons.iter().any(|reason| {
                reason == "mixed-config-fingerprint" || reason == "mixed-candidate-fingerprint"
            }) {
                EvidenceOutcome::KeepShadowing
            } else {
                aggregate_outcome(&candidates)
            }
        } else {
            aggregate_outcome(&candidates)
        };
        if outcome == EvidenceOutcome::KeepShadowing && reasons.is_empty() {
            reasons.push("evidence-inconclusive".to_owned());
        }
        Self {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            scorer_versions: scorer_versions.into_iter().collect(),
            config_fingerprints: config_fingerprints.into_iter().collect(),
            candidate_fingerprints: candidate_fingerprints.into_iter().collect(),
            sampled: snapshot.sampled,
            completed: snapshot.completed,
            shadow_errors: snapshot.errors,
            dropped_in_flight: snapshot.dropped_in_flight,
            candidates,
            outcome,
            reasons,
        }
    }

    /// Render a bounded operator-readable report without external resources.
    #[must_use]
    pub fn render_text(&self) -> String {
        let mut text = String::new();
        text.push_str("Wayfinder evidence report ");
        text.push_str(self.schema_version);
        text.push('\n');
        text.push_str("Outcome: ");
        text.push_str(outcome_name(self.outcome));
        text.push('\n');
        text.push_str(&format!(
            "Shadow samples: {} sampled, {} completed, {} errors, {} dropped\n",
            self.sampled, self.completed, self.shadow_errors, self.dropped_in_flight
        ));
        if !self.reasons.is_empty() {
            text.push_str("Reasons: ");
            text.push_str(&self.reasons.join(", "));
            text.push('\n');
        }
        for candidate in &self.candidates {
            text.push_str("\nCandidate: ");
            text.push_str(&candidate.candidate_route);
            text.push('\n');
            text.push_str(&format!(
                "  decisions={} completed={} missing={} divergent={} errors={}\n",
                candidate.samples,
                candidate.completed,
                candidate.missing_candidate,
                candidate.divergent,
                candidate.errors
            ));
            text.push_str(&format!(
                "  provider comparisons={} successes={} errors={} cost_class={}\n",
                candidate.provider.comparisons,
                candidate.provider.successes,
                candidate.provider.errors,
                candidate.provider.cost_class
            ));
            text.push_str(&format!(
                "  quality labels={} decisive={} wins={} losses={} ties={} abstentions={}\n",
                candidate.quality.labels,
                candidate.quality.decisive,
                candidate.quality.wins,
                candidate.quality.losses,
                candidate.quality.ties,
                candidate.quality.abstentions
            ));
            text.push_str("  outcome: ");
            text.push_str(outcome_name(candidate.outcome));
            text.push('\n');
            if !candidate.reasons.is_empty() {
                text.push_str("  reasons: ");
                text.push_str(&candidate.reasons.join(", "));
                text.push('\n');
            }
        }
        text
    }
}

fn candidate_report(
    route: String,
    records: Vec<&ShadowRecord>,
    labels: &[EvidenceLabel],
) -> EvidenceCandidate {
    let samples = records.len() as u64;
    let completed = records
        .iter()
        .filter(|record| record.status == "completed")
        .count() as u64;
    let missing_candidate = records
        .iter()
        .filter(|record| record.shadow_model.is_none())
        .count() as u64;
    let divergent = records.iter().filter(|record| record.divergent).count() as u64;
    let errors = samples.saturating_sub(completed);
    let provider_records = records
        .iter()
        .filter(|record| record.provider_compared)
        .copied()
        .collect::<Vec<_>>();
    let provider = provider_report(&provider_records);
    let quality = quality_report(&records, labels);
    let mut reasons = Vec::new();
    if quality.decisive < EVIDENCE_MIN_SAMPLES {
        reasons.push(format!(
            "insufficient-quality-labels:{}/{}",
            quality.decisive, EVIDENCE_MIN_SAMPLES
        ));
    } else if quality.delta_interval.is_none() {
        reasons.push("quality-interval-unavailable".to_owned());
    }
    if provider.error_rate.is_some_and(|rate| rate > 0.20) {
        reasons.push("provider-error-rate-above-20-percent".to_owned());
    }
    let outcome = if provider.error_rate.is_some_and(|rate| rate > 0.20) {
        EvidenceOutcome::DoNotEnforce
    } else if quality.decisive < EVIDENCE_MIN_SAMPLES {
        EvidenceOutcome::KeepShadowing
    } else if quality
        .delta_interval
        .is_some_and(|interval| interval.lower >= 0.0)
    {
        EvidenceOutcome::Enforce
    } else if quality
        .delta_interval
        .is_some_and(|interval| interval.upper < 0.0)
    {
        EvidenceOutcome::DoNotEnforce
    } else {
        EvidenceOutcome::KeepShadowing
    };
    EvidenceCandidate {
        candidate_route: route,
        samples,
        completed,
        missing_candidate,
        divergent,
        errors,
        provider,
        quality,
        outcome,
        reasons,
    }
}

fn provider_report(records: &[&ShadowRecord]) -> EvidenceProvider {
    if records.is_empty() {
        return EvidenceProvider {
            cost_class: "unknown".to_owned(),
            ..EvidenceProvider::default()
        };
    }
    let successes = records
        .iter()
        .filter(|record| {
            record
                .provider_status
                .is_some_and(|status| (200..300).contains(&status))
        })
        .count() as u64;
    let errors = records.len() as u64 - successes;
    let latencies = records
        .iter()
        .filter_map(|record| record.provider_latency_ms.map(|value| value as f64))
        .collect::<Vec<_>>();
    let costs = records
        .iter()
        .filter_map(|record| record.provider_cost)
        .collect::<Vec<_>>();
    let cost_class = if costs.is_empty() {
        "unknown".to_owned()
    } else {
        records
            .iter()
            .find_map(|record| record.provider_cost_class.as_deref())
            .unwrap_or("unknown")
            .to_owned()
    };
    EvidenceProvider {
        comparisons: records.len() as u64,
        successes,
        errors,
        error_rate: Some(round((errors as f64) / records.len() as f64)),
        mean_latency_ms: (!latencies.is_empty())
            .then(|| round(latencies.iter().sum::<f64>() / latencies.len() as f64)),
        latency_samples: latencies.len() as u64,
        cost_samples: costs.len() as u64,
        estimated_cost: (!costs.is_empty()).then(|| round(costs.iter().sum())),
        cost_class,
    }
}

fn quality_report(records: &[&ShadowRecord], labels: &[EvidenceLabel]) -> EvidenceQuality {
    let keys = records
        .iter()
        .map(|record| (record.request_id.as_str(), record.candidate_route.as_str()))
        .collect::<BTreeSet<_>>();
    let labels = labels
        .iter()
        .filter(|label| keys.contains(&(label.request_id.as_str(), label.candidate_route.as_str())))
        .collect::<Vec<_>>();
    let mut quality = EvidenceQuality {
        labels: labels.len() as u64,
        coverage: if records.is_empty() {
            0.0
        } else {
            round(labels.len() as f64 / records.len() as f64)
        },
        ..EvidenceQuality::default()
    };
    let mut by_key = BTreeMap::<(&str, &str), BTreeMap<String, EvidenceLabelValue>>::new();
    for label in labels {
        *quality
            .evaluator_counts
            .entry(label.evaluator.key())
            .or_default() += 1;
        match label.label {
            EvidenceLabelValue::Win => quality.wins += 1,
            EvidenceLabelValue::Loss => quality.losses += 1,
            EvidenceLabelValue::Tie => quality.ties += 1,
            EvidenceLabelValue::Abstain => quality.abstentions += 1,
        }
        by_key
            .entry((label.request_id.as_str(), label.candidate_route.as_str()))
            .or_default()
            .insert(label.evaluator.key(), label.label);
    }
    quality.decisive = quality.wins.saturating_add(quality.losses);
    if quality.decisive > 0 {
        quality.win_rate = Some(round(quality.wins as f64 / quality.decisive as f64));
        quality.loss_rate = Some(round(quality.losses as f64 / quality.decisive as f64));
        quality.delta = Some(round(
            (quality.wins as f64 - quality.losses as f64) / quality.decisive as f64,
        ));
        let win = wilson(quality.wins, quality.decisive);
        let loss = wilson(quality.losses, quality.decisive);
        quality.delta_interval = Some(EvidenceInterval {
            confidence: EVIDENCE_CONFIDENCE,
            lower: round(win.lower - loss.upper),
            upper: round(win.upper - loss.lower),
        });
    }
    let automated_versions = quality
        .evaluator_counts
        .keys()
        .filter_map(|key| key.strip_prefix("automated:"))
        .collect::<Vec<_>>();
    for version in automated_versions {
        let automated_key = format!("automated:{version}");
        let mut pairs = Vec::new();
        for values in by_key.values() {
            let Some(human) = values.get("human") else {
                continue;
            };
            let Some(automated) = values.get(&automated_key) else {
                continue;
            };
            if !matches!(human, EvidenceLabelValue::Abstain)
                && !matches!(automated, EvidenceLabelValue::Abstain)
            {
                pairs.push((*human, *automated));
            }
        }
        if !pairs.is_empty() {
            quality
                .judge_agreement
                .insert(version.to_owned(), agreement(&pairs));
        }
    }
    quality
}

fn agreement(pairs: &[(EvidenceLabelValue, EvidenceLabelValue)]) -> EvidenceAgreement {
    let samples = pairs.len() as u64;
    let matching = pairs.iter().filter(|(left, right)| left == right).count() as f64;
    let agreement_rate = matching / samples as f64;
    let labels = [
        EvidenceLabelValue::Win,
        EvidenceLabelValue::Loss,
        EvidenceLabelValue::Tie,
    ];
    let mut observed = 0.0;
    let mut expected = 0.0;
    for label in labels {
        let left =
            pairs.iter().filter(|(value, _)| *value == label).count() as f64 / samples as f64;
        let right =
            pairs.iter().filter(|(_, value)| *value == label).count() as f64 / samples as f64;
        observed += pairs
            .iter()
            .filter(|(left_value, right_value)| *left_value == label && *right_value == label)
            .count() as f64
            / samples as f64;
        expected += left * right;
    }
    let kappa = if (1.0 - expected).abs() <= f64::EPSILON {
        Some(if (observed - expected).abs() <= f64::EPSILON {
            1.0
        } else {
            0.0
        })
    } else {
        Some(round((observed - expected) / (1.0 - expected)))
    };
    EvidenceAgreement {
        samples,
        agreement: round(agreement_rate),
        kappa,
    }
}

fn wilson(successes: u64, total: u64) -> EvidenceInterval {
    if total == 0 {
        return EvidenceInterval {
            confidence: EVIDENCE_CONFIDENCE,
            lower: 0.0,
            upper: 0.0,
        };
    }
    let z = 1.959_963_984_540_054;
    let n = total as f64;
    let p = successes as f64 / n;
    let denominator = 1.0 + z * z / n;
    let centre = (p + z * z / (2.0 * n)) / denominator;
    let spread = z * ((p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt()) / denominator;
    EvidenceInterval {
        confidence: EVIDENCE_CONFIDENCE,
        lower: round((centre - spread).max(0.0)),
        upper: round((centre + spread).min(1.0)),
    }
}

fn aggregate_outcome(candidates: &[EvidenceCandidate]) -> EvidenceOutcome {
    if candidates.is_empty() {
        return EvidenceOutcome::KeepShadowing;
    }
    let mut outcome = EvidenceOutcome::Enforce;
    for candidate in candidates {
        if candidate.outcome.rank() > outcome.rank() {
            outcome = candidate.outcome;
        }
    }
    outcome
}

fn outcome_name(outcome: EvidenceOutcome) -> &'static str {
    match outcome {
        EvidenceOutcome::Enforce => "enforce",
        EvidenceOutcome::KeepShadowing => "keep-shadowing",
        EvidenceOutcome::DoNotEnforce => "do-not-enforce",
    }
}

fn round(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(request_id: &str, route: &str, divergent: bool) -> ShadowRecord {
        ShadowRecord {
            request_id: request_id.to_owned(),
            candidate_route: route.to_owned(),
            production_model: "production".to_owned(),
            shadow_model: Some("candidate".to_owned()),
            production_score: 0.5,
            shadow_score: Some(0.5),
            divergent,
            status: "completed".to_owned(),
            reason_codes: Vec::new(),
            provider_compared: false,
            provider_status: None,
            provider_latency_ms: None,
            provider_cost: None,
            provider_cost_class: None,
            scorer_version: 1,
            config_fingerprint: "config".to_owned(),
            candidate_fingerprint: "candidate".to_owned(),
        }
    }

    #[test]
    fn sparse_and_missing_labels_keep_shadowing_without_quality_claims() {
        let snapshot = ShadowSnapshot {
            records: vec![record("request-1", "candidate", true)],
            sampled: 1,
            completed: 1,
            ..ShadowSnapshot::default()
        };
        let report = EvidenceReport::from_snapshot(&snapshot, &[]);
        assert_eq!(report.outcome, EvidenceOutcome::KeepShadowing);
        assert!(
            report
                .reasons
                .iter()
                .any(|reason| reason.starts_with("insufficient-shadow-samples"))
        );
        assert_eq!(report.candidates[0].quality.decisive, 0);
    }

    #[test]
    fn deterministic_labels_can_reach_enforce_and_agreement_is_separate() {
        let records = (0..EVIDENCE_MIN_SAMPLES)
            .map(|index| record(&format!("request-{index}"), "candidate", true))
            .collect::<Vec<_>>();
        let snapshot = ShadowSnapshot {
            records,
            sampled: EVIDENCE_MIN_SAMPLES,
            completed: EVIDENCE_MIN_SAMPLES,
            ..ShadowSnapshot::default()
        };
        let mut labels = Vec::new();
        for index in 0..EVIDENCE_MIN_SAMPLES {
            labels.push(EvidenceLabel {
                request_id: format!("request-{index}"),
                candidate_route: "candidate".to_owned(),
                label: EvidenceLabelValue::Win,
                evaluator: EvidenceEvaluator::Human,
            });
            labels.push(EvidenceLabel {
                request_id: format!("request-{index}"),
                candidate_route: "candidate".to_owned(),
                label: EvidenceLabelValue::Win,
                evaluator: EvidenceEvaluator::Automated {
                    version: "heuristic-1".to_owned(),
                },
            });
        }
        let report = EvidenceReport::from_snapshot(&snapshot, &labels);
        assert_eq!(report.outcome, EvidenceOutcome::Enforce);
        assert_eq!(
            report.candidates[0].quality.judge_agreement["heuristic-1"].kappa,
            Some(1.0)
        );
    }

    #[test]
    fn labels_reject_prompt_response_fields_and_store_is_bounded() {
        let parsed = serde_json::from_str::<EvidenceLabel>(
            r#"{"request_id":"r","candidate_route":"c","label":"win","evaluator":{"kind":"human"},"prompt":"secret"}"#,
        );
        assert!(parsed.is_err());
        let store = EvidenceStore::new(1);
        let label = EvidenceLabel {
            request_id: "r".to_owned(),
            candidate_route: "c".to_owned(),
            label: EvidenceLabelValue::Tie,
            evaluator: EvidenceEvaluator::Human,
        };
        assert_eq!(store.insert([label.clone()]), Ok(1));
        assert_eq!(store.insert([label]), Ok(0));
        let second = EvidenceLabel {
            request_id: "r2".to_owned(),
            candidate_route: "c".to_owned(),
            label: EvidenceLabelValue::Tie,
            evaluator: EvidenceEvaluator::Human,
        };
        assert_eq!(store.insert([second]), Err(EvidenceError::Capacity));
    }

    #[test]
    fn rejected_label_batch_is_atomic() {
        let store = EvidenceStore::new(2);
        let labels = (0..3)
            .map(|index| EvidenceLabel {
                request_id: format!("request-{index}"),
                candidate_route: "candidate".to_owned(),
                label: EvidenceLabelValue::Win,
                evaluator: EvidenceEvaluator::Human,
            })
            .collect::<Vec<_>>();

        assert_eq!(store.insert(labels), Err(EvidenceError::Capacity));
        assert!(store.snapshot().is_empty());
    }
}
