# Compatibility and release evidence

This matrix is the supported surface for the current Wayfinder plugin source. It is deliberately narrower than generic QML, OpenAI, or Anthropic compatibility. The machine-readable authority is [`compatibility.json`](../compatibility.json); CI rejects drift between it, the manifest, installer, coding-agent pins, and this document.

Checked: 2026-08-23.

| Surface | Supported contract | Evidence in this repository |
| --- | --- | --- |
| Wayfinder plugin | `0.3.0`, manifest schema `1` | Local package validation plus the official Omarchy validator |
| Omarchy | Quattro `4.0.0.alpha` at [`f4f3d4c71a0a5c392b20ce05291531881a1b3bfe`](https://github.com/basecamp/omarchy/commit/f4f3d4c71a0a5c392b20ce05291531881a1b3bfe) | CI checks out that exact source commit and runs its `omarchy-plugin-validate` against this plugin |
| Quickshell | Omarchy-packaged `quickshell` `0.3.1` or newer | The pinned Omarchy source contains the reviewed switch to packaged Quickshell at [`2c593dbbaad67698e7b9b0809d082d86540a7a1c`](https://github.com/basecamp/omarchy/commit/2c593dbbaad67698e7b9b0809d082d86540a7a1c); model and package tests run without a graphical shell |
| Router | `router-v2026.8.0` | Native archive download, SHA-256 verification, layout inspection, execution, provenance, no-clobber, and ownership-checked removal tests |
| Linux | glibc `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` | Both release archives are pinned by digest; Router release CI builds and smokes both targets |
| Codex | `0.149.0` | Real streaming `/v1/responses` tool round-trip through a candidate Router |
| Claude Code | `2.1.241` | Real streaming `/v1/messages` tool round-trip through the same candidate Router |

## Evidence levels

- **Release-gated** means the exact version or archive is exercised automatically before its pin can move.
- **Contract-validated** means the plugin is checked against an exact upstream source contract, but a full graphical Omarchy session is not running in hosted CI.
- **Documented** means the operator path is explicit and reversible, but it is not yet sufficient for a release claim.
- **Blocked** means the current source intentionally refuses to claim the capability.

The official Omarchy manifest validator is contract evidence, not a substitute for a real Quickshell desktop smoke. Before a coordinated release, a clean supported Omarchy machine must run the [native smoke gate](native-smoke.md) to prove widget loading, first-run setup, and service survival across a shell restart. Ownership-safe removal remains covered by the installer harness and must be repeated for the coordinated candidate.

## Current lifecycle status

| Operation | Status | Current behavior |
| --- | --- | --- |
| Fresh plugin and Router install | Release-gated | Downloads one checksum-pinned native archive and records exact provenance |
| Install with an existing Router | Release-gated | Uses the existing executable and does not record or replace it |
| Plugin source update | Documented | Omarchy shows the Git diff and fast-forwards the plugin checkout |
| Plugin source rollback | Documented | Disable, select the previously reviewed source commit, validate, then re-enable |
| Router upgrade | Contract-validated | Replaces only a digest-matched plugin-owned on-disk binary through a recoverable atomic promotion; service restart remains explicit |
| Router rollback | Contract-validated | Swaps to the verified last-known-good on-disk binary and retains the displaced version as the next rollback target |
| Project profiles | Unavailable in the install pin | The `2026.8.0` Router reports the capability as unavailable; the panel fails closed |

The lifecycle harness proves ownership refusal, atomic promotion, last-known-good retention, rollback, and recovery both before and after the promotion rename. Upgrade and rollback remain short of **release-gated** until a later coordinated candidate moves the real Router pin and exercises two checksum-verified release archives end to end.

## Pin movement rule

Do not edit one version in isolation. A coordinated candidate changes the plugin version, Router release and both archive digests, exact Omarchy source evidence, coding-agent versions where necessary, this matrix, troubleshooting/rollback notes, and the Router repository mirror in reviewed pull requests. CI never moves these pins automatically.
