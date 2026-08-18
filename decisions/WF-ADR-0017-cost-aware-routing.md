---
schema_version: 1
id: WF-ADR-0017
type: decision
tags: [routing, cost, calibration, config, gateway]
---

# WF-ADR-0017: Cost-Aware Routing and Calibration

## Status

Accepted

## Category

Technical

## Context

Routing is expressed as a raw `0.0`–`1.0` threshold (or tiers). But the thing an
operator actually optimizes is cost versus quality: "spend as little as possible
while staying good enough." The v0.1.6 benchmark (WF-ADR-0015) already reasons in
exactly those terms — it reports cost savings and picks a cost-quality knee
(`PGR × cost_savings`) — yet the config and the `calibrate` path have no notion of
cost at all. The user translates a threshold into money by hand.

## Decision

Two additive pieces, scoped deliberately tight:

1. **Cost metadata.** An optional `cost` on `Tier` (an additive field on the frozen
   dataclass) and `cost_per_1k` on `[gateway.models.<name>]`. Purely
   informational: surfaced in the dashboard and the metrics endpoint
   (WF-ADR-0018), and consumed by calibration. It does **not** touch per-request
   scoring.
2. **A cost-aware calibration objective.** `wayfinder-router calibrate --objective
   cost-quality --target-savings X` (or `--max-cost`) selects the threshold or
   tiers that maximize quality subject to a cost ceiling — the benchmark knee
   logic, moved into `calibrate`.

The scored path stays deterministic and free. Cost only affects *where the cut is
placed* (at calibration time) and *what is reported*; it never enters the
per-request decision. The WF-ADR-0001/0004 boundary holds.

### Amendment (v0.2.0): the historical `knee` objective and weight emission

The held-out RouterBench evaluation (`benchmarks/calibration-eval.md`) showed the
`accuracy` objective *collapses to always-routing-high* when one model is usually
right (high accuracy, ~0 savings), and that `cost-quality` forces the operator to
guess a `--target-savings`. So `calibrate --objective knee` is added: it picks the
cut maximizing **quality-recovered × cost-saved** directly — the benchmark knee
(WF-ADR-0015), now with no target to guess. It orders the two arms by *cost* (the
expensive arm routes above the cut), which is robust to score ties that the
mean-score order inverts. Separately, `calibrate --weights …` re-scores the prompts
with custom feature weights (e.g. the lexical opt-in) and emits them with the cut,
so the result is a complete, deployable config rather than a cut over the default
structural score. Both are still offline, deterministic, and outside the scored path.

### Erratum (2026-08-18): price-sensitive native calibration

The `knee` objective above is not price-sensitive when each arm has a fixed
cost. Its `PGR × cost_savings` score contains the constant multiplier
`1 - C_low / C_high`, so changing only the configured cost ratio cannot change
the selected cut. This amendment supersedes any earlier sentence that calls
`knee` cost-aware or says configured prices affect its selected cut.

The historical name `knee` may be retained for reproducibility, but its contract
is a quality/call-fraction heuristic. The native price-sensitive calibrator uses
the `min-cost` objective proposed in issue #170 and first implemented for the
retired Python tree in PR #78:

```text
L(c) = (1 / N) × Σ [
  C(route_c(x_i))
  + Q × I(label_i = high and route_c(x_i) = low)
]
```

The selected cut minimizes `L(c)`. `Q >= 0` is the operator's cost of sending a
prompt that requires the high-quality arm to the low-cost arm. It uses the same
units as the configured arm costs. The default is `Q = C_high`, equivalent to
one additional high-arm request to repair a wrong cheap answer.

### Native `min-cost` implementation (2026-08-18)

The reviewed Rust replacement restores only `calibrate --mode threshold
--objective min-cost` under this contract:

- Input is UTF-8 JSONL with `text` and non-empty `label` strings. Blank lines
  are ignored. Exactly two labels must be present.
- `--costs LABEL=COST,LABEL=COST` is required and must name those labels
  exactly. Both costs are finite and non-negative; the cheaper label is the low
  arm, and equal costs are rejected because they define no cheap/high ordering.
- Prompts are scored by the pure Rust routing core. Default weights and lexicon
  are used unless the operator supplies an explicit `--config`; calibration
  makes no model or network call.
- Candidate cuts are `0.0`, `1.0`, and observed scores rounded to four decimal
  places. The high arm owns the inclusive `score >= cut` boundary. Objective
  ties choose the upper median candidate deterministically.
- `Q` defaults to the high-arm cost. A supplied `--quality-penalty` must be
  finite and non-negative.
- Output is a deterministic, parse-verified routing fragment with arm costs.
  A `0.0` optimum is emitted as one high-arm tier because the strict tier schema
  cannot represent two different models at the same inclusive boundary.
  `--out` creates a new file and refuses to replace an existing file.

This implementation does not restore `accuracy`, `knee`, `cost-quality`, tier
or classifier fitting, automatic config replacement, or `recalibrate`. Those
surfaces continue to fail closed under WF-ADR-0046. Costs and `Q` remain
calibration inputs only and never enter the per-request scored path.

Explicitly out of scope for v1: live spend metering and token-level costing (which
would need a tokenizer dependency and per-provider price tables). v1 uses a flat
per-request or per-1k-words cost so the harness stays deterministic and
dependency-free. Live metering is a separate future decision.

## Consequences

### Positive

- Separates descriptive savings from price-sensitive optimization.
- A future native calibrator has an explicit economic loss function instead of
  relying on the historical `knee` label.

### Negative

- More config surface, and the cost numbers and `Q` penalty are operator
  estimates, not billed or universal truth.

### Risks

- Scope creep toward a billing system. Mitigation: this ADR fences v1 to config
  metadata plus a calibration objective; anything live is a later decision.

## Alternatives Considered

### Route by live token cost at request time

#### Disadvantages

- Needs a tokenizer (a dependency) and per-provider price tables, is
  non-deterministic, and pulls billing concerns onto the scored path. Rejected.

### Do nothing; the threshold is enough

#### Disadvantages

- Leaves the cost story implicit and makes the operator translate threshold into
  savings by hand — the exact gap the benchmark already exposes.

## Success Measures

- Documentation and tests show that changing only fixed arm prices cannot move
  the historical `knee` cut.
- A future native `min-cost` implementation can move the selected cut when arm
  costs or `Q` change, using deterministic fixtures and tie-breaking.
- The scored path is unchanged and still deterministic.

## Related Decisions

- WF-ADR-0015 (the benchmark cost-quality framing this reuses)
- WF-ADR-0002 / WF-ADR-0003 (the tiers and classifier a cut is placed on)
- WF-ADR-0001 / WF-ADR-0004 (the boundary preserved)
- WF-ADR-0018 (the metrics endpoint that surfaces the cost counters)
- WF-ADR-0046 (the Rust-only cutover that removed legacy calibration commands)
- Issue #170 and PR #78 (proof, discussion, and retired-Python reference implementation)
