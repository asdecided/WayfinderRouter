# Wayfinder Desktop v0.1.0 release evidence

Status: template; the release is not approved until this record describes the final downloaded
artifact and every required gate is complete. Do not record secrets, tokens, credential files,
private prompts, or unsanitized account information.

## Artifact identity

| Field | Evidence |
|---|---|
| Commit | Pending |
| Tag | Pending (`desktop-v0.1.0` or RC) |
| Desktop version/build | Pending |
| ZIP SHA-256 | Pending |
| Downloaded release URL | Pending |
| Mac model / architecture | Pending / Apple Silicon arm64 |
| macOS version | Pending |
| Developer ID common name / Team ID | Pending |
| Notary submission ID/status | Pending |
| `/Applications/ChatGPT.app` / Codex version tested | Pending |
| Apple system-model availability | Pending |

## Archive and trust verification

| Check | Command/evidence | Result |
|---|---|---|
| Checksum matches published value | `shasum -a 256 Wayfinder.zip` | Pending |
| Final ZIP extracts cleanly | `ditto -x -k Wayfinder.zip <dir>` | Pending |
| All four executables are exactly arm64 | `script/verify_release_bundle.sh <dir>/Wayfinder.app` | Pending |
| Bundle/helper/manifest versions agree | verifier output | Pending |
| Manifest/capability architecture agrees | verifier output | Pending |
| Nested signatures and Team IDs agree | `codesign --verify --deep --strict --verbose=2` | Pending |
| Stapled ticket validates | `xcrun stapler validate` | Pending |
| Gatekeeper accepts extracted app | `spctl --assess --type execute --verbose=2` | Pending |

## Clean-account lifecycle

Record the observed state and bounded recovery for each scenario:

- [ ] Fresh install and setup with no prior config.
- [ ] Existing hand-authored config is detected and preserved.
- [ ] Set Up Later, interrupted setup, retry, repair, and already-complete setup.
- [ ] Gateway install, one launchd owner, `/healthz`, UI quit, gateway crash/restart, logout/login,
  sleep/wake, and app relaunch.
- [ ] Missing, locked, denied, rotated, and deleted Keychain items with no secret output.
- [ ] Upgrade/reinstall and forced rollback preserve config, history, ledger/cache state, and
  Keychain items.
- [ ] Rollback to the previous verified app bundle leaves one Rust gateway label/port owner.

## Apple Foundation Models

Run on eligible macOS 26+ Apple Silicon hardware against the final signed app:

- [ ] Availability and not-ready/ineligible states are truthful.
- [ ] Buffered and streaming local replies complete through the bundled gateway.
- [ ] Cancellation and gateway/XPC restart remain bounded.
- [ ] Delivery works while the menu-bar UI is closed.
- [ ] Copied, ad-hoc, wrong-Team, malformed, oversized, and timed-out broker interactions fail closed
  with sanitized errors.
- [ ] On an eligible never-configured Mac, Apple Local is preselected only after a live `available`
  response and still requires confirmation; existing configuration and Chat's `Automatic`
  destination remain unchanged.

## ChatGPT authentication and Sol delivery

Use the separately installed, compatible, correctly signed `/Applications/ChatGPT.app` version
recorded above:

- [ ] Release discovery ignores development overrides and rejects sibling, unsigned, wrong-owner,
  wrong-Team, incompatible, or malformed helpers.
- [ ] Browser/device login, refresh, cancellation, logout, reauthentication, and model discovery work.
- [ ] A configured Sol model completes buffered and streaming Chat through the Rust gateway.
- [ ] Stop, Busy, timeout, process EOF/restart, sleep/wake, signed-out, expired, and usage-limited
  states remain bounded and do not poison provider health incorrectly.
- [ ] Offline mode excludes ChatGPT; an explicitly pinned unavailable ChatGPT destination never
  silently falls back.
- [ ] Adversarial prompts cannot invoke tools, approvals, commands, file reads/writes, project/home
  access, browser/apps/plugins/skills, or sandboxed extra network activity.
- [ ] Wayfinder output, logs, accessibility, crash state, and diagnostics contain no token or auth
  file material.

## Native fidelity

- [ ] `docs/desktop-fidelity.md` is complete for this exact artifact.
- [ ] Reference screenshots are attached for required Light/Dark and accessibility configurations.
- [ ] No P0/P1 finding remains open.

## Automated verification

Record workflow URLs and exact local commands/results for:

- [ ] Rust format, tests, docs, Clippy, compatibility, audit/policy checks.
- [ ] Swift tests on macOS 14 and macOS 26 Apple Silicon runners.
- [ ] Ad-hoc arm64 release-bundle build and deterministic architecture-policy test.
- [ ] Signed/notarized desktop release workflow.

## Approval

| Role | Name | Date | Decision |
|---|---|---|---|
| Builder | Pending | Pending | Pending |
| Fidelity reviewer | Pending | Pending | Pending |
| Maintainer | Pending | Pending | Pending |
