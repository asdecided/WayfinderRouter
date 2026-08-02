---
schema_version: 1
id: WF-ADR-0056
type: decision
status: accepted
date: 2026-08-01
tags: [rust, gateway, enterprise, routing, privacy, capabilities]
---

# Filter provider destinations before delivery

## Context

The shared routing core already defines typed execution boundaries, provider
readiness, context limits, capabilities, and stable exclusion reasons. The
Rust gateway still treated those values as an embedded/mobile-only contract:
its HTTP path could score a request and let a pinned or fallback destination
reach delivery before checking whether the destination was compatible with the
request or allowed by the caller's privacy posture.

That is unsafe for a shared enterprise gateway. A request that requires tools,
images, or streaming must not be sent to a text-only adapter, and an
`on-device-only` request must not reach a hosted endpoint through a fallback.

## Decision

The gateway constructs a secret-free `RoutingRequest` for every chat turn and
uses the shared `assess_destination` contract to gate delivery candidates.
Hard eligibility runs independently of the deterministic complexity score and
is applied to the primary, preset members, configured fallbacks, and outer
failover ladder. For a deployment pool, it is also applied to every concrete
member before weighted rotation; eligibility of the public alias is not a
substitute for eligibility of the endpoint that will receive the request.

The request privacy posture is selected with the bounded
`x-wayfinder-privacy-posture` header:

- `on-device-only` permits only Apple local providers and literal loopback
  endpoints;
- `local-devices` also permits literal private-network/`.local` endpoints;
- `hosted-allowed` is the compatibility default.

The existing gateway `offline` setting and `x-wayfinder-offline` override
always imply `on-device-only`. Responses expose the effective posture through
`x-wayfinder-router-privacy-posture`.

Both OpenAI- and Anthropic-compatible request surfaces preserve this bounded
control contract when entering the shared chat path.

Request requirements are derived from the bounded OpenAI-compatible body:
estimated prompt plus requested output context, image content, tool/function
declarations, and streaming. Provider defaults are conservative for the two
text-only native adapters; non-text surfaces are additionally governed by the
explicit capability contract in WF-ADR-0067 and fail closed until a reviewed
adapter is enabled. Tool declarations remain an explicit provider capability.

Explicit pins and named `@route/<name>` presets fail closed with
`422 wayfinder_router_destination_ineligible` when no eligible destination
remains. Automatic and preset fallback candidates are filtered before the
reliability plan, so fallback cannot cross the selected privacy boundary or a
hard capability exclusion. Existing score, threshold, and credential scopes
remain unchanged.

## Consequences

- Privacy and capability policy is enforced in the runtime rather than being a
  UI-only hint.
- The gateway and embedded Apple runtime share the same exclusion vocabulary
  and boundary semantics.
- A provider with unknown native readiness is not guessed ready; missing
  credentials are excluded before delivery.
- Generic OpenAI-compatible endpoints remain backward-compatible for their
  existing text, tool, and streaming requests; non-text payloads are rejected
  until their modality adapters are independently enabled.
- Explicit pins may now return a specific eligibility error instead of a
  provider-side malformed-request or privacy leak.

## Rejected alternatives

- **Score first, then let delivery decide.** This permits an incompatible
  primary to win and makes privacy dependent on fallback ordering.
- **Treat every non-loopback URL as local.** DNS and public endpoints cannot
  establish a trustworthy local boundary without an explicit private literal.
- **Add an LLM or provider probe to eligibility.** Eligibility must remain
  deterministic, bounded, and free of provider calls on the decision path.

## Related

- WF-ADR-0048 — shared routing-core contracts and exclusion reasons
- WF-ADR-0050 — managed gateway surface boundary
- WF-ADR-0054 — stable aliases and deployment pools
- WF-ADR-0055 — named routing presets
- WF-ROADMAP-0010 — enterprise gateway substrate
