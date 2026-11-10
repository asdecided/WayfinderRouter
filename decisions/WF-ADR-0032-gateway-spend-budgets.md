---
schema_version: 1
id: WF-ADR-0032
type: decision
tags: [gateway, budget, cost, degrade, invocation, reliability]
---

# WF-ADR-0032: Gateway Spend Budgets (degrade-by-default, invocation-layer)

## Status

Accepted

## Category

Technical

## Context

The gateway already accounts for what each routed turn costs versus an always-frontier
baseline and persists it as a per-period savings report (WF-DESIGN-0007). What it cannot
yet do is *act* on that number: a runaway loop, a misbehaving agent, or simply a busy day
can spend without bound. WF-ROADMAP-0006 (item 6) calls for a **spend cap** — and crucially,
for the cap to **degrade to the cheaper tier** rather than hard-fail, reusing the `degrade`
primitive built for failover (WF-ADR-0031, rule 7). A budget that keeps you working at lower
cost is more on-brand for a *router* than one that simply turns the gateway off.

The same constraint as failover frames this:

- **The deterministic core is sacred (WF-ADR-0001).** The scored decision — "this prompt is
  tier T" — is computed once, offline, with no model call. A budget must change only *which
  tier a request is delivered to*, never *how the prompt was scored*. It is not a second router.
- **Spend is only real when costs are real.** The price table falls back to relative units
  (cheapest 0.2 .. dearest 1.0) when no `cost_per_1k` is configured (WF-DESIGN-0007). A
  dollar cap over relative units is meaningless, so a budget must be a no-op there.

## Decision

1. **A budget is an invocation-layer cap on realized spend; the scored decision is never
   recomputed.** For a given prompt and threshold the score is identical with or without a
   budget. Reaffirms WF-ADR-0001.

2. **Spend is read from the savings ledger's realized cost** (WF-DESIGN-0007) over a
   configurable **window**: `day` (today's UTC bucket), `month` (current calendar month), or
   `all` (all-time). No new accounting — the cap reads the number the gateway already keeps.

3. **On breach, behaviour is governed by an explicit `on_breach`, default `degrade`:**
   - **`degrade` (default)** — route the request to the **cheapest tier** (the lowest rung of
     the score ladder), the same downward step `failover = "degrade"` takes. Keeps serving and
     **never raises cost**. A no-op if the chosen tier is already the cheapest, or in classifier
     mode (no tier ladder to descend).
   - **`block`** — refuse the request with **HTTP 402** (`wayfinder_router_budget_exhausted`),
     for callers who would rather stop than silently drop to a weaker model. No upstream is called.

4. **The budget overrides the *route*, not just delivery** — and this is the deliberate
   distinction from failover-degrade. Failover (WF-ADR-0031) keeps the scored tier as the
   *decision* and changes only the *served* endpoint on upstream failure
   (`x-wayfinder-router-served-by`). A budget-degrade changes the route the gateway *chooses*
   up front, exactly as a pin or threshold override does: the reported `mode` becomes
   `budget-degraded` and `x-wayfinder-router-model` reflects the cheaper tier — while the
   `score` header still carries the true, unchanged complexity score. The cap reshapes routing;
   it does not relabel the score.

5. **A budget enforces only when the price table is real (`priced`).** With no `cost_per_1k`
   configured the figures are relative units, so the cap is skipped entirely. A dollar budget
   is for a gateway with dollar costs.

6. **A budget is never silent.** A breach adds `x-wayfinder-router-budget: degraded` (or
   `blocked`) to the response, alongside the existing decision headers, so the cap is
   observable rather than a mysterious tier drop. The realized turn is still costed and
   recorded against the tier that actually served.

7. **Configured per-gateway** under `[gateway.budget]` (`limit`, `window`, `on_breach`);
   absent means no cap. `limit` must be positive; `window` ∈ {day, month, all}; `on_breach`
   ∈ {degrade, block}.

## Amendment (2026-11-10 — classifier-mode degrade and an honest budget header)

Rules 3 and 6 are refined; where this amendment conflicts with them, it supersedes them:

- **Classifier-mode degrade target.** A classifier has no tier ladder, so a degrade is no
  longer a no-op there: it routes to the **cheapest priced model** among the classifier's
  configured models, using the active price table. A classifier model with no configured
  `cost_per_1k` is never a candidate (the gateway never degrades to a model whose cost is
  unknown), and when the authenticated key has a `models` allowlist only allowed models are
  candidates. Equal-cost candidates resolve to the first in the configured
  `routing.classifier.models` order. The switch happens only when the target differs from the
  chosen model and the chosen model is unpriced or strictly dearer; an equal-cost model is not
  a downgrade. Tier mode keeps rule 3's target (the lowest rung of the ladder).
- **The header reports what the budget did.** `x-wayfinder-router-budget: degraded` now means
  the budget actually changed the route (`mode: budget-degraded`). When a degrade budget is
  exhausted but the route is unchanged — already on the cheapest route, no cheaper priced
  classifier model, no candidate at all, a key allowlist clamping the request back to its
  original route, or an offline request (offline delivery already uses the cheapest local
  tier, WF-ADR-0039) — the header is the new value **`exhausted`**. `blocked` is unchanged.
  This applies to tier and classifier mode alike.

## Consequences

- **Cost safety without a kill-switch**: the default keeps the gateway useful through a spend
  spike by serving the cheaper arm, which is what a router is *for* — `block` remains for those
  who want a hard stop.
- **Reuses the failover `degrade` primitive** (WF-ADR-0031): no second mechanism for "serve the
  cheaper tier," just a second trigger for it.
- **Determinism is preserved and testable**: assert the `score` header is unchanged across a
  budget-degraded request and an under-budget one for the same prompt.
- **Risk — a degrade returns a weaker answer.** Mitigated by being surfaced in headers/`mode`
  (never silent) and by `block` as the strict alternative.
- **Limitation — spend is per process** (the ledger is in-memory, persisted best-effort), so a
  multi-process deployment caps per worker, not globally; a shared store is a later option,
  consistent with the breaker's limitation in WF-ADR-0031.
- **Limitation — the window is wall-clock UTC**, matching the savings ledger's daily buckets;
  no per-key or per-caller budgets in v1 (a natural follow-up).
- Fully testable with a fake upstream and a seeded ledger — no network, no keys.

## Alternatives Considered

- **`block` as the default** — simpler, but turns a cost spike into an outage and throws away
  the router's headroom; a cheaper answer beats no answer for most callers. Made opt-in.
- **A budget that re-scores or picks a "cheap enough" tier by estimated cost** — a second
  routing policy on the decision path; breaks WF-ADR-0001. The cap degrades to the *cheapest*
  rung deterministically instead.
- **Enforcing a budget over relative units** — caps a number that isn't money; rejected in
  favour of the `priced` gate.
- **Delegating budgets to an upstream proxy** — concedes a table-stakes cost-control feature
  and splits accounting from enforcement. Rejected, as in WF-ADR-0031.

## Success Measures

- With a tiny `limit` and real costs, the first over-budget request degrades to the cheapest
  tier (or returns 402 under `block`), surfaced in headers, with an **identical scored decision**.
- An unpriced (relative-unit) gateway ignores the budget entirely.
- The cap reads existing ledger spend — no double counting, no new persistence format.

## Related

- WF-ADR-0001 (deterministic, offline, no-model-call core — preserved by rule 1)
- WF-ADR-0031 (failover policy — the `degrade` primitive this reuses; rule 7 there foresaw this)
- WF-ADR-0017 (cost metadata — what makes a budget enforceable)
- WF-DESIGN-0007 (cost/savings accounting — the ledger the cap reads)
- WF-ROADMAP-0006 (item 6: budgets including the degrade primitive)
