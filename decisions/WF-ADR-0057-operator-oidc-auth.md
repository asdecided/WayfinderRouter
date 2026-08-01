---
schema_version: 1
id: WF-ADR-0057
title: OIDC authentication for the managed operator surface
status: accepted
date: 2026-08-01
tags: [gateway, oidc, operator, security, rust]
---

# OIDC authentication for the managed operator surface

## Decision

Wayfinder keeps virtual keys as the data-plane credential and adds an optional,
separately configured OIDC boundary for operator endpoints. The gateway accepts
`[gateway.auth]` with `mode = "vkeys" | "oidc" | "both"` (default `vkeys`),
an expected `issuer`, `audience`, `jwks_url`, and a truthy `admin_claim` name.

When `oidc` is selected, `/router/*`, `/metrics`, `/v1/savings`, `/savings`,
and configuration mutation routes require a signed RS256 bearer JWT. The token
must have matching `iss` and `aud`, a future `exp`, an optional not-yet-valid
`nbf` within the bounded clock skew, a non-empty `sub`, and the configured admin
claim. `both` also permits an already configured virtual key for migration.

The JWKS document is fetched over the configured URL, retained only as a
short-lived in-memory public-key cache, and never written to the gateway
configuration, ledger, logs, or audit data. No sessions, browser callbacks, IdP
client secrets, or user store are introduced. Token validation fails closed;
unknown keys or an unavailable JWKS endpoint do not grant access.

## Boundary

The operator middleware is attached only to the local operator router. Chat,
model discovery, and Anthropic/OpenAI data-plane routes retain their existing
virtual-key behavior. The default `vkeys` mode leaves the existing loopback
operator behavior unchanged. The bounded local Codex account-control routes
remain protected by their separate exact loopback header and are not exposed by
OIDC.

The first implementation supports RS256 JWKs with explicit `kid`, `n`, and `e`
members. It rejects other algorithms and key uses rather than accepting a
provider-specific fallback. TLS termination remains the deployment boundary;
the gateway does not add in-process TLS.

## Consequences

- A managed deployment can place dashboards, routing controls, metrics, and
  savings behind the organisation's existing IdP without moving API keys into
  that IdP flow.
- A `vkeys` to `both` migration can be staged before requiring OIDC, while
  `oidc` prevents data-plane credentials from being treated as operator identity.
- JWKS rotation is handled by `kid` lookup and a bounded refresh; an IdP outage
  may temporarily return 503 rather than accepting a stale or unverifiable
  token.
- OIDC claim and issuer configuration is operator-owned TOML, not remote code or
  a downloaded provider plugin.
- The adjacent audit slice uses the same operator boundary and records
  authentication failures, configuration reloads, and exports without prompt
  or provider payloads. Redis-backed deployments enqueue the same event into a
  namespace-scoped shared list; memory mode uses the append-locked JSONL file.

## Verification

- config parsing rejects incomplete OIDC configuration and round-trips the
  auth table;
- RS256/JWK parsing rejects non-RSA, non-signing, and non-RS256 keys;
- operator routes return 401 without an OIDC token while health remains public;
- the gateway has no path from an operator JWT to a provider credential or
  prompt body.

## References

- WF-ADR-0050 — separate the managed model data plane from local operator surfaces
- WF-ROADMAP-0010 — enterprise trust surface
