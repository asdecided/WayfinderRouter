---
schema_version: 1
id: WF-ADR-0055
type: decision
status: accepted
date: 2026-08-01
tags: [rust, gateway, enterprise, routing, presets, aliases]
---

# Select bounded operator-owned delivery paths with named routing presets

## Context

Enterprise clients should not need to encode provider URLs, regions, model
revisions, or deployment-pool members in application configuration. Stable
Wayfinder model aliases hide those details, but a client still needs an
explicit way to say “use the coding path” or “use the regulated path” without
turning that policy into a second scoring implementation.

## Decision

Add bounded named routing presets under `[gateway.routes.<name>]`:

```toml
[gateway.routes.coding]
models = ["local-code", "cloud-code"]
```

An OpenAI-compatible request selects a preset with
`model = "@route/coding"`. The router still computes the normal deterministic
score and emits the same route receipt, but the named preset supplies the
ordered delivery candidates. The first available candidate is attempted, and
later candidates are used for the existing transport/`429`/`5xx` failover
semantics. The preset does not add provider credentials, change the score, or
invoke a model to make a routing decision.

The preset's ordered aliases are intersected with the effective workspace and
virtual-key model allowlist before eligibility or delivery. An empty
intersection returns `422 wayfinder_router_destination_ineligible`; a preset
cannot widen a caller's model policy, and a model-policy clamp cannot escape
the selected preset.

Preset names are secret-free visible identifiers, limited to 64 routes with 32
unique aliases per route. Configuration validation rejects empty, duplicate,
or unknown model references. The local operator surface exposes a read-only
`GET /router/routes` inventory. An unknown `@route/...` token fails with a
structured `400 wayfinder_router_unknown_route`; it never silently falls back
to `Automatic`.

The response carries `x-wayfinder-router-route: @route/<name>` and
`x-wayfinder-router-mode: preset`. The public model remains the selected model
alias; deployment pools and per-deployment health stay behind that alias.

## Consequences

- Applications can select reviewed routing policy by a stable name while
  operators retain endpoint and credential ownership.
- Preset ordering is transparent and reproducible, while score semantics and
  automatic routing remain unchanged.
- Presets are local configuration, not a user-editable policy language or a
  remote plugin mechanism.
- The preset candidate list is intentionally bounded and does not expose
  deployment topology or provider account identity.

## Rejected alternatives

- **Put policy in provider model names.** This leaks deployment details and
  makes aliases impossible to rotate safely.
- **Use an LLM to classify `@route` requests.** That violates the model-free,
  deterministic decision boundary.
- **Treat a preset as a new scoring tier.** Presets are explicit delivery
  paths; changing tier thresholds would silently change `Automatic` behavior.
- **Silently fall back when a preset is misspelled.** A typo should be visible
  to the caller instead of unexpectedly sending data to another route.

## Related

- WF-ADR-0001 — deterministic model-free routing
- WF-ADR-0031 — retries, failover, and circuit breakers
- WF-ADR-0052 — workspace-scoped model routing
- WF-ADR-0054 — bounded model deployment pools
- WF-ROADMAP-0010 — enterprise gateway substrate
