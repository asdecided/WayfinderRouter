//! Read-only spend-budget policy over the shared realized-cost ledger.
//!
//! Budget checks never score prompts or reserve speculative cost. They inspect
//! already-realized spend immediately before routing and either preserve the
//! route, request a cheapest-tier degrade, or block delivery.

use std::collections::BTreeMap;

use wayfinder_config::gateway::{Budget, GatewayConfig};
use wayfinder_service::pricing::{LedgerError, SavingsLedger, UtcDate};

/// Result of applying every budget relevant to one authenticated request.
#[derive(Clone, Debug, PartialEq)]
pub enum BudgetDecision {
    /// No configured cap is exhausted, or the active price table is unpriced.
    Allow,
    /// At least one cap is exhausted and delivery should use the cheapest tier.
    Degrade,
    /// A hard cap is exhausted and online delivery must stop.
    Block {
        /// Configured accounting window used in the error message.
        window: String,
        /// Configured spend ceiling used in the error message.
        limit: f64,
    },
}

/// Immutable gateway-wide and per-key spend caps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BudgetPolicy {
    global: Option<Budget>,
    by_key: BTreeMap<String, Budget>,
}

impl BudgetPolicy {
    /// Copy validated budget configuration without retaining credentials.
    #[must_use]
    pub fn from_gateway_config(config: &GatewayConfig) -> Self {
        Self {
            global: config.budget.clone(),
            by_key: config
                .keys
                .iter()
                .filter_map(|(id, key)| key.budget.clone().map(|budget| (id.clone(), budget)))
                .collect(),
        }
    }

    /// Whether any request can be affected by this policy.
    #[must_use]
    pub fn active(&self) -> bool {
        self.global.is_some() || !self.by_key.is_empty()
    }

    /// Evaluate realized spend for the global cap and the authenticated key's cap.
    ///
    /// A hard online block wins over a degrade. Offline requests soften every
    /// hard block to a degrade because cheapest/local delivery incurs no dearer
    /// network spend. Unpriced relative-unit tables bypass dollar budgets.
    pub fn evaluate(
        &self,
        ledger: &SavingsLedger,
        priced: bool,
        key_id: Option<&str>,
        today: UtcDate,
        offline: bool,
    ) -> Result<BudgetDecision, LedgerError> {
        if !priced {
            return Ok(BudgetDecision::Allow);
        }

        let mut degraded = false;
        if let Some(budget) = &self.global {
            match evaluate_one(ledger, budget, None, today, offline)? {
                BudgetDecision::Allow => {}
                BudgetDecision::Degrade => degraded = true,
                blocked @ BudgetDecision::Block { .. } => return Ok(blocked),
            }
        }
        if let Some((key_id, budget)) =
            key_id.and_then(|key_id| self.by_key.get(key_id).map(|budget| (key_id, budget)))
        {
            match evaluate_one(ledger, budget, Some(key_id), today, offline)? {
                BudgetDecision::Allow => {}
                BudgetDecision::Degrade => degraded = true,
                blocked @ BudgetDecision::Block { .. } => return Ok(blocked),
            }
        }
        Ok(if degraded {
            BudgetDecision::Degrade
        } else {
            BudgetDecision::Allow
        })
    }
}

fn evaluate_one(
    ledger: &SavingsLedger,
    budget: &Budget,
    key_id: Option<&str>,
    today: UtcDate,
    offline: bool,
) -> Result<BudgetDecision, LedgerError> {
    if ledger.spent(&budget.window, key_id, today)? < budget.limit {
        return Ok(BudgetDecision::Allow);
    }
    if budget.on_breach == "block" && !offline {
        return Ok(BudgetDecision::Block {
            window: budget.window.clone(),
            limit: budget.limit,
        });
    }
    Ok(BudgetDecision::Degrade)
}

/// Format a validated finite budget limit like Python's JSON-compatible float text.
#[must_use]
pub fn format_limit(limit: f64) -> String {
    serde_json::to_string(&limit).unwrap_or_else(|_| limit.to_string())
}

#[cfg(test)]
mod tests {
    use wayfinder_config::gateway::VirtualKey;
    use wayfinder_service::pricing::TurnCost;

    use super::*;

    fn budget(limit: f64, on_breach: &str) -> Budget {
        Budget {
            limit,
            window: "day".to_owned(),
            on_breach: on_breach.to_owned(),
        }
    }

    fn seeded_ledger(key: Option<&str>) -> Result<(SavingsLedger, UtcDate), LedgerError> {
        let today = UtcDate::new(2026, 7, 11)?;
        let ledger = SavingsLedger::new(400, true);
        let cost = TurnCost {
            route: "cloud".to_owned(),
            realized: 1.0,
            baseline: 1.0,
            savings: 0.0,
            prompt_tokens: 1_000,
            completion_tokens: 0,
            estimated: false,
        };
        ledger.record(&cost, today, key)?;
        Ok((ledger, today))
    }

    #[test]
    fn hard_block_wins_and_offline_softens_it() -> Result<(), LedgerError> {
        let (ledger, today) = seeded_ledger(Some("team-a"))?;
        let mut config = GatewayConfig {
            budget: Some(budget(0.5, "degrade")),
            ..GatewayConfig::default()
        };
        config.keys.insert(
            "team-a".to_owned(),
            VirtualKey {
                hash: "0".repeat(64),
                tags: Vec::new(),
                workspace: None,
                budget: Some(budget(1.0, "block")),
                rate_limit: None,
                models: Vec::new(),
            },
        );
        let policy = BudgetPolicy::from_gateway_config(&config);
        assert_eq!(
            policy.evaluate(&ledger, true, Some("team-a"), today, false)?,
            BudgetDecision::Block {
                window: "day".to_owned(),
                limit: 1.0
            }
        );
        assert_eq!(
            policy.evaluate(&ledger, true, Some("team-a"), today, true)?,
            BudgetDecision::Degrade
        );
        Ok(())
    }

    #[test]
    fn unpriced_and_other_key_spend_do_not_trip_a_key_budget() -> Result<(), LedgerError> {
        let (ledger, today) = seeded_ledger(Some("team-b"))?;
        let mut config = GatewayConfig::default();
        config.keys.insert(
            "team-a".to_owned(),
            VirtualKey {
                hash: "0".repeat(64),
                tags: Vec::new(),
                workspace: None,
                budget: Some(budget(0.5, "block")),
                rate_limit: None,
                models: Vec::new(),
            },
        );
        let policy = BudgetPolicy::from_gateway_config(&config);
        assert_eq!(
            policy.evaluate(&ledger, true, Some("team-a"), today, false)?,
            BudgetDecision::Allow
        );
        assert_eq!(
            policy.evaluate(&ledger, false, Some("team-b"), today, false)?,
            BudgetDecision::Allow
        );
        assert_eq!(format_limit(2.0), "2.0");
        Ok(())
    }
}
