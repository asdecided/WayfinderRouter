//! Bounded, prompt-free recent routing metadata.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use wayfinder_routing_core::ExecutionBoundary;

/// Python compatibility bound for `/router/recent`.
pub const MAX_RECENT_ENTRIES: usize = 200;

/// One routing decision's non-content metadata.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RecentEntry {
    /// Twelve-hex request correlation id.
    pub request_id: String,
    /// Reported routing model.
    pub model: String,
    /// Rounded decision score.
    pub score: f64,
    /// Policy mode such as `scored` or `pinned`.
    pub mode: String,
    /// Immutable policy content identity used for this decision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_version: Option<String>,
    /// Immutable activation snapshot identity used for this decision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    /// Resolved reusable profile identity for managed policy requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_profile: Option<String>,
    /// Public route that actually served the request after failover.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub served_by: Option<String>,
    /// Actual content execution boundary for the selected concrete deployment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_boundary: Option<ExecutionBoundary>,
    /// Latest bounded delivery state observed for this request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<RecentOutcome>,
    /// Explicit prompt-free user judgment linked to this retained receipt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_outcome: Option<UserOutcome>,
    /// Upstream HTTP status when one was observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// Stable Wayfinder error type for a failed delivery, never provider content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    /// Unix timestamp supplied by the invocation boundary.
    pub ts: f64,
    /// Optional realized turn-cost metadata, nested as in the Python API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<RecentCost>,
    /// Optional virtual-key attribution id (never the credential).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Optional workspace attribution id (never a filesystem path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Optional exact-cache outcome (`hit` or `miss`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<String>,
}

/// Prompt-free lifecycle state for one selected delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecentOutcome {
    /// A streaming upstream accepted the request and may still be producing data.
    Streaming,
    /// Delivery completed successfully.
    Succeeded,
    /// Delivery failed after the route decision was recorded.
    Failed,
    /// The downstream client disconnected before stream completion.
    Cancelled,
    /// The response was served from Wayfinder's local exact cache.
    CacheHit,
}

/// Explicit user judgment attached to one prompt-free retained receipt.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UserOutcome {
    /// The served result was usable without a correction.
    Success,
    /// The served result required a user correction.
    Correction,
    /// The served result was not usable.
    Failure,
}

/// Prompt-free realized cost metadata attached after a successful delivery.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RecentCost {
    /// Cost of the selected/served model.
    pub realized: f64,
    /// Cost of the always-frontier baseline.
    pub baseline: f64,
    /// Difference between baseline and realized cost.
    pub saved: f64,
    /// Aggregate prompt and completion tokens.
    pub tokens: u64,
    /// `usd` when priced, otherwise `relative`.
    pub unit: String,
    /// Whether token usage was estimated rather than provider-reported.
    pub estimated: bool,
}

/// `/router/recent` response schema.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RecentReport {
    /// Entries currently retained, before the query limit.
    pub total: usize,
    /// Counts across every retained entry.
    pub by_model: BTreeMap<String, u64>,
    /// Scored automatic decisions currently retained, excluding pinned and
    /// policy-forced requests.
    pub scored_total: usize,
    /// Model counts across scored automatic decisions only.
    pub scored_by_model: BTreeMap<String, u64>,
    /// Newest-first entries under the clamped limit.
    pub recent: Vec<RecentEntry>,
}

/// Actual execution-boundary counts in one workspace's retained receipts.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RecentBoundaryCounts {
    /// Requests executed on the current device.
    pub on_device: u64,
    /// Requests executed by a trusted local-network destination.
    pub local_network: u64,
    /// Requests executed by a hosted destination.
    pub hosted: u64,
    /// Requests for which delivery did not expose an actual boundary.
    pub unknown: u64,
}

/// Prompt-free delivery evidence for one workspace in the shared process ring.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RecentWorkspaceReport {
    /// Explicit retention contract for this evidence source.
    pub retention: &'static str,
    /// Maximum entries across all workspaces in the shared process ring.
    pub shared_capacity: usize,
    /// Entries for this workspace currently retained.
    pub retained: u64,
    /// Earliest retained Unix timestamp for this workspace.
    pub first_observed_ts: Option<f64>,
    /// Latest retained Unix timestamp for this workspace.
    pub last_observed_ts: Option<f64>,
    /// Requests with a terminal outcome.
    pub terminal: u64,
    /// Successful terminal deliveries.
    pub succeeded: u64,
    /// Failed terminal deliveries.
    pub failed: u64,
    /// Client-cancelled terminal deliveries.
    pub cancelled: u64,
    /// Exact-cache hits treated as successful terminal deliveries.
    pub cache_hits: u64,
    /// Accepted streaming deliveries not yet terminal.
    pub in_progress: u64,
    /// Selected requests without a delivery observation.
    pub delivery_unobserved: u64,
    /// Terminal receipts carrying an explicit user judgment.
    pub user_labelled: u64,
    /// Explicit usable-without-correction judgments.
    pub user_successes: u64,
    /// Explicit correction judgments.
    pub user_corrections: u64,
    /// Explicit unusable-result judgments.
    pub user_failures: u64,
    /// Failed terminal deliveries divided by all terminal deliveries.
    pub failure_rate_pct: Option<f64>,
    /// Actual content execution boundaries across retained receipts.
    pub boundaries: RecentBoundaryCounts,
    /// Actual served-route counts, falling back to the selected model only
    /// when delivery has not yet supplied a concrete route.
    pub by_route: BTreeMap<String, u64>,
}

/// Synchronization failure.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RecentError {
    /// Internal state could not be synchronized.
    #[error("recent-route state lock is unavailable")]
    LockPoisoned,
}

/// Outcome-label rejection that does not expose request content.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum UserOutcomeError {
    /// The bounded receipt is absent or belongs to another workspace.
    #[error("no retained receipt matches that request and workspace")]
    ReceiptNotFound,
    /// Only terminal delivery receipts can receive a user judgment.
    #[error("the retained receipt is not terminal")]
    ReceiptNotTerminal,
    /// Internal state could not be synchronized.
    #[error("recent-route state lock is unavailable")]
    LockPoisoned,
}

/// Thread-safe bounded recent-decision ring.
#[derive(Debug)]
pub struct RecentRoutes {
    capacity: usize,
    entries: Mutex<VecDeque<RecentEntry>>,
}

impl RecentRoutes {
    /// Construct a ring. A zero capacity retains no entries.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.min(MAX_RECENT_ENTRIES);
        Self {
            capacity,
            entries: Mutex::new(VecDeque::with_capacity(capacity)),
        }
    }

    /// Append one decision and drop the oldest entry when over capacity.
    pub fn record(&self, entry: RecentEntry) -> Result<(), RecentError> {
        let mut entries = self.entries.lock().map_err(|_| RecentError::LockPoisoned)?;
        if self.capacity == 0 {
            return Ok(());
        }
        entries.push_back(entry);
        while entries.len() > self.capacity {
            let _ = entries.pop_front();
        }
        Ok(())
    }

    /// Attach realized cost metadata to an already-recorded request.
    ///
    /// The newest matching request wins, mirroring the append-then-enrich flow
    /// in the Python gateway while keeping prompt content out of this store.
    pub fn update_cost(&self, request_id: &str, cost: RecentCost) -> Result<bool, RecentError> {
        let mut entries = self.entries.lock().map_err(|_| RecentError::LockPoisoned)?;
        let Some(entry) = entries
            .iter_mut()
            .rev()
            .find(|entry| entry.request_id == request_id)
        else {
            return Ok(false);
        };
        entry.cost = Some(cost);
        Ok(true)
    }

    /// Attach an exact-cache outcome to an already-recorded request.
    pub fn update_cache(&self, request_id: &str, cache: &str) -> Result<bool, RecentError> {
        let mut entries = self.entries.lock().map_err(|_| RecentError::LockPoisoned)?;
        let Some(entry) = entries
            .iter_mut()
            .rev()
            .find(|entry| entry.request_id == request_id)
        else {
            return Ok(false);
        };
        entry.cache = Some(cache.to_owned());
        Ok(true)
    }

    /// Attach the actual prompt-free delivery receipt after target selection.
    pub fn update_delivery(
        &self,
        request_id: &str,
        served_by: &str,
        execution_boundary: ExecutionBoundary,
        outcome: RecentOutcome,
        http_status: Option<u16>,
        error_type: Option<&str>,
    ) -> Result<bool, RecentError> {
        let mut entries = self.entries.lock().map_err(|_| RecentError::LockPoisoned)?;
        let Some(entry) = entries
            .iter_mut()
            .rev()
            .find(|entry| entry.request_id == request_id)
        else {
            return Ok(false);
        };
        entry.served_by = Some(served_by.to_owned());
        entry.execution_boundary = Some(execution_boundary);
        entry.outcome = Some(outcome);
        entry.http_status = http_status;
        entry.error_type = error_type.map(str::to_owned);
        Ok(true)
    }

    /// Advance a previously selected delivery without changing its target.
    pub fn update_outcome(
        &self,
        request_id: &str,
        outcome: RecentOutcome,
        http_status: Option<u16>,
        error_type: Option<&str>,
    ) -> Result<bool, RecentError> {
        let mut entries = self.entries.lock().map_err(|_| RecentError::LockPoisoned)?;
        let Some(entry) = entries
            .iter_mut()
            .rev()
            .find(|entry| entry.request_id == request_id)
        else {
            return Ok(false);
        };
        entry.outcome = Some(outcome);
        entry.http_status = http_status;
        entry.error_type = error_type.map(str::to_owned);
        Ok(true)
    }

    /// Attach or replace one explicit user judgment on a terminal workspace receipt.
    pub fn label_user_outcome(
        &self,
        request_id: &str,
        workspace: &str,
        outcome: UserOutcome,
    ) -> Result<(), UserOutcomeError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| UserOutcomeError::LockPoisoned)?;
        let Some(entry) = entries.iter_mut().rev().find(|entry| {
            entry.request_id == request_id && entry.workspace.as_deref() == Some(workspace)
        }) else {
            return Err(UserOutcomeError::ReceiptNotFound);
        };
        if !matches!(
            entry.outcome,
            Some(
                RecentOutcome::Succeeded
                    | RecentOutcome::Failed
                    | RecentOutcome::Cancelled
                    | RecentOutcome::CacheHit
            )
        ) {
            return Err(UserOutcomeError::ReceiptNotTerminal);
        }
        entry.user_outcome = Some(outcome);
        Ok(())
    }

    /// Clone retained prompt-free receipts for one workspace.
    pub fn entries_for_workspace(&self, workspace: &str) -> Result<Vec<RecentEntry>, RecentError> {
        let entries = self.entries.lock().map_err(|_| RecentError::LockPoisoned)?;
        Ok(entries
            .iter()
            .filter(|entry| entry.workspace.as_deref() == Some(workspace))
            .cloned()
            .collect())
    }

    /// Snapshot current metadata with the Python `1..=200` query clamp.
    pub fn report(&self, requested_limit: i64) -> Result<RecentReport, RecentError> {
        let entries = self.entries.lock().map_err(|_| RecentError::LockPoisoned)?;
        let mut by_model = BTreeMap::new();
        let mut scored_by_model = BTreeMap::new();
        let mut scored_total = 0_usize;
        for entry in &*entries {
            let count = by_model.entry(entry.model.clone()).or_insert(0_u64);
            *count = count.saturating_add(1);
            if entry.mode == "scored" {
                scored_total = scored_total.saturating_add(1);
                let count = scored_by_model.entry(entry.model.clone()).or_insert(0_u64);
                *count = count.saturating_add(1);
            }
        }
        let clamped = requested_limit.clamp(1, MAX_RECENT_ENTRIES as i64) as usize;
        Ok(RecentReport {
            total: entries.len(),
            by_model,
            scored_total,
            scored_by_model,
            recent: entries.iter().rev().take(clamped).cloned().collect(),
        })
    }

    /// Summarize prompt-free delivery receipts for one workspace.
    pub fn report_for_workspace(
        &self,
        workspace: &str,
    ) -> Result<RecentWorkspaceReport, RecentError> {
        let entries = self.entries.lock().map_err(|_| RecentError::LockPoisoned)?;
        let mut retained = 0_u64;
        let mut first_observed_ts = None;
        let mut last_observed_ts = None;
        let mut terminal = 0_u64;
        let mut succeeded = 0_u64;
        let mut failed = 0_u64;
        let mut cancelled = 0_u64;
        let mut cache_hits = 0_u64;
        let mut in_progress = 0_u64;
        let mut delivery_unobserved = 0_u64;
        let mut user_labelled = 0_u64;
        let mut user_successes = 0_u64;
        let mut user_corrections = 0_u64;
        let mut user_failures = 0_u64;
        let mut boundaries = RecentBoundaryCounts::default();
        let mut by_route = BTreeMap::new();

        for entry in entries
            .iter()
            .filter(|entry| entry.workspace.as_deref() == Some(workspace))
        {
            retained = retained.saturating_add(1);
            first_observed_ts =
                Some(first_observed_ts.map_or(entry.ts, |value: f64| value.min(entry.ts)));
            last_observed_ts =
                Some(last_observed_ts.map_or(entry.ts, |value: f64| value.max(entry.ts)));
            match entry.outcome {
                Some(RecentOutcome::Succeeded) => {
                    terminal = terminal.saturating_add(1);
                    succeeded = succeeded.saturating_add(1);
                }
                Some(RecentOutcome::Failed) => {
                    terminal = terminal.saturating_add(1);
                    failed = failed.saturating_add(1);
                }
                Some(RecentOutcome::Cancelled) => {
                    terminal = terminal.saturating_add(1);
                    cancelled = cancelled.saturating_add(1);
                }
                Some(RecentOutcome::CacheHit) => {
                    terminal = terminal.saturating_add(1);
                    cache_hits = cache_hits.saturating_add(1);
                }
                Some(RecentOutcome::Streaming) => {
                    in_progress = in_progress.saturating_add(1);
                }
                None => {
                    delivery_unobserved = delivery_unobserved.saturating_add(1);
                }
            }
            match entry.user_outcome {
                Some(UserOutcome::Success) => {
                    user_labelled = user_labelled.saturating_add(1);
                    user_successes = user_successes.saturating_add(1);
                }
                Some(UserOutcome::Correction) => {
                    user_labelled = user_labelled.saturating_add(1);
                    user_corrections = user_corrections.saturating_add(1);
                }
                Some(UserOutcome::Failure) => {
                    user_labelled = user_labelled.saturating_add(1);
                    user_failures = user_failures.saturating_add(1);
                }
                None => {}
            }
            match entry.execution_boundary {
                Some(ExecutionBoundary::OnDevice) => {
                    boundaries.on_device = boundaries.on_device.saturating_add(1);
                }
                Some(ExecutionBoundary::LocalNetwork) => {
                    boundaries.local_network = boundaries.local_network.saturating_add(1);
                }
                Some(ExecutionBoundary::Hosted) => {
                    boundaries.hosted = boundaries.hosted.saturating_add(1);
                }
                None => {
                    boundaries.unknown = boundaries.unknown.saturating_add(1);
                }
            }
            let route = entry.served_by.as_ref().unwrap_or(&entry.model);
            let count = by_route.entry(route.clone()).or_insert(0_u64);
            *count = count.saturating_add(1);
        }

        let failure_rate_pct = (terminal > 0).then(|| {
            wayfinder_routing_core::python_round(100.0 * failed as f64 / terminal as f64, 1)
        });
        Ok(RecentWorkspaceReport {
            retention: "process-local-bounded-shared-ring",
            shared_capacity: self.capacity,
            retained,
            first_observed_ts,
            last_observed_ts,
            terminal,
            succeeded,
            failed,
            cancelled,
            cache_hits,
            in_progress,
            delivery_unobserved,
            user_labelled,
            user_successes,
            user_corrections,
            user_failures,
            failure_rate_pct,
            boundaries,
            by_route,
        })
    }
}

impl Default for RecentRoutes {
    fn default() -> Self {
        Self::new(MAX_RECENT_ENTRIES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(request_id: &str, model: &str, score: f64) -> RecentEntry {
        RecentEntry {
            request_id: request_id.to_owned(),
            model: model.to_owned(),
            score,
            mode: "scored".to_owned(),
            policy_version: None,
            snapshot_id: None,
            policy_profile: None,
            served_by: None,
            execution_boundary: None,
            outcome: None,
            user_outcome: None,
            http_status: None,
            error_type: None,
            ts: 1_700_000_000.0,
            cost: None,
            key: None,
            workspace: None,
            cache: None,
        }
    }

    #[test]
    fn report_is_newest_first_and_counts_full_ring() -> Result<(), RecentError> {
        let recent = RecentRoutes::new(3);
        recent.record(entry("a", "local", 0.1))?;
        recent.record(entry("b", "cloud", 0.8))?;
        recent.record(entry("c", "local", 0.2))?;
        let report = recent.report(2)?;
        assert_eq!(report.total, 3);
        assert_eq!(report.by_model["local"], 2);
        assert_eq!(report.by_model["cloud"], 1);
        assert_eq!(report.scored_total, 3);
        assert_eq!(report.scored_by_model, report.by_model);
        assert_eq!(
            report
                .recent
                .iter()
                .map(|entry| entry.request_id.as_str())
                .collect::<Vec<_>>(),
            ["c", "b"]
        );
        Ok(())
    }

    #[test]
    fn scored_summary_excludes_pinned_routes() -> Result<(), RecentError> {
        let recent = RecentRoutes::new(3);
        recent.record(entry("a", "local", 0.0))?;
        let mut pinned = entry("b", "cloud", 0.0);
        pinned.mode = "pinned".to_owned();
        recent.record(pinned)?;
        let report = recent.report(1)?;
        assert_eq!(report.total, 2);
        assert_eq!(report.by_model["cloud"], 1);
        assert_eq!(report.scored_total, 1);
        assert_eq!(report.scored_by_model.get("cloud"), None);
        assert_eq!(report.scored_by_model["local"], 1);
        Ok(())
    }

    #[test]
    fn capacity_drops_oldest_and_zero_retains_nothing() -> Result<(), RecentError> {
        let recent = RecentRoutes::new(2);
        for id in ["a", "b", "c"] {
            recent.record(entry(id, "local", 0.1))?;
        }
        assert_eq!(
            recent
                .report(50)?
                .recent
                .iter()
                .map(|entry| entry.request_id.as_str())
                .collect::<Vec<_>>(),
            ["c", "b"]
        );
        let disabled = RecentRoutes::new(0);
        disabled.record(entry("x", "cloud", 0.9))?;
        assert_eq!(disabled.report(50)?.total, 0);
        Ok(())
    }

    #[test]
    fn delivery_receipt_tracks_actual_boundary_and_bounded_outcome() -> Result<(), RecentError> {
        let recent = RecentRoutes::new(1);
        recent.record(entry("request", "cloud", 0.8))?;
        assert!(recent.update_delivery(
            "request",
            "local",
            ExecutionBoundary::OnDevice,
            RecentOutcome::Streaming,
            Some(200),
            None,
        )?);
        assert!(recent.update_outcome(
            "request",
            RecentOutcome::Failed,
            None,
            Some("wayfinder_router_upstream_error"),
        )?);

        let report = recent.report(1)?;
        let entry = &report.recent[0];
        assert_eq!(entry.served_by.as_deref(), Some("local"));
        assert_eq!(entry.execution_boundary, Some(ExecutionBoundary::OnDevice));
        assert_eq!(entry.outcome, Some(RecentOutcome::Failed));
        assert_eq!(entry.http_status, None);
        assert_eq!(
            entry.error_type.as_deref(),
            Some("wayfinder_router_upstream_error")
        );
        Ok(())
    }

    #[test]
    fn user_outcomes_require_a_terminal_receipt_in_the_same_workspace()
    -> Result<(), Box<dyn std::error::Error>> {
        let recent = RecentRoutes::new(2);
        let mut value = entry("request", "local", 0.3);
        value.workspace = Some("project-a".to_owned());
        recent.record(value)?;
        assert_eq!(
            recent.label_user_outcome("request", "project-a", UserOutcome::Correction),
            Err(UserOutcomeError::ReceiptNotTerminal)
        );
        recent.update_delivery(
            "request",
            "local",
            ExecutionBoundary::OnDevice,
            RecentOutcome::Succeeded,
            Some(200),
            None,
        )?;
        assert_eq!(
            recent.label_user_outcome("request", "project-b", UserOutcome::Correction),
            Err(UserOutcomeError::ReceiptNotFound)
        );
        recent.label_user_outcome("request", "project-a", UserOutcome::Correction)?;
        let report = recent.report_for_workspace("project-a")?;
        assert_eq!(report.user_labelled, 1);
        assert_eq!(report.user_corrections, 1);
        assert_eq!(report.user_successes, 0);
        assert_eq!(report.user_failures, 0);
        assert_eq!(
            recent.report(1)?.recent[0].user_outcome,
            Some(UserOutcome::Correction)
        );
        Ok(())
    }

    #[test]
    fn query_limit_clamps_to_at_least_one() -> Result<(), RecentError> {
        let recent = RecentRoutes::new(3);
        recent.record(entry("a", "local", 0.1))?;
        recent.record(entry("b", "local", 0.2))?;
        assert_eq!(recent.report(0)?.recent.len(), 1);
        assert_eq!(recent.report(-100)?.recent.len(), 1);
        Ok(())
    }

    #[test]
    fn serialized_entry_has_no_content_field() -> Result<(), Box<dyn std::error::Error>> {
        let serialized = serde_json::to_value(entry("a", "local", 0.1))?;
        assert!(serialized.get("prompt").is_none());
        assert!(serialized.get("messages").is_none());
        assert!(serialized.get("content").is_none());
        Ok(())
    }

    #[test]
    fn cost_is_nested_with_the_python_field_set() -> Result<(), Box<dyn std::error::Error>> {
        use std::collections::BTreeSet;

        let mut value = entry("a", "cloud", 0.9);
        value.cost = Some(RecentCost {
            realized: 0.01,
            baseline: 0.02,
            saved: 0.01,
            tokens: 1_000,
            unit: "usd".to_owned(),
            estimated: false,
        });
        let serialized = serde_json::to_value(value)?;
        let object = serialized["cost"]
            .as_object()
            .ok_or_else(|| std::io::Error::other("missing nested cost"))?;
        assert_eq!(
            object.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "realized",
                "baseline",
                "saved",
                "tokens",
                "unit",
                "estimated"
            ])
        );
        assert!(serialized.get("realized").is_none());
        Ok(())
    }

    #[test]
    fn recorded_entry_can_be_enriched_by_request_id() -> Result<(), RecentError> {
        let recent = RecentRoutes::new(2);
        recent.record(entry("a", "local", 0.1))?;
        recent.record(entry("b", "cloud", 0.9))?;
        let cost = RecentCost {
            realized: 0.01,
            baseline: 0.02,
            saved: 0.01,
            tokens: 1_000,
            unit: "usd".to_owned(),
            estimated: false,
        };
        assert!(recent.update_cost("a", cost.clone())?);
        assert!(!recent.update_cost("missing", cost)?);
        assert!(recent.update_cache("b", "miss")?);
        assert!(!recent.update_cache("missing", "hit")?);
        let report = recent.report(2)?;
        assert_eq!(report.recent[0].cost, None);
        assert_eq!(report.recent[0].cache.as_deref(), Some("miss"));
        assert_eq!(
            report.recent[1].cost.as_ref().map(|cost| cost.tokens),
            Some(1_000)
        );
        Ok(())
    }

    #[test]
    fn workspace_report_isolatedly_counts_boundaries_and_terminal_failures()
    -> Result<(), RecentError> {
        let recent = RecentRoutes::new(4);
        let mut first = entry("a", "selected-cloud", 0.8);
        first.workspace = Some("project-a".to_owned());
        recent.record(first)?;
        recent.update_delivery(
            "a",
            "actual-local",
            ExecutionBoundary::OnDevice,
            RecentOutcome::Succeeded,
            Some(200),
            None,
        )?;
        let mut second = entry("b", "cloud", 0.9);
        second.workspace = Some("project-a".to_owned());
        second.ts = 1_700_000_010.0;
        recent.record(second)?;
        recent.update_delivery(
            "b",
            "cloud",
            ExecutionBoundary::Hosted,
            RecentOutcome::Failed,
            Some(503),
            Some("wayfinder_router_upstream_error"),
        )?;
        let mut other = entry("c", "cloud", 0.9);
        other.workspace = Some("project-b".to_owned());
        recent.record(other)?;

        let report = recent.report_for_workspace("project-a")?;
        assert_eq!(report.retained, 2);
        assert_eq!(report.terminal, 2);
        assert_eq!(report.succeeded, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(report.failure_rate_pct, Some(50.0));
        assert_eq!(report.boundaries.on_device, 1);
        assert_eq!(report.boundaries.hosted, 1);
        assert_eq!(report.by_route["actual-local"], 1);
        assert_eq!(report.by_route["cloud"], 1);
        assert_eq!(report.first_observed_ts, Some(1_700_000_000.0));
        assert_eq!(report.last_observed_ts, Some(1_700_000_010.0));
        Ok(())
    }
}
