---
schema_version: 1
id: WF-ROADMAP-0016
type: roadmap
status: accepted
date: 2026-07-24
tags: [ios, ipados, v0.2.0, v0.2.1, swiftui, rust, routing, providers, pairing]
---

# Roadmap: standalone native Wayfinder v0.2 for iPhone and iPad

## Release thesis

Wayfinder runs natively wherever the user is. Each Apple device contributes the
capabilities it can legitimately execute; a paired Mac expands the available
set rather than making the mobile app possible.

The mobile product combines one deterministic router with on-device execution,
direct approved providers, local threads, explicit privacy boundaries, and
optional trusted-host destinations.

## Status

Accepted for phased implementation. Phase 0 documentation is the first pull
request. No UI, provider, FFI, or pairing implementation belongs in that PR.

## Releases

### v0.2.0 — standalone iOS and iPadOS

Release-gating scope:

- native iPhone and iPad app, requiring iOS/iPadOS 18 or later;
- authoritative embedded Rust routing core;
- direct provider execution without a Mac or local gateway;
- API-key credentials held in Keychain;
- generalized account/authentication framework;
- Kimi account auth only if WF-QUAL-0001 becomes approved;
- Apple Foundation Models when the live device reports it available;
- Chat, threads, destinations, settings, route receipts, streaming,
  cancellation, recovery, retention, export, and deletion;
- enforced on-device, local-network, and hosted privacy boundaries.

### v0.2.1 — optional paired Mac provider

- QR/manual pairing and nearby discovery;
- authenticated encrypted transport and revocable device trust;
- normalized destination/route inventory;
- Mac-owned Chat execution and reconciliation;
- Mac-local and desktop-helper destinations;
- explicit per-host Automatic opt-in.

Pairing may be developed in parallel but cannot delay or weaken v0.2.0.

## Product requirements

### First launch

Standalone choices lead:

```text
Welcome to Wayfinder

Use AI from this iPhone or iPad

[ Use Apple On-Device ]     when currently available
[ Connect an Account ]
[ Add an API Key ]

More ways to use Wayfinder
[ Connect a Mac ]
```

The user may skip every option and enter a useful no-destination state.
Connecting a hosted provider does not itself select Hosted Allowed.

### Information architecture

iPhone launches into Chat and keeps the transcript as the dominant surface.
Threads, Destinations, and Settings are reached from a leading navigation
drawer rather than a persistent bottom tab bar. Each section keeps independent
`NavigationStack` state behind that shell.

iPad uses `NavigationSplitView`: threads and shortcuts in the persistent
sidebar and transcript/composer in the primary detail. Route detail is opened
from a compact receipt, not reserved as a permanently visible inspector.
Single-window lifecycle is proven before multiwindow support.

### Destination and receipt truth

Destinations are grouped as On This Device, Direct Cloud, and Macs. Detail
shows provider/model, boundary, auth, readiness, capabilities, context, billing
class, Automatic participation, and remediation.

Every terminal turn has a compact receipt:

```text
Ran on this iPhone · Apple On-Device
Ran in hosted cloud · Kimi API · score 0.71
Ran on Tom's Mac · Qwen 3 · local network
```

Receipts do not become large inline dashboards.

## Native runtime topology

```text
SwiftUI
  -> AppModel / NativeWayfinderRuntime
      -> ConversationStore
      -> ProviderRegistry
      -> CredentialStore
      -> embedded wayfinder-routing-core
      -> Swift and/or portable provider adapters
          -> Apple Foundation Models actor
          -> direct hosted providers
          -> optional PairedHost provider
```

The app runs no internal localhost server. Views consume snapshots and never
call providers or Keychain directly.

## Routing contract

Eligibility precedes scoring. Exclusions include:

- provider not ready, signed out, expired, or usage limited;
- denied privacy boundary;
- missing network;
- unsupported platform, context, modality, or tools;
- explicit route deny;
- removed model;
- unavailable paired host.

`Automatic` filters candidates, scores with the shared core, maps the result to
the configured route/tier, chooses the first ready destination using stable
tie-breaks, and emits a receipt.

A pinned destination never falls back. Automatic falls back only inside the
configured route and privacy posture, and only before response content begins.
Cancellation is not provider-health failure.

## Phase 0 — source of truth and risky seams

Deliverables:

- WF-ADR-0047: independent mobile product and optional pairing;
- WF-ADR-0048: shared routing-core ownership and native embedding;
- WF-DESIGN-0019: provider, account, OAuth, Keychain, and execution boundaries;
- WF-DESIGN-0020: thread-first mobile Chat, navigation, and route receipts;
- this roadmap and the Apple-platform capability matrix;
- Kimi qualification note;
- supersession links from current macOS, Rust, Apple, ChatGPT, and old mobile
  planning documents;
- UniFFI versus generated C ABI bridge spike;
- Rust HTTP versus Swift `URLSession` execution spike;
- SwiftData versus SQLite persistence spike.

Exit:

- no current document describes mobile as companion-only;
- ownership of routing, credentials, providers, Apple calls, persistence, and
  pairing is explicit;
- simulator and physical-device prototypes score a prompt through the shared
  core;
- Kimi remains classified as approved, API-key-only, or blocked.

## Phase 1 — extract the portable routing core

Implementation note: the pure routing crate and typed runtime-contract crate
are extracted, and current gateway/compatibility consumers use the renamed
core. A generated UniFFI bridge and local XCFramework now compile for Apple
Silicon macOS, arm64 iOS devices, and arm64 iOS Simulator, with Swift contract
tests running on macOS. Runtime execution on a real iOS device remains before
the phase exit gate is complete.

- isolate pure routing and runtime-contract crates;
- remove host/server assumptions from route planning;
- add typed request, candidate, plan, explanation, and receipt contracts;
- share the golden corpus between gateway and bridge;
- add gateway-versus-embedded parity;
- assemble iOS static library/XCFramework output.

Exit: identical fixtures produce identical decisions/reason codes through both
hosts; no Swift routing implementation exists; simulator and device targets
build.

## Phase 2 — native shell and local data

Implementation note: the native shell has universal iPhone/iPad targets, root
Observation state, adaptive drawer/split navigation, and an honest
routing flow backed by the generated Rust bridge. The persistence slice adds
the `ConversationStore` boundary, versioned SwiftData model actor, thread and
draft restoration, retention, deterministic export, deletion, and bounded
storage failure recovery. The final Phase 2 slice adds a deterministic,
network-free provider contract and the complete Chat lifecycle: ordered
deltas, one terminal assistant message, cancellation, interruption recovery,
failure, and retry. It intentionally does not add credentials or a live
provider.

- create iPhone/iPad Xcode targets;
- add adaptive navigation and root `AppModel`;
- implement the selected conversation store, draft restoration, retention,
  export/delete seams, and migrations;
- add deterministic mock providers;
- implement the Chat state machine without live providers.

Exit: the app launches with no Mac/gateway; primary states render on iPhone and
iPad; lifecycle, restoration, cancellation UI, and accessibility tests pass.

## Phase 3 — Keychain and direct API providers

- implement `CredentialStore`;
- API-key lifecycle UI;
- generic OpenAI-compatible direct provider;
- OpenAI Platform, Moonshot/Kimi Platform, and OpenRouter presets;
- streaming, cancellation, bounds, timeout, retry classification, usage, and
  model inventory;
- eligibility updates from provider snapshots.

Exit: add key, discover/configure model, route, stream, stop, retry, remove key,
and prove no secret leakage; Automatic works with two direct destinations.

## Phase 4 — generalized account providers

- normalized account state and challenge UI;
- PKCE authorization-code and RFC 8628 device-code engines;
- fake OAuth server harness;
- refresh, reauthentication, usage-limit, and sign-out;
- model discovery and destination publication;
- Kimi adapter only if WF-QUAL-0001 is approved.

Exit: physical-device lifecycle passes; tokens remain in Keychain; cancelled,
expired, and denied flows leave no partial credential; account connection does
not mutate routes.

## Phase 5 — Apple Foundation Models

- native availability and execution actor;
- capability/context discovery, buffered generation, streaming, cancellation;
- deterministic fake provider for CI;
- on-device-only posture and no-egress tests;
- physical-device availability and prompt-version evidence;
- contract reconciliation with the existing macOS provider.

Exit: an eligible device completes Chat with network unavailable; ineligible
states are truthful; On-Device Only cannot egress; receipts identify
on-device execution.

## Phase 6 — integrated routing and product polish

- destination inventory/detail, route editor, privacy control, receipts, and
  inspector;
- no-provider, single/multi-provider, expired auth, offline, usage-limited, and
  Apple-unavailable state matrix;
- Dynamic Type, VoiceOver, keyboard/iPad, Reduced Motion, contrast, and
  screenshot review.

Exit: all v0.2.0 acceptance criteria pass and no release path or screenshot
requires a Mac.

## Phase 7 — paired Mac provider

- one-time challenge, QR/manual discovery, shared verification value;
- device-bound credentials, host records, revocation, encrypted authenticated
  transport;
- least-privilege inventory and request APIs;
- streaming, cancel, and resume reconciliation;
- truthful local-Mac versus cloud-via-Mac receipts;
- explicit route and Automatic participation.

Exit: removing the Mac leaves mobile intact; no provider credentials cross;
revocation works; remote clients cannot access account control, files, tools,
shell, or raw logs.

## Phase 8 — release hardening

- privacy manifest/disclosures;
- physical-device matrix and adversarial network/auth tests;
- install/update/rollback/migration evidence;
- crash, memory, launch, and binary-size budgets;
- TestFlight external plan and support/recovery docs.

## Pull-request sequence

1. docs/ADRs/roadmap/capability/qualification;
2. routing-core extraction;
3. Apple FFI/XCFramework;
4. iOS shell;
5. conversation store;
6. credential store;
7. generic direct provider;
8. provider presets;
9. OAuth engines with fake server;
10. qualified Kimi adapter, if approved;
11. Apple Foundation Models;
12. integrated routing UI;
13. pairing trust protocol;
14. PairedHost inventory/execution;
15. release hardening.

Core extraction, new auth, Apple execution, and pairing never share one PR.

## v0.2.0 acceptance

1. A user completes a real conversation without owning or pairing a Mac.
2. The shared Rust core makes the authoritative route.
3. One direct API provider streams and cancels end-to-end.
4. Secrets exist only in Keychain and are absent from ordinary logs/app data.
5. Foundation Models works on an eligible device and degrades truthfully.
6. Automatic chooses across on-device/hosted while enforcing capabilities and
   privacy.
7. On-Device Only cannot send content to cloud or a paired host.
8. Connecting providers never silently changes `Automatic`.
9. A pinned unavailable destination fails without fallback.
10. Threads, drafts, replies, failures, and receipts restore correctly.
11. Backgrounding never duplicates a turn or claims false completion.
12. iPhone/iPad layouts pass accessibility and state-matrix review.
13. No companion-only language or disabled host-only controls ship.
14. Existing macOS routing, keys, Chat, Apple, and ChatGPT behavior remains
    compatible.
15. Privacy manifest and product copy match actual behavior.

Kimi account auth is optional and does not gate the release.

## Explicit exclusions

- requiring a Mac or running the desktop gateway/helper on iOS;
- importing tokens from CLIs, browsers, ChatGPT, Kimi, or filesystem state;
- undocumented subscription access or downloadable executable connectors;
- an always-running mobile gateway/background daemon;
- cloud egress described as local because transport first reached a Mac;
- tools, shell, filesystem, MCP, browser automation, or agent approvals;
- CloudKit before retention/conflict/encryption/deletion are specified;
- replacing macOS gateway-first architecture for uniformity.

## Related

- WF-ADR-0047
- WF-ADR-0048
- WF-ADR-0049
- WF-DESIGN-0019
- WF-DESIGN-0020
- `docs/apple-platform-capability-matrix.md`
- `docs/qualifications/WF-QUAL-0001-kimi-account-auth.md`
