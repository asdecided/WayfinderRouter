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

The runtime filesystem is split deliberately: projected configuration is a
read-only directory at `/etc/wayfinder`, while each gateway Pod owns a writable
`/var/lib/wayfinder` volume for its local audit fallback and savings snapshot.
A shared file PVC is not mounted across replicas. The image and chart run as
UID/GID `10001`, and the chart requires an explicit image tag or digest rather
than silently deriving an image reference from `appVersion`.

## Boundaries

The gateway remains the authority for virtual-key authentication, model
allowlists, routing, delivery, rate limits, budgets, audit, and readiness. The
chart never renders API keys, bearer tokens, or provider responses. It mounts a
complete operator-supplied TOML file and exposes only the environment variables
from an operator-supplied credentials Secret.

TLS terminates at the Ingress/controller or an external load balancer. The Rust
listener remains HTTP inside the cluster, matching the managed-surface and
security documentation. Ingress fails to render without a TLS configuration
unless an explicit development-only escape hatch is enabled. Gateway ingress is
denied by the default NetworkPolicy until trusted peer selectors are supplied;
default egress is limited to selected cluster DNS and the exact bundled Redis
Pods. Provider, OTLP, and external Redis destinations require explicit peer or
IPBlock rules. Production ingress also requires controller-specific annotations
that redirect or disable plaintext HTTP. The bundled Redis StatefulSet is a durable
single-replica default with AOF, `appendfsync everysec`, and a `ReadWriteOnce`
claim. Production installations should still prefer a managed Redis endpoint
with an operator-owned availability, backup, TLS, and recovery policy.
Disabling persistence is an explicit disposable-development choice.
Authenticated external Redis URLs belong in the operator-owned configuration
Secret and should use certificate-validated `rediss://`; they are not Helm
values or ConfigMap data.

## Consequences

- The deployment path is reproducible and reviewable without embedding cluster
  policy in the gateway binary.
- Two gateway replicas can share the Redis policy backend without relying on
  process-local counters.
- Read-only-root containers fail at startup with an actionable path error when
  audit or savings state is not writable, rather than appearing healthy and
  losing persistence later.
- A default `helm install` fails until the operator explicitly selects a
  published image tag or digest, then the process still refuses traffic until
  supplied with a virtual key and model configuration. Both boundaries fail
  closed.
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
