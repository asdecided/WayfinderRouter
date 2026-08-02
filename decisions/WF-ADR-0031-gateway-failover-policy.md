---
schema_version: 1
id: WF-ADR-0031
type: decision
tags: [gateway, reliability, failover, retry, circuit-breaker, cost, invocation]
---

# WF-ADR-0031: Gateway Failover Policy (deterministic, invocation-layer)

## Status

Accepted

## Category

Technical

## Context

The gateway forwards a scored request to one chosen tier's endpoint and, on failure,
gives up: a transport error becomes a `502` and an upstream HTTP error (`429`/`5xx`) is
relayed as-is. There is no retry and no failover (WF-ADR-0004). WF-DESIGN-0010 proposes
the resilience features that daily production reliance needs — but one of them, *crossing
tiers* on failure, changes a property callers may depend on (answer quality, or cost), so
it warrants a decision rather than a silent default.

Two constraints frame the decision:

- **The deterministic core is sacred (WF-ADR-0001).** The scored decision — "this prompt
  is tier T" — is computed once, offline, with no model call. Any failover must therefore
  change only *how a request is delivered*, never *what was decided*. Failover is not a
  second, hidden router.
- **Two failure shapes exist.** Transport failures (timeout, connection refused) raise
  `UpstreamError`; upstream HTTP errors (`429`, `5xx`) are returned as a status. Both must
  be considered "this attempt failed," while ordinary `4xx` (bad request, auth) must not be
  retried.

## Decision

1. **Failover is an invocation-layer concern only; the scored decision is never
   recomputed.** For a given prompt and threshold the decision is identical with or without
   any failover. This reaffirms WF-ADR-0001 and is the property every other rule preserves.

2. **Retries and the circuit breaker are always on** — they change *when* and *whether* we
   call the chosen target, not *which tier* was chosen, so they need no policy:
   - **Retryable**: a transport `UpstreamError`, or a status in `{429, 500, 502, 503, 504}`.
     **Non-retryable**: any other `4xx` (fail fast — retrying a malformed or unauthorized
     request only wastes time).
   - Bounded retries with exponential backoff + jitter; bounds are configurable.
   - A **per-target circuit breaker** (`CLOSED → OPEN` after N consecutive failures →
     one single-flight `HALF-OPEN` probe → `CLOSED`) with a cooldown, so a known-dead
     provider is skipped instead of hammered. An abandoned half-open probe fails closed
     and restarts the cooldown. State is in-memory, per process and shared by buffered and
     streaming delivery. All of it is computed from observed transport outcomes — no model
     call.

3. **Failover across endpoints is governed by an explicit `failover` policy, default
   `same-tier`:**
   - **`same-tier` (default)** — only try alternate endpoints configured for the *chosen
     tier*. The agreed tier is still served, just from a different endpoint; cost and answer
     quality are unchanged. With no alternates configured this degrades to "retry, then the
     existing error" — i.e. no surprise.
   - **`degrade`** — on exhaustion, walk **down** the tier ladder (cheaper). Keeps serving
     and **never raises cost**; may return a weaker answer.
   - **`escalate`** — on exhaustion, walk **up** the ladder (dearer). **Raises cost**;
     strictly opt-in.
   - Set per-gateway (`[gateway] failover = "same-tier" | "degrade" | "escalate"`), with an
     optional per-request override header (`X-Wayfinder-Failover`). The plan is **bounded**:
     candidates are tried in order, one step at a time, until one succeeds or the plan is
     exhausted — never an unbounded walk of the whole ladder.

4. **The default is `same-tier`** because it preserves the routing contract exactly: the
   tier the deterministic core chose is what gets served. `degrade` changes answer quality
   and `escalate` changes cost — each is a property a caller may rely on, so both are
   deliberate opt-ins rather than defaults.

5. **Failover is never silent.** Response headers report the target that served the request
   and whether a failover/degrade occurred (`x-wayfinder-router-served-by`,
   `x-wayfinder-router-failover`), so a `degrade` is observable rather than a silent quality
   drop. Cost and savings accounting (WF-DESIGN-0007) bill the target that *actually* served.

6. **Streaming retries and fails over only before the response is established.** Transport
   failures while opening the stream and retryable upstream statuses receive the same
   bounded retry policy as buffered requests. Once an upstream response has been accepted,
   the target lease travels with that stream and no second request is started: switching
   upstreams after response commitment would risk duplicate work or corrupt the response.
   A mid-stream failure therefore stays a terminal SSE error.

7. **`degrade`-to-local is the same primitive a budget breach will reuse** (WF-ROADMAP-0006
   item 6): "out of headroom → serve from the cheaper tier." Building it here means the
   budget feature inherits it.

## Consequences

- **Production resilience** without touching the decision path — and the determinism is
  testable: assert the scored decision is identical regardless of failover.
- **On-brand degrade**: a router that already knows a cheaper arm can keep serving through
  an outage where a single-target proxy would just `502`.
- **Risk — `escalate` raises spend.** Mitigated by being opt-in, surfaced in headers, and
  later bounded by budgets (WF-ADR-0017, WF-ROADMAP-0006 item 6).
- **Risk — `degrade` returns a weaker answer.** Mitigated by being opt-in and surfaced in
  headers/logs (never silent).
- **Limitation — breaker/retry state is per process.** Multi-process deployments share
  nothing yet; a shared store (e.g. Redis) is a deliberate later option, not v1.
- **Limitation — no mid-stream failover** (inherent to streaming).
- Fully testable with a fake upstream that fails then succeeds — no network, no keys.

## Alternatives Considered

- **`degrade` as the default** — more resilient out of the box, but it silently swaps a
  weaker model in for an answer the caller expected at the chosen tier. Made opt-in instead,
  surfaced via headers.
- **Always escalate on failure** — silently increases spend and can stampede a pricier
  provider during an incident. Rejected as a default.
- **Re-score or learn the failover target** — a model call and/or non-determinism on the
  decision path; breaks WF-ADR-0001. Rejected.
- **Delegate failover to an upstream proxy (e.g. LiteLLM in front)** — concedes a
  table-stakes feature and pushes operational complexity onto users; a router that cannot
  survive a provider hiccup will not be trusted in production. Rejected.
- **Status quo (better error messages, no failover)** — insufficient for daily production
  reliance.

## Success Measures

- A chaos test (kill the chosen upstream) yields graceful behavior matching the configured
  policy, no dropped request, correct cost attribution to the serving target, and an
  **identical scored decision**.
- `escalate` / `degrade` fire **only** when explicitly configured; the `same-tier` default
  never changes cost or answer quality unprompted.
- Retries with bounded backoff demonstrably ride out a transient `429`/`5xx` burst, and the
  breaker stops repeated calls to a downed target.

## Related

- WF-ADR-0001 (deterministic, offline, no-model-call core — preserved by rule 1)
- WF-ADR-0004 (the OpenAI-compatible gateway this extends)
- WF-ADR-0017 (cost metadata — what `escalate` increases and `degrade` avoids)
- WF-DESIGN-0010 (gateway reliability design this ratifies)
- WF-DESIGN-0007 (cost/savings accounting — bills the target that served)
- WF-ROADMAP-0006 (item 4 reliability; item 6 budgets reuse the `degrade` primitive)
