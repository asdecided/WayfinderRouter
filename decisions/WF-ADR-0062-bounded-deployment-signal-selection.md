---
schema_version: 1
id: WF-ADR-0062
type: decision
status: accepted
date: 2026-08-02
tags: [rust, gateway, enterprise, deployments, health, throughput, cost]
---

# Select interchangeable deployments with bounded runtime signals

## Context

WF-ADR-0054 introduced stable public aliases with bounded weighted deployment
pools. Weighted rotation is predictable, but equivalent deployments can have
different current latency, time-to-first-token, throughput, availability, rate
limit, capacity, and price characteristics. A fleet needs a way to use those
signals without turning routing into a model call, changing the scored model
decision, or creating an unbounded telemetry store.

## Decision

Add an optional `deployment_selection` policy to an OpenAI-compatible model
alias:

```toml
[gateway.models.cloud]
base_url = "https://primary.example/v1"
model = "gpt-5"
deployments = [
  { id = "eu", base_url = "https://eu.example/v1", model = "gpt-5", weight = 2, cost_per_1k = 0.01 },
  { id = "backup", base_url = "https://backup.example/v1", model = "gpt-5", cost_per_1k = 0.02 },
]

[gateway.models.cloud.deployment_selection]
strategy = "latency" # weighted | latency | throughput | availability | cost | capacity
observation_ttl = 60
max_cost_per_1k = 0.02
```

The compatibility default remains `weighted`. The other strategies only
reorder concrete members that have already passed the normal capability,
readiness, context, privacy, offline, access-policy, and reliability checks.
They can never select another public alias, tier, privacy boundary, capability
class, or named route. A pinned public alias therefore remains pinned.

The gateway records only bounded prompt-free observations keyed by the
sanitized `alias#deployment-id` identity. Each identity retains at most 32
recent samples and the process retains at most 1,024 identities. Samples older
than `observation_ttl` are ignored. A sparse, stale, or incomplete signal set
falls back to the existing weighted order. Stable deployment identifiers are
the final tie-break.

Buffered deliveries record round-trip latency, success/availability, rate
limits, provider capacity pressure, and measurable completion throughput.
Streaming deliveries additionally record time to first token and completion
throughput. Signals are local observations; Redis-backed provider circuits and
fleet admission remain authoritative and are checked before any delivery.

`max_cost_per_1k` is a hard member filter. A member without a known effective
price is excluded while a ceiling is active; if no member remains, the alias
fails closed rather than violating the ceiling. The `cost` strategy uses the
same effective price (deployment override, otherwise alias price) and falls
back to weighted order when a comparable price is unknown.

Each successful response exposes the concrete deployment and bounded selection
reason through `x-wayfinder-router-deployment` and
`x-wayfinder-router-deployment-selection`. Prometheus telemetry reports
selection reasons, time-to-first-token, completion throughput, and capacity
pressure without request or credential content.

## Consequences

- The authoritative scored model decision remains offline, deterministic,
  keyless, and explainable.
- Operators can choose a simple strategy per interchangeable pool without
  rewriting routes or exposing provider topology to callers.
- Runtime observations are intentionally process-local and bounded. They do
  not pretend to be a cross-replica health authority; shared circuit state and
  fleet admission continue to provide that authority when configured.
- A newly started replica behaves like the existing weighted pool until it has
  enough fresh local observations. This makes rollout and stale-data behavior
  deterministic and conservative.

## Rejected alternatives

- **Feed signals into the scored routing core.** This would make the core
  stateful and risk changing tier/privacy/capability decisions.
- **Ask a model to choose a deployment.** It adds latency, credentials, and
  non-determinism to a decision that must remain offline and explainable.
- **Store every request sample or label.** Unbounded histories and high-cardinality
  labels are an operational and privacy risk.
- **Let local observations override shared circuits.** A replica-local signal
  cannot prove fleet-wide health, so the existing shared reliability authority
  remains a hard gate.

## Related

- WF-ADR-0031 — retries, failover, and circuit breakers
- WF-ADR-0054 — stable model aliases and weighted deployment pools
- WF-ADR-0056 — hard destination eligibility
- WF-ADR-0060 — shared fleet accounting, admission, and provider health
- WF-ROADMAP-0010 — enterprise gateway substrate
