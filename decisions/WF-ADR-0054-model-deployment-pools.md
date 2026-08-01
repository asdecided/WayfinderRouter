---
schema_version: 1
id: WF-ADR-0054
type: decision
status: accepted
date: 2026-08-01
tags: [rust, gateway, enterprise, deployments, pools, failover]
---

# Keep stable model aliases while rotating bounded deployment pools

## Context

An enterprise caller should select a stable Wayfinder model alias, not a
region-specific URL or provider revision. A single alias may nevertheless need
several equivalent upstream deployments for throughput and maintenance. The
gateway already has ordered model failover, but always trying one endpoint
first leaves capacity unused during healthy traffic and couples circuit state
to the whole alias.

## Decision

Allow an OpenAI-compatible model alias to declare up to 32 additional inline
deployments. Each deployment supplies an identifier, endpoint, provider model,
optional credential reference, and bounded relative weight:

```toml
[gateway.models.cloud]
base_url = "https://primary.example/v1"
model = "gpt-5"
deployments = [
  { id = "eu", base_url = "https://eu.example/v1", model = "gpt-5", weight = 2 },
  { id = "backup", base_url = "https://backup.example/v1", model = "gpt-5-mini" },
]
```

The alias's own endpoint is the `default` member with weight one. The gateway
uses deterministic weighted rotation for the first member of each request and
then tries the remaining members in stable order before leaving the alias's
existing failover ladder. Circuit-breaker and upstream-latency state use
`alias#deployment-id` identities, while discovery, routing receipts, billing,
and client-visible `model` names remain the public alias.

Pool members inherit provider kind, locality, cost, and context from the
parent. Pools are therefore capability-homogeneous and are currently limited
to the OpenAI-compatible adapter. A member's credential readiness is resolved
at startup; the alias is ready when any member is ready. Before each delivery,
the gateway nevertheless assesses every concrete member independently for
credential readiness, request context, and the effective privacy/offline
boundary. Rotation and failover can select only from that eligible subset.

## Consequences

- A deployment can spread healthy traffic across equivalent endpoints without
  changing client configuration or deterministic route scoring.
- A failed member is isolated from its siblings' circuit state and can be
  bypassed while cooling down.
- The pool is process-local; Redis shared policy state does not attempt to
  coordinate round-robin cursors or circuit state across replicas. Stable
  alias routing remains correct, while each replica makes its own bounded
  selection.
- The default model shape, native Apple/Codex providers, fallback semantics,
  and public model inventory remain unchanged.

## Rejected alternatives

- **Expose every region as a public model.** This leaks deployment topology and
  forces callers to own failover and reconfiguration.
- **Reuse `fallbacks` as a pool.** Fallback order is intentionally failure-only;
  using it for healthy traffic would change existing cost and reliability
  semantics.
- **Random selection.** Unseeded randomness makes tests, receipts, and incident
  reproduction harder; weighted deterministic rotation is sufficient here.
- **Shared pool cursor in Redis.** That would add coordination and ordering
  latency without making upstream circuit state globally correct; a later
  distributed deployment-health decision can address both together.

## Related

- WF-ADR-0031 — retries, failover, and circuit breakers
- WF-ADR-0052 — workspace-scoped model routing and multi-turn request support
- WF-ADR-0053 — Redis-backed shared policy state
- WF-ROADMAP-0010 — enterprise gateway substrate
