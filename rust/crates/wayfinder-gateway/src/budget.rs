//! Read-only spend-budget policy over the shared realized-cost ledger.
//!
//! Budget checks never score prompts or reserve speculative cost. They inspect
//! already-realized spend immediately before routing and either preserve the
//! route, request a cheapest-tier degrade, or block delivery.

use std::collections::BTreeMap;

use indexmap::IndexMap;
use wayfinder_config::gateway::{Budget, GatewayConfig};
use wayfinder_routing_core::Tier;
use wayfinder_service::pricing::{LedgerError, SavingsLedger, SpendScope, UtcDate};

use crate::state::{SharedBudgetScope, StateBackend, StateBackendError};

/// Result of applying every budget relevant to one authenticated request.
#[derive(Clone, Debug, PartialEq)]
pub enum BudgetDecision {
    /// No configured cap is exhausted, or the active price table is unpriced.
    Allow,
    /// At least one cap is exhausted and delivery should use the cheapest
    /// route (see [`degrade_target`]).
    Degrade,
    /// A hard cap is exhausted and online delivery must stop.
    Block {
        /// Configured accounting window used in the error message.
        window: String,
        /// Configured spend ceiling used in the error message.
        limit: f64,
    },
}

/// Immutable gateway-wide, workspace, and per-key spend caps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BudgetPolicy {
    global: Option<Budget>,
    by_workspace: BTreeMap<String, Budget>,
    by_key: BTreeMap<String, Budget>,
}

impl BudgetPolicy {
    /// Copy validated budget configuration without retaining credentials.
    #[must_use]
    pub fn from_gateway_config(config: &GatewayConfig) -> Self {
        Self {
            global: config.budget.clone(),
            by_workspace: config
                .workspaces
                .iter()
                .filter_map(|(id, workspace)| {
                    workspace.budget.clone().map(|budget| (id.clone(), budget))
                })
                .collect(),
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
        self.global.is_some() || !self.by_workspace.is_empty() || !self.by_key.is_empty()
    }

    /// Return the configured dimensions that participate in a shared spend
    /// reservation. The order is stable so Redis scripts and tests remain
    /// deterministic.
    #[must_use]
    pub fn shared_scopes(
        &self,
        workspace_id: Option<&str>,
        key_id: Option<&str>,
        offline: bool,
    ) -> Vec<SharedBudgetScope> {
        let mut scopes = Vec::new();
        if let Some(budget) = &self.global {
            scopes.push(shared_scope("global", budget, offline));
        }
        if let Some((workspace_id, budget)) =
            workspace_id.and_then(|id| self.by_workspace.get(id).map(|budget| (id, budget)))
        {
            scopes.push(shared_scope(
                &format!("workspace:{workspace_id}"),
                budget,
                offline,
            ));
        }
        if let Some((key_id, budget)) =
            key_id.and_then(|id| self.by_key.get(id).map(|budget| (id, budget)))
        {
            scopes.push(shared_scope(&format!("key:{key_id}"), budget, offline));
        }
        scopes
    }

    /// Evaluate global, workspace, and key caps for one authenticated request.
    ///
    /// A hard online block wins over a degrade. Offline requests soften every
    /// hard block to a degrade because cheapest/local delivery incurs no dearer
    /// network spend. Unpriced relative-unit tables bypass dollar budgets.
    pub fn evaluate_for(
        &self,
        ledger: &SavingsLedger,
        priced: bool,
        workspace_id: Option<&str>,
        key_id: Option<&str>,
        today: UtcDate,
        offline: bool,
    ) -> Result<BudgetDecision, LedgerError> {
        if !priced {
            return Ok(BudgetDecision::Allow);
        }

        let mut degraded = false;
        if let Some(budget) = &self.global {
            match evaluate_one(ledger, budget, SpendScope::Global, today, offline)? {
                BudgetDecision::Allow => {}
                BudgetDecision::Degrade => degraded = true,
                blocked @ BudgetDecision::Block { .. } => return Ok(blocked),
            }
        }
        if let Some((workspace_id, budget)) =
            workspace_id.and_then(|id| self.by_workspace.get(id).map(|budget| (id, budget)))
        {
            match evaluate_one(
                ledger,
                budget,
                SpendScope::Workspace(workspace_id),
                today,
                offline,
            )? {
                BudgetDecision::Allow => {}
                BudgetDecision::Degrade => degraded = true,
                blocked @ BudgetDecision::Block { .. } => return Ok(blocked),
            }
        }
        if let Some((key_id, budget)) =
            key_id.and_then(|key_id| self.by_key.get(key_id).map(|budget| (key_id, budget)))
        {
            match evaluate_one(ledger, budget, SpendScope::Key(key_id), today, offline)? {
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

    /// Evaluate shared global, workspace, and key caps against the fleet-wide
    /// realized-cost ledger.
    ///
    /// This path is used only when the configured state backend can provide
    /// shared accounting. Callers retain the local-ledger fallback when the
    /// backend is unavailable.
    pub async fn evaluate_shared_for(
        &self,
        backend: &dyn StateBackend,
        priced: bool,
        workspace_id: Option<&str>,
        key_id: Option<&str>,
        today: UtcDate,
        offline: bool,
    ) -> Result<BudgetDecision, StateBackendError> {
        if !priced {
            return Ok(BudgetDecision::Allow);
        }
        let date = today.to_string();
        let mut degraded = false;
        if let Some(budget) = &self.global {
            match evaluate_shared_one(backend, budget, "global", &date, offline).await? {
                BudgetDecision::Allow => {}
                BudgetDecision::Degrade => degraded = true,
                blocked @ BudgetDecision::Block { .. } => return Ok(blocked),
            }
        }
        if let Some((workspace_id, budget)) =
            workspace_id.and_then(|id| self.by_workspace.get(id).map(|budget| (id, budget)))
        {
            let scope = format!("workspace:{workspace_id}");
            match evaluate_shared_one(backend, budget, &scope, &date, offline).await? {
                BudgetDecision::Allow => {}
                BudgetDecision::Degrade => degraded = true,
                blocked @ BudgetDecision::Block { .. } => return Ok(blocked),
            }
        }
        if let Some((key_id, budget)) =
            key_id.and_then(|key_id| self.by_key.get(key_id).map(|budget| (key_id, budget)))
        {
            let scope = format!("key:{key_id}");
            match evaluate_shared_one(backend, budget, &scope, &date, offline).await? {
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

async fn evaluate_shared_one(
    backend: &dyn StateBackend,
    budget: &Budget,
    scope: &str,
    date: &str,
    offline: bool,
) -> Result<BudgetDecision, StateBackendError> {
    let spent = backend.spend(&budget.window, scope, date).await?.realized;
    Ok(evaluate_spent(spent, budget, offline))
}

fn shared_scope(scope: &str, budget: &Budget, offline: bool) -> SharedBudgetScope {
    SharedBudgetScope {
        scope: scope.to_owned(),
        window: budget.window.clone(),
        limit: budget.limit,
        blocks: budget.on_breach == "block" && !offline,
    }
}

fn evaluate_spent(spent: f64, budget: &Budget, offline: bool) -> BudgetDecision {
    if spent < budget.limit {
        return BudgetDecision::Allow;
    }
    if budget.on_breach == "block" && !offline {
        return BudgetDecision::Block {
            window: budget.window.clone(),
            limit: budget.limit,
        };
    }
    BudgetDecision::Degrade
}

fn evaluate_one(
    ledger: &SavingsLedger,
    budget: &Budget,
    scope: SpendScope<'_>,
    today: UtcDate,
    offline: bool,
) -> Result<BudgetDecision, LedgerError> {
    let spent = ledger.spent(&budget.window, scope, today)?;
    Ok(evaluate_spent(spent, budget, offline))
}

/// Choose the route a budget degrade switches to, or `None` when the budget
/// cannot lower the route.
///
/// In tier mode (`tiers` is `Some`) the target is the lowest-`min_score` tier.
/// In classifier mode (`tiers` is `None`, `models` is `Some`) the target is the
/// cheapest classifier model present in `costs`; models without a configured
/// price are never candidates, and when `allowed` is `Some` only models in that
/// key allowlist are candidates. Equal-cost candidates resolve to the first in
/// configured classifier order. A classifier target is returned only when it
/// differs from `chosen` and `chosen` is unpriced or strictly dearer.
#[must_use]
pub fn degrade_target(
    tiers: Option<&[Tier]>,
    models: Option<&[String]>,
    costs: &IndexMap<String, f64>,
    allowed: Option<&[String]>,
    chosen: &str,
) -> Option<String> {
    if let Some(tiers) = tiers {
        return tiers
            .iter()
            .min_by(|left, right| left.min_score.total_cmp(&right.min_score))
            .map(|tier| tier.model.clone())
            .filter(|target| target != chosen);
    }
    let mut cheapest: Option<(&String, f64)> = None;
    for model in models? {
        if allowed.is_some_and(|allowed| !allowed.contains(model)) {
            continue;
        }
        let Some(cost) = costs.get(model).copied() else {
            continue;
        };
        if cheapest.is_none_or(|(_, best)| cost < best) {
            cheapest = Some((model, cost));
        }
    }
    let (target, target_cost) = cheapest?;
    if target == chosen {
        return None;
    }
    match costs.get(chosen) {
        Some(chosen_cost) if target_cost >= *chosen_cost => None,
        _ => Some(target.clone()),
    }
}

/// Format a validated finite budget limit like Python's JSON-compatible float text.
#[must_use]
pub fn format_limit(limit: f64) -> String {
    serde_json::to_string(&limit).unwrap_or_else(|_| limit.to_string())
}

#[cfg(test)]
mod tests {
    use wayfinder_config::gateway::{VirtualKey, Workspace};
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
            policy.evaluate_for(&ledger, true, None, Some("team-a"), today, false)?,
            BudgetDecision::Block {
                window: "day".to_owned(),
                limit: 1.0
            }
        );
        assert_eq!(
            policy.evaluate_for(&ledger, true, None, Some("team-a"), today, true)?,
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
            policy.evaluate_for(&ledger, true, None, Some("team-a"), today, false)?,
            BudgetDecision::Allow
        );
        assert_eq!(
            policy.evaluate_for(&ledger, false, None, Some("team-b"), today, false)?,
            BudgetDecision::Allow
        );
        assert_eq!(format_limit(2.0), "2.0");
        Ok(())
    }

    fn workspace_ledger(
        key: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<(SavingsLedger, UtcDate), LedgerError> {
        let today = UtcDate::new(2026, 7, 11)?;
        let ledger = SavingsLedger::new(400, true);
        let cost = TurnCost {
            route: "cloud".to_owned(),
            realized: 100.0,
            baseline: 100.0,
            savings: 0.0,
            prompt_tokens: 1_000,
            completion_tokens: 0,
            estimated: false,
        };
        ledger.record_attributed(&cost, today, key, workspace)?;
        Ok((ledger, today))
    }

    fn workspace_policy(workspace_id: &str, on_breach: &str) -> BudgetPolicy {
        let mut config = GatewayConfig::default();
        config.workspaces.insert(
            workspace_id.to_owned(),
            Workspace {
                budget: Some(budget(1.0, on_breach)),
                ..Workspace::default()
            },
        );
        BudgetPolicy::from_gateway_config(&config)
    }

    #[test]
    fn workspace_budget_blocks_on_workspace_spend() -> Result<(), LedgerError> {
        let (ledger, today) = workspace_ledger(Some("alice-key"), Some("team-ws"))?;
        let policy = workspace_policy("team-ws", "block");
        assert_eq!(
            policy.evaluate_for(
                &ledger,
                true,
                Some("team-ws"),
                Some("alice-key"),
                today,
                false
            )?,
            BudgetDecision::Block {
                window: "day".to_owned(),
                limit: 1.0
            }
        );
        Ok(())
    }

    #[test]
    fn other_workspace_spend_does_not_trip_a_workspace_budget() -> Result<(), LedgerError> {
        let (ledger, today) = workspace_ledger(Some("bob-key"), Some("other-ws"))?;
        let policy = workspace_policy("team-ws", "block");
        assert_eq!(
            policy.evaluate_for(
                &ledger,
                true,
                Some("team-ws"),
                Some("alice-key"),
                today,
                false
            )?,
            BudgetDecision::Allow
        );
        Ok(())
    }

    #[test]
    fn key_and_workspace_with_the_same_id_do_not_share_spend() -> Result<(), LedgerError> {
        // Spend lands on a key named like the budgeted workspace, but in an
        // unrelated workspace, so the workspace cap must not see it.
        let (ledger, today) = workspace_ledger(Some("team-ws"), Some("other-ws"))?;
        let policy = workspace_policy("team-ws", "block");
        assert_eq!(
            policy.evaluate_for(&ledger, true, Some("team-ws"), None, today, false)?,
            BudgetDecision::Allow
        );

        // Conversely, workspace spend must not trip a key budget that merely
        // shares the workspace's id.
        let (ledger, today) = workspace_ledger(None, Some("team-a"))?;
        let mut config = GatewayConfig::default();
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
            policy.evaluate_for(&ledger, true, None, Some("team-a"), today, false)?,
            BudgetDecision::Allow
        );
        Ok(())
    }

    fn names(models: &[&str]) -> Vec<String> {
        models.iter().map(|model| (*model).to_owned()).collect()
    }

    fn costs(entries: &[(&str, f64)]) -> IndexMap<String, f64> {
        entries
            .iter()
            .map(|(model, cost)| ((*model).to_owned(), *cost))
            .collect()
    }

    #[test]
    fn tier_mode_degrades_to_the_lowest_tier_only_when_it_changes_the_route() {
        let tiers = vec![
            Tier::new(0.6, "cloud"),
            Tier::new(0.0, "local"),
            Tier::new(0.3, "mid"),
        ];
        // Tier mode ignores prices: the ladder alone defines the target.
        let table = costs(&[("local", 9.0), ("cloud", 1.0)]);
        assert_eq!(
            degrade_target(Some(&tiers), None, &table, None, "cloud"),
            Some("local".to_owned())
        );
        assert_eq!(
            degrade_target(Some(&tiers), None, &table, None, "local"),
            None
        );
    }

    #[test]
    fn classifier_mode_picks_the_cheapest_priced_model() {
        let models = names(&["large", "small", "medium"]);
        let table = costs(&[("large", 3.0), ("small", 0.5), ("medium", 1.0)]);
        assert_eq!(
            degrade_target(None, Some(&models), &table, None, "large"),
            Some("small".to_owned())
        );
        assert_eq!(
            degrade_target(None, Some(&models), &table, None, "small"),
            None
        );
    }

    #[test]
    fn classifier_mode_ignores_unpriced_models() {
        let models = names(&["free-looking", "large", "medium"]);
        let table = costs(&[("large", 3.0), ("medium", 1.0)]);
        assert_eq!(
            degrade_target(None, Some(&models), &table, None, "large"),
            Some("medium".to_owned())
        );
    }

    #[test]
    fn classifier_mode_breaks_cost_ties_by_configured_order() {
        let models = names(&["large", "zeta", "alpha"]);
        let table = costs(&[("alpha", 0.5), ("large", 3.0), ("zeta", 0.5)]);
        assert_eq!(
            degrade_target(None, Some(&models), &table, None, "large"),
            Some("zeta".to_owned())
        );
    }

    #[test]
    fn classifier_mode_does_not_switch_to_an_equal_cost_model() {
        let models = names(&["first", "second"]);
        let table = costs(&[("first", 1.0), ("second", 1.0)]);
        assert_eq!(
            degrade_target(None, Some(&models), &table, None, "second"),
            None
        );
    }

    #[test]
    fn classifier_mode_switches_away_from_an_unpriced_choice() {
        let models = names(&["mystery", "priced"]);
        let table = costs(&[("priced", 2.0)]);
        assert_eq!(
            degrade_target(None, Some(&models), &table, None, "mystery"),
            Some("priced".to_owned())
        );
    }

    #[test]
    fn classifier_mode_only_considers_key_allowed_models() {
        let models = names(&["large", "small", "medium"]);
        let table = costs(&[("large", 3.0), ("small", 0.5), ("medium", 1.0)]);
        let allowed = names(&["large", "medium"]);
        assert_eq!(
            degrade_target(None, Some(&models), &table, Some(&allowed), "large"),
            Some("medium".to_owned())
        );
    }

    #[test]
    fn no_candidates_means_no_degrade() {
        let table = costs(&[("large", 3.0)]);
        assert_eq!(degrade_target(None, None, &table, None, "large"), None);
        let models = names(&["a", "b"]);
        assert_eq!(degrade_target(None, Some(&models), &table, None, "a"), None);
        assert_eq!(degrade_target(Some(&[]), None, &table, None, "large"), None);
    }

    #[test]
    fn offline_softens_a_workspace_block_to_degrade() -> Result<(), LedgerError> {
        let (ledger, today) = workspace_ledger(Some("alice-key"), Some("team-ws"))?;
        let policy = workspace_policy("team-ws", "block");
        assert_eq!(
            policy.evaluate_for(
                &ledger,
                true,
                Some("team-ws"),
                Some("alice-key"),
                today,
                true
            )?,
            BudgetDecision::Degrade
        );
        Ok(())
    }
}
