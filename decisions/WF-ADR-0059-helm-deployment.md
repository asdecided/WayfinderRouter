---
schema_version: 1
id: WF-ADR-0059
type: decision
status: accepted
date: 2026-08-01
tags: [gateway, rust, kubernetes, helm, deployment, redis, security]
roadmap: WF-ROADMAP-0010
---

# Helm deployment for the Rust managed data plane

## Decision

Ship `deploy/helm/wayfinder-router` as the supported Kubernetes packaging
surface for the Rust managed data plane. The chart runs the existing
`wayfinder-router serve --surface data-plane` process; it does not introduce a
second gateway, a Python runtime, or a new request protocol.

The chart owns deployment wiring only:

- a horizontally scalable gateway Deployment with bounded probes and graceful
  termination;
- a ClusterIP Service on port 8088;
- optional Ingress for controller-managed TLS;
- optional single-replica Redis for shared policy state;
- a PodDisruptionBudget, security context, service account, and NetworkPolicy;
- references to operator-owned configuration and credential Secrets.

## Boundaries

The gateway remains the authority for virtual-key authentication, model
allowlists, routing, delivery, rate limits, budgets, audit, and readiness. The
chart never renders API keys, bearer tokens, or provider responses. It mounts a
complete operator-supplied TOML file and exposes only the environment variables
from an operator-supplied credentials Secret.

TLS terminates at the Ingress/controller or an external load balancer. The Rust
listener remains HTTP inside the cluster, matching the managed-surface and
security documentation. The bundled Redis StatefulSet is an ephemeral
development default; production installations should use a managed Redis
endpoint or explicitly enable persistence.

## Consequences

- The deployment path is reproducible and reviewable without embedding cluster
  policy in the gateway binary.
- Two gateway replicas can share the Redis policy backend without relying on
  process-local counters.
- A default `helm install` renders successfully but intentionally cannot serve
  traffic until the operator supplies a virtual key and model configuration;
  this preserves the gateway's fail-closed data-plane contract.
- Helm is a packaging surface, not an operator control plane. Kubernetes secret
  rotation, TLS policy, ingress authentication, and Redis durability remain
  cluster/operator responsibilities.

## Verification

Chart CI runs `helm lint` and renders both the default bundled-Redis form and an
external-config, external-Redis, Ingress-enabled form. The rendered manifests
are checked for the managed data-plane command, probes, configuration mount,
and absence of inline credentials.

## Related decisions

- [WF-ADR-0050](WF-ADR-0050-managed-gateway-surfaces.md) — managed data-plane
  boundary.
- [WF-ADR-0051](WF-ADR-0051-bounded-delivery-concurrency.md) — per-process
  capacity guardrails.
- [WF-ADR-0053](WF-ADR-0053-shared-state-backend.md) — Redis policy state.
- [WF-ADR-0058](WF-ADR-0058-opentelemetry-observability.md) — opt-in traces.
