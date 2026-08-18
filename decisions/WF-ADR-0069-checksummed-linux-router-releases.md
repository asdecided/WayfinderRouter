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

1. Router tags named `router-v<workspace-version>` build native GNU/Linux
   archives for `x86_64` and `aarch64` on matching GitHub-hosted architectures.
2. Each archive contains `wayfinder-router`, `LICENSE`, and `NOTICE` beneath one
   target-named directory. Stable timestamps, ordering, ownership, permissions,
   and gzip metadata make the packaging layer reproducible.
3. Every archive is smoke-tested on its native runner before a draft GitHub
   release is created. A sibling SHA-256 file is published for independent
   verification.
4. Release tags must match the Rust workspace version, point to `main`, and have
   committed release notes. Published release assets are immutable; reruns may
   replace assets only while a release remains a draft.
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
- Linux release publication adds two native CI builds and a manual review step:
  GitHub creates a draft release, which a maintainer publishes only after
  checking its assets and digests.
- The first release targets glibc-based Linux. Other libc or operating-system
  targets require separately built and tested artifacts rather than fallback
  execution of an incompatible binary.

## Related

- WF-ADR-0001 (standalone deterministic router)
- WF-ADR-0038 (local service surface)
- WF-ADR-0046 (Rust-only runtime)
- WF-ADR-0068 (Omarchy Quattro plugin)
