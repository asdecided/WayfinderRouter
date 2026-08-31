//! Review-only policy proposals from bounded prompt-free user outcomes.

use serde::Serialize;
use wayfinder_config::gateway::ProviderTier;
use wayfinder_routing_core::{ExecutionBoundary, RoutingConfig, Tier, python_round};

use crate::ConfiguredModel;
use crate::recent::{RecentEntry, UserOutcome};

/// Stable schema identifier for a local outcome policy proposal.
pub const OUTCOME_POLICY_SCHEMA_VERSION: &str = "wf-local-outcome-policy-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BoundaryClass {
    Local,
    Hosted,
}

/// Review-only result of evaluating the current binary threshold.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutcomePolicyReport {
    /// Stable response schema.
    pub schema_version: &'static str,
    /// Workspace selected by the operator.
    pub workspace_id: String,
    /// Explicitly bounded retention source.
    pub retention: &'static str,
    /// Stable proposal state.
    pub disposition: &'static str,
    /// Human-readable prompt-free explanation.
    pub reason: String,
    /// Current two-tier policy when the policy shape is supported.
    pub current_tiers: Vec<Tier>,
    /// Proposed tiers, or an empty list when no change is proposed.
    pub proposed_tiers: Vec<Tier>,
    /// Current upper-tier threshold when supported.
    pub current_threshold: Option<f64>,
    /// Proposed upper-tier threshold when a narrower local range is justified.
    pub proposed_threshold: Option<f64>,
    /// Retained explicit user labels in this workspace.
    pub labelled: u64,
    /// Labels that directly observed the selected local tier.
    pub actionable_local: u64,
    /// Actionable local successes.
    pub local_successes: u64,
    /// Actionable local corrections or failures.
    pub local_negatives: u64,
    /// Cross-boundary selected-versus-served labels excluded as confounded.
    pub confounded_cross_boundary: u64,
    /// Labels excluded because the selected tier was hosted.
    pub hosted_selected: u64,
    /// Labels excluded because they were pinned, forced, or otherwise non-scored.
    pub non_scored: u64,
    /// Labels excluded because the selected or actual boundary was unavailable.
    pub unknown_boundary: u64,
    /// Successful local labels that the proposed threshold would escalate.
    pub reclassified_local_successes: u64,
    /// Stable interpretation limits.
    pub limitations: Vec<&'static str>,
}

/// Evaluate a binary local/hosted score policy without mutating it.
#[must_use]
pub fn propose_local_threshold(
    workspace_id: &str,
    routing: &RoutingConfig,
    models: &[ConfiguredModel],
    entries: &[RecentEntry],
) -> OutcomePolicyReport {
    let mut report = OutcomePolicyReport {
        schema_version: OUTCOME_POLICY_SCHEMA_VERSION,
        workspace_id: workspace_id.to_owned(),
        retention: "process-local-bounded-shared-ring",
        disposition: "unsupported-policy",
        reason: "a review-only proposal requires a two-tier scored local/hosted policy".to_owned(),
        current_tiers: routing.tiers.clone(),
        proposed_tiers: Vec::new(),
        current_threshold: None,
        proposed_threshold: None,
        labelled: 0,
        actionable_local: 0,
        local_successes: 0,
        local_negatives: 0,
        confounded_cross_boundary: 0,
        hosted_selected: 0,
        non_scored: 0,
        unknown_boundary: 0,
        reclassified_local_successes: 0,
        limitations: vec![
            "the proposal uses only explicit prompt-free labels on currently retained receipts",
            "cross-boundary fallback labels are confounded and never drive a threshold change",
            "the proposal can narrow local routing but cannot expand it",
            "the proposal never edits, activates, or reloads policy",
            "retained labels may be sparse or selection-biased",
        ],
    };

    if routing.classifier.is_some() || routing.tiers.len() != 2 {
        return report;
    }
    let lower = &routing.tiers[0];
    let upper = &routing.tiers[1];
    if lower.min_score != 0.0
        || !upper.min_score.is_finite()
        || upper.min_score <= lower.min_score
        || selected_boundary(models, &lower.model) != Some(BoundaryClass::Local)
        || selected_boundary(models, &upper.model) != Some(BoundaryClass::Hosted)
    {
        return report;
    }

    report.current_threshold = Some(upper.min_score);
    let mut negative_scores = Vec::new();
    let mut successful_scores = Vec::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.workspace.as_deref() == Some(workspace_id))
    {
        let Some(label) = entry.user_outcome else {
            continue;
        };
        report.labelled = report.labelled.saturating_add(1);
        if entry.mode != "scored" {
            report.non_scored = report.non_scored.saturating_add(1);
            continue;
        }
        let Some(selected) = selected_boundary(models, &entry.model) else {
            report.unknown_boundary = report.unknown_boundary.saturating_add(1);
            continue;
        };
        let Some(actual) = entry.execution_boundary.map(actual_boundary) else {
            report.unknown_boundary = report.unknown_boundary.saturating_add(1);
            continue;
        };
        if selected != actual {
            report.confounded_cross_boundary = report.confounded_cross_boundary.saturating_add(1);
            continue;
        }
        if entry.model != lower.model {
            report.hosted_selected = report.hosted_selected.saturating_add(1);
            continue;
        }
        report.actionable_local = report.actionable_local.saturating_add(1);
        match label {
            UserOutcome::Success => {
                report.local_successes = report.local_successes.saturating_add(1);
                successful_scores.push(entry.score);
            }
            UserOutcome::Correction | UserOutcome::Failure => {
                report.local_negatives = report.local_negatives.saturating_add(1);
                negative_scores.push(entry.score);
            }
        }
    }

    if report.labelled == 0 {
        report.disposition = "insufficient-evidence";
        report.reason = "no explicit outcome labels are retained for this workspace".to_owned();
        return report;
    }
    if report.actionable_local == 0 {
        report.disposition = "no-actionable-local-evidence";
        report.reason =
            "retained labels do not directly observe a scored local selection".to_owned();
        return report;
    }
    if negative_scores.is_empty() {
        report.disposition = "retain";
        report.reason =
            "no actionable local correction or failure justifies narrowing local routing"
                .to_owned();
        return report;
    }

    let threshold = negative_scores
        .into_iter()
        .filter(|score| score.is_finite())
        .fold(upper.min_score, f64::min)
        .clamp(lower.min_score, upper.min_score);
    if threshold >= upper.min_score {
        report.disposition = "retain";
        report.reason =
            "actionable negative labels do not fall below the current hosted threshold".to_owned();
        return report;
    }

    let threshold = python_round(threshold, 6);
    report.reclassified_local_successes = successful_scores
        .iter()
        .filter(|score| **score >= threshold && **score < upper.min_score)
        .count() as u64;
    let mut proposed = routing.tiers.clone();
    proposed[1].min_score = threshold;
    report.disposition = "propose-narrower-local-range";
    report.reason = format!(
        "lower the hosted threshold from {:.6} to {:.6} so every actionable negative local score escalates; {} labelled local successes would also escalate",
        upper.min_score, threshold, report.reclassified_local_successes
    );
    report.proposed_threshold = Some(threshold);
    report.proposed_tiers = proposed;
    report
}

fn selected_boundary(models: &[ConfiguredModel], name: &str) -> Option<BoundaryClass> {
    models
        .iter()
        .find(|model| model.name() == name)
        .map(|model| {
            if model.tier() == Some(ProviderTier::Local) {
                BoundaryClass::Local
            } else {
                BoundaryClass::Hosted
            }
        })
}

const fn actual_boundary(boundary: ExecutionBoundary) -> BoundaryClass {
    match boundary {
        ExecutionBoundary::OnDevice | ExecutionBoundary::LocalNetwork => BoundaryClass::Local,
        ExecutionBoundary::Hosted => BoundaryClass::Hosted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recent::{RecentOutcome, UserOutcome};

    fn models() -> Vec<ConfiguredModel> {
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "local-model",
                None,
                true,
            )
            .with_provider(
                wayfinder_config::gateway::ProviderKind::OpenAiCompatible,
                Some(ProviderTier::Local),
            ),
            ConfiguredModel::new(
                "cloud",
                "https://example.test/v1",
                "cloud-model",
                None,
                true,
            ),
        ]
    }

    fn entry(
        id: &str,
        model: &str,
        boundary: ExecutionBoundary,
        score: f64,
        label: UserOutcome,
    ) -> RecentEntry {
        RecentEntry {
            request_id: id.to_owned(),
            model: model.to_owned(),
            score,
            mode: "scored".to_owned(),
            policy_version: None,
            snapshot_id: None,
            policy_profile: None,
            served_by: Some(model.to_owned()),
            execution_boundary: Some(boundary),
            outcome: Some(RecentOutcome::Succeeded),
            user_outcome: Some(label),
            http_status: Some(200),
            error_type: None,
            ts: 1_700_000_000.0,
            cost: None,
            key: None,
            workspace: Some("project-a".to_owned()),
            cache: None,
        }
    }

    #[test]
    fn negative_local_evidence_can_only_narrow_the_local_range() {
        let entries = vec![
            entry(
                "negative",
                "local",
                ExecutionBoundary::OnDevice,
                0.4,
                UserOutcome::Correction,
            ),
            entry(
                "success",
                "local",
                ExecutionBoundary::OnDevice,
                0.5,
                UserOutcome::Success,
            ),
        ];
        let report = propose_local_threshold(
            "project-a",
            &RoutingConfig::binary(0.65),
            &models(),
            &entries,
        );
        assert_eq!(report.disposition, "propose-narrower-local-range");
        assert_eq!(report.proposed_threshold, Some(0.4));
        assert_eq!(report.proposed_tiers[1].min_score, 0.4);
        assert_eq!(report.reclassified_local_successes, 1);
    }

    #[test]
    fn selected_hosted_served_local_negative_is_confounded_not_actionable() {
        let mut inverse = entry(
            "inverse",
            "cloud",
            ExecutionBoundary::OnDevice,
            0.8,
            UserOutcome::Failure,
        );
        inverse.served_by = Some("local".to_owned());
        let report = propose_local_threshold(
            "project-a",
            &RoutingConfig::binary(0.65),
            &models(),
            &[inverse],
        );
        assert_eq!(report.disposition, "no-actionable-local-evidence");
        assert_eq!(report.confounded_cross_boundary, 1);
        assert_eq!(report.actionable_local, 0);
        assert_eq!(report.proposed_threshold, None);
        assert!(report.proposed_tiers.is_empty());
    }
}
