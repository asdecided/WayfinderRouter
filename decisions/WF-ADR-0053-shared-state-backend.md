---
schema_version: 1
id: WF-ADR-0053
type: decision
status: accepted
date: 2026-08-01
tags: [rust, gateway, enterprise, redis, rate-limits, multi-replica]
---

# Use a Redis-backed shared policy-state contract with loud local degradation

## Context

The Rust gateway now supports workspaces and virtual-key request/token limits,
but the counters are process-local. Two replicas can therefore each admit a
full window, defeating a workspace or fleet-wide policy. The desktop default
must remain zero-configuration and must not acquire a mandatory database.

## Decision

Introduce a `StateBackend` contract in the Rust gateway. The contract owns
fixed-window request and token counters and exposes admission, token recording,
and prompt-free rate snapshots. `memory` remains the default backend and uses
the existing deterministic limiter semantics. `redis` is an opt-in backend
configured by:

```toml
[gateway.state]
backend = "redis"
url = "redis://redis.internal:6379"
namespace = "production"
```

Redis operations use Lua and Redis server time so replicas do not depend on
their process start times. Each global, workspace, and virtual-key dimension
has a bounded namespace key. Redis keys use a versioned
`wayfinder:v1:<base64url(namespace)>` root and separately encoded workspace,
virtual-key, and audit-stream components. User-selected values therefore cannot
create delimiter collisions, and limit and audit domains cannot alias the
legacy raw-key layout. A request rejected by one dimension is not sent upstream.
The first implementation keeps the existing local accounting ledger
and response cache unchanged; those are separate state migrations and must not
be implied by selecting Redis.

If Redis becomes unavailable, the gateway does not drop requests or expose a
connection error. It marks the policy degraded, falls back to its bounded
process-local limiter, and exports `wayfinder_state_degraded 1`. A successful
subsequent backend operation clears the degraded state. Operators must treat
that period as a policy-consistency incident, not as equivalent fleet-wide
enforcement.

## Consequences

- A two-replica deployment can enforce one shared RPM/TPM window per configured
  dimension when Redis is healthy.
- Redis is connected and pinged before serving, so a bad URL fails startup
  rather than creating a silently half-configured deployment.
- The default desktop and embedded paths retain the current process-local
  behavior and dependency posture.
- Redis URLs and backend errors are never echoed through responses or metrics.
- A deployment upgrading from the pre-v1 raw Redis key experiment must roll all
  gateway replicas as one coordinated policy cutover. Old fixed-window keys
  expire naturally; legacy audit keys remain separate evidence until the
  operator's retention policy removes them.
- Cross-replica budgets, savings ledgers, response caches, and distributed
  in-flight semaphores remain explicit follow-on work; this ADR does not claim
  them.

## Rejected alternatives

- **A mandatory Redis dependency.** This would add operational friction to the
  local-first desktop product and change the default behavior.
- **Best-effort local counters with no signal.** Operators would be unable to
  distinguish healthy fleet-wide enforcement from a degraded replica.
- **Client-side or wall-clock window IDs.** Replica clock/start-time skew can
  split one logical window; Redis server time keeps the shared boundary in one
  authority.
- **A separate policy implementation in every handler.** The backend contract
  keeps the rate semantics in one Rust policy layer and leaves routing
  decisions model-free.

## Related

- WF-ADR-0035 — hashed virtual keys and per-key attribution
- WF-ADR-0050 — separate managed model data plane from local operator surfaces
- WF-ADR-0051 — bounded delivery concurrency
- WF-ADR-0052 — workspace-scoped model routing and multi-turn request support
- WF-ROADMAP-0010 — shared state, token-truthful accounting, and enterprise substrate
