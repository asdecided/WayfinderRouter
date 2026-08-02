# Hosting a Wayfinder demo

The supported public "try it live" surface is the zero-server, zero-cost static
demo described in
[WF-DESIGN-0002](../designs/WF-DESIGN-0002-static-serverless-demo.md). The Rust
container is the authenticated managed data plane; it deliberately does not
expose the local operator/demo surface on a public listener.

## Why decision-only

The whole pitch — route (`● LOCAL` / `◆ CLOUD`), structural score, *why*, cost saved, the
live threshold slider — is computed by the deterministic scorer with **no model call and no
keys** (WF-ADR-0001). The static demo runs that scorer in the browser, so it has no provider
credentials, model spend, or server-side inference abuse surface. It is the honest core of
the product — "no model call to decide."

## The recipe

Build and publish the static scorer bundle from WF-DESIGN-0002 to a static host.
It requires no provider credentials, cannot create model spend, and scales with
the host's CDN. Keep a local operator surface loopback-only during development.

If a public experience must return real model output, deploy the managed data
plane using `docker-compose.example.yml` or the Helm chart. It requires
`--surface data-plane`, at least one configured virtual key, and at least one
model. See [managed gateway deployment](managed-gateway-deployment.md).

## What it costs (honestly)

The resource footprint is tiny; the cost is really about the platform's pricing model:

| Path | Cost | Catch |
| --- | --- | --- |
| Static + WASM/JS (WF-DESIGN-0002) | **$0, forever** | no cold start, infinite scale — but needs the client-side scorer build |
| Managed data plane | hosting plus provider spend | requires virtual keys, provider credentials, budgets, and operational controls |

For a launch window, the static/WASM demo is the simplest robust option. Use the
managed data plane only when returning real replies is itself the demonstration.

## If you must show real replies

A demo that returns model output needs keys and therefore guardrails. Run the gateway
**without** `--dry-run`, with `[gateway.models]` configured, and:

- keys via the platform's secret store (env vars, per `api_key_env` — never baked into the
  image);
- a hard **budget cap** on the provider account;
- **per-IP rate limiting** and a small context/token cap at the proxy;
- lock the OpenAI `model` field (don't let visitors pin everything to the expensive arm);
- ideally a cheap or self-hosted local tier so most traffic costs nothing.

This is real ops and ongoing cost. For a launch, the static decision-only demo
makes the point without any of it.
