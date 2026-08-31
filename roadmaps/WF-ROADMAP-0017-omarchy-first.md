---
schema_version: 1
id: WF-ROADMAP-0017
type: roadmap
status: accepted
date: 2026-08-22
tags: [omarchy, linux, developers, coding-agents, local-models, distribution]
---

# Roadmap: Wayfinder as the native model router for Omarchy

## Release thesis

An Omarchy developer installs one plugin and gains one inspectable local routing
layer for supported coding agents, local runtimes, account-backed models, and
API providers. Wayfinder chooses deterministically, keeps execution boundaries
truthful, and makes each decision visible in the Omarchy shell.

The flagship experience is Omarchy-native. The routing engine remains a
portable Rust release with no Omarchy dependency.

## Current position

The machine-independent v1 implementation is now landed across the portable
Router and standalone Omarchy plugin: first-run lifecycle, diagnosis, verified
coding-agent contracts, project profiles, prompt-free value evidence, and the
local-runtime proof contract are all in reviewed code.

The in-tree Omarchy mirror is synchronized to standalone plugin commit
`330e71c`; it carries plugin `0.3.3` and the checksum-pinned Router `1.0.0`
release.

The remaining v1 release claim is environmental, not another large feature. A
clean Omarchy machine still needs to run the graphical smoke, exercise a real
loaded local model, and record the complete install/service/upgrade/rollback
cycle. The plugin currently consumes the released Router `1.0.0`; changes that
land after that release move into the next reviewed Router pin rather than
being implied by the plugin wording.

## Release gate

The Omarchy-first release is complete when a clean supported machine can:

1. install the plugin and checksum-verified Router without Cargo;
2. create a no-clobber local policy and install the user service;
3. detect supported local runtimes and explain missing provider requirements;
4. connect at least two verified coding agents through reviewable changes;
5. route both buffered and streaming requests through one loopback endpoint;
6. show service health, selected model, profile, execution boundary, and reason
   in a native bar panel;
7. apply a repository-specific profile without changing unrelated projects;
8. upgrade, roll back, and remove plugin-owned files without touching
   independently owned Router state or credentials.

## Workstream 1: installation and lifecycle — code complete; live gate pending

- make marketplace installation the canonical path;
- keep native `x86_64` and `aarch64` archives pinned by reviewed SHA-256;
- initialize a no-clobber starter policy from the plugin;
- install, start, stop, restart, and diagnose the `systemd --user` service;
- define version compatibility between plugin, Router, Omarchy, and Quickshell;
- prove update, migration, rollback, partial-install recovery, and explicit
  ownership-checked removal.

Exit: a clean Omarchy install reaches a healthy Router without Rust, Cargo,
manual unit files, or provider credentials entering QML. The installer and
lifecycle harness pass; the clean-machine graphical smoke remains the release
gate.

## Workstream 2: developer environment discovery — code complete; live model proof pending

- detect Ollama, LM Studio, and configured OpenAI-compatible local endpoints;
- detect supported coding agents without reading their secrets;
- show provider readiness as requirements and remediation, not inferred routing;
- keep newly connected destinations outside Automatic until explicitly enabled;
- extend `wayfinder-router doctor` with Omarchy, user-service, socket, config,
  release provenance, and client-connection checks.

Exit: diagnosis explains every missing step without printing a credential or
calling a provider merely to score a request. The Router's fixed-catalog and
bounded first-inference contracts are merged; a genuinely loaded local model
still needs an operator-run proof.

## Workstream 3: verified coding-agent integrations — CI complete; live Omarchy launch pending

Begin with Codex, Claude Code, OpenCode, and Pi. For each client, record:

- supported endpoint and authentication configuration;
- model-name and compatibility expectations;
- streaming, cancellation, tools, and error behavior;
- exactly which files or environment values a proposed connection changes;
- a reversible manual path when automatic editing is not safe.

A client enters the supported matrix only after a repeatable smoke test. Support
claims remain narrower than generic OpenAI or Anthropic compatibility.

Exit: at least two agents pass a real end-to-end route test and every documented
connection can be reviewed and reversed. The five client smokes pass against
the reviewed Router; a real graphical Omarchy launcher exercise remains.

## Workstream 4: Omarchy-native daily surface — implemented

The bar and panel expose:

- Router and user-service health;
- current release and update compatibility;
- recent route, selected destination, profile, score, and reason;
- local-versus-hosted distribution and bounded savings evidence;
- actionable failures and direct remediation;
- explicit modes such as Automatic and Local Only where they map to existing
  Router policy rather than hidden QML state.

The panel never becomes a second Router, credential store, or request-path
dependency.

Exit: the common lifecycle and inspection tasks require no terminal, while
advanced configuration remains transparent and file-based. The panel is now
the native setup, health, route, project-value, and remediation surface.

## Workstream 5: project-aware routing — implemented

- resolve repository policy from trusted local launch context rather than
  caller-spoofable headers;
- reuse versioned profiles for coding, planning, review, lightweight work, and
  user-defined modes;
- make repository policy additive and bounded by the user's global eligibility
  and privacy posture;
- surface the active profile and policy version in receipts;
- retain a last-known-good local snapshot and explicit rollback.

Exit: two repositories can use different profiles concurrently without a fleet
control plane, hosted identity, or request-path storage dependency. The Router
and plugin use the ownership-marked, last-known-good profile lifecycle.

## Workstream 6: release and community hardening — final evidence pending

- keep the standalone plugin repository and in-tree mirror synchronized;
- test against supported Omarchy/Quickshell revisions;
- publish a compatibility matrix and concise troubleshooting guide;
- sign or checksum every distributed Router artifact;
- use issues and discussions to select integrations from demonstrated developer
  demand;
- keep telemetry absent by default; measure adoption through public release,
  marketplace, issue, and contribution signals.

Exit: each release has pinned artifacts, native smoke evidence, rollback notes,
and an explicit supported-environment matrix. The pins, mirror, checksum
contracts, and documents are in place; the native Omarchy record is the last
release-gated evidence item.

## Pull-request sequence

1. [x] strategy ADR, roadmap, front-door copy, and issue reset;
2. [x] first-run initialization and user-service lifecycle;
3. [x] Omarchy-aware doctor and local-runtime discovery;
4. [x] verified coding-agent integrations;
5. [x] panel receipt, value, and remediation surface;
6. [x] project-aware profile resolution;
7. [ ] update, rollback, compatibility CI, and release documentation — live
   Omarchy evidence is still required;
8. [ ] additional integrations selected from demonstrated demand.

## Next release actions

1. Run the native Omarchy smoke on a clean Quattro machine: install, enable,
   setup, launch a supported agent, inspect the bar, restart the shell, and
   verify service survival.
2. Run the loaded-model probe against one supported local runtime and attach
   the prompt-free terminal receipt to the release record.
3. Refresh the compatibility status from contract-validated to release-gated
   only after those records exist.
4. Tag and publish the coordinated plugin release; keep the Router release,
   plugin pin, mirror, and compatibility matrix in the same reviewable chain.

One PR owns one reviewable boundary. Router behavior lands in the portable
Router repository before the plugin consumes a pinned release.

## Non-goals

- organization hierarchy, SCIM, RBAC, chargeback, or fleet administration;
- Kubernetes or Helm as the flagship deployment;
- an Omarchy-specific routing fork or QML scorer;
- silent edits to coding-agent configuration;
- importing credentials from agent, browser, shell-history, or unrelated files;
- promising support for clients that cannot use a compatible custom endpoint;
- broad Apple product development during this roadmap;
- mandatory product telemetry.

## Prioritization

Every proposed issue and PR is evaluated in this order:

1. Can a new Omarchy user reach one successful routed agent request?
2. Can a daily user understand and control the chosen route?
3. Does it improve a verified developer workflow on one workstation?
4. Is the portable-core work strictly required for one of the above?

If the answer is no to all four, the work remains outside the active roadmap.

## Related

- WF-ADR-0073 (Omarchy-first, portable-core strategy)
- WF-ADR-0077 (standalone plugin release authority and verified mirror)
- WF-ADR-0068 (Omarchy shell/runtime separation)
- WF-ADR-0069 (native Linux distribution)
- WF-ADR-0070 (activation surface)
- WF-ADR-0071 (versioned local policy)
