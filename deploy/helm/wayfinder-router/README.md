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
  --set config.existingSecret=wayfinder-router-config
```

Put the complete TOML, including the credential-bearing `rediss://` URL, in the
configuration Secret. Do not pass an authenticated Redis URL through Helm
values or commit it to a ConfigMap. The Rust image includes certificate-
validated Redis TLS support. External Redis also needs an explicit
`networkPolicy.egress` rule appropriate to its cluster endpoint.

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

The chart does not terminate TLS in the Rust gateway. Ingress is disabled by
default and refuses to render without `ingress.tls`; the
`ingress.allowInsecureDevelopment` escape hatch is only for an isolated local
cluster. Production ingress must also provide controller-specific
`ingress.httpsOnlyAnnotations` that redirect or disable plaintext HTTP. The
default NetworkPolicy denies all gateway ingress, restricts DNS to the
configured cluster-DNS selectors, and permits bundled Redis only to its exact
Pod selector. Hosted providers, OTLP, and external Redis remain denied until the
operator supplies explicit peer/IPBlock and port rules. Supply only the ingress
controller/caller selectors and egress destinations the deployment needs.

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
  --set ingress.allowInsecureDevelopment=true \
  --set config.existingSecret=router-config
```

See [`docs/managed-gateway-deployment.md`](../../../docs/managed-gateway-deployment.md)
and [WF-ADR-0059](../../../decisions/WF-ADR-0059-helm-deployment.md) for the
managed data-plane boundary and production guidance.
