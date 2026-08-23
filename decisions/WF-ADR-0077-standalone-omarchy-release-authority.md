---
schema_version: 1
id: WF-ADR-0077
type: decision
status: accepted
date: 2026-08-23
tags: [omarchy, release, mirror, compatibility, supply-chain]
---

# Make the standalone Omarchy plugin the release authority

## Context

WF-ADR-0068 made `integrations/omarchy-wayfinder` a root-ready plugin source.
Wayfinder later established `asdecided/omarchy-wayfinder` as the marketplace
repository and flagship product surface. Both trees continued to change, but no
mechanical contract identified which one governed releases or detected drift.

A stale mirror can pass Router CI while the marketplace ships different QML,
installation, removal, or credential handling. Automatically pushing between
repositories would hide the review boundary and require broader write
credentials than validation needs.

## Decision

1. `asdecided/omarchy-wayfinder` is the canonical plugin release authority. A
   Router change required by Omarchy lands in the portable core first; the
   standalone plugin then consumes it in a separate review.
2. `integrations/omarchy-wayfinder` is a complete, byte-for-byte snapshot of one
   reviewed standalone commit, including its tests and workflow source. It is
   not an independent development branch.
3. The Router repository records the exact 40-character standalone commit
   outside the mirrored directory. CI checks out that commit read-only and
   compares the complete trees, excluding only Git metadata.
4. Mirror synchronization remains an explicit pull request after the
   corresponding standalone change merges. CI detects drift but never pushes,
   merges, tags, or changes a release pin.
5. A coordinated release is blocked unless the standalone commit, in-tree
   mirror, plugin manifest version, Router artifact version, archive digests,
   compatibility evidence, and rollback notes all describe the same candidate.

## Consequences

- Router CI now proves which reviewed marketplace tree it validates.
- The nested integration remains directly publishable and preserves the full
  evidence surface instead of a hand-selected subset.
- Cross-repository work has one additional synchronization review, but no
  release can silently combine unrelated Router and plugin states.
- This refines WF-ADR-0068's source-location decision without changing its
  shell/runtime, credential, or removal boundaries.

## Related

- WF-ADR-0068 (Omarchy Quattro plugin boundary)
- WF-ADR-0069 (checksummed Linux Router releases)
- WF-ADR-0073 (Omarchy-first portable core and one release train)
- WF-ROADMAP-0017 (Omarchy-first release hardening)
