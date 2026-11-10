# Wayfinder Native macOS App

This directory contains the shipping native macOS product. Wayfinder Desktop is a thin SwiftUI and
AppKit client over its bundled Rust gateway. Rust is the sole router and gateway runtime under
WF-ADR-0046.

Native iPhone and iPad work follows a different host topology: v0.2 embeds the shared routing core
and remains independently useful without this app or its gateway. That does not change the current
macOS gateway-first boundary. See WF-ADR-0047, WF-ADR-0048, and WF-ROADMAP-0016.

## Inspection Summary

- The bundled Rust implementation lives under `rust/crates/`; `wayfinder-gateway` owns native
  routing and OpenAI-compatible delivery.
- The macOS app never computes an authoritative route, calls a provider directly, or owns provider
  secrets. It consumes the bundled gateway contract.
- The legacy desktop client was removed from the tree on 2026-11-10 (it remains in git history);
  this native app is the only desktop product.
- WF-ADR-0042 and WF-ROADMAP-0012 define the accepted native shell and Chat contract.

## Native Layout

```text
macos/
  README.md
  WayfinderMac/
    Package.swift
    Sources/
      WayfinderMacApp/
        WayfinderMacMain.swift
      WayfinderMac/
        WayfinderMacApp.swift
        MenuBar/
        Windowing/
        UI/
          MenuBarPopover/
          Chat/
          Settings/
        State/
        Services/
        Models/
    Tests/
      WayfinderMacTests/
```

This is a Swift Package executable with a production bundle assembly path. It creates a native `NSStatusItem`,
opens a transient AppKit panel, hosts SwiftUI with `NSHostingController`, and opens native Settings
through an AppKit-owned `NSWindow`. Chat ships in v0.1.0 as a dedicated thin-client window over the
same gateway. Desktop v0.1.0 is Apple Silicon-only: `script/build_release_bundle.sh` assembles the
app, arm64 Rust helper, credential-broker and Foundation Models XPC services, signed manifest,
hardened-runtime signatures, and optional notarization/stapling. Intel and a universal artifact are
deferred. `Packaging/RELEASE.md` defines the physical-Mac release and rollback evidence.

## Native v0.1.0 Surfaces

- Shipping v0.1.0: an accessory menu-bar app, routing/gateway and endpoint status, native
  Settings, service controls, routing configuration, provider-key management through the Keychain
  boundary, privacy, Help, About, and focused Chat through the bundled gateway.
- The compact popover has one enabled Chat row that opens a retained, reusable native window.
- Settings window: native sidebar, Keys screen, provider picker/form, existing key status row, Keychain info box.
- Service boundary: `WayfinderClient` supports `route(prompt:)`, `loadStats(range:)`, and `loadOverview()`. The app entrypoint uses `GatewayWayfinderClient` for live status/stat rendering; `MockWayfinderClient` remains available for previews and tests.

## Chat boundary

Chat is a shipping v0.1.0 surface, but it remains a thin client: it sends bounded conversation
history only to the gateway, renders the gateway's authoritative assistant reply and routing
decision, and never scores, contacts a provider directly, or owns credentials. The chronological
transcript stays complete and thread-first; a quiet receipt selects the turn while provider, mode,
score, explanation, and signal detail live in the persistent, collapsible routing inspector on the
right. Navigator search and route filters never remove messages from the transcript. WF-ROADMAP-0012
governs its delivery and fidelity gate.

### ChatGPT account destination

Desktop Chat can use the opt-in gateway-owned `codex-app-server` provider with an eligible ChatGPT
Codex account. Add a `codex-app-server` model to the gateway config and restart the service before
opening **Settings → Accounts** to sign in. The destination appears in Chat only when the configured
model is present in the runtime catalog; `Automatic` remains the default, and a pinned unavailable
destination never silently falls back.

Desktop v0.1.0 requires a separately installed, correctly signed ChatGPT app at
`/Applications/ChatGPT.app`. Wayfinder does not bundle or redistribute its Codex executable. Release
builds ignore development helper overrides and reject colocated, sibling, unsigned, wrong-identity,
or incompatible runtimes, so ChatGPT account routing is unavailable when the verified app is absent.

The Swift app receives only bounded account status, display identity, and model names through the
literal-loopback control API. It never reads Codex auth files or receives tokens. OpenAI Platform
keys remain under **Keys**, and the existing credential broker is unchanged. ChatGPT-authenticated
requests are hosted and leave the Mac; Offline mode disables the provider. See
[WF-DESIGN-0018](../designs/WF-DESIGN-0018-codex-chatgpt-provider.md) for the runtime, release, and
adversarial-isolation gates.

## Integration Strategy

Recommended path:

1. Keep the native app behind the `WayfinderClient` protocol.
2. Use `GatewayWayfinderClient` as the real source-of-truth integration for menu-bar status, routing mix, and savings data.
3. Keep `LocalWayfinderClient` as a prototype/degraded preview only; it is not an authoritative
   route implementation.
4. Avoid storing provider keys in the app. Follow the gateway and Keychain pattern recorded in WF-ADR-0042.

The first UI patch ran with `MockWayfinderClient` so the menu-bar, chat, and settings surfaces could be shaped without bootstrapping the gateway. The native menu now starts with `AppDelegate(client: GatewayWayfinderClient())` in `Sources/WayfinderMacApp/WayfinderMacMain.swift`.

## Release Boundaries

- The Swift preview scorer is non-authoritative and remains limited to deterministic previews.
- The release script assembles and signs the production `.app`; public distribution still requires
  the real Developer ID identity, notarization, stapling, and the physical-Mac evidence matrix in
  `Packaging/RELEASE.md`.
- Desktop v0.1.0 supports Apple Silicon only. Its optional ChatGPT account destination depends on the
  separately installed verified ChatGPT app and is not a self-contained bundled runtime.
- Provider-key writes and reads stay behind the existing narrow Keychain and authenticated XPC
  boundaries. Chat does not broaden either broker.
- The bundled gateway remains the routing source of truth, matching WF-ADR-0042.
- Mobile extraction may introduce shared Apple packages incrementally, but v0.2 does not move the
  macOS provider, credential, or lifecycle boundaries merely for code uniformity.

## Run

```bash
cd macos/WayfinderMac
./script/build_and_run.sh
```

The script builds and stages a local `dist/WayfinderMac.app` bundle before launch. Use
`--verify` to confirm the process starts, or `--debug`, `--logs`, and `--telemetry` for
focused diagnostics.
For repeatable native Chat-window visual QA, launch a staged app with `--args --open-chat` to use
the real gateway or `--args --preview-chat` to use an explicit deterministic preview client. Normal
launches remain menu-bar-only and always use the real gateway.

## Test

```bash
cd macos/WayfinderMac
swift test
```

If the managed environment blocks Swift's default module cache, use:

```bash
CLANG_MODULE_CACHE_PATH=/private/tmp/wayfinder-swift-module-cache swift test --disable-sandbox
```
