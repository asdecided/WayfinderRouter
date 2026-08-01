---
schema_version: 1
id: WF-ADR-0050
type: decision
status: accepted
date: 2026-08-01
tags: [rust, gateway, enterprise, security, authentication, deployment]
---

# Separate the managed model data plane from local operator surfaces

## Context

Wayfinder's Rust gateway was originally a loopback service for one user and one
Desktop application. Its single Axum router therefore combined inference,
model discovery, health, metrics, recent-route metadata, savings, configuration
rendering, and local account controls.

The CLI allowed an explicit non-loopback bind but only printed a warning when
virtual-key authentication was absent. Virtual keys already protected inference
requests, but operator and diagnostic endpoints remained on the same listener.
That is not an acceptable foundation for a remotely reachable or replicated
deployment: one configuration mistake could expose operational metadata, and
there was no route-level boundary for an ingress or service mesh to enforce.

## Decision

Wayfinder has two explicit HTTP surface classes:

1. **Local surface** — the existing loopback-only Desktop/developer contract,
   including health, metrics, router metadata, savings, configuration rendering,
   inference, and eligible local account controls.
2. **Managed data plane** — a network-exposable, fail-closed surface containing
   only inference compatibility routes, authenticated model discovery, and
   minimal liveness/readiness probes.

`wayfinder-router serve` defaults to `--surface local`. A local surface refuses
to bind beyond loopback. Operators must choose `--surface data-plane` for a
network listener, and that mode refuses to start unless:

- at least one virtual key is configured; and
- at least one model destination is configured.

The managed data plane exposes:

- `GET /livez` without authentication, returning only process liveness;
- `GET /readyz` without authentication, returning only ready/not-ready;
- authenticated `GET /v1/models` and `GET /models`, filtered by the presented
  virtual key's model allowlist;
- authenticated OpenAI- and Anthropic-compatible inference routes.

It does not expose `/healthz`, `/metrics`, `/router/*`, savings endpoints, or
local account-control routes. Model names, provider endpoints, environment
reference names, recent routing metadata, and accounting are therefore absent
from the unauthenticated probe surface.

The native Rust `keys new` command is restored as a deliberately narrow secret
creation seam. It prints a high-entropy virtual key once and emits only its SHA-256
digest in the pasteable TOML entry. It does not edit configuration or persist
the plaintext.

## Consequences

- Existing Wayfinder Desktop and loopback users retain the complete local
  surface with no route or default change.
- Accidental public exposure of the local operator surface becomes a startup
  error rather than a warning.
- Ingress and service-mesh policy can target a small, stable data-plane route
  set before OIDC or a dedicated operator listener exists.
- `/livez` and `/readyz` are suitable for orchestrator probes and reveal no
  configured model or credential metadata.
- Metrics and administration are intentionally unavailable on the managed
  listener. A separate authenticated operator listener is the next boundary;
  this decision does not treat virtual data-plane keys as administrator
  identity.
- TLS remains terminated by a trusted ingress or service mesh. This change does
  not add in-process TLS, Redis, OIDC, an audit log, or multi-tenant identity.

## Rejected alternatives

- **Protect every existing route with a virtual key.** Virtual keys represent
  applications and teams, not operators. Giving data-plane clients access to
  metrics, accounting, configuration, or account controls collapses two
  different authority classes.
- **Keep warning-only non-loopback binds.** This leaves a serious deployment
  error as an operator convention instead of an enforceable invariant.
- **Expose metrics publicly for easy scraping.** Metric labels include bounded
  operational attribution. They belong on a separately authenticated operator
  surface, not the public model listener.

## Follow-on sequence

1. Add a separately bound operator surface with OIDC authorization and an
   append-only audit contract.
2. Add bounded process-local concurrency/backpressure controls and a
   real-listener concurrency contract (WF-ADR-0051).
3. Introduce a shared state backend and prove two-replica rate-limit, budget,
   accounting, and admission consistency. The Redis-first rate/token counter
   contract is now WF-ADR-0053; budget, ledger, cache, and distributed
   admission remain follow-ons.
4. Package the two-plane topology through a Helm chart with ingress TLS,
   NetworkPolicy, PodDisruptionBudget, and probe defaults.

## Related

- WF-ADR-0035 — hashed virtual keys and per-key attribution
- WF-ADR-0045 — Rust gateway/helper architecture and loopback default
- WF-ADR-0046 — Rust-only runtime
- WF-ADR-0051 — bounded delivery concurrency
- WF-ROADMAP-0010 — shared state, OIDC, audit, telemetry, and Helm
- WF-ROADMAP-0011 — identity and governance plane
