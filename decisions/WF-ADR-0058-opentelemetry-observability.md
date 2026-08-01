---
schema_version: 1
id: WF-ADR-0058
title: Opt-in OpenTelemetry gateway observability
status: accepted
date: 2026-08-01
tags: [gateway, observability, opentelemetry, tracing, rust]
---

# Opt-in OpenTelemetry gateway observability

## Decision

The Rust gateway keeps its existing prompt-free Prometheus metrics and adds an
optional `otel` Cargo feature. The feature installs a tracing subscriber and,
when `OTEL_EXPORTER_OTLP_ENDPOINT` (or the explicit
`WAYFINDER_ROUTER_OTEL_EXPORTER=1`) is present, an OTLP/HTTP span exporter.
`WAYFINDER_ROUTER_OTEL=1` enables the tracing layer; the independent
`WAYFINDER_ROUTER_JSON_LOGS=1` toggle selects newline-delimited JSON logs.

The instrumentation has three bounded spans: `wayfinder.request`,
`wayfinder.decision`, and `wayfinder.delivery` (or
`wayfinder.delivery.stream`). W3C `traceparent` and `tracestate` are extracted
from an inbound request, attached to the request span, and propagated only to
the configured provider request. No prompt, response, authorization value,
credential, or raw provider payload is recorded.

## Boundary

The default build has no OpenTelemetry dependencies, does not install a global
subscriber, and leaves `/metrics` unchanged. The gateway does not add TLS or
accept arbitrary exporter URLs from request data; the exporter is configured
only through process environment. The provider transport copies only the two
W3C propagation headers and cannot override its own authorization header.

## Consequences

- Operators can connect the request-to-decision-to-delivery path to an existing
  OpenTelemetry Collector without changing the deterministic router.
- JSON logs are an explicit deployment choice rather than a default that could
  change existing log consumers.
- A feature-enabled library used without a subscriber still forwards a valid
  inbound `traceparent`, while an installed subscriber creates a child context
  for upstream delivery.
- The feature adds a small optional exporter dependency set; ordinary desktop
  and embedded builds remain dependency-free with respect to OpenTelemetry.

## Verification

- default-feature workspace builds retain the existing dependency surface;
- `--features otel` builds and tests the propagator and JSON-log wiring;
- provider transport tests verify only `traceparent`/`tracestate` are copied;
- a mock upstream receives the propagated context and never receives prompt or
  credential data through telemetry fields.

## References

- WF-ROADMAP-0010 — enterprise trust surface
- WF-ADR-0018 — prompt-free gateway metrics
