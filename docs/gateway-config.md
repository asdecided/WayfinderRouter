# Gateway configuration reference

The operator knobs for `wayfinder-router serve` — timeouts, observability, reliability
and failover, budget, cache, rate limiting, and virtual keys. The [README](../README.md)
covers the deploy architecture (gateway as a service or sidecar, the one-`base_url`
change); this is the settings reference.

Most settings live in `wayfinder-router.toml` under `[gateway]` (and its sub-tables);
a few are environment variables or `serve` flags, noted inline. The routing decision
itself stays deterministic and offline — none of these touch the scored path.

## Basics

| setting | effect |
| --- | --- |
| `WAYFINDER_ROUTER_TIMEOUT` / `serve --timeout` | upstream timeout in seconds (default 60) |
| `WAYFINDER_ROUTER_FEEDBACK_TOKEN` | when set, `/v1/feedback` requires `Authorization: Bearer <token>` |
| `serve --dry-run` | return routing decisions without calling any upstream |
| `serve --surface local\|data-plane` | `local` is the default loopback Desktop/operator surface; `data-plane` is the fail-closed network surface and requires virtual keys plus a configured model |

## Operator authentication

Operator metadata and policy endpoints can use the organisation's OIDC issuer
without changing the virtual-key contract used by inference. The default keeps
the existing loopback behavior:

```toml
[gateway.auth]
mode = "oidc" # vkeys (default), oidc, or both
issuer = "https://login.example.com/"
audience = "wayfinder-operators"
jwks_url = "https://login.example.com/.well-known/jwks.json"
admin_claim = "wayfinder_admin"
```

`oidc` requires a signed RS256 bearer JWT for `/router/*`, `/metrics`,
`/v1/savings`, `/savings`, and configuration mutation. The token must have the
configured issuer and audience, a live `exp`, a non-empty subject, and a truthy
configured admin claim (`true`, `admin`, or `operator`, including in a list).
`both` also accepts a configured virtual key during migration; `vkeys` leaves
the legacy local operator surface unchanged. JWKS keys are cached in memory for
five minutes and refreshed by `kid`; no token, session, or IdP secret is stored.
Unknown algorithms, key IDs, malformed claims, and unavailable JWKS endpoints
fail closed. See [WF-ADR-0057](../decisions/WF-ADR-0057-operator-oidc-auth.md).

When the native CLI serves the gateway, operator events are appended to
`wayfinder-audit.jsonl` beside the selected configuration. Override the path
with `WAYFINDER_ROUTER_AUDIT_FILE`. Records cover configuration reloads,
operator-auth failures, and routing/savings exports; they contain only a
bounded actor, action, timestamp, and sanitized metadata. Prompt and provider
payloads are never written. The file is opened in append mode and serialized
under one process-wide lock so concurrent writers cannot interleave JSONL
records. With `[gateway.state] backend = "redis"`, the same bounded event is
queued to the namespace's shared Redis audit list so replicas retain one
operator history; the local JSONL sink remains the fallback for memory mode.

## ChatGPT account provider (opt-in)

`codex-app-server` is a distinct hosted provider for models made available through an eligible
ChatGPT Codex account. It does not turn a ChatGPT subscription into an OpenAI Platform API key and
does not replace the existing `openai-compatible` provider.

Add an explicit route, then restart the gateway:

```toml
[gateway.models.chatgpt-sol]
provider = "codex-app-server"
model = "gpt-5.6-sol"
context_window = 1050000
```

This provider requires `model` and rejects `base_url`, `api_key_env`, `api_key_cmd`, and native
`tier`. It is always hosted, has no invented dollar-cost estimate, and is unavailable while offline
mode is active. Signing in never adds this route to a ladder or changes the desktop's `Automatic`
destination.

The managed runtime serves one inference turn at a time. A concurrent turn returns HTTP `409` or a
streamed `wayfinder_router_busy` terminal without affecting the route's circuit-breaker health.

On a literal loopback listener, the native app uses these normalized controls with the exact
`X-Wayfinder-Local-Control: 1` header:

- `GET /router/codex/account`
- `GET /router/codex/models`
- `POST /router/codex/login`
- `POST /router/codex/login/cancel`
- `POST /router/codex/logout`

Wayfinder never returns or brokers the account tokens. The managed runtime uses a separate
Wayfinder-owned Codex home and empty workspace with tool-bearing features disabled. Development
builds may use an explicitly selected or colocated helper. Release builds reject unverified sibling
executables; the fixed ChatGPT-app fallback is accepted only when its runtime and signing checks
pass. Desktop v0.1.0 therefore requires the separately installed, correctly signed app at
`/Applications/ChatGPT.app`; it does not bundle or redistribute Codex and is intentionally not
self-contained for this provider. Bundling Codex later would require a separate reviewed
release decision covering licensing, pinning, architecture, nested signing, version, and digest
verification. See
[WF-DESIGN-0018](../designs/WF-DESIGN-0018-codex-chatgpt-provider.md) and the official
[Codex app-server](https://learn.chatgpt.com/docs/app-server),
[authentication](https://learn.chatgpt.com/docs/auth#openai-authentication), and
[permissions](https://learn.chatgpt.com/docs/permissions) contracts.

## Observability

| setting | effect |
| --- | --- |
| `GET /healthz` | reports `degraded` and lists `missing_keys` when a configured `api_key_env` is unset |
| `GET /router` | read-only dashboard of recent decisions, with `X-Wayfinder-Debug: true` surfacing one in the body |
| `GET /v1/savings?period=today\|7d\|30d\|all` | realized vs always-frontier cost and the savings between them, per route (WF-DESIGN-0007) |
| `WAYFINDER_ROUTER_SAVINGS_FILE` | where the savings ledger is persisted (default `<config-dir>/wayfinder-savings.json`) |

## Reliability and failover

| setting | effect |
| --- | --- |
| `[gateway] retries` / `breaker_threshold` / `breaker_cooldown` | reliability: bounded retries on transport/`429`/`5xx`, and a per-target circuit breaker (WF-ADR-0031) |
| `[gateway] failover = same-tier\|degrade\|escalate` | on exhaustion, stay on the tier (default), fall to a cheaper one (never raises cost), or a dearer one (opt-in); per-request `X-Wayfinder-Failover` |
| `[gateway.models.<name>] fallbacks = [...]` / `deployments = [...]` / `context_window` | same-tier endpoints to try on failure; weighted concrete deployments behind one alias for healthy throughput; skip a target whose window can't fit the prompt. Responses carry `x-wayfinder-router-served-by` |

## Concurrency and backpressure

| setting | effect |
| --- | --- |
| `[gateway.concurrency] max_in_flight` / `max_queued` / `queue_timeout` | bound simultaneous provider deliveries (default 32), waiting requests (default 64), and queue wait in seconds (default 2). Saturation returns `503 wayfinder_router_overloaded` with `Retry-After` and `x-wayfinder-router-overload`; streams hold capacity for their body lifetime. Cache hits and decision-only requests bypass delivery admission (WF-ADR-0051) |

## Budget

| setting | effect |
| --- | --- |
| `[gateway.budget] limit` / `window = day\|month\|all` / `on_breach = degrade\|block` | spend cap: once `limit` realized cost is reached, degrade to the cheapest tier (default, never raises cost) or block with HTTP 402. Surfaced via `x-wayfinder-router-budget`; needs real `cost_per_1k` prices (WF-ADR-0032) |

## Cache

| setting | effect |
| --- | --- |
| `[gateway.cache] enabled` / `ttl` / `max_entries` / `max_bytes` | exact-match response cache: replay a stored answer for an identical deterministic request — instant, free repeats. Off by default; in-memory only; raise `max_bytes` (default 64 MiB) for more. A hit is free and surfaced via `x-wayfinder-router-cache: hit\|miss`; disabling purges it (WF-ADR-0033) |

## Rate limiting

| setting | effect |
| --- | --- |
| `[gateway.rate_limit] rpm` / `tpm` / `window` | cap requests-per-minute and/or upstream-tokens-per-minute over a fixed `window` (default 60s); on breach returns `429` with `Retry-After`. The outermost guardrail (checked before scoring); gateway-wide. Successful responses carry `X-RateLimit-Limit`/`-Remaining`/`-Reset` so clients can self-pace; surfaced via `x-wayfinder-router-rate-limit` and `wayfinder_router_rate_limited_total` (WF-ADR-0034) |
| `[gateway.state] backend = memory\|redis` / `url` / `namespace` | choose the shared policy-counter backend. `memory` is the zero-configuration default; `redis` uses atomic server-time fixed windows for global, workspace, and virtual-key RPM/TPM across replicas and requires a Redis URL. A Redis outage falls back to bounded process-local counters and sets `wayfinder_state_degraded 1`; it does not drop requests. The savings ledger and response cache remain process-local until their own migrations (WF-ADR-0053) |

## Virtual API keys

| setting | effect |
| --- | --- |
| `[gateway.keys.<id>] hash` / `tags` / `models` (+ nested `budget` / `rate_limit`) | virtual API keys: when any is set, inference requires a valid `Authorization: Bearer` token (else `401`). Mint with `wayfinder-router keys new --id <id>`; the plaintext is printed once and only the SHA-256 hash belongs in config. Spend & **savings** are attributed per key (`by_key` in `/v1/savings`, `wayfinder_router_key_requests_total`); a key can carry its own budget/rate-limit (strictest wins) and a `models` allowlist (clamps to the nearest allowed tier) (WF-ADR-0035) |
| `[gateway.workspaces.<id>] models` (+ nested `rate_limit`) / `[gateway.keys.<id>] workspace` | group multiple keys under one model policy and shared RPM/TPM envelope. With `[gateway.state] backend = "redis"`, the envelope is fleet-wide; otherwise it is process-local. A key inherits the workspace model list and may only narrow it. Successful inference returns `x-wayfinder-router-workspace`; discovery uses the same effective list (WF-ADR-0052) |

## Privacy and capability eligibility

Before a provider call, the gateway filters every primary and fallback
destination through the shared routing-core eligibility contract. The request
may select a privacy boundary with `x-wayfinder-privacy-posture`:

| value | permitted execution boundary |
| --- | --- |
| `on-device-only` | Apple local providers and literal loopback endpoints |
| `local-devices` | on-device plus literal private-IP or `.local` endpoints |
| `hosted-allowed` | all configured boundaries (the compatibility default) |

`[gateway].offline = true` and `x-wayfinder-offline: true` always force
`on-device-only`. The effective value is returned as
`x-wayfinder-router-privacy-posture`.

The body also contributes hard requirements: estimated prompt plus requested
output context, image content, tool/function declarations, and streaming.
Apple Foundation Models and the bounded ChatGPT adapter are text-only and are
excluded for image/tool requests; OpenAI-compatible adapters retain their
existing pass-through contract. Missing credentials, declared windows that are
too small, unsupported capabilities, and denied privacy boundaries are
excluded before reliability retries or failover. Models that omit
`context_window` retain the legacy prompt-precheck behavior. A pinned destination or
named preset with no eligible member returns
`422 wayfinder_router_destination_ineligible` with stable reason names rather
than silently switching privacy boundary (WF-ADR-0056).

## Named routing presets

Use a named preset when an application should select an ordered, operator-owned
delivery path without knowing provider endpoints or deployment topology:

```toml
[gateway.routes.coding]
models = ["local-code", "cloud-code"]
```

Clients select it with the normal OpenAI-compatible model field:
`model = "@route/coding"`. Wayfinder still computes the deterministic score for
the receipt, but the preset's ordered aliases are the only delivery candidates;
the first compatible, available alias wins and later aliases are tried on
transport/`429`/`5xx` failure. This is an explicit route choice, not a new
scoring model or a credential-broker path. Unknown `@route/...` names fail with
`400 wayfinder_router_unknown_route` rather than silently becoming `Automatic`.

The local operator surface exposes the configured, secret-free inventory at
`GET /router/routes`. Responses identify the selected preset with
`x-wayfinder-router-route: @route/<name>` and `x-wayfinder-router-mode: preset`.
Presets are bounded to 64 names and 32 unique model aliases each; model aliases
remain the stable public contract, so deployment pools and provider credentials
stay behind the selected route (WF-ADR-0055).

Names under `[gateway.models.<name>]` are public Wayfinder model aliases. The
alias is what clients discover and request; `model = "..."` is the provider's
upstream identifier, and `fallbacks` gives the alias an ordered same-tier
delivery ladder. An OpenAI-compatible alias may add up to 32 inline
`deployments` with an `id`, `base_url`, `model`, optional `api_key_env`, and
optional `weight` (1–100). The alias's own endpoint is the default member;
weighted deterministic rotation picks a healthy member first, then tries the
remaining members before the outer fallback ladder. Client-visible model names
stay aliases while circuit and latency state use `alias#deployment` identities.
Multi-turn chats remain stateless: clients resend their full transcript, while
`route_on` and optional `sticky` decide how it is routed.

For a non-loopback listener, follow the [managed gateway deployment](managed-gateway-deployment.md)
contract. The local surface refuses external binds; the managed surface omits
operator metadata and authenticates its model inventory as well as inference.
