//! Prompt-free per-workspace value reporting contracts.

use std::collections::BTreeMap;

use serde::Serialize;
use wayfinder_service::pricing::{SavingsBreakdown, WorkspaceSavingsReport};

use crate::recent::RecentWorkspaceReport;

/// Stable public schema identifier for project value reports.
pub const PROJECT_VALUE_SCHEMA_VERSION: &str = "wf-project-value-v1";

/// Durable, successful-request accounting for one workspace.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProjectAccountingReport {
    /// Requested trailing-day window; absent means all retained days.
    pub period_days: Option<u32>,
    /// UTC day through which the report was generated.
    pub through_utc: String,
    /// First UTC day containing a workspace-attributed request in this window.
    pub first_observed_utc: Option<String>,
    /// Last UTC day containing a workspace-attributed request in this window.
    pub last_observed_utc: Option<String>,
    /// Explicit migration boundary for pre-existing aggregate ledger entries.
    pub attribution_scope: &'static str,
    /// `usd` for configured prices, otherwise `relative`.
    pub unit: String,
    /// Whether amounts are backed by configured real prices.
    pub priced: bool,
    /// Successfully delivered requests with accounted usage.
    pub requests: u64,
    /// Accounted requests whose token usage was estimated.
    pub estimated_requests: u64,
    /// Prompt plus completion tokens.
    pub tokens: u64,
    /// Realized cost of the served routes.
    pub realized: f64,
    /// Cost of the disclosed baseline rate for the same token usage.
    pub baseline: f64,
    /// Baseline less realized cost.
    pub saved: f64,
    /// Savings divided by baseline.
    pub saved_pct: f64,
    /// Stable alphabetical served-route breakdown.
    pub by_route: BTreeMap<String, SavingsBreakdown>,
}

impl ProjectAccountingReport {
    /// Convert the durable ledger response without exposing virtual-key lines.
    #[must_use]
    pub fn from_workspace(report: WorkspaceSavingsReport, through_utc: String) -> Self {
        let WorkspaceSavingsReport {
            report,
            first_observed_utc,
            last_observed_utc,
        } = report;
        Self {
            period_days: report.period_days,
            through_utc,
            first_observed_utc,
            last_observed_utc,
            attribution_scope: "workspace-attributed-requests-recorded-after-schema-activation",
            unit: report.unit,
            priced: report.priced,
            requests: report.requests,
            estimated_requests: report.estimated_requests,
            tokens: report.tokens,
            realized: report.realized,
            baseline: report.baseline,
            saved: report.saved,
            saved_pct: report.saved_pct,
            by_route: report.by_route,
        }
    }
}

/// Counterfactual used for every accounting line in the report.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProjectBaseline {
    /// Stable baseline selection rule.
    pub kind: &'static str,
    /// Every configured route tied at the selected maximum rate.
    pub routes: Vec<String>,
    /// Selected configured rate per one thousand tokens.
    pub rate_per_1k: f64,
    /// `usd` for configured prices, otherwise `relative`.
    pub unit: String,
    /// Fingerprint of the complete price table active at report generation.
    pub price_table_version: String,
}

/// Explicit user-reported quality evidence retained with delivery receipts.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProjectQualityReport {
    /// Current collection state.
    pub status: &'static str,
    /// Terminal receipts that could later receive a user outcome label.
    pub eligible_receipts: u64,
    /// Terminal receipts with an explicit user outcome label.
    pub labelled_receipts: u64,
    /// Labelled receipts divided by eligible receipts.
    pub coverage_pct: Option<f64>,
    /// Explicit corrections divided by labelled receipts.
    pub correction_rate_pct: Option<f64>,
    /// Explicit failures divided by labelled receipts.
    pub failure_rate_pct: Option<f64>,
    /// Why quality values are present or absent.
    pub reason: &'static str,
}

/// Complete per-project value report assembled from bounded prompt-free facts.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProjectValueReport {
    /// Stable schema identifier.
    pub schema_version: &'static str,
    /// Workspace selected by the caller.
    pub workspace_id: String,
    /// Unix timestamp at report generation.
    pub generated_at_ts: f64,
    /// Durable cost and savings evidence.
    pub accounting: ProjectAccountingReport,
    /// Bounded process-local delivery evidence.
    pub delivery: RecentWorkspaceReport,
    /// Explicit prompt-free user outcome evidence.
    pub quality: ProjectQualityReport,
    /// Disclosed counterfactual and price-table identity.
    pub baseline: ProjectBaseline,
    /// Stable evidence limitations that materially affect interpretation.
    pub limitations: Vec<&'static str>,
}

impl ProjectValueReport {
    /// Assemble a report without deriving any score, recommendation, or policy mutation.
    #[must_use]
    pub fn new(
        workspace_id: String,
        generated_at_ts: f64,
        accounting: ProjectAccountingReport,
        delivery: RecentWorkspaceReport,
        price_table_version: String,
        prices: &indexmap::IndexMap<String, f64>,
    ) -> Self {
        let rate_per_1k = prices.values().copied().fold(0.0_f64, f64::max);
        let routes = prices
            .iter()
            .filter_map(|(route, rate)| {
                if rate.total_cmp(&rate_per_1k).is_eq() {
                    Some(route.clone())
                } else {
                    None
                }
            })
            .collect();
        let coverage_pct = (delivery.terminal > 0).then(|| {
            wayfinder_routing_core::python_round(
                100.0 * delivery.user_labelled as f64 / delivery.terminal as f64,
                1,
            )
        });
        let correction_rate_pct = (delivery.user_labelled > 0).then(|| {
            wayfinder_routing_core::python_round(
                100.0 * delivery.user_corrections as f64 / delivery.user_labelled as f64,
                1,
            )
        });
        let failure_rate_pct = (delivery.user_labelled > 0).then(|| {
            wayfinder_routing_core::python_round(
                100.0 * delivery.user_failures as f64 / delivery.user_labelled as f64,
                1,
            )
        });
        let quality = ProjectQualityReport {
            status: if delivery.user_labelled > 0 {
                "collected"
            } else {
                "not-collected"
            },
            eligible_receipts: delivery.terminal,
            labelled_receipts: delivery.user_labelled,
            coverage_pct,
            correction_rate_pct,
            failure_rate_pct,
            reason: if delivery.user_labelled > 0 {
                "rates use only explicit prompt-free user outcomes on retained terminal receipts"
            } else {
                "no retained terminal receipt has an explicit user outcome label"
            },
        };
        let mut limitations = vec![
            "pre-activation aggregate accounting has no workspace attribution",
            "delivery evidence is bounded to the current process shared ring",
            "historical amounts retain their recorded baseline but not each prior price-table fingerprint",
        ];
        if delivery.user_labelled == 0 {
            limitations
                .push("correction evidence is unavailable until explicit outcome labels exist");
        } else if delivery.user_labelled < delivery.terminal {
            limitations
                .push("user outcome evidence covers only explicitly labelled retained receipts");
        }
        if accounting.requests == 0 {
            limitations.push("no workspace-attributed accounting exists in this window");
        }
        if accounting.estimated_requests > 0 {
            limitations.push("some accounted token usage is estimated");
        }
        if !accounting.priced {
            limitations.push("configured real prices are unavailable; amounts are relative");
        }
        if delivery.retained == 0 {
            limitations.push("no delivery receipts for this workspace are currently retained");
        }
        let baseline = ProjectBaseline {
            kind: "dearest-configured-rate",
            routes,
            rate_per_1k,
            unit: accounting.unit.clone(),
            price_table_version,
        };
        Self {
            schema_version: PROJECT_VALUE_SCHEMA_VERSION,
            workspace_id,
            generated_at_ts,
            accounting,
            delivery,
            quality,
            baseline,
            limitations,
        }
    }
}
