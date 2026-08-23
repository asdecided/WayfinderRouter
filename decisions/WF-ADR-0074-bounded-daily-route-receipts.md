---
schema_version: 1
id: WF-ADR-0074
type: decision
status: accepted
date: 2026-08-23
tags: [omarchy, gateway, receipts, observability, cancellation]
---

# Expose bounded delivery receipts for daily Omarchy inspection

## Context

The Omarchy panel reads the prompt-free `/router/recent` ring. That surface
records the route selected by policy, its score, mode, and active policy
identity, but it cannot currently distinguish the selected route from the
destination that actually served after failover. It also cannot state where
content executed or whether a stream completed, failed, or was cancelled.

The panel must not infer those facts from endpoint names, duplicate routing
logic in QML, retain prompts, or become a persistent audit system.

## Decision

1. `/router/recent` remains a bounded, in-memory, prompt-free ring and gains
   additive delivery fields: `served_by`, `execution_boundary`, `outcome`,
   `http_status`, and `error_type`.
2. The initial route decision is recorded before delivery as today. The Router
   enriches that same request entry only after a concrete delivery target is
   selected.
3. `execution_boundary` is calculated from the concrete selected deployment,
   using the same eligibility boundary applied before routing. An exact-cache
   hit is `on-device` because no provider receives content for that request.
4. Buffered delivery records `succeeded` or `failed` with the observed HTTP
   status. A stream records `streaming` after upstream establishment, advances
   to `succeeded` or `failed`, and records `cancelled` if the downstream body is
   dropped before a terminal result.
5. Failure metadata is a stable Wayfinder error type only. Provider response
   bodies, prompts, tool arguments, credentials, and raw error text never enter
   the recent ring.
6. Fields that cannot be proved are omitted. A pre-delivery failure may have an
   outcome and error type without a served destination or execution boundary.
7. Omarchy and other local inspectors may render this contract but must not
   alter it or use it as routing input.

## Consequences

- The daily surface can truthfully show selected versus served model, actual
  execution boundary, completion state, cancellation, and a bounded failure
  code.
- Existing `/router/recent` consumers remain compatible because the schema is
  additive and absent values are omitted.
- The ring is operational state, not durable audit history. Restarting the
  Router clears it, and the existing 200-entry bound remains authoritative.
- Remediation copy still belongs in the consuming interface; the Router emits
  stable facts rather than user-interface prose.

## Related

- WF-ADR-0001 (standalone deterministic Router)
- WF-ADR-0038 (local service surface)
- WF-ADR-0068 (Omarchy plugin boundary)
- WF-ADR-0073 (Omarchy-first portable core)
- WF-ROADMAP-0017 (Omarchy-first delivery)
