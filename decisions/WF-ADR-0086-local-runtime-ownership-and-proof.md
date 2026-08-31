---
schema_version: 1
id: WF-ADR-0086
type: decision
status: accepted
date: 2026-08-31
tags: [omarchy, local, runtime, evidence, privacy]
---

# Keep local runtime ownership with the operator and prove delivery

## Context

Wayfinder can generate a local configuration without proving that a model is
actually loaded. A first-run surface must distinguish configured, discoverable,
and proven states without installing software, downloading weights, scanning
the network, or claiming that a hosted fallback was local.

## Decision

1. Runtime installation, model downloads, endpoint selection, and policy
   activation remain explicit operator actions. Wayfinder may inspect only the
   fixed literal-loopback catalogs and an endpoint the operator supplies.
2. A local first-run proof consists of one bounded fixed public request through
   the running Router, a non-empty normalized response, and the matching
   terminal prompt-free receipt. The receipt must identify an on-device or
   local-network execution boundary.
3. The proof is observational and single-shot. It never stores prompt or
   response content, credentials, repository paths, tool arguments, or private
   reasoning, and it never changes routing configuration.
4. A missing runtime, unloaded model, unavailable receipt, hosted boundary,
   failed response, or ambiguous route is visibly `not-ready`; no health or
   discovery response may promote it to ready.
5. The acceptance surface is repeatable on a real operator-owned runtime. A
   mock HTTP fixture may test parsing and bounds, but it cannot be presented as
   evidence that a model was loaded or that local inference succeeded.

## Consequences

Omarchy gets a truthful setup gate while the operator retains control of
runtime and model state. CI can validate the bounded contract and a real local
runtime can supply the final loaded-model evidence; environments without one
must report that proof as unavailable rather than passing by simulation.

## Related

- WF-ADR-0001 (offline deterministic decision path)
- WF-ADR-0068 (Omarchy Quattro plugin boundary)
- WF-ADR-0070 (native activation surface)
- WF-ADR-0085 (prompt-free outcomes and review-only policy proposals)
- WF-ADR-0087 (first local inference evidence)
- WF-ROADMAP-0017 (Omarchy-first delivery)
