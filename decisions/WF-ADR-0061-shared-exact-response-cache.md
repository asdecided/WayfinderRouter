---
schema_version: 1
id: WF-ADR-0061
type: decision
status: accepted
date: 2026-08-02
tags: [rust, gateway, enterprise, redis, cache, response, privacy, retention]
---

# Add an optional bounded Redis exact-response cache while keeping memory the default

## Context

WF-ADR-0033 established an exact-match response cache for deterministic,
buffered requests. It is deliberately opt-in and bounded because its values
are response bodies rather than prompt-free telemetry. That cache was local to
one process, which is safe but makes a replicated gateway miss the same answer
on every worker. Issue #149 asks for a shared tier without weakening routing,
authentication, privacy, or retention guarantees.

## Decision

Keep `memory` as the zero-configuration cache backend. An operator may opt into
the shared tier only when the policy state backend is also Redis:

```toml
[gateway.state]
backend = "redis"
url = "rediss://redis.internal:6379"
namespace = "production"

[gateway.cache]
backend = "redis"
enabled = true
ttl = 300
max_entries = 1024
max_bytes = 67108864
```

The configuration parser rejects a Redis cache paired with a memory state
backend. This keeps one authenticated, namespaced Redis authority for the
shared cache and the other fleet policy domains; it does not make Redis a
requirement for desktop or single-process use.

The shared cache uses the existing exact-match eligibility rules. Streaming,
tools/tool choice, non-zero sampling, unsupported message content, and other
nondeterministic requests bypass both lookup and storage. Codex/account-backed
managed requests remain excluded by the existing cacheability contract.

Each key is a SHA-256 digest of a versioned canonical request envelope. The
envelope includes the cache schema version, virtual-key partition, effective
privacy posture, public route, served provider model, and deterministic request
projection. A deployment-specific generation digest, derived from the active
gateway configuration and cache schema, is carried into the Redis namespace
metadata. A generation change invalidates old entries atomically before a new
entry can be read or written. No prompt text is used as a Redis key component.

Redis stores the complete buffered response as an opaque, versioned value. One
Lua operation uses Redis server time to perform get/recency bookkeeping; a
second atomically replaces an entry, applies TTL, tracks body bytes, and evicts
the oldest values until both `max_entries` and `max_bytes` are satisfied. A
third operation purges the namespace when shared retention is disabled. Values
are bounded by the gateway request/body limits and are never written to logs,
metrics, route receipts, audit events, or ordinary app data.

The cache remains downstream of the normal request path. Authentication,
privacy-posture filtering, hard capability checks, deterministic route
selection, budgets, and delivery admission still run before a lookup. A cache
hit is free and does not mutate realized-cost accounting or provider health.
The hit/miss response header and aggregate cache metrics retain their existing
semantics.

## Failure and privacy rules

- A Redis cache read, write, decode, or purge failure is a cache miss/no-store;
  it never bypasses authentication, routing, privacy, budget, or admission and
  never fails an otherwise deliverable request.
- Redis degradation is visible through the existing shared-state health signal;
  operators must treat a cache outage as lost reuse, not as permission to relax
  policy.
- A shared response cache is retained prompt/response data. Operators using
  zero-data-retention or equivalent policy must leave it disabled. Production
  deployments use TLS-validated `rediss://` and Redis encryption/access
  controls appropriate to the data; the gateway does not claim that Redis
  makes hosted responses on-device or private.
- Cache partitions include the effective virtual-key and privacy posture, so a
  response cannot cross tenants or an execution-boundary policy. A route/model
  change or config generation change cannot replay an old provider result.
- Disabling the cache purges the shared namespace on startup/reconfiguration
  when Redis is reachable. TTL and bounded eviction still cap retained data if
  a process exits before the purge completes.

## Consequences

- Replicas can reuse the same deterministic buffered answer while preserving
  the local default and existing desktop dependency posture.
- Redis now holds response bodies when this explicit mode is enabled; this is a
  larger privacy commitment than WF-ADR-0033's in-memory mode and must be
  documented in deployment/privacy reviews.
- Cache misses during Redis degradation add no request failure, but the gateway
  loses shared reuse until the backend recovers.
- Generation invalidation avoids stale cross-deployment values at the cost of
  discarding the old generation's retained bodies.

## Verification

- Config tests cover memory defaults, Redis/state-backend coupling, and TOML
  round trips.
- Shared-cache tests cover replica visibility, tenant/generation isolation,
  TTL expiry, LRU entry bounds, byte bounds, and malformed-value rejection.
- Existing deterministic cache tests continue to prove streaming/tools and
  provider/account exclusions and exact response replay.

## Related

- #149 — enterprise shared exact-response cache
- #146 — enterprise gateway epic
- WF-ADR-0033 — deterministic exact-match response cache (memory default)
- WF-ADR-0053 — Redis-backed shared policy state and degraded operation
- WF-ADR-0060 — fleet accounting, admission, and provider health
- WF-ROADMAP-0010 — shared state, token-truthful accounting, and enterprise substrate
