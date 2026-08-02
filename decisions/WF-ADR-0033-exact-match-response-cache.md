---
schema_version: 1
id: WF-ADR-0033
type: decision
tags: [gateway, cache, response, cost, reliability, privacy, invocation]
---

# WF-ADR-0033: Exact-Match Response Cache (deterministic, opt-in, memory default)

## Status

Accepted

## Category

Technical

## Context

The gateway forwards a scored request to its tier's endpoint on every call — even when the
request is byte-for-byte identical to one it just served. Identical repeats are common in the
traffic Wayfinder now sits in front of: agentic coding tools (Claude Code, via the `/v1/messages`
adapter, WF-DESIGN-0011), evaluation/CI runs, and tight dev loops. An exact-match response cache
turns those repeats into instant, free replays — and a cache hit is the strongest possible
savings signal (WF-DESIGN-0007). This is WF-ROADMAP-0006 item #10.

Two constraints frame the decision:

- **The deterministic core is sacred (WF-ADR-0001).** A cache changes only *delivery*; it never
  re-scores and never makes a model call. Semantic/embedding caching is therefore explicitly out
  of scope — it needs a model call — and only the exact-match subset is built.
- **A response cache stores response *bodies*.** Every prior in-memory surface is metadata-only
  by decree (WF-ADR-0011/0014: the `/router` ring and `/metrics` hold "decision metadata only,
  never prompt text"). A cache's *value* is the completion text. Hashing the request into the key
  protects the lookup key (the prompt), not the stored value. So a cache is precisely the
  body-capture category WF-DESIGN-0008 governs: opt-in, off by default, retention-bounded.

## Decision

1. **Exact-match only; the scored decision is never recomputed.** A request is cacheable only if
   it is *contractually deterministic* — non-streaming, `temperature` 0/absent, `top_p` 1/absent,
   `n` 1/absent, no `seed`, no `tools`/`tool_choice`, no non-empty `logit_bias`, and all message
   `content` plain strings. Anything else passes straight through, uncached. This guarantees a hit
   can never differ from a fresh call in a way the caller asked for.

2. **Offline, no model call** on hit or miss (WF-ADR-0001). The key is `SHA-256` of a canonical
   JSON of the request, keyed on the **served upstream model id** (not the inbound routing
   directive), so two routes that resolve to the same upstream share an entry and a different
   model never replays another's answer. Pure arithmetic; reuses the `pricing.table_version` idiom.

3. **First response-body store → governed by WF-DESIGN-0008, not WF-ADR-0014.** The cache is the
   project's first sanctioned store of response-body text. It is therefore **off by default**,
   **memory-backed by default**, bounded by an LRU `max_entries`, a `max_bytes` ceiling, and a
   `ttl`, and **purged on disable**. Cached bodies are **never logged** and **never surfaced** in
   `/router/recent`, `/metrics`, or the `X-Wayfinder-Debug` payload; only the hit/miss header and
   aggregate counters are exposed. An explicit bounded Redis mode is specified separately by
   WF-ADR-0061 and carries the same opt-in retention rules.

4. **A cache hit is free.** A hit records realized cost `0` and does **not** advance the savings
   ledger or the budget's `spent()` — no upstream tokens were bought, so charging for them would
   be untruthful and would let a hot cache trip a budget that costs nothing. The cost a hit
   *avoided* (the chosen tier's price for the stored turn) is reported on a **separate** metric
   (`wayfinder_router_cache_avoided_cost_total`), kept distinct from the always-frontier routing
   savings so the two counterfactuals are never conflated.

5. **Never cache a poisoned success.** Many OpenAI-compatible upstreams return HTTP 200 with an
   error-shaped or empty body under load. The cache stores only a real `200` JSON completion with
   non-empty string content, no top-level `error`, and no `tool_calls`. The store keys on the
   model that *actually* served, so a failover turn populates the served model's key (never the
   chosen model's).

6. **Streaming is excluded in v1.** The streamed path reconstructs text from `delta.content` only;
   replaying a synthesized stream would drop tool-call, `finish_reason`, role, and `usage` frames
   and could hand a Messages-API client a structurally broken stream. Streaming requests pass
   through uncached; revisit if demand appears.

7. **Never silent.** Every cacheable response carries `x-wayfinder-router-cache: hit | miss`, and
   a hit also reports `x-wayfinder-router-served-by`. The cache is a `[gateway.cache]` block
   (`enabled`, `ttl`, `max_entries`, `max_bytes`) that round-trips through `dump_gateway_toml` and
   hot-reloads like the rest of the gateway config.

## Consequences

- **Instant, free repeats** for the idempotent traffic that dominates eval/CI/dev and agentic
  loops, with a hit reported as avoided cost — reinforcing the savings wedge.
- **One layer covers both endpoints**: placed in `chat_completions`, it serves `/v1/chat/completions`
  and (via the adapter's internal re-entry) `/v1/messages` with no second implementation.
- **Determinism is preserved and testable**: the score header is unchanged on a hit; a hit makes
  no upstream call and leaves the circuit breaker untouched.
- **Risk — staleness.** A cached answer can outlive a model/config change. Mitigated by the TTL,
  purge-on-disable, and the opt-in posture (the operator accepts bounded staleness when enabling).
- **Risk — body retention.** Completions live in memory while enabled. Mitigated by off-by-default,
  hashed keys (no prompt plaintext stored), in-memory only, the `max_bytes` ceiling, and never
  logging/surfacing bodies.
- **Limitation — memory default.** The cache is in-memory and per worker unless the operator
  explicitly selects the bounded Redis mode in WF-ADR-0061. There is still no on-disk tier.
- **Limitation — no per-model TTL, no force-refresh header, no streaming/cross-mode** in v1. A
  force-refresh header was rejected because the chat endpoint is unauthenticated (it would be a
  write/probe primitive); revisit after virtual keys (WF-ROADMAP-0006 #5) add an auth boundary.

## Alternatives Considered

- **Semantic / embedding cache** — needs a model call to embed; breaks WF-ADR-0001. Out of scope;
  exact-match is the compatible subset.
- **Cache any temperature ("a hit is a valid prior sample")** — silently removes the variation a
  sampling caller asked for. Rejected; only deterministic requests are cached.
- **Cache streaming via reconstructed SSE** — lossy (drops tool/finish/usage frames), can break
  Messages-API clients. Rejected for v1.
- **Charge a hit the tier price (so budgets stay "honest")** — untruthful: no tokens were spent,
  and it would let a hot cache trip a zero-cost budget. Rejected; a hit is free, avoided cost is a
  separate metric.
- **On-disk persistence** — a persisted body store is a far larger privacy commitment; deferred to
  a WF-DESIGN-0008-aligned follow-up.
- **A request-header force-refresh/no-store control** — an unauthenticated write/probe primitive on
  the open chat endpoint. Deferred until there is an auth boundary.

## Success Measures

- With `[gateway.cache] enabled = true`, a second identical deterministic request is served from
  the cache (`x-wayfinder-router-cache: hit`) with no second upstream call and an **identical
  scored decision**.
- A non-deterministic request (temperature > 0, tools, streaming) is never cached.
- An HTTP-200 error/empty body is never stored or replayed.
- A hit adds nothing to `realized`/budget `spent()`; the avoided cost shows on the cache metric.
- Disabling the cache purges retained bodies immediately.

## Related

- WF-ADR-0001 (deterministic, offline, no-model-call core — preserved by rules 1–2)
- WF-ADR-0011 / WF-ADR-0014 (metadata-only posture this consciously extends to opt-in body capture)
- WF-DESIGN-0008 (the opt-in / retention-bounded framing this cache adopts)
- WF-DESIGN-0007 (savings accounting — a hit reports avoided cost, kept separate)
- WF-ADR-0031 (circuit breaker — untouched by a hit) / WF-DESIGN-0011 (the adapter a hit also covers)
- WF-ADR-0061 (optional shared Redis cache and retained-data boundary)
- WF-ROADMAP-0006 (item #10: exact-match response cache)
