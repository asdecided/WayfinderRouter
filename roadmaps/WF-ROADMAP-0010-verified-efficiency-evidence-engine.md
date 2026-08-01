---
schema_version: 1
id: WF-ROADMAP-0010
type: roadmap
tags: [evidence, shadow-mode, canary, rollout, enterprise, trust, redis, otel, oidc, audit, finops, benchmarks]
---

# Roadmap: prove it, then scale it — the verified-efficiency evidence engine and the enterprise substrate to run it fleet-wide

## Status

Active — Rust enterprise substrate

## Rust runtime amendment (2026-08-01)

WF-ADR-0046 made Rust the sole production router and removed the Python runtime
referenced throughout the original proposal. The outcomes and sequencing remain
governing, but every implementation now lands in the Rust workspace. Historical
`gateway.py`, `ratelimit.py`, `pricing.py`, and Python-extra references below
describe the capability seam, not an implementation authority.

Enterprise hardening begins with WF-ADR-0050: the existing local surface stays
loopback-only, while an explicit authenticated managed data plane exposes only
inference, allowlist-filtered model discovery, and content-free probes. This
precedes Redis, OIDC, audit, OpenTelemetry, and Helm because those systems must
attach to explicit data-plane and operator-plane authorities rather than a
single mixed listener.

Current Rust baseline already includes bounded request/response/streaming
delivery, virtual keys, per-key model allowlists, per-key and global limits,
budgets, cache, retry/failover/breakers, prompt-free metrics, last-good hot
reload, and bounded graceful shutdown. Track B therefore starts from these
implemented primitives rather than rebuilding them.

WF-ADR-0051 adds the next process-level capacity boundary: 32 simultaneous
deliveries, 64 bounded waiters, a two-second queue deadline by default, explicit
`503` overload responses, and a real-listener test proving twenty concurrent
requests reach delivery without router serialization. These are single-process
limits; shared admission and accounting remain prerequisites for a truthful
multi-replica claim.

WF-ADR-0052 adds bounded workspace policy above virtual keys: shared
process-local RPM/TPM, inherited model allowlists, stable public model aliases,
and explicit workspace response attribution. It also fixes the enterprise
multi-turn contract as stateless transcript forwarding plus deterministic
route/latch evaluation. This is useful single-process isolation, not a
substitute for Initiative 4's Redis-backed fleet consistency.

## Context

Every conversation about adopting a router — any router — ends on the same question: **"will
downrouting hurt quality on *my* traffic?"** Nothing Wayfinder currently ships answers it. The
savings ledger (`/v1/savings`, WF-DESIGN-0007) says what routing *saved*; nothing says what routing
*cost in quality*, on the adopter's own prompts, with enough statistical honesty to defend in a
budget meeting. That question — not features — is the enterprise adoption blocker, and answering it
empirically is the single capability that changes what Wayfinder *is*: from "a router you must
trust" to "a router that proves itself on your traffic before it touches your traffic."

Wayfinder is unusually well positioned to answer it, for a reason that looks like a weakness:
`benchmarks/blind-eval.md` reports, in public, that the **stock structural scorer scores below
random on RouterBench** (mean skill **−0.049** across the realistic operating band; the opted-in
lexical weights lift it to **+0.039**). No competing router publishes its own negative result. The
honest conclusion the blind eval already draws is that Wayfinder's value is *unlocked by
calibration on the traffic it will route* — which means the missing product is the loop that makes
calibration-then-proof a first-class, zero-risk operation:

> **shadow** (route nothing, score everything, sample small-vs-frontier comparisons off-path) →
> **calibrate** on the labels shadow produced (`calibrate` / `recalibrate`, WF-ADR-0007) →
> **evidence report** on the calibrated config, with trust gates and a flip/don't-flip verdict →
> **canary** a fraction with quality tripwires → **enforce**.

Shadow mode is therefore two things at once: the zero-risk deployment posture enterprises need on
day one, and the **label faucet** the calibration loop has always wanted (today `onboard` asks the
operator to generate labels interactively; shadow harvests them from real traffic).

Most of the machinery already exists and was built to be reused this way: the sufficiency judge and
its trust gates are already offline, deterministic, and re-runnable from saved logs
(`judge.py`, `sufficiency.py`, WF-ADR-0037); `build_app` already has a `dry_run` seam
(`gateway.py:1441`); the ledger already computes counterfactual cost (`pricing.py:181`,
WF-DESIGN-0007). What's new is wiring them into the gateway as an off-request-path evidence
pipeline — and building the substrate an enterprise needs to run that pipeline across a fleet:
shared state for the counters the tripwires and budgets depend on, operator identity, an audit
trail, traces, and a chart to deploy it all.

The hard constraint is unchanged and non-negotiable: **the per-request routing decision stays
offline, deterministic, and model-call-free (WF-ADR-0001)**. Shadow sampling, judging, evidence
statistics, and tripwire evaluation all run *off* the request path — background and post-response —
exactly as the judge does today ("offline / calibration-time only… no model, key, or network on the
decision path"). Every default in this roadmap preserves today's behavior byte-for-byte; existing
deployments upgrade as no-ops.

Two tracks, six initiatives. Track A is the evidence engine (shadow → report → canary). Track B is
the enterprise substrate (shared state + truthful money; identity/audit/traces/chart) and the
public benchmark program that makes the whole story reproducible. Each initiative carries its own
acceptance criteria — testable, empirical, and mostly phrased as regression guarantees.

## Outcomes

- An enterprise can deploy the gateway in **shadow mode** — 100% of traffic still served by their
  incumbent frontier arm — and after N days hold a **rendered evidence report**: projected savings,
  quality win/loss/tie with confidence intervals, trust-gate verdicts, and an unhedged
  **enforce / keep shadowing / do not enforce** recommendation.
- The path from shadow to full routing is **graduated and reversible**: a deterministic canary
  fraction, quality tripwires that fall back to the frontier arm automatically, and an audit record
  for every state change.
- A **two-replica deployment behind one Redis** enforces a single shared budget, rate limit, and
  tripwire — the counters stop being per-process fiction.
- Cost claims become **token-truthful and exportable**: input/output-split pricing, `usage` captured
  on streaming too, and a FinOps CSV export that reconciles against the provider invoice.
- Operators authenticate with **OIDC** for the dashboard/admin surface (virtual keys stay the
  data-plane auth), every consequential action lands in an **append-only audit log**, traces
  propagate via **OpenTelemetry** (opt-in extra), and `helm install` stands the whole thing up.
- The claims regenerate in public: a **standing benchmark program** (router skill, judge validity,
  evidence-statistics recovery, decision latency) republished per release with CI gates, plus an
  **operator's guide** (`docs/evidence-trial.md`) that takes a stranger from install to a defensible
  report without reading source.

## Initiatives

### Track A — the evidence engine

### 1. Shadow mode: score everything, route nothing, sample comparisons off-path

The zero-risk posture: the gateway forwards every request to the incumbent arm exactly as a
pass-through proxy would, while the scorer runs as it always does and the decision that *would*
have been made is recorded. A sampled fraction additionally gets a **background comparison**: after
the client's response is sent, the same prompt is sent to the would-have-routed arm and the two
answers are judged by the existing deterministic `HeuristicJudge` (`judge.py:98`) — producing both
evidence rows and `{text, label}` calibration rows in one motion.

- **A rollout knob, not a scoring mode.** `[gateway] rollout = "shadow" | "canary" | "enforce"`,
  default `enforce` (today's behavior — upgrades are no-ops). Named `rollout` deliberately: the
  existing `x-wayfinder-router-mode` header and its `scored | pinned | threshold-override` contract
  (`gateway.py:41`) describe the *decision*, which shadow does not change. New response headers join
  the family (`gateway.py:2029`): `x-wayfinder-router-rollout: shadow` and
  `x-wayfinder-router-would-route: <model>`, so a client can see the counterfactual per request.
  `build_app`'s `dry_run` (`gateway.py:1441`) is the seam this generalizes — dry-run returns the
  decision *instead of* the answer; shadow returns the answer and *records* the decision.
- **Sampling is background, budgeted, and off-path.** A `[gateway.shadow]` table: `sample_rate`
  (0.0–1.0), `arms` (names validated against `[gateway.models.*]` exactly like `fallbacks`,
  `gateway.py:392`), `daily_budget` (a spend cap on shadow-arm calls, reusing the `[gateway.budget]`
  idiom, WF-ADR-0032), `store` (JSONL path), `store_text` (default **false**). The comparison call
  fires after the client response completes, via the existing `aforward_request`
  (`gateway.py:1248`); it never delays or gates the served response. Background arm-calls from the
  gateway are genuinely new machinery (the judge runs only in the offline CLI loop today) and get
  their own ADR.
- **Metadata-only by default.** The shadow store keeps score, decision, verdict, token counts, and
  cost — not prompt or response text — honoring the standing posture (WF-ADR-0011/0014; the `recent`
  ring keeps "metadata only"). `store_text = true` is the explicit, documented opt-in for teams that
  want re-judgeable logs, following the `judge --save-comparisons` precedent (off by default, a
  response-body store). Retention: `store_days` prunes on write.
- **Projected savings become first-class.** The ledger (`SavingsLedger`, `pricing.py:181`) gains a
  `projected` bucket recorded from shadow decisions; `/v1/savings` reports `projected_savings`
  alongside realized, and the dashboard labels shadow deployments as such. Shadow-arm spend is
  recorded separately (it is a real cost, capped by `daily_budget`) and never counts as savings.
  New metrics in the zero-dep registry (`Metrics`, `gateway.py:192`):
  `wayfinder_shadow_comparisons_total{verdict=…}`, `wayfinder_shadow_spend`,
  `wayfinder_shadow_projected_savings`.
- **The store feeds calibration directly.** `wayfinder calibrate --from-shadow <store>` consumes
  comparison rows as labeled samples — the same shape `onboard` produces — closing the
  shadow → calibrate loop with no new formats.

**Acceptance:**

- In `rollout = "shadow"`, the forwarded upstream request body is **byte-identical** to an
  enforce-mode deployment pinned to the same arm (regression test on both streaming and
  non-streaming paths).
- `sample_rate = 0` produces exactly zero additional upstream calls (mock-counted).
- With a shadow arm deliberately delayed 10s, client-observed latency is unchanged — the comparison
  fires after the response is sent (timing test).
- Shadow-arm spend appears in its own ledger bucket, stops at `daily_budget`, and never increments
  realized savings.
- With `store_text` unset, the shadow store contains no prompt or response text (content-level
  test on a store produced from known prompts).
- The WF-ADR-0001 guard stays green: the decision path gains no model call, no key, no network.

### 2. Evidence reports: the flip-to-enforce artifact

Turn a shadow store into a document a platform lead can defend: statistically honest, deterministic,
and ending in a verdict rather than a hedge. This is a **pure-stdlib module**
(`wayfinder_router/evidence.py`), unit-tested like `sufficiency.py`, reusing the trust gates
wholesale: `cohens_kappa` (`sufficiency.py:41`), `confusion_matrix` (`sufficiency.py:64`),
`evaluate`/`GateReport` (`sufficiency.py:162,123`), and the κ floor discipline
(`DEFAULT_KAPPA_FLOOR = 0.6`, `sufficiency.py:35` — below it, refuse).

- **CLI first, endpoint second.** `wayfinder evidence <shadow-store> [--gold gold.jsonl]
  [--out report.html | report.json] [--min-n N] [--sufficiency-floor F]`. The gateway adds
  `GET /v1/evidence` (JSON summary over the live store; operator-authenticated once Initiative 5
  lands). The HTML artifact is self-contained, no CDN — the `demo.html` / dashboard precedent.
- **What the report says.** Win/loss/tie/**abstain** counts with Wilson score intervals on the
  sufficiency rate; per-score-band, per-vkey, and per-tag breakdowns; judge agreement **κ against a
  human-labeled gold subsample** (the operator gold-labels ~100 sampled comparisons; below the κ
  floor the report refuses a verdict and prints the confusion matrix, exactly the `GateReport`
  register); projected cost delta pinned to `price_table_version` (`pricing.py:54`) and showing the
  **estimated-token fraction** until Initiative 4's pricing work lands; provenance stamps — judge
  `version` (`judge.py:87`, `heuristic-1`), config hash, store row count, date range.
- **A tri-state verdict, never a hedge**: **enforce** (gates passed, quality delta within the
  operator's floor), **keep shadowing** (insufficient n, κ below floor, or intervals too wide — the
  report says which and how much more data is needed), or **do not enforce** (gates passed and the
  answer is no). Abstentions are reported, never folded into wins — the `HeuristicJudge` honesty
  rule carried up a layer.
- **Deterministic and replayable.** Same store + same gold + same flags → byte-identical JSON. The
  judge is already "re-runnable from a saved comparison log" (WF-ADR-0037); the report inherits it.

**Acceptance:**

- Identical inputs produce byte-identical `report.json` across runs and machines (golden-file test).
- Below `--min-n`, or with gold-κ under the floor, the report emits **keep shadowing** with the
  confusion matrix and refuses a flip verdict (the `sufficiency.py` refusal register).
- A synthetic store with a planted sufficiency rate and cost delta is recovered within the report's
  own stated confidence intervals (statistical correctness test, seeded).
- Abstain counts appear in every table; no aggregate treats abstain as a win.
- `report.html` renders offline (no external requests — same bar as the dashboard).

### 3. Graduated rollout: canary fractions and quality tripwires

Nobody flips 0% → 100%. The canary routes a deterministic fraction of traffic per the scored
decision while the rest stays on the incumbent arm; sampled comparisons continue on the canary
slice and feed a **tripwire** — a rolling quality floor that, when breached, sends 100% of traffic
back to the frontier arm on its own. The tripwire is the undo button that makes "enforce" a
reversible decision, and it is deliberately shaped like the circuit breaker
(`reliability.CircuitBreaker`, wired at `gateway.py:1552`): a small state machine with sticky trips.

- **Deterministic assignment, per conversation.** `[gateway.canary]`: `fraction`,
  `tripwire_floor` (rolling sufficiency-agreement floor), `tripwire_window` (comparisons),
  `on_trip = "shadow" | "frontier"`. Assignment is SHA-256 of the conversation key modulo 10,000 —
  no RNG anywhere, and **per-conversation**, riding the same latch that already prevents mid-thread
  tier flapping (`conversation_high_water`, `gateway.py:1189`, WF-ADR-0022). A conversation is
  either in the canary or not, for its whole life.
- **The tripwire runs off-path.** Comparison verdicts stream into a rolling window; the floor check
  happens on comparison completion, never inside a request. A trip flips the effective rollout to
  `on_trip`, increments `wayfinder_tripwire_trips_total`, writes an audit record (Initiative 5),
  and **stays tripped until an operator acts** — no automatic flap-back, the breaker's half-open
  subtlety deliberately omitted where quality is at stake.
- **Visible everywhere.** `x-wayfinder-router-rollout: canary` on responses;
  `wayfinder_canary_fraction` in `/metrics`; the dashboard shows fraction, window fill, current
  agreement rate, and distance to the floor.

**Acceptance:**

- Over N synthetic conversations, fraction `f` puts `f ± ε` of *conversations* (not requests) on
  the canary side, and the same conversation key always lands on the same side (determinism test).
- With a mock small model degraded to refusals mid-run, the tripwire fires within
  `tripwire_window` comparisons; afterwards 100% of new requests serve from the frontier arm, one
  audit record exists, and the metric incremented exactly once.
- Tripwire evaluation adds no per-request model call and no per-request latency (the WF-ADR-0001
  guard extended to the canary path).
- A trip is sticky across config hot-reloads until explicitly cleared; clearing writes its own
  audit record.

### Track B — the enterprise substrate

### 4. Shared state (Redis first) and token-truthful money

Every counter the evidence engine and the control plane rely on is per-process today — the code
says so itself: "one long-lived limiter; its window counters survive config hot-reloads"
(`gateway.py:1564`); the ledger persists to a local JSON file; the cache is an in-process dict.
Fleet-wide budgets, rate limits, canary tripwires, and a shared shadow store are impossible without
a shared backend. Separately, cost claims must survive an adversarial reader: the plumbing is
further along than it looks — `usage_tokens` (`pricing.py:60`) already prefers the upstream's real
`usage` counts and flags `estimated=True` on fallback — but a single blended `cost_per_1k` per
model and missing streaming `usage` leave the numbers approximate exactly where the evidence
report needs them exact.

- **A `StateBackend` protocol, Redis first.** `[gateway.state]`: `backend = "memory" | "redis"`
  (default `memory` — byte-identical to today), `url`, `namespace`. A new
  `wayfinder_router/state.py` defines the protocol; the adapters live where the state lives:
  `RateLimiter` is already clock-injected and lock-guarded (`ratelimit.py:33`) — its fixed windows
  become shared atomic counters; `SavingsLedger.record/spent` (`pricing.py:195,286`) writes through;
  `cache_key` is already the SHA-256 of the normalized request (`cache.py:38`) — trivially a Redis
  key. The redis client ships behind a `wayfinder-router[redis]` extra; the pure core imports
  nothing new (WF-ADR-0001 import hygiene).
- **Degrade loudly, never drop.** Redis unavailable → fall back to per-process counters with a
  prominent log line and a `wayfinder_state_degraded` gauge; requests are never refused for state
  reasons (the ledger's best-effort idiom, generalized).
- **Input/output-split pricing.** `[gateway.models.<name>]` gains `cost_in_per_1k` /
  `cost_out_per_1k`; blended `cost_per_1k` remains supported (deprecated, documented). Streaming
  forwards inject `stream_options: {"include_usage": true}` so streamed turns carry real `usage`
  and `estimated` goes to false. An optional tokenizer plugin ships as an extra for providers that
  return no usage — never on the decision path.
- **FinOps export.** `GET /v1/savings/export?format=csv&period=30d`: per-day, per-vkey, per-model
  rows — realized spend, counterfactual frontier spend, savings, token counts, estimated fraction,
  `price_table_version` — with FOCUS-compatible column names, building on the per-vkey attribution
  the ledger already does (`gateway.py:1502`, WF-ADR-0035).

The Rust implementation of the shared policy-state contract, Redis server-time
fixed-window counters, and loud process-local degradation is tracked in
WF-ADR-0053. Its first PR deliberately leaves the local savings ledger and
response cache process-local; those migrations require their own compatibility
and recovery evidence.

Stable public aliases may now carry bounded weighted OpenAI-compatible
deployments under WF-ADR-0054. Rotation and deployment-health state remain
process-local by design; Redis coordinates policy windows, not a hidden global
round-robin cursor.

WF-ADR-0055 adds bounded named routing presets for application-owned delivery
policy. `model = "@route/<name>"` selects an operator-defined ordered alias
list, exposes a secret-free `/router/routes` inventory, and fails closed on an
unknown route. Presets do not alter the deterministic score or expand the
credential boundary.

WF-ADR-0056 wires the shared destination contract into the Rust gateway. Hard
capability, readiness, context, and privacy-boundary filters now run before
delivery retries and fallback, with explicit pins failing closed and the
effective privacy posture echoed in response metadata.

WF-ADR-0057 adds the first enterprise trust-surface slice: an optional OIDC
operator boundary with explicit issuer, audience, JWKS, and admin-claim
configuration. Virtual keys remain the data-plane credential; audit records,
OpenTelemetry, and Helm packaging remain separate follow-on slices.

**Acceptance:**

- Two gateway replicas against one Redis enforce a **single** shared RPM limit and budget: N
  requests split across replicas trip at exactly the shared threshold (docker-compose integration
  test, run in CI).
- `backend = "memory"` leaves every existing test green with zero behavioral diff (regression).
- Against a mock provider returning real `usage`, a 30-day CSV export reconciles with the mock's
  invoice within 1%, and every streamed row has `estimated=false`.
- Killing Redis mid-run degrades to per-process counters with the gauge set and no dropped
  requests; restoring Redis resumes shared counting.

### 5. The enterprise trust surface: OIDC, audit log, OpenTelemetry, Helm

Procurement clears on identity, auditability, and deployability. The evidence report is only
credible if access to it — and to rollout flips — is governed and recorded. Data-plane auth is
already right-sized (virtual keys: hashed bearer tokens with per-key budgets/limits/allowlists,
`vkeys.py:30,46`, WF-ADR-0035) and **stays**; what's missing is operator identity on the admin
surface, currently a single env-var bearer token in the feedback style (`_FEEDBACK_TOKEN_ENV`,
`gateway.py:107`).

- **OIDC for operators, vkeys for the data plane.** `[gateway.auth]`: `mode = "vkeys" | "oidc" |
  "both"` (default `vkeys` — today's behavior), `issuer`, `audience`, `jwks_url`, `admin_claim`.
  OIDC (JWT validation against the IdP's JWKS) guards `/router*`, `/v1/savings*`, `/v1/evidence`,
  and rollout-flip operations; `/v1/chat/completions` and `/v1/messages` keep virtual keys. No
  sessions, no user store — the IdP owns identity.
- **Append-only audit log.** `wayfinder-audit.jsonl`: config reloads, rollout flips, tripwire trips
  and clears, auth failures on admin surfaces, exports — each record actor (OIDC subject or vkey
  id), timestamp, action, before/after. Never prompt text. Written through the `StateBackend` when
  shared, local JSONL otherwise.
- **OpenTelemetry as an extra, zero-dep posture preserved.** `wayfinder-router[otel]`: spans for
  request → decision → delivery, `traceparent` propagated to upstreams, and a JSON structured-log
  toggle. The Rust implementation is documented in WF-ADR-0058. With the extra absent, the
  gateway imports nothing new and `/metrics` (`gateway.py:192`, WF-ADR-0018) is unchanged — OTel
  augments, never replaces.
- **Helm chart.** `deploy/helm/wayfinder-router/`: replicas + Redis (Initiative 4) + ingress with
  TLS terminated at the ingress — the gateway itself grows no TLS; the chart documents the
  boundary, consistent with the SECURITY.md posture.

**Acceptance:**

- With `mode = "oidc"`, dashboard/savings/evidence endpoints return 401 without a valid JWT while
  chat with a valid vkey is unaffected; with `mode = "vkeys"` (default), behavior is byte-identical
  to today (regression).
- Every rollout flip, tripwire trip/clear, and config reload yields exactly one audit record with a
  non-empty actor; the audit file is append-only under concurrent writers.
- With the otel extra not installed, `import wayfinder_router.gateway` pulls no new third-party
  modules (import-hygiene test, same style as the standalone-invariant CI gate); with it installed,
  `traceparent` arrives at a mock upstream.
- `helm install` on a kind cluster with 2 replicas + Redis passes Initiative 4's shared-budget
  integration test end-to-end.

### 6. The public benchmark program and the operator's evidence-trial guide

"Open-source gold standard" means the numbers regenerate in public — including the unflattering
ones. `blind-eval.md` set the register; this initiative makes it a standing program with CI teeth,
and closes the loop with a guide that turns the whole roadmap into a repeatable playbook.

- **Judge validation — the keystone benchmark.** The evidence engine's credibility rests entirely
  on `HeuristicJudge`, so the judge gets the blind-eval treatment: replay it over RouterBench's
  graded answer pairs (36,497 prompts, 11 models — `routerbench_adapter.py`) and publish its κ
  against the real graded labels, per category, **with its abstention rate**, in a new
  `benchmarks/judge-validation.md`, regenerated per release. If the judge is weak somewhere, the
  table says so, and the evidence report's κ-floor refusal is the safety net.
- **Evidence-statistics replay.** `benchmarks/evidence_replay.py`: generate synthetic shadow stores
  with planted effect sizes; assert the report recovers them within its stated intervals.
  Statistical correctness as a benchmark artifact (`benchmarks/evidence-replay.md`), not just a
  unit test.
- **Per-release regeneration, wired into RELEASE.md.** Router skill/PGR at the knee
  (`harness.py:81,149,154`) on RouterBench + RouterArena + blind splits; decision latency
  (`latency_us`, `harness.py:43`); the judge and replay tables. `make bench` reproduces every
  published table bit-for-bit from cached datasets, offline.
- **CI gates:** skill/PGR must not regress below the committed floor per dataset; mean decision
  latency stays sub-millisecond; evidence reports are byte-deterministic; the existing JS↔Python
  parity gate continues to cover the decision core.
- **`docs/evidence-trial.md` — the operator's guide.** The end-to-end playbook: deploy shadow →
  run N days → gold-label ~100 sampled comparisons → `wayfinder evidence --gold` → read the
  verdict → calibrate from the shadow store if it says keep-shadowing → canary → enforce. Written
  for a platform engineer who will never read the source, with the report's statistics explained
  in plain language.

**Acceptance:**

- `make bench` on a clean checkout with cached datasets reproduces every published table
  bit-for-bit, fully offline.
- The release checklist fails if regenerated benchmark tables differ from the committed ones.
- `benchmarks/judge-validation.md` ships per release with κ per category, the confusion matrix,
  and the abstention rate — never omitted.
- A fresh operator following `docs/evidence-trial.md` against a dry-run gateway reaches a rendered
  evidence report without touching Python internals (walkthrough-tested each release).

## The empirical benchmark program at a glance

| What | Dataset | Published | CI gate |
| --- | --- | --- | --- |
| Router skill / PGR at knee (structural + lexical opt-in) | RouterBench (36,497 graded prompts), RouterArena (809-prompt join), blind splits | `benchmarks/routerbench-results.md` + per-release delta | no regression below committed floor |
| Judge validity (κ vs graded labels, abstention rate) | RouterBench answer pairs | `benchmarks/judge-validation.md` (new) | κ floor per category; abstention always reported |
| Evidence-report statistical recovery | synthetic stores, planted deltas | `benchmarks/evidence-replay.md` (new) | recovery within stated intervals |
| Decision latency | canonical `dataset.jsonl` | results tables | sub-millisecond mean |
| Determinism | all of the above | — | byte-identical re-runs; existing JS↔Python parity gate |

## Non-goals (explicit, for this roadmap)

- **No LLM-as-judge — deliberately deferred, and local-only if ever.** The `Judge` protocol
  (`judge.py:78`) keeps the seam open, but an LLM judge is *not* a near-term path. The standing
  signal is the deterministic heuristic judge plus a human-labelled gold sample; where the
  heuristic cannot tell, coverage is reported honestly, not guessed. If an LLM judge is ever
  adopted it stays an explicitly labelled opt-in, off the decision path, and — to preserve the
  offline / air-gapped and "prompts never leave the building" guarantees (WF-ADR-0039) — it must
  run as a **local model co-located in the Wayfinder deployment, never a call to an external
  provider** — the standing constraint, WF-ADR-0043.
- **No change to WF-ADR-0001.** Shadow sampling, judging, evidence statistics, and tripwires all
  run off the request path; the per-request decision stays offline, deterministic, and keyless.
- **No quality *guarantee*.** The evidence report measures judge agreement on sampled traffic with
  stated intervals and abstention. It is recall-imperfect by construction and says so — the
  `blind-eval.md` register, not a marketing claim.
- **No hosted control plane or SaaS.** Self-hosted, bring-your-own-key, as ever.
- **No general RDBMS persistence layer.** Redis first, behind the `StateBackend` protocol;
  Postgres is a possible later backend, not this roadmap.
- **No multi-tenant org hierarchy or SCIM.** Virtual keys + tags remain the attribution unit; OIDC
  governs *operator* access only. *(Repealed by WF-ROADMAP-0011, which makes org identity the
  point; the non-goal stands for this roadmap's own scope.)*
- **No in-process TLS.** Terminate at the ingress; the Helm chart documents the boundary.

## Alternatives Considered

### The enterprise control plane as the headline

SSO/RBAC, shared state, OTel, audit, and a Helm chart as the roadmap's whole story. Everything in
that list is genuinely missing — but it's table stakes, not differentiation: LiteLLM, Kong's AI
gateway, and Portkey ship or are shipping all of it, and clearing procurement checkboxes never
answers *why route through Wayfinder at all*. Demoted rather than rejected: it is Track B here,
because the evidence engine can't run fleet-wide without it.

### Semantic / ML routing quality

Fix the below-random stock scorer with embeddings or a learned semantic classifier, competing
head-on with the vLLM Semantic Router. Rejected on identity: WF-ADR-0001 is the invariant every
shipped feature has been fenced to preserve, an opt-in `ClassifierModel` mode already exists for
those who want a fitted model, and `blind-eval.md` shows calibration-on-your-own-traffic recovers
the value deterministically. This path builds a worse Semantic Router instead of a better
Wayfinder.

### The ecosystem / distribution play

LiteLLM and LangChain plugins, an Envoy filter, Kubernetes Gateway API integration. Real adoption
leverage, but it multiplies *reach*, not *value* — it puts an unproven router in more places.
Sequences naturally after trust is solved, not before.

### Privacy / PII-aware routing as the flagship

Already proposed as WF-ROADMAP-0008, and worth doing — but it is an incremental deterministic gate
on the existing scorer, a checkbox rather than a 10x capability. Complementary, not competing; it
proceeds on its own track.

### A hosted control plane / SaaS

The highest theoretical leverage and the clearest contradiction: Wayfinder's posture is
self-hosted, bring-your-own-key, offline-first (WF-ADR-0039), and "open-source gold standard"
argues for doubling down on that, not hedging it.

**The tiebreaker.** The evidence engine is the only candidate that attacks the actual adoption
blocker — trust — rather than a checkbox; it is assembled mostly from machinery already in-tree
(`judge.py`, `sufficiency.py`, the savings ledger, the `dry_run` seam); it converts the published
negative benchmark into the product's central loop; and it is the "backed by empirical benchmarks
and guidance" goal turned into a feature. No competing router offers "prove it on your own traffic
before routing a single request."

## Success Measures

- **Time-to-evidence:** a fresh install reaches a defensible evidence report in ≤ 7 days at
  ≥ 1,000 requests, following only `docs/evidence-trial.md`.
- **Flip rate:** the fraction of shadow deployments whose report reaches an **enforce** verdict
  after on-traffic calibration — the one number that proves the loop works, and stays honest when
  it doesn't.
- **Fleet-readiness:** a 2-replica Helm deployment passes the shared-budget and tripwire
  integration tests; at least one external reproduction of the published benchmark tables.
- **Zero regression:** `rollout = "enforce"`, `backend = "memory"`, `auth = "vkeys"` defaults keep
  every current deployment byte-identical — verified by the existing suite running unmodified.
- **The artifact in the wild:** an evidence report exported and cited outside this repo — the
  budget-meeting test.

## Sequencing and dependencies

- **1 → 2 → 3 strictly.** The store format precedes the report; the report's verdict machinery
  precedes the tripwire (the tripwire *is* the report's floor applied on a rolling window).
- **Initiative 4's pricing half lands before Initiative 2 is called "defensible."** Cost-delta
  claims want `estimated=false`; until then the report displays its estimated-token fraction.
- **Initiative 4's Redis half precedes fleet-wide Initiative 3.** Tripwire windows and canary
  state must be shared across replicas; single-replica canary can ship earlier.
- **Initiative 5's audit log lands with or before Initiative 3.** Rollout flips without an audit
  trail undermine the trust story. OIDC can trail slightly; OTel and Helm parallelize.
- **Initiative 6 starts immediately.** The judge-validation benchmark has no dependencies and
  de-risks Initiative 2's core assumption — that `HeuristicJudge` agreement predicts real quality.
- Suggested release shape (CalVer point releases, per house convention): **R1** shadow mode +
  judge-validation benchmark; **R2** evidence report + pricing split; **R3** canary/tripwires +
  Redis + audit; **R4** OIDC + OTel + Helm + the operator's guide.

## Related Decisions

- WF-ADR-0001 (deterministic, offline, no-model-call core — the invariant every initiative is
  fenced against)
- WF-ADR-0004 (invocation layer + OpenAI-compatible gateway)
- WF-ADR-0007 (scheduled recalibration — the loop shadow labels feed)
- WF-ADR-0011 / WF-ADR-0014 (metadata-only logging posture — the shadow store's default)
- WF-ADR-0015 (benchmark methodology), WF-DESIGN-0003 (confidence/abstention), WF-DESIGN-0004
  (calibration loop), WF-DESIGN-0007 (savings counterfactual — projected savings extend it),
  WF-DESIGN-0008 (observability/cost dashboard)
- WF-ADR-0017 (cost metadata — split pricing extends it), WF-ADR-0018 (/metrics)
- WF-ADR-0022 (conversation latch — canary assignment rides it)
- WF-ADR-0031 / 0032 / 0033 / 0034 / 0035 (breaker, budgets, cache, rate limiting, virtual keys —
  everything Initiative 4 makes shareable and Initiative 5 governs)
- WF-ADR-0037 (automated sufficiency judge — the reused heart of the evidence engine)
- WF-ADR-0039 (offline-first delivery)
- WF-ADR-0043 (Wayfinder's own model use is local and in-container — the standing constraint behind
  the "no LLM-as-judge, local-only if ever" non-goal)
- WF-ROADMAP-0011 (the deterministic AI governance plane) extends this roadmap: it promotes
  Track B from table stakes to load-bearing prerequisite and repeals the org-hierarchy/SCIM
  non-goal above.
- Future designs, written when the work is scheduled: WF-DESIGN-0013 (shadow capture & comparison
  sampling), WF-DESIGN-0014 (evidence statistics & artifact format), WF-DESIGN-0015 (shared state
  backend).
- Future ADRs, named descriptively — IDs assigned at authoring time: rollout modes
  (shadow/canary/enforce); background comparison sampling; evidence statistics & the flip
  threshold; canary assignment & quality tripwires; pluggable shared state; input/output-split
  pricing & FinOps export; OIDC alongside virtual keys; the audit log; OpenTelemetry as an opt-in
  extra.
