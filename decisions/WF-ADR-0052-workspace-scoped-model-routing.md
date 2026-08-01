---
schema_version: 1
id: WF-ADR-0052
type: decision
status: accepted
date: 2026-08-01
tags: [rust, gateway, enterprise, workspaces, multi-model, multi-turn]
---

# Scope virtual keys through bounded workspaces without owning conversations

## Context

An enterprise gateway serves multiple applications, environments, and groups
through one model endpoint. Per-key model lists and rate limits already isolate
individual callers, but operators must repeat common policy for every key.
That becomes error-prone when several callers share the same approved model
estate or throughput envelope.

Wayfinder also already supports ordinary multi-turn OpenAI and Anthropic chat:
the client sends the complete transcript on every request, `route_on` chooses
which transcript projection is scored, and the optional sticky latch derives a
high-water route from that transcript. The gateway does not need to retain
conversation content to make multi-turn routing work.

## Decision

Add bounded `[gateway.workspaces.<id>]` policy namespaces. A workspace may
define:

- a public configured-model allowlist; and
- one shared process-local RPM/TPM limit.

A virtual key opts into exactly one workspace with `workspace = "<id>"`.
The key inherits the workspace model policy and limiter. A key-level `models`
list may narrow its workspace list but configuration validation rejects any
attempt to broaden it. Gateway, workspace, and key limiters all apply; the
tightest remaining limit is returned to the caller. Multiple keys in one
workspace consume the same workspace counters.

Configured names under `[gateway.models.<name>]` are the stable, public model
aliases returned by model discovery and accepted in requests. Each alias maps
to one provider model and may carry ordered same-tier `fallbacks`. Upstream
provider identifiers remain an implementation detail.

Successful inference returns `x-wayfinder-router-workspace` when a key is
workspace-scoped. Model discovery and routing use the same effective allowlist.
Workspace ids are bounded to 128 visible ASCII bytes; the number of workspaces
and virtual keys is independently bounded at 256.

Multi-turn remains stateless and content-minimizing. Wayfinder forwards the
complete caller-supplied transcript unchanged, applies the same deterministic
route/latch policy, and retains no server-side thread. Workspace policy is
derived only from the authenticated key, never from an untrusted conversation
or request field.

## Consequences

- Operators can express LiteLLM-style project or OpenRouter-style workspace
  boundaries without introducing an organization hierarchy or hosted control
  plane.
- One shared workspace limiter protects aggregate process-local capacity while
  key limits can impose stricter caller-specific ceilings.
- A stable Wayfinder alias can move between upstream model revisions without
  changing client configuration.
- Conversation isolation does not depend on sticky server sessions, affinity
  cookies, or retained prompt content.
- Workspace counters are still process-local. This decision does not satisfy
  the two-replica Redis gate in WF-ROADMAP-0010 and must not be described as a
  fleet-wide quota.
- Budgets and accounting remain per key plus gateway-wide until the shared
  state and ledger-dimension work lands.

## Rejected alternatives

- **Store conversation transcripts in the gateway.** This creates content
  custody, migration, affinity, and replica-consistency obligations without
  improving OpenAI/Anthropic multi-turn compatibility.
- **Let request bodies select a workspace.** A caller could escape its key's
  policy boundary.
- **Allow key lists to broaden workspace policy.** Child policy must narrow,
  never expand, its parent boundary.
- **Call workspaces teams or departments.** WF-ROADMAP-0011 reserves those
  identity and ledger concepts for the later governed organization plane.
- **Claim distributed limits now.** Shared enforcement requires the planned
  `StateBackend` and two-replica Redis evidence.

## Related

- WF-ADR-0021 — multi-turn routing scope
- WF-ADR-0022 — conversation latch
- WF-ADR-0031 — provider fallbacks
- WF-ADR-0034 — rate limiting
- WF-ADR-0035 — virtual keys
- WF-ADR-0051 — bounded delivery concurrency
- WF-ROADMAP-0010 — shared state and enterprise substrate
- WF-ROADMAP-0011 — organization identity and governance plane
