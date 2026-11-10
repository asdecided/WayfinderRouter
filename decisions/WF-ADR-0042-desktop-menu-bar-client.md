---
schema_version: 1
id: WF-ADR-0042
type: decision
tags: [desktop, macos, tauri, menu-bar, clients, distribution, packaging, keys, offline]
---

# WF-ADR-0042: The Desktop App is a Thin Menu-Bar Client Over the Gateway (One Backend, Many Clients)

## Status

Accepted

> Supersedes WF-ADR-0027 (the thin `/demo`-URL wrapper). The spike that ADR deferred to was run;
> the outcome is bigger than a URL wrapper and different in kind: a real menu-bar app, but one that
> is architecturally forbidden from becoming a second router. This ADR is the constitution for
> `clients/` — everything in it renders decisions; nothing in it makes them.
>
> Historical Tauri amendment (WF-DESIGN-0014): §3's 360×480 webview popover was revised to
> **400×550** for that implementation. This is **not** the current native Swift requirement.
>
> Native Swift v0.1.0 amendment (WF-ROADMAP-0012): the shipping app is a compact routing utility using
> a 340 pt target width and intrinsic content height, clamped to a 420 pt maximum at the default
> text size. Chat ships in `desktop-v0.1.0` as a dedicated native window over the same bundled
> gateway; it never scores, calls a provider directly, or owns credentials. The service-first
> lifecycle, Keychain boundary, semantic route colors, and privacy ruling below are unchanged.
>
> Desktop v0.1.0 release amendment (WF-ROADMAP-0015): the native product ships for Apple Silicon
> only, with an arm64 Rust gateway inside the signed app. The distribution artifact is a signed,
> notarized, stapled app in a ZIP; the universal DMG and automatic-updater decisions below remain
> historical Tauri targets, not v0.1.0 requirements. The optional ChatGPT account provider requires
> the separately installed verified ChatGPT app. Wayfinder does not bundle Codex in this release.
>
> Rust-only amendment (WF-ADR-0046): Python, PyPI, and delegated-command language below records the
> migration history. Rust is now the sole production router and gateway.
>
> Mobile amendment (WF-ADR-0047/0048): this ADR remains the macOS constitution. Native iPhone and
> iPad are independently useful, embed the shared pure routing core, own their credentials and
> threads, and do not run or require this desktop gateway. That is not a second routing algorithm.
>
> Source-removal amendment (2026-11-10): the Tauri client source (`clients/desktop`) was removed from
> the tree and remains in git history; it is superseded by the native macOS app under `macos/`.

## Category

Technical

## Context

2026.7.0 made the gateway an always-on local service (WF-ADR-0038) with offline-first delivery
(WF-ADR-0039). What's missing is a *face*: a way to see, at a glance, that routing is on, where the
last turn went and why, and to use it — without a terminal. The natural macOS shape for an always-on
router is a **menu-bar app**: ambient status in the tray, a popover for the decision and a chat turn,
gone when you click away.

The danger is equally natural: desktop apps grow. A bespoke client that re-implements scoring, holds
keys, or grows its own delivery logic would fork the product's one non-negotiable — the routing
decision is computed **offline, deterministically, with no model call and no key** (WF-ADR-0001), and
provider keys live in the environment/secret store, never in an app or a file (WF-ADR-0004).

We studied June (`open-software-network/os-june`), a mature Tauri v2 desktop AI assistant, as a
reference. Its *craft* is worth adopting — signed/notarized universal builds, a disciplined
RC→promote release train, an auto-updater with hard key custody, restrained native-feeling design.
Its *architecture* is the opposite of ours and is explicitly rejected: June's model calls go to a
mandatory remote backend that holds the provider keys (made trustworthy with TEE attestation).
Wayfinder needs none of that machinery because nothing leaves the machine to make a decision — the
honest statement of our posture is an app that *provably does less*.

## Decision

1. **One backend, many thin clients.** The desktop app is a renderer over the local gateway's
   loopback HTTP API (`/v1/chat/completions` + `X-Wayfinder-Debug`, `/healthz`, `/router/models`,
   `/v1/savings`). It never scores, never forwards to a provider, never holds a key. The shared wire
   client and render helpers live in `clients/shared` (`@wayfinder/shared`) and are the only way
   clients consume decisions.

2. **The embedded JS scorer is a parity-gated degraded mode, not a second source of truth.**
   `clients/shared/src/scorer.js` is a byte-for-byte port of `wayfinder_router/complexity`, trusted
   only while the golden-corpus parity job (`tools/golden.py` → `clients/shared/test/parity.mjs`,
   blocking in CI) is green. It runs **only** when the gateway is unreachable, and its output is
   always framed as a preview ("local mirror — start the gateway"), never as a routed decision.

3. **Tauri v2, menu-bar accessory shell (historical implementation).** No Dock icon; a template
   tray icon with three health states; one borderless 360×480 vibrancy popover (hide-on-blur, state
   preserved); ⌥Space toggle (rebindable); single-instance. The webview reaches the gateway by
   direct loopback fetch — not Rust IPC — so the Rust command surface and capabilities stay minimal
   and auditable.

4. **Service-first lifecycle — the app never owns the gateway process.** Detect via `/healthz` and
   attach. When nothing is running, the primary CTA is one-click **Install the service**, shelling
   out to the existing `wayfinder-router service install` (WF-ADR-0038); Start/Stop go through
   `launchctl`/the `service` verbs. Two launch agents, two jobs: `tauri-plugin-autostart` starts the
   *app* at login; the WF-ADR-0038 agent starts the *gateway*. No app-owned supervisor, no
   dual-supervisor fight over `:8088`.

5. **Keys go to the Keychain via `api_key_cmd` — the app is glue, not a keeper.** First-run may
   scaffold a config (shell-out to `init --preset`) and accept a provider key, but the key is written
   straight to the macOS Keychain (`security add-generic-password`) and only an
   `api_key_cmd = "security find-generic-password -w …"` reference lands in the gateway config —
   the existing WF-ADR-0004 secret-store seam. The key never enters the webview's state, the app's
   storage, or any file.

6. **Platform ruling: Tailwind v4 + macOS 14.0 minimum.** The UI stack is React 19 + Tailwind v4 +
   shadcn/ui themed with the canonical Wayfinder tokens (WF-DESIGN-0012). Tailwind v4 needs Safari
   16.4+; WKWebView tracks the OS, so the bundle minimum rises from 13.0 to **14.0** (macOS 13 has
   left Apple security support; June ships 14+ too). Windows/Linux are non-goals for v1 — the shared
   core stays portable, the shell does not.

7. **Historical Tauri distribution decision.** Signed + notarized + stapled
   universal DMG; the Tauri updater with `createUpdaterArtifacts` and a keypair whose private key is
   custodied from day one (its loss permanently bricks auto-update); releases on a `desktop-v*` tag
   lane (verified: matches neither of `release.yml`'s globs, so it cannot fire the PyPI publish);
   right-sized RC→promote; generated `THIRD_PARTY_NOTICES`. The PyInstaller sidecar (bundling the
   gateway into the app) is **deferred**: PyInstaller's self-extraction interacts badly with the
   hardened runtime notarization requires, and universal2 Python builds are their own fight — the
   service-first model makes the sidecar unnecessary for v1.

   The native Swift v0.1.0 release does not implement that Tauri lane. WF-ROADMAP-0015 instead
   requires one thin arm64 app, notarized and stapled before a final ZIP is produced and verified.
   DMG packaging, Intel/universal support, and automatic updates are deferred.

8. **The privacy story is told honestly.** A "verify-lite" panel states exactly what is true: the
   decision is computed locally with no model call and no key in the app; prompts go only to the
   provider you route to, under your own keys; **offline mode** (WF-ADR-0039) is the only mode that
   guarantees nothing leaves — and it is a one-click toggle in the popover. No claim of "your data
   never leaves your machine" outside that mode. No telemetry, ever.

9. **The native Swift v0.1.0 ships with focused Chat.** Chat is a dedicated persistent window, not
   content crowded into the menu-bar popover. It sends bounded conversation history only to the
   bundled gateway's OpenAI-compatible endpoint, renders the authoritative assistant reply and
   routing decision, supports streaming cancellation and recovery, and never scores, contacts a
   provider directly, or stores a credential. The routing decision remains the product-specific
   signature element and is inspectable for every completed turn. In the native window, detailed
   routing metadata belongs to the persistent right inspector; the chronological transcript keeps
   only a quiet receipt that selects that inspector.

10. **ChatGPT account access remains a gateway provider, never a desktop credential path.** The
    optional `codex-app-server` provider in WF-DESIGN-0018 owns managed ChatGPT authentication,
    token refresh, model discovery, and bounded response translation behind the Rust gateway. The
    Swift app receives normalized account state and opens the returned browser flow, but never
    receives tokens, reads Codex auth storage, calls a provider directly, or changes routing merely
    because an account signed in. The existing API-key provider and credential broker are unchanged.

## Consequences

### Positive

- The decision core and key discipline survive contact with a GUI: the app *cannot* drift into a
  second router because it has no scoring path (except the parity-gated mirror) and no key storage.
- Ambient, glanceable proof that routing works in the popover, with focused Chat shipping as a
  separate native v0.1.0 window rather than competing for the same compact surface.
- The distribution craft (signing, updater, RC train) is inherited from a studied reference instead
  of discovered by trial and error.

### Negative

- A Rust/Node toolchain now lives beside the Python one (accepted in WF-ADR-0027's risks; contained
  to `clients/`).
- First-run on a machine with no Python/pipx requires installing the gateway before the app is
  useful — the cost of refusing the sidecar in v1.

### Risks

- **Parity drift.** The JS scorer silently diverging from Python would make the degraded mode lie.
  Mitigation: the parity job is blocking CI; the corpus regenerates from the real Python scorer on
  every run; divergence fails the build.
- **Shell-out surface.** `service install` / `launchctl` / `security` invocations are privileged
  glue. Mitigation: exact commands only, scoped in Tauri capabilities; no arbitrary shell.
- **Historical Tauri updater key custody.** One private key, permanent consequences. Mitigation for
  any future updater adoption: generate it once, store it in the password manager and CI secret, and
  document recovery before enabling updates. Desktop v0.1.0 has no automatic updater.

## Alternatives Considered

### A thin wrapper over `/demo` (WF-ADR-0027's proposal)

Cheapest, but it can't do the job: no tray presence, no service control, no offline toggle, no
first-run story — a browser tab in a costume. Superseded.

### June's architecture (remote backend holds keys, TEE attestation)

Rejected on principle: Wayfinder's differentiator is that no backend is *required* and no key ever
leaves the user's control. Adopting a mandatory remote backend — however well attested — would
delete the product's reason to exist.

### App-owned gateway supervisor (detect-then-spawn)

The app spawning and supervising the gateway duplicates WF-ADR-0038's launchd agent and creates the
two-supervisors-one-port failure mode. Rejected for v1; the service is the supervisor.

### Electron

A heavier runtime and bundle for no capability we need; Tauri's Rust shell + system webview fits a
menu-bar utility and keeps the binary small.

## Success Measures

- The app renders correct decisions with the gateway up, degraded, decision-only, offline, and
  unreachable — and in the last case is unmissably labelled a preview.
- `rg -i "api[_-]?key" clients/desktop/src` finds no key handling outside the Keychain glue command.
- The current native release gate is a signed, notarized, stapled arm64 app extracted from the final
  ZIP on a clean Apple Silicon Mac, passing Gatekeeper and the WF-ROADMAP-0015 evidence matrix.
- The signed universal DMG and in-place updater remain historical Tauri success measures and are not
  claimed for Desktop v0.1.0.
- The Python suite and the deterministic core are untouched by the entire `clients/` tree.

## Related

- WF-ADR-0027 (superseded — the URL-wrapper direction)
- WF-ADR-0001 / WF-ADR-0004 (the invariants this ADR exists to protect)
- WF-ADR-0038 (the service this app is a client of) · WF-ADR-0039 (the offline mode it surfaces)
- WF-ADR-0020 (the decision-first design language the popover inherits)
- WF-DESIGN-0012 (the popover design contract) · WF-ROADMAP-0009 (the delivery plan)
- WF-DESIGN-0018 (external verified ChatGPT-app provider boundary)
- WF-ROADMAP-0015 (Apple Silicon desktop v0.1.0 release contract)
- WF-ADR-0046 (Rust-only runtime)
- WF-ADR-0047 / WF-ADR-0048 (native mobile independence and shared routing core)
