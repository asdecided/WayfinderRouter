---
schema_version: 1
id: WF-ADR-0046
type: decision
status: accepted
date: 2026-07-24
tags: [rust, runtime, packaging, ci, distribution]
---

# Rust is Wayfinder's sole router and gateway runtime

## Context

The Rust migration reached production parity for the deterministic core,
gateway, provider delivery, operational controls, service integration, native
macOS boundaries, and the embedded Desktop helper. Continuing to ship the old
implementation created two authorities, kept a second runtime in the command
path, and made unrelated native pull requests depend on obsolete package tests.

Wayfinder Desktop is the primary product. It already embeds and verifies the
Rust helper, while native Swift surfaces own Chat and setup.

## Decision

Rust is the sole production implementation of the Wayfinder router and gateway.

- `wayfinder-router` never launches or delegates to another runtime.
- Capabilities report the native command set, no delegated commands, and
  `gateway_ready = true`.
- The legacy Python source, packaging metadata, tests, executable benchmarks,
  fixture generators, PyPI workflow, and Python container are removed.
- CI validates Rust, Swift, Docker, and the retained JavaScript preview contract.
- The Docker image builds and runs the Rust binary.
- The old `recalibrate`, `webchat`, `ui`, `chat`, `onboard`, `judge`, `init`,
  and `doctor` command surfaces are removed. They fail closed as unsupported
  until a reviewed native replacement is justified.
- WF-ADR-0017 restores only `calibrate --mode threshold --objective min-cost`
  as a bounded native Rust command. It parses labelled JSONL, scores with the
  pure core, and emits a routing fragment. No legacy runtime, other objective,
  or automatic config mutation returns.
- WF-ADR-0050 restores only `keys new` as a bounded native Rust command for
  managed data-plane virtual-key creation. It prints the plaintext once and
  persists nothing; no legacy runtime or broad key-management API returns.
- Desktop-owned setup and bounded configuration commands remain native.

Checked compatibility vectors are retained as immutable migration evidence.
Historical names or data that describe the former wire contract do not create a
runtime, build, CI, or distribution dependency.

### Retirement audit amendment (2026-08-01)

The final current-facing coexistence language is removed. Release procedures do
not offer a Python rollback, issue intake does not direct users to PyPI, and
contract tests describe the checked-in vectors as frozen migration evidence
rather than a live Python oracle. Historical ADRs, roadmaps, changelog entries,
and benchmark records remain intact as dated evidence. They are not supported
runtime instructions.

## Consequences

There is one implementation and one set of production checks. Native changes no
longer fail because an obsolete package linter changes behavior. A machine does
not need Python to build, run, containerize, test, or release Wayfinder.

This is intentionally a breaking removal of legacy standalone commands and PyPI
distribution. Reintroducing any removed workflow requires a native Rust or Swift
contract and its own tests; a hidden compatibility fallback is not allowed.

The deterministic decision path remains offline, keyless, and free of delivery
dependencies (WF-ADR-0001). This decision does not broaden the credential broker,
change Automatic routing preferences, or make Apple a global default.

## Mobile amendment

WF-ADR-0047 and WF-ADR-0048 extend the Rust-only decision to native mobile:
Wayfinder extracts a pure routing library from this workspace and embeds it in
iPhone and iPad rather than running the gateway executable or recreating the
algorithm in Swift. The macOS product remains gateway-first.
