# Wayfinder Router 2026.8.1 for Linux

This release publishes the native Rust `wayfinder-router` gateway for direct
installation on glibc-based Linux systems, including Omarchy.

## Assets

- `wayfinder-router-x86_64-unknown-linux-gnu.tar.gz`
- `wayfinder-router-aarch64-unknown-linux-gnu.tar.gz`
- one matching `.sha256` file for each archive

Each archive contains the Router executable plus its Apache-2.0 `LICENSE` and
`NOTICE`. The release workflow builds and smoke-tests each executable on a
matching native GitHub runner. Downstream installers must pin this release and
verify the reviewed SHA-256 digest before extracting or executing the binary.

The Router remains an independent local process. Installing an executable does
not create or start a service, change routing policy, or add provider
credentials. The standalone Omarchy plugin will pin these exact assets only
after their checksums have been reviewed.

## Included Router changes

- Added a versioned, last-known-good policy lifecycle with deterministic
  profile resolution and explicit rollback.
- Added authenticated project profiles plus canonical GitHub repository
  `project setup`, `project status`, and `project rollback` commands. Project
  capabilities are stored only as SHA-256 hashes, and setup does not overwrite
  unowned configuration.
- Added bounded daily route receipts that distinguish the selected route from
  the destination that actually served a request without storing prompts,
  provider bodies, credentials, or raw errors.
- Added bounded workstation diagnosis and deterministic connection recipes for
  Codex, Claude Code, and OpenCode.
- Added Codex Responses tool-call parity while preserving the existing routing,
  privacy, credential, budget, and accounting boundaries.
- Added the native service lifecycle and smoke-test contracts required by the
  independently released Omarchy plugin, including recoverable install,
  update, repair, restart, and uninstall behavior.
- Replaced the silent all-local developer defaults with one evidence-backed
  `0.01` starter cut across unconfigured routing, generated project profiles,
  and automatic two-arm presets. The release gate runs the compiled Router over
  the complete 154-prompt independent corpus: 122 route high (90/94 hard) and
  32 remain local. This is a transparent bootstrap baseline, not a universal
  accuracy claim; explicit policy and native calibration remain authoritative.

Existing native `min-cost` threshold calibration and the bounded `init`,
`doctor`, `connect`, and `open` activation surface remain included.
