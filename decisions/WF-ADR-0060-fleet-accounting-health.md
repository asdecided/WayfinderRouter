---
schema_version: 1
id: WF-ADR-0060
type: decision
status: accepted
date: 2026-08-02
tags: [rust, gateway, enterprise, redis, accounting, admission, health]
---

# Coordinate fleet accounting, delivery admission, and provider health in Redis

## Context

Issue #147 closes the remaining replica-local policy gaps identified during the
enterprise gateway review. Rate limits were already shared, but realized spend,
in-flight delivery capacity, and provider circuit state could still diverge
between replicas. That divergence could overshoot a configured budget, send
too many requests to a degraded provider, or make a healthy replica appear
available while another replica had already opened its circuit.

## Decision

When `[gateway.state].backend = "redis"`, the gateway shares three bounded
policy domains through atomic Lua operations:

1. **Accounting.** A successful turn writes one idempotent request observation
   to global, workspace, virtual-key, route, daily, monthly, and all-time
   ledgers. Realized, baseline, savings, prompt tokens, completion tokens, and
   estimated status are stored as integer fields. Repeating a request ID is a
   no-op, so retries, reconnects, and stream terminal races cannot double
   charge the fleet.
2. **Admission.** Each provider delivery acquires a request-ID lease against
   the configured `max_in_flight` limit. The lease is released on normal
   request completion and expires after a bounded six-hour TTL if a worker
   disappears. Exhaustion is a visible `503` with the `fleet-limit` overload
   reason. Redis errors retain the existing process-local admission path and
   set the degraded health signal.
3. **Provider health.** Each concrete target has a shared failure counter,
   open-until timestamp, and one half-open probe owner. Replicas therefore
   observe the same cooldown and do not stampede a recovering provider. The
   local breaker remains paired with the shared lease for zero-configuration
   memory mode and for degraded Redis operation.

Redis server time and bounded, encoded namespace keys remain authoritative for
   expiry and identity. No prompt, response, credential, provider payload, or
   raw error is written to shared state. The response cache and local JSONL
   evidence sink remain process-local by design.

## Failure and compatibility rules

- Redis is an opt-in enterprise dependency; memory mode preserves existing
  single-process behavior and does not require a database.
- A Redis operation failure never silently grants a budget decision or marks a
  provider healthy. The gateway falls back only to the bounded local primitive,
  sets `wayfinder_state_degraded 1`, and exposes the incident through health
  and metrics.
- A pinned destination still fails closed when its shared or local circuit is
  open. Automatic may continue through the configured route ladder only after
  the shared eligibility check.
- Workspace budgets are configured as
  `[gateway.workspaces.<id>.budget]`; global and virtual-key budgets keep their
  existing locations. All three scopes use the same realized-cost ledger.

## Verification

The contract is covered by unit tests for key/date/cost validation and by a
two-backend live Redis test that proves idempotent accounting, shared spend,
one-slot admission, and single-flight health recovery. The memory backend and
existing gateway parity tests remain unchanged.

## Related

- #146 — enterprise gateway epic
- #147 — fleet-wide accounting, admission, and provider health
- WF-ADR-0051 — bounded delivery concurrency
- WF-ADR-0053 — Redis-backed shared policy state and degraded operation
- WF-ROADMAP-0010 — shared state, token-truthful accounting, and enterprise substrate
