---
schema_version: 1
id: WF-ADR-0015
type: decision
tags: [benchmark, evaluation, positioning, reproducibility, honesty]
---

# WF-ADR-0015: Benchmark Methodology

## Status

Accepted

## Category

Technical

## Context

Wayfinder claims to be a *deterministic, offline, no-model-call* router. A claim like
that is only credible with a reproducible benchmark — but the LLM-routing space already
has a converged metric vocabulary ([RouteLLM](https://www.lmsys.org/blog/2024-07-01-routellm/)'s
PGR / call-fraction; [RouterArena](https://arxiv.org/abs/2510.00202)'s accuracy / cost /
latency; [RouterBench](https://arxiv.org/pdf/2403.12031)), so unlike RAC's grounding eval
(ADR-066, invented because no standard existed) we should **align to the standard, not
invent one**.

Two honesty hazards shape the design:

- The compelling competitors (RouteLLM, NotDiamond, Martian, OpenRouter) are either
  *trained* (needing checkpoints + model inference) or *hosted* (needing paid API access).
  They cannot be run in an offline, key-free CI. Reporting numbers for routers we did not
  actually run would be fabrication.
- A self-published benchmark is suspect if it only flatters the author. Wayfinder's
  structural score has a real, known weakness (short-but-hard prompts are structurally
  invisible), and a naive length baseline is competitive on short-prose traffic. A
  trustworthy benchmark must surface these.

## Decision

A small benchmark harness under `benchmarks/`, deterministic and offline, with metrics
aligned to the routing literature.

- **A router is a pure function `prompt -> "local" | "cloud"`** — the whole interface the
  literature evaluates — so any router (including a learned or hosted one, behind an
  adapter) is a one-function addition.
- **Metrics are the standard ones**: quality, cost, call-fraction, **PGR** (RouteLLM),
  cost-savings, and **decision latency** (RouterArena's latency axis — where a structural
  router wins by deciding in microseconds with no model call). A single threshold is one
  point on the **cost-quality curve**; the harness sweeps it and reports the curve plus a
  cost-quality knee (`PGR × cost_savings`; corrected below).
- **Labels are the oracle; no model is called** to produce a number. Each dataset row
  carries per-model correctness labels, so the harness is reproducible byte-for-byte
  (apart from the wall-clock latency column).
- **We publish only numbers we ran.** Routers we cannot run offline (RouteLLM, NotDiamond,
  …) are represented by (a) a documented pluggable adapter so anyone with access can run
  them on the same dataset, and (b) a comparison table citing **their own published
  numbers with provenance**, loudly marked *different dataset, not directly comparable*.
  No competitor number is presented as ours or as head-to-head.
- **We ship honest baselines and an honest dataset.** The harness includes always-local,
  always-cloud, a stable-random, a tuned length-threshold, and an oracle upper bound; the
  shipped dataset is a small *illustrative* set (clearly labelled, not a general claim)
  that **includes Wayfinder's failure mode**, and the harness reads any real public set
  (RouterBench / RouterArena) in the same format.
- **Positioning follows the numbers.** The README claim is the precise, defensible one —
  the only offline, zero-model-call, calibrate-on-your-data, self-hosted structural router
  — not "best PGR" or "nothing like this".

### Erratum (2026-08-18): `knee` is price-ratio invariant

For two arms with fixed per-request costs `C_high > C_low >= 0`, let `f_low(c)`
be the fraction of prompts sent to the low-cost arm at cut `c`. Savings against
always using the high-cost arm factorizes as:

```text
cost_savings(c) = f_low(c) × (1 - C_low / C_high)
knee(c) = PGR(c) × f_low(c) × (1 - C_low / C_high)
```

The final price term is positive and constant across candidate cuts. It changes
the reported savings and knee value, but it cannot change the cut selected by
the `argmax`. The historical `knee` is therefore a quality/call-fraction
heuristic, not a price-sensitive optimizer. Changing only the configured cost
ratio cannot move the selected cut.

Historical benchmark results remain reproducible as reported. WF-ADR-0017
defines `min-cost` as the price-sensitive objective for any future native
calibration surface.

## Consequences

### Positive

- The benchmark is reproducible by anyone with no network and no keys — the strongest
  possible support for a "deterministic, offline" claim.
- Surfacing the failure mode and a competitive baseline makes the result *credible*
  precisely because it is not self-serving.
- The harness is the RouterArena contribution path: drop in an adapter or a real dataset
  and compare apples-to-apples.

### Negative

- The shipped numbers are on a small illustrative set, so they are directional, not
  general; this must be stated wherever they appear.
- We cannot present live head-to-head numbers against the commercial routers in this repo.

### Risks

- A reader takes the illustrative numbers as a general benchmark. Mitigation: every
  surface (results, README, this ADR) labels the shipped dataset as illustrative and
  points to RouterBench / RouterArena for general numbers.

## Alternatives Considered

### Invent a bespoke metric (as RAC's grounding eval did)

#### Disadvantages

- Routing already has converged metrics; a bespoke one would look like dodging comparison.
  Align to PGR / cost / latency instead.

### Estimate competitor numbers from their papers and tabulate them as a head-to-head

#### Disadvantages

- Different datasets and model pairs make the numbers non-comparable; presenting them as a
  head-to-head would be misleading. Cite-with-provenance and a pluggable adapter is honest.

### Curate the dataset to make Wayfinder win

#### Disadvantages

- Destroys the only thing a self-published benchmark has going for it — credibility. The
  dataset deliberately includes the cases Wayfinder gets wrong.

## Success Measures

- `python -m benchmarks.run` reproduces `benchmarks/results.md` (apart from latency) with
  no network or keys.
- The report shows the cost-quality curve, an honest per-difficulty breakdown including the
  short-hard failure, and baselines that can beat Wayfinder.
- No number attributed to a competitor was produced by us; each carries a citation.

## Related Decisions

- WF-ADR-0001 (the deterministic, no-model-call core whose claim this benchmark backs)
- WF-ADR-0002 / WF-ADR-0003 (the tiers / classifier the harness can evaluate)
- RAC ADR-066 (the contrasting case — a bespoke deterministic eval, invented because no
  standard existed; here a standard exists, so we align)
