---
schema_version: 1
id: WF-DESIGN-0019
type: design
status: accepted-for-implementation
date: 2026-07-24
tags: [ios, ipados, providers, accounts, oauth, keychain, privacy, kimi]
---

# Mobile provider, account, and credential framework

## Summary

Wayfinder mobile publishes concrete destinations from built-in provider
adapters. Authentication, model discovery, execution, routing eligibility, and
route mutation are separate concerns.

Accounts are official vendor entitlements. Keys are developer/API credentials.
On-device providers need neither. Paired hosts use revocable device trust.
Connecting any of them makes destinations available but never silently changes
`Automatic`.

## Normalized types

Every provider declares typed authentication, execution, and billing:

```swift
enum ProviderAuthenticationMode: String, Codable, Sendable {
    case none
    case apiKey
    case oauthAuthorizationCodePKCE
    case oauthDeviceCode
    case verifiedDesktopRuntime
    case pairedHost
}

enum ExecutionBoundary: String, Codable, Sendable {
    case onDevice
    case localNetwork
    case hosted
}

enum BillingClass: String, Codable, Sendable {
    case onDevice
    case subscription
    case apiMetered
    case unknown
}
```

`ProviderDescriptor` contains stable identity, display metadata,
implementation version, supported platforms, auth modes, boundary, billing,
capabilities, documentation, and a concise privacy summary.

Readiness is normalized as checking, signed out, authorizing, ready,
reauthentication required, usage limited, model unavailable, network
unavailable, unsupported platform, unavailable, or failed.

No snapshot contains access tokens, refresh tokens, keys, cookies, raw provider
payloads, credential paths, or authorization headers.

## Runtime seams

Authentication, catalog, and execution remain separately testable:

```swift
protocol ProviderAccountController: Sendable {
    var descriptor: ProviderDescriptor { get }
    func state() async -> ProviderAccountState
    func beginAuthorization() async throws -> AuthorizationChallenge
    func pollAuthorization(
        _ authorizationID: AuthorizationID
    ) async throws -> ProviderAccountState
    func cancelAuthorization(_ authorizationID: AuthorizationID) async
    func refresh() async throws -> ProviderAccountState
    func signOut() async throws
}

protocol ProviderModelCatalog: Sendable {
    func models(
        account: ProviderAccountSnapshot?
    ) async throws -> [ModelDescriptor]
}

protocol ProviderExecutor: Sendable {
    func stream(
        _ request: ProviderExecutionRequest
    ) -> AsyncThrowingStream<ProviderExecutionEvent, Error>
    func cancel(requestID: RequestID) async
}
```

The root `AppModel` observes value snapshots. Actors own provider registry,
credentials, conversations, and execution. SwiftUI views never read Keychain
or contact providers from `body`.

## Credential custody

- Secrets live only in Keychain with a device-only accessibility class suitable
  for foreground use.
- Non-secret provider configuration is stored separately.
- Explicit sign-out deletes the credential and verifies that subsequent reads
  fail.
- Credentials never enter SwiftData/SQLite, UserDefaults, route receipts,
  logs, analytics, crashes, previews, screenshots, or accessibility labels.
- Reads occur only inside the provider/runtime boundary.
- The existing macOS credential broker is unchanged.

## Authorization code

Official authorization-code providers use `ASWebAuthenticationSession`, PKCE,
exact `state` validation, exact callback ownership, minimum scopes, and an
approved HTTPS redirect or reviewed custom scheme. Cancellation, denial,
callback mismatch, timeout, and token exchange failure are distinct terminal
states. A general `WKWebView` is not an authentication agent.

The shared authorization-code controller owns the pending verifier and state,
token exchange, and Keychain write. SwiftUI receives only a bounded challenge:
the provider ID, authorization URL, callback scheme, and an opaque
authorization ID. The verifier and returned credential never enter view or
`AppModel` state.

Provider-specific adapters supply reviewed endpoints, callback shape, scopes,
request encoding, and the response field containing the resulting credential.
The controller accepts HTTPS authorization and exchange endpoints only, bounds
the exchange response, rejects duplicate callback parameters, and clears
pending authorization on every terminal result. Adding this engine does not
approve any provider account flow; each adapter still requires its own
qualification artifact and compiled configuration.

## Device authorization

Documented RFC 8628 providers expose verification URL, user code, expiry, and
provider identity with copy, open, cancel, and retry actions. Polling respects
the provider interval and distinguishes pending, slow-down, expiry, denial, and
success. Partial token responses are rejected. Polling ends when the
authorization UI ends or the challenge expires.

A provider-required device identifier is random, app-scoped, and stored in
Keychain. Wayfinder does not fingerprint hardware, use IDFA, imitate another
client, or import another client's identifier.

## Provider catalog

Executable adapter logic is compiled into the reviewed App Store binary.
Bundled or separately signed metadata may define display information, model
aliases, documented auth endpoints/scopes, required headers, model catalog
shape, capability defaults, minimum adapter version, and deprecation state.

Metadata cannot add executable behavior, change credential custody, enable an
adapter absent from the binary, or authorize an unreviewed auth flow.

## Initial provider set

Phase 3 implements API-key providers in this order:

1. generic OpenAI-compatible endpoint;
2. OpenAI Platform preset;
3. Moonshot/Kimi Platform preset;
4. OpenRouter preset.

Anthropic and Gemini follow only after their distinct request and streaming
contracts have shared fixtures.

OpenAI Platform and ChatGPT/Codex remain separate:

- OpenAI Platform is a direct iOS API-key provider.
- ChatGPT/Codex is a macOS-only verified desktop-runtime provider.
- iOS may see ChatGPT/Codex only through a paired Mac until an official,
  permitted native account-execution contract exists.

No consumer subscription is treated as general API entitlement.

## Kimi boundary

Official Kimi Code documentation confirms that `kimi login` uses an RFC 8628
device-code flow. It does not establish that Wayfinder may reuse Kimi Code's
client identity or model endpoint.

`docs/qualifications/WF-QUAL-0001-kimi-account-auth.md` therefore classifies
Kimi account authentication as **blocked pending provider approval**. v0.2.0
may ship the Moonshot/Kimi Platform API-key preset. “Sign in with Kimi” remains
absent unless every qualification gate is closed.

## OpenRouter account boundary

`WF-QUAL-0002` approves OpenRouter's documented OAuth PKCE flow for the first
native account adapter. OpenRouter returns a user-controlled API key after
authorization. Wayfinder stores that key under the same device-only credential
ID as manual OpenRouter API-key setup, so authentication does not create a
second credential path.

The adapter uses `ASWebAuthenticationSession`'s HTTPS callback matcher, S256,
and a random state encoded in the exact callback path. The provider-specific
exchange sends only the documented code, verifier, and challenge method.
Connecting publishes
OpenRouter destinations but does not make either destination eligible for
`Automatic`.

`OpenRouter Free` pins the documented `openrouter/free` router. It is presented
as no-cost and rate-limited, retains the `apiMetered` accounting class rather
than inventing savings, and never falls back to `OpenRouter Auto` or another
paid model.

## Apple on-device provider

The iOS adapter calls Foundation Models in-process behind an actor. It queries
actual availability, context, languages, capabilities, and readiness. It
distinguishes unsupported OS, ineligible device, Apple Intelligence disabled,
model not ready, and unknown unavailability.

The adapter streams ordered output, supports cancellation, persists no native
session, and translates only parameters supported by the framework. Under
On-Device Only it can never fall back to a hosted or paired destination.

The implemented adapter treats `LanguageModelSession.streamResponse` values as
aggregated snapshots and emits only their ordered suffixes. It bounds message
count, per-message bytes, total request bytes, instructions, and response
bytes. Because the public framework does not expose a stable context-window
value, the destination publishes no fabricated context limit.

Apple on-device is eligible for appropriate local/easy work when available; it
is not injected into existing routes. Onboarding may recommend it only with
visible confirmation.

## Logging

Allowed metadata: provider/model IDs, execution boundary, latency, bounded
usage, request status, cancellation, sanitized error class, score/reason codes,
and app/runtime version.

Forbidden: content, system instructions, authorization material, identity,
paired credentials, raw catalog payloads containing account metadata, or raw
provider errors.

## Required deterministic coverage

- Keychain create/read/update/delete and secret-leak scans;
- PKCE/state/callback mismatch and cancellation;
- RFC 8628 pending/slow-down/expiry/denial/cancel/success;
- refresh rotation, revocation, partial token rejection, and sign-out;
- fragmented/malformed streaming and exactly one terminal event;
- timeout, network transition, usage limit, removed model, and cancellation;
- eligibility refresh after account state changes;
- connecting a provider never mutates routes;
- pinned unavailable destinations never fall back.

## References

- Apple `ASWebAuthenticationSession`:
  https://developer.apple.com/documentation/authenticationservices/aswebauthenticationsession
- Apple Foundation Models:
  https://developer.apple.com/documentation/foundationmodels
- OAuth PKCE (RFC 7636):
  https://www.rfc-editor.org/rfc/rfc7636
- OAuth device authorization (RFC 8628):
  https://www.rfc-editor.org/rfc/rfc8628
- Kimi Code `kimi login`:
  https://www.kimi.com/code/docs/en/kimi-code-cli/reference/kimi-command
- OpenRouter OAuth PKCE:
  https://openrouter.ai/docs/guides/overview/auth/oauth
- OpenRouter Free Models Router:
  https://openrouter.ai/docs/cookbook/get-started/free-models-router-playground
- WF-ADR-0047
- WF-ADR-0048
- WF-DESIGN-0018
