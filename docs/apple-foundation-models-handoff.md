# Handoff: Apple Foundation Models as WAYFINDER's preferred local provider

Status snapshot: 2026-07-13

> Scope amendment (2026-07-24): this handoff remains the implementation record
> for the macOS gateway/XPC provider. It does not define the mobile topology.
> Native iPhone and iPad v0.2 uses the same typed provider and availability
> truth but executes Foundation Models in-process behind a Swift actor, embeds
> the shared Rust routing core, and does not require a Mac. See WF-ADR-0047,
> WF-ADR-0048, WF-DESIGN-0019, and WF-ROADMAP-0016.

## Goal

Add Apple's on-device `SystemLanguageModel` as WAYFINDER's preferred local provider on eligible
Apple Silicon Macs. It must be capability-detected, not assumed: the Foundation Models framework
starts at macOS 26 and availability also depends on Apple Intelligence device eligibility, region,
language, model download/readiness, and current system state.

Implementation note (2026-07-19): the provider and bounded authenticated inference XPC path are now
implemented. On an eligible, never-configured Mac, Setup preselects Apple Local only after a live
`available` response and still requires confirmation. Existing configuration and route ladders are
unchanged, and Chat still opens on `Automatic`. Final Developer ID clean-machine evidence remains a
Desktop v0.1.0 release gate under WF-ROADMAP-0015.

When available, a new installation should prefer Apple Foundation Models for the local tier. When
unavailable, WAYFINDER must retain the configured Ollama/OpenAI-compatible local provider or show a
clear no-local-provider state. It must never silently turn an offline request into cloud egress.

Apple API reference:
[`SystemLanguageModel`](https://developer.apple.com/documentation/foundationmodels/systemlanguagemodel).

## Read first

- [`WF-ROADMAP-0014-rust-gateway-migration.md`](../roadmaps/WF-ROADMAP-0014-rust-gateway-migration.md)
- [`WF-DESIGN-0016-signed-rust-helper-integration.md`](../designs/WF-DESIGN-0016-signed-rust-helper-integration.md)
- [`rust-migration-capability-matrix.md`](rust-migration-capability-matrix.md)
- `macos/WayfinderMac/Package.swift`
- `macos/WayfinderMac/Sources/WayfinderCredentialBroker/main.swift`
- `rust/crates/wayfinder-macos-xpc/`
- `rust/crates/wayfinder-gateway/src/delivery.rs`

Preserve the dirty worktree. Work on a `codex/*` branch; do not commit or push to the default
branch.

## Accepted ownership

- The Rust gateway remains the source of truth for routing, delivery planning, budgets, rate
  limits, accounting, cache, health, models, cancellation, and public OpenAI/Anthropic HTTP APIs.
- A small Swift XPC service owns the Foundation Models framework call because it is a native Swift
  API and should remain available while the menu-bar UI is closed.
- Swift does not score prompts, select tiers, author TOML, supervise the helper, or call cloud
  providers.
- The menu-bar app renders availability and repair guidance; it does not infer model readiness.
- `offline = true` may use Apple Foundation Models only when the complete selected delivery plan is
  local. No XPC failure may fall back to cloud under offline mode.

## Proposed topology

```text
client request
    -> Rust Axum gateway
    -> deterministic route and delivery plan
    -> AppleFoundationModelDelivery
    -> authenticated, bounded XPC call
    -> Swift FoundationModelBroker.xpc
    -> SystemLanguageModel.default / LanguageModelSession
```

Use a separate inference service or a separately named protocol from the credential broker. Do not
expand the credential broker into a general control API. Both services must authenticate the
bundled Rust helper's signing identity.

The production packaging implementation uses
`Wayfinder.app/Contents/Helpers/WayfinderGateway.app` as the containing helper application. The
Rust gateway is its main executable and the credential and Foundation Models services live under
that helper application's `Contents/XPCServices`. Release signing proceeds inner-to-outer and
requires the main app, helper app, and both XPC services to share one non-empty Team ID. Ad-hoc
development bundles remain buildable but are not considered Apple-provider-ready.

## Provider identity and configuration

Introduce a typed provider kind rather than pretending Apple is OpenAI-compatible. Suggested
semantic configuration:

```toml
[gateway.models.apple-local]
provider = "apple-foundation-models"
model = "system-default"
tier = "local"
```

The exact TOML shape must go through the existing document-preserving config layer and receive
valid/invalid compatibility fixtures. Do not encode an internal Apple model version as stable:
Apple updates the system model with OS releases.

Bootstrap behavior:

1. On macOS 26+, query availability through the native service.
2. If `.available`, offer/select `apple-local` as the preferred local tier.
3. If `.unavailable(.deviceNotEligible)`, retain Ollama/manual local setup.
4. If `.unavailable(.modelNotReady)`, report a temporary not-ready state and preserve fallback
   configuration without claiming provider downtime.
5. On macOS 14–15 or non-macOS platforms, the provider is unsupported without affecting other
   providers.

Existing configurations must never be rewritten implicitly.

## XPC protocol v1

Keep the protocol narrow and versioned. Suggested operations:

- `availability(request)` -> available, device-not-eligible, model-not-ready, unsupported, or
  unavailable;
- `generate(request)` -> bounded buffered response and usage metadata;
- `stream(request)` -> ordered bounded events plus one terminal result;
- `cancel(requestID)` -> idempotent cancellation of one request.

Request fields should include only:

- protocol version and opaque request ID;
- bounded instructions/system text;
- bounded normalized messages/content;
- generation parameters that Foundation Models actually supports;
- bounded tool schemas when tool support is deliberately implemented.

Do not forward gateway credentials, arbitrary file paths, shell commands, environment variables,
Keychain references, or configuration documents.

Every request, message, tool schema, generated chunk, accumulated response, queue, and deadline
must have an explicit bound. The service should reject unsupported OpenAI parameters rather than
silently claiming to honor them.

## Delivery semantics

Implement `AppleFoundationModelDelivery` behind the existing buffered and streaming delivery
traits. It must:

- establish XPC/model availability before committing HTTP 200 streaming headers;
- translate native output into the existing OpenAI-compatible and Anthropic response contracts;
- preserve ordered streaming and one terminal marker;
- propagate downstream cancellation to the XPC session;
- account only completed/defined partial responses according to the gateway contract;
- classify unavailable/not-ready failures for delivery planning without counting user
  cancellation as a breaker failure;
- never retry generated content after the first output byte;
- expose a truthful local/provider identity in `/healthz` and `/router/models`.

Apple's model context size and supported languages are runtime capabilities. Query them when
available and enforce the smaller of Apple's reported limit and WAYFINDER's configured bound.

## Privacy and observability

- Apple on-device delivery is local, but WAYFINDER should say “on-device” rather than making a
  broader privacy claim unless offline mode proves the whole delivery closure is local.
- No prompt, response, tool arguments, or generated content in unified logging, Rust tracing,
  metrics, crash breadcrumbs, persisted state, or accessibility labels.
- Metrics may include bounded provider kind, availability category, latency, token/usage counts,
  cancellation, and sanitized error class.
- Do not persist Apple session transcripts in the inference service.

## Implementation sequence

1. Add an ADR/design amendment for the inference XPC boundary and typed provider kind.
2. Add pure Swift availability/result models and tests, guarded with `@available(macOS 26, *)`.
3. Add the authenticated inference XPC service and bounded protocol.
4. Add a dedicated macOS Rust client crate, following `wayfinder-macos-xpc`'s isolated FFI pattern.
5. Add the typed config/provider model without changing existing presets.
6. Implement buffered delivery against a fake native service.
7. Implement streaming and cancellation against deterministic fake sessions.
8. Add live Apple Silicon integration behind an explicit test/environment gate.
9. Update bootstrap/setup UI to prefer Apple only when available.
10. Add capability, packaging, signing, and clean-machine evidence. The containing helper-app
    topology, matching-Team build validation, and runtime readiness gate are implemented; signed
    clean-machine release evidence remains required before the exit gate is satisfied.

## Required tests

Pure/deterministic tests:

- macOS 26 availability categories and older-OS fallback;
- malformed/version-skewed XPC payloads;
- caller-signing rejection;
- request, response, queue, tool, context, and timeout bounds;
- buffered response translation;
- fragmented streaming, missing terminal event, service crash, stall, and cancellation;
- pre-first-byte failover and post-first-byte no-retry behavior;
- offline no-egress closure;
- model-not-ready versus provider-down readiness wording;
- no prompt/secret/body in errors, debug output, logs, metrics, or snapshots;
- configuration preservation and explicit preset mutation.

Real Apple Silicon tests:

- eligible and model-ready Mac;
- Apple Intelligence disabled/not-ready state where reproducible;
- menu-bar UI closed while inference succeeds;
- helper and inference-service crash/restart;
- streaming cancellation and sustained sequential requests;
- signed genuine helper accepted and unsigned/copied/wrong-identity callers rejected;
- app update and rollback with configuration preserved.

## Original exit gate and remaining release evidence

The availability-gated new-setup preference must retain these gates:

- availability-based selection is truthful and reversible;
- buffered and streaming public API fixtures pass;
- cancellation, bounds, accounting, breaker, cache, and offline semantics pass;
- the signed arm64 app/helper/XPC topology passes on a clean Apple Silicon Mac;
- Ollama/manual local configuration remains usable;
- the capability matrix links direct evidence.

Rust subsequently became the sole router and gateway runtime under WF-ADR-0046.
That cutover does not change the macOS XPC security boundary described here.
