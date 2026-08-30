# Compatibility and release evidence

This matrix is the supported surface for the current Wayfinder plugin source. It is deliberately narrower than generic QML, OpenAI, or Anthropic compatibility. The machine-readable authority is [`compatibility.json`](../compatibility.json); CI rejects drift between it, the manifest, installer, coding-agent pins, and this document.

Checked: 2026-08-26.

| Surface | Supported contract | Evidence in this repository |
| --- | --- | --- |
| Wayfinder plugin | `0.3.2`, manifest schema `1` | Local package validation plus the official Omarchy validator |
| Omarchy | Quattro `4.0.0.alpha` at [`f4f3d4c71a0a5c392b20ce05291531881a1b3bfe`](https://github.com/basecamp/omarchy/commit/f4f3d4c71a0a5c392b20ce05291531881a1b3bfe) | CI checks out that exact source commit and runs its `omarchy-plugin-validate` against this plugin |
| Quickshell | Omarchy-packaged `quickshell` `0.3.1` or newer | The pinned Omarchy source contains the reviewed switch to packaged Quickshell at [`2c593dbbaad67698e7b9b0809d082d86540a7a1c`](https://github.com/basecamp/omarchy/commit/2c593dbbaad67698e7b9b0809d082d86540a7a1c); model and package tests run without a graphical shell |
| Router | `router-v2026.8.1` | Native archive download, SHA-256 verification, layout inspection, execution, provenance, no-clobber, and ownership-checked removal tests |
| Linux | glibc `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` | Both release archives are pinned by digest; Router release CI builds and smokes both targets |
| Codex | `0.149.0` | Real streaming `/v1/responses` tool round-trip through a candidate Router |
| Claude Code | `2.1.241` | Real streaming `/v1/messages` tool round-trip through the same candidate Router |
| OpenCode | `1.18.21` | Real streaming `/v1/chat/completions` `bash` round-trip, upstream error propagation, and disconnect cancellation through the same candidate Router, with its auxiliary title requests counted separately |
| Pi | `0.84.3` | Real streaming `/v1/chat/completions` `bash` round-trip, structured upstream error propagation, and disconnect cancellation through the same candidate Router |
| Aider | `0.86.1` | Real streaming `/v1/chat/completions` one-file edit through the same candidate Router, with updates, analytics, auto-commits, and repository maps disabled |

## Evidence levels

- **Release-gated** means the exact version or archive is exercised automatically before its pin can move.
- **Contract-validated** means the plugin is checked against an exact upstream source contract, but a full graphical Omarchy session is not running in hosted CI.
- **Documented** means the operator path is explicit and reversible, but it is not yet sufficient for a release claim.
- **Blocked** means the current source intentionally refuses to claim the capability.

The official Omarchy manifest validator is contract evidence, not a substitute for a real Quickshell desktop smoke. Before a coordinated release, a clean supported Omarchy machine must run the [native smoke gate](native-smoke.md) to prove widget loading, first-run setup, and service survival across a shell restart. Ownership-safe removal remains covered by the installer harness and must be repeated for the coordinated candidate.

## Current lifecycle status

| Operation | Status | Current behavior |
| --- | --- | --- |
| Standard Omarchy install | Contract-validated | `omarchy plugin add … --enable`, then the explicit in-panel setup action invokes only the installed plugin's checksum-pinned Router bootstrap; the native smoke remains the graphical release gate |
| Fresh plugin and Router install | Release-gated | Downloads one checksum-pinned native archive and records exact provenance |
| Install with an existing Router | Release-gated | Uses the existing executable and does not record or replace it |
| Plugin source update | Documented | Omarchy shows the Git diff and fast-forwards the plugin checkout |
| Plugin source rollback | Documented | Disable, select the previously reviewed source commit, validate, then re-enable |
| Router upgrade | Contract-validated | Replaces only a digest-matched plugin-owned on-disk binary through a recoverable atomic promotion; service restart remains explicit |
| Router rollback | Contract-validated | Swaps to the verified last-known-good on-disk binary and retains the displaced version as the next rollback target |
| Project profiles | Available in the install pin | The `2026.8.1` Router exposes authenticated local project profiles; the panel still fails closed when the capability is absent |

The automated lifecycle harness proves ownership refusal, atomic promotion,
last-known-good retention, rollback, and recovery both before and after the
promotion rename. Upgrade and rollback remain **contract-validated** until an
operator records a complete archive cycle on Omarchy.

After a coordinated plugin source pins Router `1.0.0`, run the real gate
from that installed plugin source:

```sh
bash scripts/record-router-release-cycle.sh \
  --from 2026.8.1 \
  --to 1.0.0
```

The command verifies the plugin-owned provenance, performs
`2026.8.1 → 1.0.0 → 2026.8.1 → 1.0.0`, restarts and health-checks the
user service at every transition, checks the Omarchy shell, and ends on the
candidate. It writes a mode-0600, host-identity-free
`wf-omarchy-router-cycle-v1` record under the user's home directory. Attach
that record to the release evidence before changing the compatibility status
to release-gated.

`2026.8.1` is the immutable final Router DateVer release. Router `1.0.0` and
later use SemVer. The lifecycle validates strict numeric SemVer cores and uses
exact versions plus reviewed checksums as identities; it never orders versions
across the numbering change.

## Pin movement rule

Do not edit one version in isolation. A coordinated candidate changes the plugin version, Router release and both archive digests, exact Omarchy source evidence, coding-agent versions where necessary, this matrix, troubleshooting/rollback notes, and the Router repository mirror in reviewed pull requests. CI never moves these pins automatically.
