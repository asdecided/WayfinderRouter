---
schema_version: 1
id: WF-ADR-0068
type: decision
status: accepted
date: 2026-08-12
tags: [omarchy, linux, quickshell, desktop, distribution]
---

# Ship an Omarchy Quattro control surface over the existing Linux service

## Context

Wayfinder already ships the machine-level behavior an Omarchy user needs: the
Rust gateway can run as a `systemd --user` service on a stable localhost
endpoint, and it exposes prompt-free health, model, recent-route, and savings
state. Omarchy Quattro now supports third-party QML plugins with long-lived
services and bar widgets installed from public Git repositories.

Reimplementing routing in QML would create a second authority, put desktop-shell
reloads on the request path, and conflict with WF-ADR-0001 and WF-ADR-0046. The
Omarchy shell also executes third-party plugins without a sandbox, so provider
credentials must not enter plugin settings or QML process arguments.

## Decision

Wayfinder will ship an Omarchy Quattro integration with these boundaries:

1. **The Rust gateway remains the only router.** The plugin observes and
   controls the existing `wayfinder-router` executable and Linux user service.
   It does not score prompts, proxy provider traffic, or reproduce provider
   adapters.
2. **The Omarchy plugin is a control surface.** A headless QML `service`
   polls the existing localhost HTTP surfaces and `systemd --user`; a
   `bar-widget` renders status and a theme-native popover for recent routing,
   model readiness, savings, service lifecycle, and the stable endpoint.
3. **The request path is independent of the shell.** Restarting or reloading
   `omarchy-shell` cannot stop the gateway. The gateway remains supervised by
   systemd and continues serving clients while the plugin is absent or disabled.
4. **No credential custody is added.** The plugin stores only endpoint,
   refresh-interval, and optional config-path settings. API keys and account
   credentials remain behind the reviewed gateway/provider boundaries. The
   plugin never displays or copies credential values.
5. **Installation is explicit and reversible.** The plugin can install a
   checksum-pinned, project-published Linux Router binary into a user-owned path,
   but it never replaces an independent Router installation. Plugin enablement
   and service installation are user actions. Removing the plugin does not
   silently remove the independently useful Wayfinder service or configuration.
   Service removal remains the explicit `wayfinder-router service uninstall`
   command, and a plugin-owned binary is removable only through an explicit
   ownership-checked path.
6. **The marketplace package stays separable.** Its source lives in
   `integrations/omarchy-wayfinder` with a root-ready manifest, README, license,
   and validation scripts so that directory can be published as one public
   plugin repository without copying production routing code into it.
7. **Linux is the plugin runtime.** Apple Foundation Models and Apple-specific
   account helpers remain Apple-only. Omarchy can use Linux-capable providers,
   local OpenAI-compatible endpoints, or a separately configured remote
   Wayfinder gateway.

## Consequences

- Omarchy users get a native bar surface while every OpenAI-compatible client
  continues to use the same localhost endpoint.
- The integration can evolve independently without weakening the deterministic
  core or provider-security boundaries.
- The plugin repository remains a small, reviewable control surface while its
  installer can fetch a concrete Router release and verify its reviewed SHA-256
  digest before extraction. A pinned Cargo build remains an explicit source
  fallback rather than a first-run requirement.
- Operator endpoints protected by OIDC may be unavailable to an uncredentialed
  local plugin. Health remains visible and the UI degrades without requesting or
  storing an operator token.

## Success Measures

- Enabling the plugin shows gateway and user-service state in the Omarchy bar.
- The popover refreshes prompt-free route, model, and savings metadata without
  putting QML on the delivery path.
- Shell reloads do not interrupt `wayfinder-router.service`.
- Plugin validation, JavaScript model tests, Rust tests, and the existing
  Rust-only runtime guard pass in CI.

## Related

- WF-ADR-0001 (offline deterministic core)
- WF-ADR-0004 (OpenAI-compatible gateway boundary)
- WF-ADR-0038 (Linux systemd user service)
- WF-ADR-0046 (Rust-only runtime)
