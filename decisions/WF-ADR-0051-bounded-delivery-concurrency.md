---
schema_version: 1
id: WF-ADR-0051
type: decision
status: accepted
date: 2026-08-01
tags: [rust, gateway, enterprise, throughput, concurrency, backpressure]
---

# Bound concurrent delivery and prove the 20-user gateway contract

## Context

Axum, Tokio, and the shared Reqwest client already allow Wayfinder to process
requests concurrently. The routing calculation is synchronous but short, and
the process-wide metrics, rate-limit, cache, breaker, and ledger locks do not
span provider I/O.

The missing production boundary was admission. A slow provider or long-lived
stream could create an unbounded number of in-flight deliveries and waiting
tasks. That is a resource-exhaustion risk, not useful throughput. It also made
the operator-visible behavior under overload undefined.

The immediate product requirement is one gateway used by approximately twenty
people at once. That must be demonstrated through the real HTTP server without
claiming that every provider can execute twenty turns simultaneously.

## Decision

Every process owns one bounded delivery admission policy:

- `max_in_flight = 32` simultaneous upstream deliveries by default;
- `max_queued = 64` bounded waiting requests by default;
- `queue_timeout = 2.0` seconds by default.

Operators may override those values under `[gateway.concurrency]`. At least one
in-flight slot is required. The waiting count may be zero. Queue timeouts must
be positive.

Admission occurs after authentication, validation, deterministic routing, and
cache lookup, immediately before a real provider delivery. Decision-only
requests and cache hits do not consume delivery capacity. A streaming request
holds its permit until its response body completes or is dropped, not merely
until the upstream response headers arrive.

When capacity and the bounded queue are full, the router returns HTTP `503`
with error type `wayfinder_router_overloaded`, `Retry-After`, and
`x-wayfinder-router-overload: queue-full`. A queued request that misses its
deadline returns the same status and error type with `queue-timeout`. Overload
does not count as an upstream failure and therefore does not trip a provider
circuit breaker.

Prompt-free metrics record current and peak admitted deliveries, queue-wait
latency, and rejections by stable reason.

The acceptance test sends twenty requests concurrently through a real bound
HTTP listener and blocks the fake provider until all twenty have entered
delivery. It asserts a peak of twenty and successful completion of every
request. Separate deterministic tests prove bounded queue-full, timeout, permit
release, and streaming-body lifetime behavior.

## Consequences

- A default process has room for twenty simultaneous cloud-style deliveries
  without serializing them in the router.
- Slow or unavailable providers produce bounded waiting and explicit overload
  instead of unbounded process growth.
- The configured limit is process-local. It is not an organization-wide quota
  and does not make a multi-replica deployment consistent.
- Provider capacity remains an independent constraint. The current
  ChatGPT/Codex managed runtime serves one turn at a time, and local-model
  throughput depends on the device and provider adapter.
- The twenty-user test proves concurrency and admission correctness, not model
  tokens per second, end-to-end latency, or upstream vendor capacity.

## Rejected alternatives

- **Allow unlimited Tokio tasks.** Async execution prevents thread blocking but
  does not bound memory, sockets, or long-lived response streams.
- **Return `429` for process saturation.** Rate limits describe caller policy;
  temporary process capacity is service unavailability and uses `503`.
- **Count routing and cache hits as delivery.** Those operations do not consume
  scarce upstream capacity and would reduce useful throughput.
- **Release a stream permit after response headers.** The upstream connection
  and relay remain active for the lifetime of the body.
- **Claim twenty-user support from an in-process unit test alone.** The release
  contract exercises the actual TCP listener and HTTP stack.

## Related

- WF-ADR-0031 — retries, failover, and circuit breakers
- WF-ADR-0034 — rate limiting
- WF-ADR-0050 — separate managed and local gateway surfaces
- WF-ROADMAP-0010 — enterprise substrate and fleet evidence
