---
schema_version: 1
id: WF-ADR-0069
type: decision
status: accepted
date: 2026-08-18
tags: [linux, packaging, releases, omarchy, supply-chain]
---

# Publish checksummed native Linux Router releases

## Context

The Omarchy integration can build `wayfinder-router` from a pinned source commit,
but that makes a Rust toolchain part of first-run installation. The QML plugin
must not absorb routing or provider execution merely to remove that dependency:
the independently supervised Rust process remains the request-path boundary in
WF-ADR-0068.

A downloadable executable is a stronger supply-chain boundary only when its
source, target, archive layout, and digest are immutable and reviewable. A
mutable latest URL, unverified download, cross-compiled binary, or installer
that replaces an unrelated local Router would weaken that boundary.

## Decision

1. Publishing a GitHub release tagged `router-v<workspace-version>` triggers
   native GNU/Linux builds for `x86_64` and `aarch64` on matching runners.
   There is no manual workflow dispatch or tag-push prerequisite.
2. Each archive contains `wayfinder-router`, `LICENSE`, and `NOTICE` beneath one
   target-named directory. Stable timestamps, ordering, ownership, permissions,
   and gzip metadata make the packaging layer reproducible.
3. Every archive is smoke-tested on its native runner before downloads are attached to the published GitHub
   release. A sibling SHA-256 file is published for independent
   verification.
4. Release tags must match the Rust workspace version, point to `main`, and have
   committed release notes. Existing asset bytes are never replaced: retries retain identical assets
   and reject conflicting content. GitHub immutable releases are incompatible
   with post-publication uploads; the workflow rejects them without changing
   repository settings.
5. A commit-pinned SurfaceCheck gate derives a temporary manifest from the Rust
   workspace version and verifies the release notes and public installation
   facts before packaging. The temporary adapter does not become another
   version source.
6. Downstream installers pin a concrete release URL and the reviewed archive
   digest. They verify the digest before extraction or execution, install only
   to user-owned paths, reuse independent Router installations, and record
   provenance for any binary they own.
7. Installing the binary does not install or start the gateway service. Service
   creation remains an explicit user action, and shell reloads remain outside
   the request path.

## Consequences

- Omarchy users can install a complete Wayfinder runtime without Rust or Cargo.
- The shell plugin still contains no routing implementation, provider
  credential, or in-process gateway.
- A maintainer publishes the release once. Downloads appear automatically after
  both native CI builds pass. The page is public while building; failed builds
  leave downloads unavailable until repaired and rerun. Downstream consumers
  wait for all archives and digests before updating pins.
- The first release targets glibc-based Linux. Other libc or operating-system
  targets require separately built and tested artifacts rather than fallback
  execution of an incompatible binary.

## Related

- WF-ADR-0001 (standalone deterministic router)
- WF-ADR-0038 (local service surface)
- WF-ADR-0046 (Rust-only runtime)
- WF-ADR-0068 (Omarchy Quattro plugin)
