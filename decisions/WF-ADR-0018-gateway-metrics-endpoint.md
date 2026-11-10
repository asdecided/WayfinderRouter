---
schema_version: 1
id: WF-ADR-0018
type: decision
tags: [gateway, observability, metrics, prometheus, operations]
---

# WF-ADR-0018: Gateway Metrics Endpoint

## Status

Accepted

> Savings-persistence amendment (2026-11-10): the Rust gateway also exports the counter
> `wayfinder_router_savings_persist_failures_total` (savings-ledger writes that failed and will be retried).

## Category

Technical

## Context

The gateway exposes `GET /router` and `GET /router/recent` — a bounded in-memory
ring of the last N routing decisions, metadata only, never prompt text
(WF-ADR-0014). That answers "is routing working, and where?" for a human at a
glance, but it is not a metrics backend: it is bounded, not cumulative, has no
histograms, and is not scrapeable. Anyone running the gateway in production expects
a Prometheus-scrapeable surface for dashboards and alerting. It is table stakes.

## Decision

Add `GET /metrics` in the Prometheus text exposition format, **hand-rolled with no
new dependency**. The format is trivial to emit, and keeping it dependency-free
holds the gateway's surface to `fastapi` / `uvicorn` / `httpx`.

Counters and histograms live in memory for the process lifetime and are
incremented at the **same point in `chat_completions` where decisions and errors
are already recorded** (alongside the `recent` ring), so the endpoint inherits
WF-ADR-0014's metadata-only, never-prompt-text stance by construction.

Series:

- `wayfinder_router_requests_total{model,mode}`
- `wayfinder_router_decision_latency_seconds` — the scoring time, the metric where
  a structural router wins outright (sub-millisecond, no model call)
- `wayfinder_router_upstream_latency_seconds{model}`
- `wayfinder_router_upstream_errors_total{model}`
- `wayfinder_router_config_reload_failures_total`
- `wayfinder_router_build_info{version}` (a constant `1` gauge)

The endpoint is off the scored path: a pure read of in-memory counters, no key, no
model call, no network.

## Consequences

### Positive

- Real observability for production, complementing the human dashboard, and it
  quantifies the sub-millisecond decision-latency claim with data.
- Zero added dependencies.

### Negative

- The hand-rolled exposition format is a little code to maintain, though it is
  small and well-specified.

### Risks

- Label cardinality — `model` and `mode` are bounded, so this is safe.
- Counters reset on restart; this is normal for Prometheus, which handles counter
  resets.

## Alternatives Considered

### Use `prometheus_client`

#### Disadvantages

- A dependency for a format we can emit in a few dozen lines. The gateway keeps its
  dependency surface minimal. It remains a drop-in replacement if richer needs
  arise.

### Rely on the existing `/router/recent` ring

#### Disadvantages

- A bounded ring is a human view, not a metrics backend: not cumulative, not
  scrapeable, and without histograms.

## Success Measures

- `GET /metrics` returns `text/plain`; the named series appear and increment after
  requests; no prompt text leaks; the output scrapes clean with `promtool`.

## Related Decisions

- WF-ADR-0014 (the decision-recording hook and metadata-only stance this reuses)
- WF-ADR-0004 / WF-ADR-0013 (the gateway and its operational surface)
- WF-ADR-0001 (the boundary preserved — the endpoint is off the scored path)
