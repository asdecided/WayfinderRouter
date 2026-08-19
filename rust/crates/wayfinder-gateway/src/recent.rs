//! Bounded, prompt-free recent routing metadata.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use serde::Serialize;
use thiserror::Error;

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
    /// Unix timestamp supplied by the invocation boundary.
    pub ts: f64,
    /// Optional realized turn-cost metadata, nested as in the Python API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<RecentCost>,
    /// Optional virtual-key attribution id (never the credential).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Optional exact-cache outcome (`hit` or `miss`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<String>,
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
    /// Newest-first entries under the clamped limit.
    pub recent: Vec<RecentEntry>,
}

/// Synchronization failure.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RecentError {
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

    /// Snapshot current metadata with the Python `1..=200` query clamp.
    pub fn report(&self, requested_limit: i64) -> Result<RecentReport, RecentError> {
        let entries = self.entries.lock().map_err(|_| RecentError::LockPoisoned)?;
        let mut by_model = BTreeMap::new();
        for entry in &*entries {
            let count = by_model.entry(entry.model.clone()).or_insert(0_u64);
            *count = count.saturating_add(1);
        }
        let clamped = requested_limit.clamp(1, MAX_RECENT_ENTRIES as i64) as usize;
        Ok(RecentReport {
            total: entries.len(),
            by_model,
            recent: entries.iter().rev().take(clamped).cloned().collect(),
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
            ts: 1_700_000_000.0,
            cost: None,
            key: None,
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
}
