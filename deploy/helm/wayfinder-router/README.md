# Wayfinder Router Helm chart

This chart packages the Rust `wayfinder-router` managed data plane for a
Kubernetes cluster. It deploys the gateway as a stateless `Deployment`, an
optional single-replica Redis state service, a `ClusterIP` service, optional
Ingress, a restrictive security context, a PodDisruptionBudget, and a default
NetworkPolicy.

The chart does not create provider credentials or invent model destinations.
The data-plane process intentionally refuses to serve without at least one
virtual key and one configured model. Supply the complete TOML through an
existing Secret (recommended) or ConfigMap:

```sh
helm upgrade --install wayfinder ./deploy/helm/wayfinder-router \
  --namespace wayfinder --create-namespace \
  --set image.tag=<published-version> \
  --set config.existingSecret=wayfinder-router-config \
  --set credentials.existingSecret=wayfinder-router-credentials
```

The configuration Secret should contain a `wayfinder-router.toml` key. Keep
API keys in the credentials Secret, using environment-variable names referenced
by the TOML `api_key_env` fields. Do not put bearer tokens or provider keys in
chart values, a ConfigMap, route receipts, or logs.

## Redis and replicas

Redis is enabled by default so a two-replica installation has one shared policy
counter backend. The bundled single-replica Redis uses AOF with `everysec`
fsync and an 8 GiB `ReadWriteOnce` claim by default. For a production service,
prefer an externally managed Redis with its own availability, backup, TLS, and
recovery policy and disable the bundled StatefulSet:

```sh
helm upgrade --install wayfinder ./deploy/helm/wayfinder-router \
  --set image.tag=<published-version> \
  --set redis.enabled=false \
  --set redis.url=redis://redis.production.example:6379
```

For disposable evaluation only, set `redis.persistence.enabled=false`; that
renders an `emptyDir` and all shared policy state is lost with the Pod. Redis is
not a provider credential store.

## Runtime filesystem

The container runs as UID/GID `10001` with a read-only root filesystem. The
complete configuration is mounted read-only at `/etc/wayfinder`, without a
`subPath`, so projected Secret/ConfigMap updates remain visible. Each gateway
replica receives its own writable `emptyDir` at `/var/lib/wayfinder` for the
local audit fallback and savings snapshot. Do not attach one shared file PVC to
multiple replicas; use Redis for fleet-wide policy state.

Set either an explicit published `image.tag` or `image.digest`; the chart has no
default image reference and never infers one from `appVersion`. For production
promotion, prefer an immutable digest, for example
`--set image.digest=sha256:<64-lowercase-hex>`.

## TLS and network policy

The chart does not terminate TLS in the Rust gateway. Configure TLS on the
Ingress/controller or an external load balancer and keep the gateway Service
internal where possible. Ingress is disabled by default. The default
NetworkPolicy allows traffic to the gateway service from any namespace and
allows DNS, HTTP(S), and bundled Redis egress; set `networkPolicy.ingress` and
`networkPolicy.egress` to the selectors and ports appropriate for the cluster.

## OpenTelemetry

The repository Docker image compiles OpenTelemetry support in, but it remains
runtime-disabled unless configured. `otel.endpoint` sets
`OTEL_EXPORTER_OTLP_ENDPOINT`, while `otel.jsonLogs` enables structured JSON
logs.

## Local validation

```sh
helm lint deploy/helm/wayfinder-router --set image.tag=local-test
helm template wayfinder deploy/helm/wayfinder-router --namespace wayfinder \
  --set image.tag=local-test
helm template wayfinder deploy/helm/wayfinder-router \
  --set image.tag=local-test \
  --set ingress.enabled=true --set redis.enabled=false \
  --set config.existingSecret=router-config
```

See [`docs/managed-gateway-deployment.md`](../../../docs/managed-gateway-deployment.md)
and [WF-ADR-0059](../../../decisions/WF-ADR-0059-helm-deployment.md) for the
managed data-plane boundary and production guidance.
