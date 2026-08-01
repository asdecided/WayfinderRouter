# Managed Rust gateway deployment

Wayfinder's default HTTP surface is a loopback service for Desktop and local
development. Do not expose that surface through an ingress or LAN bind.

For a remotely reachable model endpoint, use the explicit managed data plane:

```sh
wayfinder-router keys new --id team-a --workspace production
```

Store the plaintext key in the calling application's secret manager. Add only
the printed SHA-256 entry to `wayfinder-router.toml`, optionally applying model,
budget, and rate-limit scope:

```toml
[gateway.keys.team-a]
hash = "<sha256 printed by keys new>"
workspace = "production"

[gateway.workspaces.production]
models = ["local", "cloud"]

[gateway.workspaces.production.rate_limit]
rpm = 600
tpm = 1000000
window = 60

[gateway.keys.team-a.rate_limit]
rpm = 120
tpm = 200000
window = 60

[gateway.state]
backend = "redis"
url = "redis://redis.internal:6379"
namespace = "production"
```

`memory` is the default and keeps policy counters inside one process. Select
`redis` when replicas must share the global/workspace/key RPM/TPM windows. The
gateway pings Redis before serving and uses Redis server time for window
boundaries. If Redis later becomes unavailable, requests continue through a
bounded process-local fallback and `wayfinder_state_degraded 1` makes the
consistency loss visible; treat that state as an operational incident.

Then start the data plane on the address supplied by the deployment platform:

```sh
wayfinder-router serve \
  --surface data-plane \
  --host 0.0.0.0 \
  --port 8088 \
  --config /etc/wayfinder/wayfinder-router.toml
```

The process refuses to start if the data plane has no virtual keys or no model
destinations. A non-loopback bind without `--surface data-plane` also fails.

## Concurrent users and overload

One process admits 32 simultaneous upstream deliveries and up to 64 bounded
waiters by default. That leaves headroom for twenty concurrent cloud-style
requests without serializing them inside the router. Tune the process boundary
explicitly when the provider and host have measured capacity:

```toml
[gateway.concurrency]
max_in_flight = 32
max_queued = 64
queue_timeout = 2.0
```

`max_queued = 0` disables waiting. When every delivery and queue slot is
occupied, or a waiter exceeds `queue_timeout`, inference returns HTTP `503`
with `Retry-After`, error type `wayfinder_router_overloaded`, and
`x-wayfinder-router-overload: queue-full|queue-timeout`.

Streaming requests retain a delivery slot until the response body finishes or
the client disconnects. Cache hits and decision-only requests do not use a
delivery slot. The local operator metrics include current/peak in-flight
deliveries, queue wait, and overload totals; the managed listener still does
not expose metrics.

These limits protect the router process; they do not increase the capacity of
an upstream. In particular, the ChatGPT/Codex account runtime remains
single-turn, while local-model capacity depends on its host. Validate provider
latency and token throughput before raising the defaults.

## Network contract

The managed listener exposes only:

| Route | Authentication | Purpose |
| --- | --- | --- |
| `GET /livez` | none | content-free process liveness |
| `GET /readyz` | none | content-free delivery readiness |
| `GET /v1/models`, `GET /models` | virtual key | models permitted to that key |
| `POST /v1/chat/completions`, `POST /chat/completions` | virtual key | OpenAI-compatible inference |
| `POST /v1/messages`, `POST /messages` | virtual key | Anthropic-compatible inference |

Requests authenticate with `Authorization: Bearer <virtual-key>`. Model listing
respects the effective workspace/key `models` allowlist. Inference retains the
existing per-key attribution and budget behavior. Workspace and key rate
limits both apply, and successful responses identify the bounded policy scope
through `x-wayfinder-router-workspace`.

Every inference candidate is hard-filtered for the request's capabilities and
privacy posture before delivery. Callers may send
`x-wayfinder-privacy-posture: on-device-only|local-devices|hosted-allowed`;
`hosted-allowed` is the compatibility default and the effective value is
echoed as `x-wayfinder-router-privacy-posture`. Missing credentials,
insufficient context, unsupported image/tool/streaming requirements, and
disallowed execution boundaries are excluded from primary and fallback plans.
A pinned destination with no eligible path fails with
`422 wayfinder_router_destination_ineligible`; it never silently crosses the
requested boundary.

Model names in this API are stable Wayfinder aliases. A configured alias maps
to its provider model and ordered same-tier fallbacks, allowing operators to
change upstream revisions without changing every client. An alias may also
rotate a bounded pool of equivalent deployments; the public model name remains
the alias while per-deployment health is isolated internally. Multi-turn
requests remain OpenAI/Anthropic-compatible and stateless: the caller sends the
complete transcript each turn, so no conversation content is retained by the
gateway.

Application-owned delivery policy can use a configured named preset through
`model = "@route/<name>"` on a request. Presets are not exposed on the managed
`/router/*` surface; they select only the operator-defined model aliases that
are already allowed for the presented virtual key. An unknown preset fails
closed with `400 wayfinder_router_unknown_route` rather than becoming
`Automatic`.

The listener deliberately does **not** expose `/healthz`, `/metrics`,
`/router/*`, savings, config rendering, or local ChatGPT/Codex account controls.
Those are operator/local surfaces and must not be routed through the public
model ingress.

## Deployment boundary

- Terminate TLS at a trusted ingress or service mesh; the Rust process does not
  terminate public TLS.
- Keep the configuration and provider credentials outside the container image.
- Treat virtual keys as application credentials and rotate by minting a new key,
  updating the caller, then removing the old hashed entry.
- Use `/livez` for liveness and `/readyz` for readiness. Do not scrape either for
  model inventory.
- With the default `memory` state backend, counters, budgets, cache, breakers,
  and ledger are process-local. Redis shares only the global/workspace/key
  request/token windows in this first slice; budgets, cache, ledger, and
  distributed admission remain separate migrations.
- A separately authenticated operator listener, OIDC, audit log, shared state,
  and Helm packaging remain follow-on work; this first boundary does not claim
  those capabilities.

See [WF-ADR-0050](../decisions/WF-ADR-0050-managed-gateway-surfaces.md) for the
authority separation and threat model, and
[WF-ADR-0051](../decisions/WF-ADR-0051-bounded-delivery-concurrency.md) for the
process-local throughput contract, and
[WF-ADR-0052](../decisions/WF-ADR-0052-workspace-scoped-model-routing.md) for
workspace, alias, and multi-turn boundaries, and
[WF-ADR-0053](../decisions/WF-ADR-0053-shared-state-backend.md) for the
shared-state contract and degradation behavior, and
[WF-ADR-0054](../decisions/WF-ADR-0054-model-deployment-pools.md) for stable
aliases and weighted deployment pools, and
[WF-ADR-0056](../decisions/WF-ADR-0056-hard-destination-eligibility.md) for
hard privacy and capability filtering.
