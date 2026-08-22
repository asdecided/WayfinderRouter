---
schema_version: 1
id: WF-ADR-0073
type: decision
status: accepted
date: 2026-08-22
tags: [product, omarchy, linux, developers, routing, strategy]
---

# Make Omarchy the primary product while preserving a portable Router

## Context

Wayfinder now has a portable Rust Router, native Linux release archives, coding
agent activation commands, an Omarchy Quattro plugin, Apple clients, and a
substantial managed-enterprise substrate. These directions can all use the same
routing core, but they cannot all be the primary product at once.

The enterprise roadmap optimizes for organizations, fleets, identity,
governance, Kubernetes, and administrative control. The strongest immediate
fit is instead an opinionated developer environment where coding agents, local
models, shell-native tools, and inspectable behavior are already normal.
Omarchy makes those concerns first-class and supplies a native plugin surface.

The product therefore needs one flagship customer and one protected
architectural hedge: focus the experience on an Omarchy developer workstation
without coupling the Router to Omarchy.

## Decision

1. **Omarchy is Wayfinder's primary product environment.** The public promise is:
   install one Omarchy plugin, connect eligible local and hosted models, and
   give supported coding agents one local endpoint with deterministic routing
   receipts.
2. **The Rust Router remains portable.** Scoring, policy, provider delivery,
   compatibility endpoints, configuration, and receipts must not depend on
   Quickshell, Hyprland, or Omarchy. Omarchy consumes the Router as a released
   Linux product.
3. **The plugin is the flagship product surface.** It owns Omarchy-native
   installation, lifecycle, diagnosis, discovery, status, and interaction. The
   Router remains an independently supervised `systemd --user` service so a
   shell reload cannot interrupt requests.
4. **The target user is one technical person on one developer workstation.**
   Project-aware policy, coding-agent integration, local runtimes, explicit
   provider setup, and understandable route receipts take priority over fleet
   administration.
5. **Agent integrations are verified individually.** A client is documented as
   supported only after its endpoint, authentication, streaming, cancellation,
   and model-selection behavior are tested. Wayfinder does not claim that every
   AI tool accepts a custom gateway.
6. **Existing safety boundaries remain.** Eligibility precedes scoring; pinned
   destinations do not silently fall back; connecting an account or key does
   not silently change Automatic; credentials remain at the delivery boundary;
   privacy and economics claims match observed execution.
7. **Enterprise product expansion stops.** Organization hierarchy, SCIM, RBAC,
   fleet OIDC, chargeback, Kubernetes-first operations, and an administrative
   control-plane UI are not active roadmap work. Existing managed-gateway
   safety contracts remain supported as portable Router capabilities.
8. **Apple product expansion pauses.** Shipped clients remain supportable, and
   accepted platform/security decisions remain valid, but macOS, iOS, and
   iPadOS do not drive current product scope.
9. **One release train governs the product.** Plugin releases pin reviewed
   Router artifacts and checksums. Router changes required by Omarchy land in
   the portable core first; plugin changes then expose them without duplicating
   routing behavior.

## Pull-request filter

A new change must satisfy at least one condition:

- improve first-run or daily-use Wayfinder on Omarchy;
- improve verified routing for a coding agent on one Omarchy workstation; or
- harden portable Router infrastructure required by either outcome.

Changes that satisfy none of these conditions are deferred. Provider breadth,
enterprise infrastructure, and platform clients are not independent reasons to
merge work.

## Supersession and preservation

- WF-ROADMAP-0011 is superseded as a product strategy.
- WF-ROADMAP-0010 becomes maintenance-only; its implemented safety boundaries
  remain valid.
- WF-ROADMAP-0016 is paused, not architecturally repealed.
- WF-ADR-0047 through WF-ADR-0049 remain valid if native mobile work resumes.
- WF-ADR-0050 through WF-ADR-0067 remain valid technical contracts for the
  portable managed gateway, but they do not authorize additional enterprise
  product work.
- WF-ADR-0068 remains the shell/runtime separation contract and is promoted
  from an integration decision to the flagship product boundary.
- WF-ADR-0070 is the activation foundation for the Omarchy experience.
- WF-ADR-0071's versioned profiles and bindings are redirected toward local,
  project-aware developer policy. They do not require an administrative HTTP
  API or fleet control plane.

## Consequences

### Positive

- Product language, issues, releases, and engineering work target one coherent
  technical audience.
- Omarchy can provide a deeply native experience while the Router remains
  useful on other Linux systems and embeddable by other clients.
- Prior work on deterministic profiles, durable policy, receipts, service
  supervision, and Linux packaging becomes workstation infrastructure rather
  than abandoned enterprise work.

### Negative

- Enterprise opportunities and Apple feature development are deliberately
  deferred.
- The product depends on a fast-moving external shell ecosystem and must track
  Omarchy compatibility explicitly.
- Client support grows more slowly because each integration needs behavioral
  verification rather than a generic compatibility claim.

## Success measures

Success is evaluated without mandatory telemetry:

- a clean Omarchy machine can install, initialize, start, diagnose, and remove
  Wayfinder without Cargo or manual service authoring;
- the supported-agent matrix contains reproducible verification for each claim;
- one repository can select local policy without changing global user policy;
- route receipts explain model, execution boundary, profile, and reason;
- Router and plugin releases remain pinned, checksummed, reversible, and
  independently testable.

## Related

- WF-ADR-0001 (standalone deterministic Router)
- WF-ADR-0038 (local service surface)
- WF-ADR-0068 (Omarchy Quattro plugin boundary)
- WF-ADR-0069 (checksummed Linux releases)
- WF-ADR-0070 (native activation surface)
- WF-ADR-0071 (versioned policy lifecycle)
- WF-ROADMAP-0017 (Omarchy-first delivery)
