---
schema_version: 1
id: WF-ADR-0071
type: decision
status: accepted
date: 2026-08-19
tags: [control-plane, policy, lifecycle, snapshots, audit]
---

# Add a versioned policy lifecycle before an administrative interface

## Context

Wayfinder can hot-reload complete gateway configurations and retain the last
known-good state after a parsing failure. It does not yet have stable contracts
for reusable routing policy, bindings, administrative mutations, activation,
or rollback. Building issue #160's administrative interface first would make
that interface the accidental contract.

The data plane must continue to route when a future control plane is absent or
unavailable. Provider credentials must also remain at the delivery boundary;
they are not control-plane identity and must never enter policy or audit data.

## Decision

The Rust gateway owns a bounded `wf-policy-v1` lifecycle:

1. A policy document contains routing-only profiles and bindings from client,
   workspace, or virtual-key identity to a profile. Existing stock lexicon
   profiles and `/router/profiles` keep their current meaning.
2. Drafting creates a content-addressed, immutable policy version. Validation
   is a distinct typestate transition and parses every routing profile before
   the version can be activated.
3. Activation installs one complete immutable `AppState` in the existing
   last-known-good holder. Each activation has a unique snapshot id. Rollback
   restores the preceding policy version as another new snapshot.
4. The request path reads only the locally held snapshot. Failed preparation
   or an unavailable control plane cannot replace it.
5. Lifecycle mutations take a secret-free administrative identity. Policy
   contracts contain no provider credential fields. Provider secrets continue
   to resolve only at the existing delivery boundary.
6. Routed-request receipts and prompt-free audit events carry both the policy
   version and snapshot identity.
7. Every policy has one explicit default profile. Request resolution is
   deterministic: an authenticated virtual-key binding wins over an
   authenticated workspace binding, which wins over a trusted host-supplied
   client binding, followed by the default profile. The public HTTP gateway
   does not accept caller-asserted client identity.

No administrative HTTP API, UI, persistence engine, hosted execution path, or
fleet rollout mechanism is added here. Issue #160 may become an interface over
these contracts after they are stable.

## Consequences

One running data plane can activate a validated version, expose its identity on
requests, survive a failed control-plane update, and roll back without being
restarted. Version ids identify immutable content; snapshot ids distinguish
separate activations, including rollback.

Managed request receipts also identify the resolved profile. Existing
single-profile documents infer that sole profile as their default; documents
with multiple profiles must name the default explicitly. Unmanaged local
configuration keeps its existing response shape and routing behaviour.

The first implementation keeps lifecycle storage in process. A durable source
may provide versions later, but it cannot become a request-path dependency.

## Related

- WF-ADR-0001 (standalone deterministic router)
- WF-ADR-0035 (virtual keys and workspaces)
- WF-ADR-0045 (Rust gateway lifecycle ownership)
- WF-ADR-0057 (operator OIDC boundary)
- WF-ADR-0070 (bounded native activation surface)
- Issue #160 (operator control plane)
