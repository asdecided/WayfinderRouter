---
schema_version: 1
id: WF-ADR-0072
type: decision
status: accepted
date: 2026-08-19
tags: [control-plane, policy, persistence, recovery, concurrency]
---

# Persist policy versions before adding an administrative API

## Context

WF-ADR-0071 defines immutable policy versions, activation snapshots, rollback,
and last-known-good data-plane operation. The first lifecycle implementation is
process-local. An administrative API built directly on that state would lose
drafts and the selected policy on restart, and concurrent administrators could
overwrite each other without detection.

The durable source must not become a request-path dependency. It must also keep
provider credentials outside the policy repository and give future shared
backends one explicit concurrency contract.

## Decision

Wayfinder defines a backend-independent policy repository contract:

1. Draft and validated policy documents use their existing `wpv1-` content
   identity. Stored versions are immutable and are rehashed and revalidated on
   every load.
2. Each small immutable activation-head record contains a monotonic generation,
   the active policy/snapshot receipt, and at most 256 preceding activation
   receipts. The highest generation is current.
3. Activation and rollback update the head only when the caller's expected
   generation matches durable state. A stale administrative writer fails
   without changing the selected policy.
4. The initial single-process implementation stores immutable JSON versions in
   a private directory and creates each generation atomically after durable
   file flush. It retains earlier heads as recovery evidence.
5. Restart recovery verifies both the selected head and its referenced policy.
   If either is corrupt, Wayfinder may recover only the preceding verified
   head. It does not guess, skip arbitrarily through history, or accept a hash
   mismatch.
6. The data plane continues to read only its in-memory immutable `AppState`.
   Repository reads and writes occur on the future administrative/control
   path, never while routing a request.
7. Policy storage contains routing policy and secret-free administrative
   identity only. Provider credentials remain at the existing delivery
   boundary.

The atomic-file backend is a local reference implementation, not the fleet
storage decision. A future shared repository must preserve the same immutable
version, compare-and-set generation, bounded history, and verified recovery
semantics.

No administrative HTTP API, UI, hosted service, database dependency, fleet
propagation protocol, or provider credential store is added here.

## Consequences

The next administrative slice can create, validate, activate, and roll back
through a stable durable contract. A restart can recover the selected policy
without consulting a provider or placing storage on the request path.

The local backend assumes one control-plane writer process. Multi-replica
control-plane operation requires a shared implementation with an atomic
compare-and-set primitive; it cannot treat the local file lock as distributed
coordination.

## Related

- WF-ADR-0035 (virtual keys and workspaces)
- WF-ADR-0045 (Rust gateway lifecycle ownership)
- WF-ADR-0057 (operator OIDC boundary)
- WF-ADR-0071 (versioned policy lifecycle)
- Issue #160 (operator control plane)
