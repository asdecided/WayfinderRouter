//! Deterministic token, cost, and savings arithmetic.
//!
//! This module contains no clock, filesystem, network, or provider calls.  It
//! turns already-observed usage into auditable cost data.  Persisted ledger
//! state is a separate boundary so these calculations remain independently
//! differential-testable against Python.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use indexmap::IndexMap;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use wayfinder_routing_core::python_round;

/// The compatibility estimate used when an upstream omits token usage.
pub const CHARS_PER_TOKEN: usize = 4;
/// Current on-disk ledger envelope. Legacy unversioned snapshots remain readable.
pub const LEDGER_SCHEMA_VERSION: u64 = 2;

/// Failures possible while creating stable pricing metadata.
#[derive(Debug, Error)]
pub enum PricingError {
    /// Cost tables must contain finite, non-negative values.
    #[error("price for model '{model}' must be finite and non-negative")]
    InvalidPrice { model: String },
    /// Stable JSON serialization failed.
    #[error("cannot serialize the price table: {0}")]
    Json(#[from] serde_json::Error),
}

/// Failures at the persisted ledger boundary.
#[derive(Debug, Error)]
pub enum LedgerError {
    /// A supplied date was not a real ISO calendar date.
    #[error("invalid UTC date: {0}")]
    InvalidDate(String),
    /// The ledger's synchronization lock was poisoned.
    #[error("savings ledger lock is unavailable")]
    LockPoisoned,
    /// Reading, syncing, or atomically replacing a ledger file failed.
    #[error("ledger I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Persisted JSON was malformed.
    #[error("invalid ledger JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// A price mapping plus whether its unit is real currency or relative.
#[derive(Clone, Debug, PartialEq)]
pub struct PriceTable {
    /// Per-model cost per one thousand tokens, preserving configuration order.
    pub costs: IndexMap<String, f64>,
    /// `true` when at least one real `cost_per_1k` value was configured.
    pub priced: bool,
}

/// A rough token count (~four Unicode scalar values per token).
///
/// Python's `len(str)` counts Unicode code points, so Rust must use `chars()`
/// rather than UTF-8 byte length for differential parity.
#[must_use]
pub fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let estimated = text.chars().count() / CHARS_PER_TOKEN;
    u64::try_from(estimated.max(1)).unwrap_or(u64::MAX)
}

/// Build a real price table when any costs exist, otherwise relative units.
pub fn price_table(
    model_costs: &IndexMap<String, Option<f64>>,
    tier_ladder: &[String],
) -> Result<PriceTable, PricingError> {
    for (model, cost) in model_costs {
        if cost.is_some_and(|value| !value.is_finite() || value < 0.0) {
            return Err(PricingError::InvalidPrice {
                model: model.clone(),
            });
        }
    }

    let real = model_costs
        .iter()
        .filter_map(|(model, cost)| cost.map(|value| (model.clone(), value)))
        .collect::<IndexMap<_, _>>();
    if !real.is_empty() {
        return Ok(PriceTable {
            costs: real,
            priced: true,
        });
    }

    let names = if tier_ladder.is_empty() {
        model_costs.keys().cloned().collect::<Vec<_>>()
    } else {
        tier_ladder.to_vec()
    };
    if names.is_empty() {
        return Ok(PriceTable {
            costs: IndexMap::new(),
            priced: false,
        });
    }

    let denominator = names.len().saturating_sub(1).max(1) as f64;
    let step = (1.0 - 0.2) / denominator;
    let mut costs = IndexMap::new();
    for (index, model) in names.into_iter().enumerate() {
        costs.insert(model, python_round(0.2 + index as f64 * step, 3));
    }
    Ok(PriceTable {
        costs,
        priced: false,
    })
}

/// Return the Python-compatible 12-hex SHA-256 fingerprint of a price table.
pub fn table_version(costs: &IndexMap<String, f64>) -> Result<String, PricingError> {
    let mut sorted = BTreeMap::new();
    for (model, cost) in costs {
        if !cost.is_finite() || *cost < 0.0 {
            return Err(PricingError::InvalidPrice {
                model: model.clone(),
            });
        }
        sorted.insert(model, cost);
    }
    let json = serde_json::to_string(&sorted)?;
    let mut ascii_json = String::with_capacity(json.len());
    for character in json.chars() {
        if character.is_ascii() {
            ascii_json.push(character);
        } else {
            for code_unit in character.encode_utf16(&mut [0; 2]) {
                ascii_json.push_str(&format!("\\u{code_unit:04x}"));
            }
        }
    }
    let digest = Sha256::digest(ascii_json.as_bytes());
    let mut fingerprint = String::with_capacity(12);
    for byte in digest.iter().take(6) {
        fingerprint.push(char::from(b"0123456789abcdef"[usize::from(*byte >> 4)]));
        fingerprint.push(char::from(b"0123456789abcdef"[usize::from(*byte & 0x0f)]));
    }
    Ok(fingerprint)
}

/// Upstream or estimated token counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsageTokens {
    /// Prompt/input tokens.
    pub prompt: i64,
    /// Completion/output tokens.
    pub completion: i64,
    /// Whether the values were estimated from text.
    pub estimated: bool,
}

/// Prefer an upstream OpenAI `usage` object, falling back to text estimates.
#[must_use]
pub fn usage_tokens(response: &Value, prompt_text: &str, completion_text: &str) -> UsageTokens {
    if let Some(usage) = response.get("usage").and_then(Value::as_object) {
        let prompt = usage.get("prompt_tokens").and_then(Value::as_i64);
        let completion = usage.get("completion_tokens").and_then(Value::as_i64);
        if let (Some(prompt), Some(completion)) = (prompt, completion) {
            return UsageTokens {
                prompt,
                completion,
                estimated: false,
            };
        }
        if let Some(total) = usage.get("total_tokens").and_then(Value::as_i64) {
            let prompt = prompt.unwrap_or(0);
            return UsageTokens {
                prompt,
                completion: total.saturating_sub(prompt).max(0),
                estimated: false,
            };
        }
    }

    UsageTokens {
        prompt: i64::try_from(estimate_tokens(prompt_text)).unwrap_or(i64::MAX),
        completion: i64::try_from(estimate_tokens(completion_text)).unwrap_or(i64::MAX),
        estimated: true,
    }
}

/// Realized and counterfactual cost for one served turn.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnCost {
    /// Routing model name actually served.
    pub route: String,
    /// Cost of the served route.
    pub realized: f64,
    /// Cost of the configured baseline route.
    pub baseline: f64,
    /// `baseline - realized`, which may truthfully be negative.
    pub savings: f64,
    /// Non-negative prompt token count.
    pub prompt_tokens: u64,
    /// Non-negative completion token count.
    pub completion_tokens: u64,
    /// Whether token usage was estimated.
    pub estimated: bool,
}

/// Calculate one turn's realized, baseline, and savings amounts.
pub fn turn_cost(
    route: &str,
    prompt_tokens: i64,
    completion_tokens: i64,
    costs: &IndexMap<String, f64>,
    estimated: bool,
    baseline_model: Option<&str>,
) -> Result<TurnCost, PricingError> {
    let mut dearest: f64 = 0.0;
    for (model, value) in costs {
        if !value.is_finite() || *value < 0.0 {
            return Err(PricingError::InvalidPrice {
                model: model.clone(),
            });
        }
        dearest = dearest.max(*value);
    }

    let prompt = u64::try_from(prompt_tokens.max(0)).unwrap_or(u64::MAX);
    let completion = u64::try_from(completion_tokens.max(0)).unwrap_or(u64::MAX);
    let total_tokens = prompt.saturating_add(completion);
    let total_thousands = total_tokens as f64 / 1_000.0;
    let baseline_rate = baseline_model
        .and_then(|model| costs.get(model))
        .copied()
        .unwrap_or(dearest);
    let route_rate = costs.get(route).copied().unwrap_or(dearest);
    let realized = python_round(route_rate * total_thousands, 6);
    let baseline = python_round(baseline_rate * total_thousands, 6);

    Ok(TurnCost {
        route: route.to_owned(),
        realized,
        baseline,
        savings: python_round(baseline - realized, 6),
        prompt_tokens: prompt,
        completion_tokens: completion,
        estimated,
    })
}

/// A validated UTC calendar date used as a ledger bucket key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct UtcDate {
    year: i32,
    month: u8,
    day: u8,
}

impl UtcDate {
    /// Construct a date in the same year range as Python's `datetime.date`.
    pub fn new(year: i32, month: u8, day: u8) -> Result<Self, LedgerError> {
        let valid = (1..=9_999).contains(&year)
            && (1..=12).contains(&month)
            && (1..=days_in_month(year, month)).contains(&day);
        if !valid {
            return Err(LedgerError::InvalidDate(format!(
                "{year:04}-{month:02}-{day:02}"
            )));
        }
        Ok(Self { year, month, day })
    }

    /// Parse a strict `YYYY-MM-DD` bucket key.
    pub fn parse(value: &str) -> Result<Self, LedgerError> {
        let bytes = value.as_bytes();
        if bytes.len() != 10 || bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
            return Err(LedgerError::InvalidDate(value.to_owned()));
        }
        let year = value
            .get(0..4)
            .and_then(|part| part.parse::<i32>().ok())
            .ok_or_else(|| LedgerError::InvalidDate(value.to_owned()))?;
        let month = value
            .get(5..7)
            .and_then(|part| part.parse::<u8>().ok())
            .ok_or_else(|| LedgerError::InvalidDate(value.to_owned()))?;
        let day = value
            .get(8..10)
            .and_then(|part| part.parse::<u8>().ok())
            .ok_or_else(|| LedgerError::InvalidDate(value.to_owned()))?;
        Self::new(year, month, day)
    }

    fn ordinal(self) -> i64 {
        // Howard Hinnant's civil-date transform. Only differences matter for
        // period filtering, so the Unix-epoch offset is intentionally omitted.
        let mut year = i64::from(self.year);
        let month = i64::from(self.month);
        let day = i64::from(self.day);
        year -= i64::from(month <= 2);
        let era = if year >= 0 { year } else { year - 399 } / 400;
        let year_of_era = year - era * 400;
        let shifted_month = month + if month > 2 { -3 } else { 9 };
        let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        era * 146_097 + day_of_era
    }

    fn month_prefix(self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }
}

impl fmt::Display for UtcDate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04}-{:02}-{:02}",
            self.year, self.month, self.day
        )
    }
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
struct LedgerStats {
    n: u64,
    realized: f64,
    baseline: f64,
    savings: f64,
    tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
struct DayBucket {
    n: u64,
    realized: f64,
    baseline: f64,
    savings: f64,
    tokens: u64,
    estimated_n: u64,
    by_route: BTreeMap<String, LedgerStats>,
    by_key: BTreeMap<String, LedgerStats>,
    by_workspace: BTreeMap<String, WorkspaceBucket>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
struct WorkspaceBucket {
    n: u64,
    realized: f64,
    baseline: f64,
    savings: f64,
    tokens: u64,
    estimated_n: u64,
    by_route: BTreeMap<String, LedgerStats>,
}

impl DayBucket {
    fn add_turn(&mut self, cost: &TurnCost) {
        self.n = self.n.saturating_add(1);
        self.realized = python_round(self.realized + cost.realized, 6);
        self.baseline = python_round(self.baseline + cost.baseline, 6);
        self.savings = python_round(self.savings + cost.savings, 6);
        self.tokens = self
            .tokens
            .saturating_add(cost.prompt_tokens.saturating_add(cost.completion_tokens));
    }

    fn add_bucket(&mut self, other: &Self) {
        self.n = self.n.saturating_add(other.n);
        self.realized = python_round(self.realized + other.realized, 6);
        self.baseline = python_round(self.baseline + other.baseline, 6);
        self.savings = python_round(self.savings + other.savings, 6);
        self.tokens = self.tokens.saturating_add(other.tokens);
        self.estimated_n = self.estimated_n.saturating_add(other.estimated_n);
        merge_stat_maps(&mut self.by_route, &other.by_route);
        merge_stat_maps(&mut self.by_key, &other.by_key);
        for (workspace, bucket) in &other.by_workspace {
            self.by_workspace
                .entry(workspace.clone())
                .or_default()
                .add_bucket(bucket);
        }
    }
}

impl WorkspaceBucket {
    fn add_turn(&mut self, cost: &TurnCost) {
        self.n = self.n.saturating_add(1);
        self.realized = python_round(self.realized + cost.realized, 6);
        self.baseline = python_round(self.baseline + cost.baseline, 6);
        self.savings = python_round(self.savings + cost.savings, 6);
        self.tokens = self
            .tokens
            .saturating_add(cost.prompt_tokens.saturating_add(cost.completion_tokens));
        if cost.estimated {
            self.estimated_n = self.estimated_n.saturating_add(1);
        }
        self.by_route
            .entry(cost.route.clone())
            .or_default()
            .add_turn(cost);
    }

    fn add_bucket(&mut self, other: &Self) {
        self.n = self.n.saturating_add(other.n);
        self.realized = python_round(self.realized + other.realized, 6);
        self.baseline = python_round(self.baseline + other.baseline, 6);
        self.savings = python_round(self.savings + other.savings, 6);
        self.tokens = self.tokens.saturating_add(other.tokens);
        self.estimated_n = self.estimated_n.saturating_add(other.estimated_n);
        merge_stat_maps(&mut self.by_route, &other.by_route);
    }
}

impl LedgerStats {
    fn add_turn(&mut self, cost: &TurnCost) {
        self.n = self.n.saturating_add(1);
        self.realized = python_round(self.realized + cost.realized, 6);
        self.baseline = python_round(self.baseline + cost.baseline, 6);
        self.savings = python_round(self.savings + cost.savings, 6);
        self.tokens = self
            .tokens
            .saturating_add(cost.prompt_tokens.saturating_add(cost.completion_tokens));
    }

    fn add_stats(&mut self, other: &Self) {
        self.n = self.n.saturating_add(other.n);
        self.realized = python_round(self.realized + other.realized, 6);
        self.baseline = python_round(self.baseline + other.baseline, 6);
        self.savings = python_round(self.savings + other.savings, 6);
        self.tokens = self.tokens.saturating_add(other.tokens);
    }
}

fn merge_stat_maps(
    target: &mut BTreeMap<String, LedgerStats>,
    source: &BTreeMap<String, LedgerStats>,
) {
    for (name, stats) in source {
        target.entry(name.clone()).or_default().add_stats(stats);
    }
}

/// One route or virtual-key line in a savings report.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SavingsBreakdown {
    /// Number of served requests.
    pub requests: u64,
    /// Realized cost.
    pub realized: f64,
    /// Always-baseline counterfactual.
    pub baseline: f64,
    /// Baseline less realized cost.
    pub saved: f64,
    /// Prompt plus completion tokens.
    pub tokens: u64,
}

impl From<&LedgerStats> for SavingsBreakdown {
    fn from(stats: &LedgerStats) -> Self {
        Self {
            requests: stats.n,
            realized: python_round(stats.realized, 6),
            baseline: python_round(stats.baseline, 6),
            saved: python_round(stats.savings, 6),
            tokens: stats.tokens,
        }
    }
}

/// Aggregate savings response used by `/v1/savings` and metrics.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SavingsReport {
    /// Requested trailing-day window; `None` means all time.
    pub period_days: Option<u32>,
    /// `"usd"` for configured prices, otherwise `"relative"`.
    pub unit: String,
    /// Whether the amounts represent real configured prices.
    pub priced: bool,
    /// Total requests.
    pub requests: u64,
    /// Requests whose token count was estimated.
    pub estimated_requests: u64,
    /// Total prompt and completion tokens.
    pub tokens: u64,
    /// Total realized cost.
    pub realized: f64,
    /// Total baseline cost.
    pub baseline: f64,
    /// Total savings.
    pub saved: f64,
    /// Savings as a percentage of baseline, rounded to one decimal.
    pub saved_pct: f64,
    /// Stable alphabetical route breakdown.
    pub by_route: BTreeMap<String, SavingsBreakdown>,
    /// Stable alphabetical virtual-key breakdown.
    pub by_key: BTreeMap<String, SavingsBreakdown>,
}

/// One workspace's durable accounting window.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WorkspaceSavingsReport {
    /// Aggregate savings fields for the selected workspace only.
    #[serde(flatten)]
    pub report: SavingsReport,
    /// First UTC day containing an attributed request in the selected window.
    pub first_observed_utc: Option<String>,
    /// Last UTC day containing an attributed request in the selected window.
    pub last_observed_utc: Option<String>,
}

/// All-time counters exported to metrics.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct SavingsTotals {
    /// Realized cost.
    pub realized: f64,
    /// Baseline cost.
    pub baseline: f64,
    /// Baseline less realized cost.
    pub saved: f64,
}

/// Thread-safe, bounded daily savings ledger.
#[derive(Debug)]
pub struct SavingsLedger {
    max_days: usize,
    priced: AtomicBool,
    days: Mutex<BTreeMap<String, DayBucket>>,
    persistence: Mutex<()>,
}

/// How a resilient persisted-ledger open was satisfied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LedgerLoad {
    /// The primary file was valid.
    Primary,
    /// The primary was corrupt and a last-good snapshot was restored.
    Recovered { quarantine: PathBuf },
    /// No persisted ledger existed, so a new empty ledger was created.
    New,
}

impl SavingsLedger {
    /// Create an empty ledger. `max_days = 0` intentionally retains no data.
    #[must_use]
    pub fn new(max_days: usize, priced: bool) -> Self {
        Self {
            max_days,
            priced: AtomicBool::new(priced),
            days: Mutex::new(BTreeMap::new()),
            persistence: Mutex::new(()),
        }
    }

    /// Whether values are denominated in configured real prices.
    #[must_use]
    pub fn priced(&self) -> bool {
        self.priced.load(Ordering::Acquire)
    }

    /// Update whether subsequently reported values use configured prices.
    ///
    /// Python updates this mode immediately before recording each turn because
    /// hot reload may add or remove model prices. The atomic keeps that mutable
    /// compatibility field independent from the mutex-protected day buckets.
    pub fn set_priced(&self, priced: bool) {
        self.priced.store(priced, Ordering::Release);
    }

    /// Record one turn and optional virtual-key attribution.
    pub fn record(
        &self,
        cost: &TurnCost,
        when: UtcDate,
        virtual_key: Option<&str>,
    ) -> Result<(), LedgerError> {
        self.record_attributed(cost, when, virtual_key, None)
    }

    /// Record one turn with optional virtual-key and workspace attribution.
    pub fn record_attributed(
        &self,
        cost: &TurnCost,
        when: UtcDate,
        virtual_key: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<(), LedgerError> {
        let mut days = self.days.lock().map_err(|_| LedgerError::LockPoisoned)?;
        let bucket = days.entry(when.to_string()).or_default();
        bucket.add_turn(cost);
        if cost.estimated {
            bucket.estimated_n = bucket.estimated_n.saturating_add(1);
        }
        bucket
            .by_route
            .entry(cost.route.clone())
            .or_default()
            .add_turn(cost);
        if let Some(key) = virtual_key {
            bucket
                .by_key
                .entry(key.to_owned())
                .or_default()
                .add_turn(cost);
        }
        if let Some(workspace) = workspace {
            bucket
                .by_workspace
                .entry(workspace.to_owned())
                .or_default()
                .add_turn(cost);
        }
        while days.len() > self.max_days {
            let _ = days.pop_first();
        }
        Ok(())
    }

    /// Aggregate the last `period_days` UTC buckets, or all buckets when absent.
    pub fn period(
        &self,
        period_days: Option<u32>,
        today: UtcDate,
    ) -> Result<SavingsReport, LedgerError> {
        let cutoff = period_days.map(|days| {
            today
                .ordinal()
                .saturating_sub(i64::from(days).saturating_sub(1))
        });
        let days = self.days.lock().map_err(|_| LedgerError::LockPoisoned)?;
        let mut aggregate = DayBucket::default();
        for (date, bucket) in &*days {
            let include = match cutoff {
                None => true,
                Some(cutoff) => UtcDate::parse(date).is_ok_and(|date| date.ordinal() >= cutoff),
            };
            if include {
                aggregate.add_bucket(bucket);
            }
        }

        let baseline = python_round(aggregate.baseline, 6);
        let saved = python_round(aggregate.savings, 6);
        let saved_pct = if baseline == 0.0 {
            0.0
        } else {
            python_round(100.0 * saved / baseline, 1)
        };
        let priced = self.priced();
        Ok(SavingsReport {
            period_days,
            unit: if priced { "usd" } else { "relative" }.to_owned(),
            priced,
            requests: aggregate.n,
            estimated_requests: aggregate.estimated_n,
            tokens: aggregate.tokens,
            realized: python_round(aggregate.realized, 6),
            baseline,
            saved,
            saved_pct,
            by_route: aggregate
                .by_route
                .iter()
                .map(|(name, stats)| (name.clone(), SavingsBreakdown::from(stats)))
                .collect(),
            by_key: aggregate
                .by_key
                .iter()
                .map(|(name, stats)| (name.clone(), SavingsBreakdown::from(stats)))
                .collect(),
        })
    }

    /// Aggregate one workspace's attributed turns over a trailing-day window.
    pub fn period_for_workspace(
        &self,
        workspace: &str,
        period_days: Option<u32>,
        today: UtcDate,
    ) -> Result<WorkspaceSavingsReport, LedgerError> {
        let cutoff = period_days.map(|days| {
            today
                .ordinal()
                .saturating_sub(i64::from(days).saturating_sub(1))
        });
        let days = self.days.lock().map_err(|_| LedgerError::LockPoisoned)?;
        let mut aggregate = WorkspaceBucket::default();
        let mut first_observed_utc = None;
        let mut last_observed_utc = None;
        for (date, bucket) in &*days {
            let include = match cutoff {
                None => true,
                Some(cutoff) => UtcDate::parse(date).is_ok_and(|date| date.ordinal() >= cutoff),
            };
            if !include {
                continue;
            }
            let Some(workspace_bucket) = bucket.by_workspace.get(workspace) else {
                continue;
            };
            if workspace_bucket.n == 0 {
                continue;
            }
            if first_observed_utc.is_none() {
                first_observed_utc = Some(date.clone());
            }
            last_observed_utc = Some(date.clone());
            aggregate.add_bucket(workspace_bucket);
        }

        let baseline = python_round(aggregate.baseline, 6);
        let saved = python_round(aggregate.savings, 6);
        let saved_pct = if baseline == 0.0 {
            0.0
        } else {
            python_round(100.0 * saved / baseline, 1)
        };
        let priced = self.priced();
        Ok(WorkspaceSavingsReport {
            report: SavingsReport {
                period_days,
                unit: if priced { "usd" } else { "relative" }.to_owned(),
                priced,
                requests: aggregate.n,
                estimated_requests: aggregate.estimated_n,
                tokens: aggregate.tokens,
                realized: python_round(aggregate.realized, 6),
                baseline,
                saved,
                saved_pct,
                by_route: aggregate
                    .by_route
                    .iter()
                    .map(|(name, stats)| (name.clone(), SavingsBreakdown::from(stats)))
                    .collect(),
                by_key: BTreeMap::new(),
            },
            first_observed_utc,
            last_observed_utc,
        })
    }

    /// Return all-time totals for metrics.
    pub fn totals(&self) -> Result<SavingsTotals, LedgerError> {
        let days = self.days.lock().map_err(|_| LedgerError::LockPoisoned)?;
        let realized = days.values().map(|day| day.realized).sum::<f64>();
        let baseline = days.values().map(|day| day.baseline).sum::<f64>();
        Ok(SavingsTotals {
            realized: python_round(realized, 6),
            baseline: python_round(baseline, 6),
            saved: python_round(baseline - realized, 6),
        })
    }

    /// Return realized spend for `day`, `month`, or all-time windows.
    pub fn spent(
        &self,
        window: &str,
        virtual_key: Option<&str>,
        today: UtcDate,
    ) -> Result<f64, LedgerError> {
        let days = self.days.lock().map_err(|_| LedgerError::LockPoisoned)?;
        let realized = |bucket: &DayBucket| {
            virtual_key.map_or(bucket.realized, |key| {
                bucket.by_key.get(key).map_or(0.0, |stats| stats.realized)
            })
        };
        let amount = match window {
            "day" => days.get(&today.to_string()).map_or(0.0, realized),
            "month" => {
                let prefix = today.month_prefix();
                days.iter()
                    .filter(|(date, _)| date.starts_with(&prefix))
                    .map(|(_, bucket)| realized(bucket))
                    .sum()
            }
            _ => days.values().map(realized).sum(),
        };
        Ok(python_round(amount, 6))
    }

    /// Return sorted ISO bucket keys for diagnostics and tests.
    pub fn day_keys(&self) -> Result<Vec<String>, LedgerError> {
        let days = self.days.lock().map_err(|_| LedgerError::LockPoisoned)?;
        Ok(days.keys().cloned().collect())
    }

    /// Serialize the current compatible ledger schema.
    pub fn to_value(&self) -> Result<Value, LedgerError> {
        #[derive(Serialize)]
        struct Snapshot<'a> {
            max_days: usize,
            priced: bool,
            days: &'a BTreeMap<String, DayBucket>,
        }

        let days = self.days.lock().map_err(|_| LedgerError::LockPoisoned)?;
        Ok(serde_json::to_value(Snapshot {
            max_days: self.max_days,
            priced: self.priced(),
            days: &days,
        })?)
    }

    /// Load an old, partial, or current ledger value.
    ///
    /// Invalid bucket fields fall back independently; malformed date keys are
    /// dropped so persisted corruption cannot make a later stats request fail.
    pub fn from_value(value: &Value) -> Result<Self, LedgerError> {
        let root = value.as_object().ok_or_else(|| {
            LedgerError::InvalidDate("ledger root must be a JSON object".to_owned())
        })?;
        let max_days = root
            .get("max_days")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(400);
        let priced = root.get("priced").and_then(Value::as_bool).unwrap_or(true);
        let mut parsed_days = BTreeMap::new();
        if let Some(days) = root.get("days").and_then(Value::as_object) {
            for (date, raw) in days {
                if UtcDate::parse(date).is_ok() {
                    if let Some(bucket) = raw.as_object() {
                        parsed_days.insert(date.clone(), coerce_bucket(bucket));
                    }
                }
            }
        }
        while parsed_days.len() > max_days {
            let _ = parsed_days.pop_first();
        }
        Ok(Self {
            max_days,
            priced: AtomicBool::new(priced),
            days: Mutex::new(parsed_days),
            persistence: Mutex::new(()),
        })
    }

    /// Atomically write a synchronized JSON snapshot beside its final path.
    pub fn save(&self, path: &Path) -> Result<(), LedgerError> {
        let _persistence = self
            .persistence
            .lock()
            .map_err(|_| LedgerError::LockPoisoned)?;
        let bytes = serde_json::to_vec(&serde_json::json!({
            "schema_version": LEDGER_SCHEMA_VERSION,
            "ledger": self.to_value()?,
        }))?;
        let temporary = temporary_path(path);
        let backup = backup_path(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| LedgerError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| LedgerError::Io {
                path: temporary.clone(),
                source,
            })?;
        file.write_all(&bytes).map_err(|source| LedgerError::Io {
            path: temporary.clone(),
            source,
        })?;
        file.sync_all().map_err(|source| LedgerError::Io {
            path: temporary.clone(),
            source,
        })?;
        if path.exists() {
            fs::copy(path, &backup).map_err(|source| LedgerError::Io {
                path: backup.clone(),
                source,
            })?;
        }
        fs::rename(&temporary, path).map_err(|source| LedgerError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if let Some(parent) = path.parent() {
            if let Ok(directory) = File::open(parent) {
                directory.sync_all().map_err(|source| LedgerError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
        }
        Ok(())
    }

    /// Read a persisted ledger snapshot.
    pub fn load(path: &Path) -> Result<Self, LedgerError> {
        let bytes = fs::read(path).map_err(|source| LedgerError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let value: Value = serde_json::from_slice(&bytes)?;
        let payload = value.get("ledger").unwrap_or(&value);
        Self::from_value(payload)
    }

    /// Load persisted state, recovering a corrupt primary from the last-good
    /// file without deleting the corrupt evidence. Missing state starts empty.
    pub fn load_resilient(
        path: &Path,
        max_days: usize,
        priced: bool,
    ) -> Result<(Self, LedgerLoad), LedgerError> {
        if !path.exists() {
            return Ok((Self::new(max_days, priced), LedgerLoad::New));
        }
        match Self::load(path) {
            Ok(ledger) => Ok((ledger, LedgerLoad::Primary)),
            Err(primary_error) => {
                let quarantine = quarantine_path(path);
                fs::rename(path, &quarantine).map_err(|source| LedgerError::Io {
                    path: quarantine.clone(),
                    source,
                })?;
                let backup = backup_path(path);
                if !backup.exists() {
                    return Err(primary_error);
                }
                let ledger = Self::load(&backup)?;
                fs::copy(&backup, path).map_err(|source| LedgerError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
                Ok((ledger, LedgerLoad::Recovered { quarantine }))
            }
        }
    }
}

impl Default for SavingsLedger {
    fn default() -> Self {
        Self::new(400, true)
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

fn backup_path(path: &Path) -> PathBuf {
    suffixed_path(path, ".last-good")
}

fn quarantine_path(path: &Path) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    suffixed_path(path, &format!(".corrupt-{stamp}"))
}

fn suffixed_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn coerce_bucket(raw: &serde_json::Map<String, Value>) -> DayBucket {
    DayBucket {
        n: compatible_u64(raw.get("n")),
        realized: compatible_f64(raw.get("realized")),
        baseline: compatible_f64(raw.get("baseline")),
        savings: compatible_f64(raw.get("savings")),
        tokens: compatible_u64(raw.get("tokens")),
        estimated_n: compatible_u64(raw.get("estimated_n")),
        by_route: coerce_stat_map(raw.get("by_route")),
        by_key: coerce_stat_map(raw.get("by_key")),
        by_workspace: coerce_workspace_map(raw.get("by_workspace")),
    }
}

fn coerce_workspace_map(value: Option<&Value>) -> BTreeMap<String, WorkspaceBucket> {
    value
        .and_then(Value::as_object)
        .map(|values| {
            values
                .iter()
                .filter_map(|(name, value)| {
                    value.as_object().map(|stats| {
                        (
                            name.clone(),
                            WorkspaceBucket {
                                n: compatible_u64(stats.get("n")),
                                realized: compatible_f64(stats.get("realized")),
                                baseline: compatible_f64(stats.get("baseline")),
                                savings: compatible_f64(stats.get("savings")),
                                tokens: compatible_u64(stats.get("tokens")),
                                estimated_n: compatible_u64(stats.get("estimated_n")),
                                by_route: coerce_stat_map(stats.get("by_route")),
                            },
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn coerce_stat_map(value: Option<&Value>) -> BTreeMap<String, LedgerStats> {
    value
        .and_then(Value::as_object)
        .map(|values| {
            values
                .iter()
                .filter_map(|(name, value)| {
                    value.as_object().map(|stats| {
                        (
                            name.clone(),
                            LedgerStats {
                                n: compatible_u64(stats.get("n")),
                                realized: compatible_f64(stats.get("realized")),
                                baseline: compatible_f64(stats.get("baseline")),
                                savings: compatible_f64(stats.get("savings")),
                                tokens: compatible_u64(stats.get("tokens")),
                            },
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn compatible_u64(value: Option<&Value>) -> u64 {
    value.and_then(Value::as_u64).unwrap_or(0)
}

fn compatible_f64(value: Option<&Value>) -> f64 {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn costs(entries: &[(&str, Option<f64>)]) -> IndexMap<String, Option<f64>> {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_owned(), *value))
            .collect()
    }

    #[test]
    fn estimate_matches_python_for_empty_ascii_and_unicode() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens(&"x".repeat(40)), 10);
        assert_eq!(estimate_tokens("😀😀😀😀"), 1);
    }

    #[test]
    fn real_costs_win_when_any_are_present() -> Result<(), PricingError> {
        let table = price_table(
            &costs(&[("local", Some(0.0)), ("cloud", Some(0.009))]),
            &["local".to_owned(), "cloud".to_owned()],
        )?;
        assert!(table.priced);
        assert_eq!(table.costs["local"], 0.0);
        assert_eq!(table.costs["cloud"], 0.009);
        Ok(())
    }

    #[test]
    fn relative_costs_span_the_tier_ladder() -> Result<(), PricingError> {
        let table = price_table(
            &costs(&[("local", None), ("mid", None), ("cloud", None)]),
            &["local".to_owned(), "mid".to_owned(), "cloud".to_owned()],
        )?;
        assert!(!table.priced);
        assert_eq!(
            table.costs.values().copied().collect::<Vec<_>>(),
            [0.2, 0.6, 1.0]
        );
        Ok(())
    }

    #[test]
    fn empty_table_remains_unpriced() -> Result<(), PricingError> {
        assert_eq!(
            price_table(&IndexMap::new(), &[])?,
            PriceTable {
                costs: IndexMap::new(),
                priced: false,
            }
        );
        Ok(())
    }

    #[test]
    fn table_fingerprint_is_order_independent_and_sensitive() -> Result<(), PricingError> {
        let a = [("local".to_owned(), 0.0), ("cloud".to_owned(), 0.009)]
            .into_iter()
            .collect();
        let b = [("cloud".to_owned(), 0.009), ("local".to_owned(), 0.0)]
            .into_iter()
            .collect();
        let c = [("local".to_owned(), 0.0), ("cloud".to_owned(), 0.01)]
            .into_iter()
            .collect();
        assert_eq!(table_version(&a)?, table_version(&b)?);
        assert_ne!(table_version(&a)?, table_version(&c)?);
        assert_eq!(table_version(&a)?, "e5701a6566b7");

        let unicode = [("lócal".to_owned(), 0.0), ("😀".to_owned(), 0.009)]
            .into_iter()
            .collect();
        assert_eq!(table_version(&unicode)?, "4fa038886566");
        Ok(())
    }

    #[test]
    fn usage_prefers_complete_upstream_counts() {
        assert_eq!(
            usage_tokens(
                &json!({"usage": {"prompt_tokens": 120, "completion_tokens": 30}}),
                "ignored",
                "",
            ),
            UsageTokens {
                prompt: 120,
                completion: 30,
                estimated: false,
            }
        );
    }

    #[test]
    fn usage_splits_total_when_completion_is_missing() {
        assert_eq!(
            usage_tokens(
                &json!({"usage": {"prompt_tokens": 100, "total_tokens": 130}}),
                "",
                "",
            ),
            UsageTokens {
                prompt: 100,
                completion: 30,
                estimated: false,
            }
        );
    }

    #[test]
    fn usage_estimates_when_absent() {
        assert_eq!(
            usage_tokens(&json!({}), &"x".repeat(40), &"y".repeat(80)),
            UsageTokens {
                prompt: 10,
                completion: 20,
                estimated: true,
            }
        );
    }

    #[test]
    fn turn_cost_uses_dearest_baseline() -> Result<(), PricingError> {
        let costs = [("local".to_owned(), 0.0), ("cloud".to_owned(), 0.009)]
            .into_iter()
            .collect();
        let cost = turn_cost("local", 1_000, 0, &costs, false, None)?;
        assert_eq!(cost.realized, 0.0);
        assert_eq!(cost.baseline, 0.009);
        assert_eq!(cost.savings, 0.009);
        Ok(())
    }

    #[test]
    fn explicit_baseline_and_negative_tokens_match_python() -> Result<(), PricingError> {
        let costs = [
            ("small".to_owned(), 0.001),
            ("mid".to_owned(), 0.005),
            ("large".to_owned(), 0.02),
        ]
        .into_iter()
        .collect();
        let cost = turn_cost("small", 2_000, -5, &costs, false, Some("mid"))?;
        assert_eq!(cost.baseline, 0.01);
        assert_eq!(cost.savings, 0.008);
        assert_eq!(cost.completion_tokens, 0);
        Ok(())
    }

    #[test]
    fn invalid_prices_are_rejected() {
        let bad = [("x".to_owned(), Some(f64::NAN))].into_iter().collect();
        assert!(price_table(&bad, &[]).is_err());
    }

    #[test]
    fn utc_date_validates_leap_days_and_orders_across_months() -> Result<(), LedgerError> {
        let leap = UtcDate::new(2024, 2, 29)?;
        let march = UtcDate::parse("2024-03-01")?;
        assert_eq!(leap.to_string(), "2024-02-29");
        assert_eq!(march.ordinal() - leap.ordinal(), 1);
        assert!(UtcDate::new(2023, 2, 29).is_err());
        assert!(UtcDate::parse("2024-2-01").is_err());
        Ok(())
    }

    #[test]
    fn ledger_records_period_route_and_key_attribution() -> Result<(), Box<dyn std::error::Error>> {
        let ledger = SavingsLedger::new(400, true);
        let costs = [("local".to_owned(), 0.0), ("cloud".to_owned(), 0.009)]
            .into_iter()
            .collect();
        let day = UtcDate::new(2026, 6, 23)?;
        let local = turn_cost("local", 1_000, 0, &costs, false, None)?;
        let cloud = turn_cost("cloud", 1_000, 0, &costs, true, None)?;
        ledger.record(&local, day, Some("team-a"))?;
        ledger.record(&cloud, day, Some("team-b"))?;

        let report = ledger.period(None, day)?;
        assert_eq!(report.requests, 2);
        assert_eq!(report.estimated_requests, 1);
        assert_eq!(report.unit, "usd");
        assert_eq!(report.realized, 0.009);
        assert_eq!(report.baseline, 0.018);
        assert_eq!(report.saved, 0.009);
        assert_eq!(report.saved_pct, 50.0);
        assert_eq!(report.by_route["local"].saved, 0.009);
        assert_eq!(report.by_key["team-b"].requests, 1);
        Ok(())
    }

    #[test]
    fn workspace_accounting_is_isolated_and_excludes_legacy_unattributed_turns()
    -> Result<(), Box<dyn std::error::Error>> {
        let ledger = SavingsLedger::new(400, true);
        let costs = [("local".to_owned(), 0.0), ("cloud".to_owned(), 0.01)]
            .into_iter()
            .collect();
        let day = UtcDate::new(2026, 8, 29)?;
        let local = turn_cost("local", 1_000, 0, &costs, false, None)?;
        let cloud = turn_cost("cloud", 2_000, 0, &costs, true, None)?;
        ledger.record(&cloud, day, Some("legacy-key"))?;
        ledger.record_attributed(&local, day, Some("key-a"), Some("project-a"))?;
        ledger.record_attributed(&cloud, day, Some("key-b"), Some("project-b"))?;

        let project_a = ledger.period_for_workspace("project-a", Some(30), day)?;
        assert_eq!(project_a.report.requests, 1);
        assert_eq!(project_a.report.estimated_requests, 0);
        assert_eq!(project_a.report.saved, 0.01);
        assert_eq!(project_a.report.by_route["local"].requests, 1);
        assert!(project_a.report.by_key.is_empty());
        assert_eq!(project_a.first_observed_utc.as_deref(), Some("2026-08-29"));
        assert_eq!(project_a.last_observed_utc.as_deref(), Some("2026-08-29"));

        let project_b = ledger.period_for_workspace("project-b", Some(30), day)?;
        assert_eq!(project_b.report.requests, 1);
        assert_eq!(project_b.report.estimated_requests, 1);
        assert_eq!(project_b.report.realized, 0.02);
        assert_eq!(ledger.period(None, day)?.requests, 3);
        Ok(())
    }

    #[test]
    fn ledger_price_mode_tracks_python_hot_reload_mutation() -> Result<(), LedgerError> {
        let ledger = SavingsLedger::default();
        let today = UtcDate::new(2026, 7, 11)?;
        assert!(ledger.period(None, today)?.priced);
        ledger.set_priced(false);
        let report = ledger.period(None, today)?;
        assert!(!report.priced);
        assert_eq!(report.unit, "relative");
        ledger.set_priced(true);
        assert_eq!(ledger.to_value()?["priced"], true);
        Ok(())
    }

    #[test]
    fn ledger_period_filters_by_real_calendar_days() -> Result<(), Box<dyn std::error::Error>> {
        let ledger = SavingsLedger::new(400, true);
        let costs = [("a".to_owned(), 1.0)].into_iter().collect();
        let cost = turn_cost("a", 1_000, 0, &costs, false, None)?;
        ledger.record(&cost, UtcDate::new(2026, 6, 1)?, None)?;
        ledger.record(&cost, UtcDate::new(2026, 6, 23)?, None)?;
        let today = UtcDate::new(2026, 6, 23)?;
        assert_eq!(ledger.period(Some(1), today)?.requests, 1);
        assert_eq!(ledger.period(Some(30), today)?.requests, 2);
        assert_eq!(ledger.period(None, today)?.requests, 2);
        Ok(())
    }

    #[test]
    fn ledger_prunes_oldest_iso_buckets() -> Result<(), Box<dyn std::error::Error>> {
        let ledger = SavingsLedger::new(2, true);
        let costs = [("a".to_owned(), 1.0)].into_iter().collect();
        let cost = turn_cost("a", 1_000, 0, &costs, false, None)?;
        for day in [1, 2, 3] {
            ledger.record(&cost, UtcDate::new(2026, 6, day)?, None)?;
        }
        assert_eq!(
            ledger.day_keys()?,
            ["2026-06-02".to_owned(), "2026-06-03".to_owned()]
        );
        Ok(())
    }

    #[test]
    fn old_partial_bucket_is_coerced_without_read_failure() -> Result<(), Box<dyn std::error::Error>>
    {
        let ledger = SavingsLedger::from_value(&json!({
            "priced": true,
            "days": {
                "2026-06-23": {
                    "n": 2,
                    "realized": 0.009,
                    "baseline": 0.018,
                    "savings": 0.009,
                    "tokens": 1000,
                    "by_route": {"local": {"n": 2, "savings": 0.009}}
                },
                "corrupt-date": {"n": 999}
            }
        }))?;
        let report = ledger.period(None, UtcDate::new(2026, 6, 23)?)?;
        assert_eq!(report.requests, 2);
        assert_eq!(report.estimated_requests, 0);
        assert_eq!(report.by_route["local"].saved, 0.009);
        assert_eq!(ledger.day_keys()?, ["2026-06-23"]);
        Ok(())
    }

    #[test]
    fn ledger_spend_windows_and_totals_match_python() -> Result<(), Box<dyn std::error::Error>> {
        let ledger = SavingsLedger::new(400, true);
        let costs = [("local".to_owned(), 0.0), ("cloud".to_owned(), 0.01)]
            .into_iter()
            .collect();
        let cloud = turn_cost("cloud", 1_000, 0, &costs, false, None)?;
        let local = turn_cost("local", 2_000, 0, &costs, false, None)?;
        ledger.record(&cloud, UtcDate::new(2026, 6, 10)?, Some("team-a"))?;
        ledger.record(&cloud, UtcDate::new(2026, 6, 23)?, Some("team-b"))?;
        ledger.record(&cloud, UtcDate::new(2026, 5, 31)?, Some("team-a"))?;
        ledger.record(&local, UtcDate::new(2026, 6, 23)?, None)?;
        let today = UtcDate::new(2026, 6, 23)?;
        assert_eq!(ledger.spent("day", None, today)?, 0.01);
        assert_eq!(ledger.spent("month", None, today)?, 0.02);
        assert_eq!(ledger.spent("all", None, today)?, 0.03);
        assert_eq!(ledger.spent("all", Some("team-a"), today)?, 0.02);
        assert_eq!(
            ledger.totals()?,
            SavingsTotals {
                realized: 0.03,
                baseline: 0.05,
                saved: 0.02,
            }
        );
        Ok(())
    }

    #[test]
    fn save_load_round_trip_uses_atomic_sidecar() -> Result<(), Box<dyn std::error::Error>> {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_PATH: AtomicU64 = AtomicU64::new(0);
        let suffix = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "wayfinder-savings-{}-{suffix}.json",
            std::process::id()
        ));
        let ledger = SavingsLedger::new(400, true);
        let costs = [("cloud".to_owned(), 0.009)].into_iter().collect();
        let cost = turn_cost("cloud", 1_000, 500, &costs, false, None)?;
        let day = UtcDate::new(2026, 6, 23)?;
        ledger.record(&cost, day, None)?;
        ledger.save(&path)?;
        let loaded = SavingsLedger::load(&path)?;
        assert_eq!(loaded.period(None, day)?, ledger.period(None, day)?);
        assert!(!temporary_path(&path).exists());
        let _ = fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn resilient_load_quarantines_corruption_and_restores_last_good()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = std::env::temp_dir().join(format!(
            "wayfinder-ledger-recovery-{}-{}",
            std::process::id(),
            UtcDate::new(2026, 7, 12)?.ordinal()
        ));
        let path = directory.join("savings.json");
        let ledger = SavingsLedger::new(400, true);
        let costs = [("cloud".to_owned(), 0.01)].into_iter().collect();
        let cost = turn_cost("cloud", 1_000, 0, &costs, false, None)?;
        let day = UtcDate::new(2026, 7, 12)?;
        ledger.record(&cost, day, None)?;
        ledger.save(&path)?;
        // A second save establishes the independently readable last-good file.
        ledger.save(&path)?;
        fs::write(&path, b"{not-json")?;

        let (recovered, outcome) = SavingsLedger::load_resilient(&path, 400, true)?;
        let LedgerLoad::Recovered { quarantine } = outcome else {
            return Err("expected recovery".into());
        };
        assert!(quarantine.exists());
        assert_eq!(recovered.period(None, day)?.requests, 1);
        assert_eq!(SavingsLedger::load(&path)?.period(None, day)?.requests, 1);
        let _ = fs::remove_dir_all(directory);
        Ok(())
    }

    #[test]
    fn concurrent_saves_leave_a_readable_versioned_snapshot()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::sync::Arc;
        use std::thread;

        let directory = std::env::temp_dir().join(format!(
            "wayfinder-ledger-concurrent-{}",
            std::process::id()
        ));
        let path = directory.join("savings.json");
        let ledger = Arc::new(SavingsLedger::new(400, true));
        let threads = (0..8)
            .map(|_| {
                let ledger = Arc::clone(&ledger);
                let path = path.clone();
                thread::spawn(move || ledger.save(&path))
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().map_err(|_| "save thread panicked")??;
        }
        let value: Value = serde_json::from_slice(&fs::read(&path)?)?;
        assert_eq!(value["schema_version"], LEDGER_SCHEMA_VERSION);
        let _ = fs::remove_dir_all(directory);
        Ok(())
    }
}
