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
  --set config.existingSecret=wayfinder-router-config \
  --set credentials.existingSecret=wayfinder-router-credentials
```

The configuration Secret should contain a `wayfinder-router.toml` key. Keep
API keys in the credentials Secret, using environment-variable names referenced
by the TOML `api_key_env` fields. Do not put bearer tokens or provider keys in
chart values, a ConfigMap, route receipts, or logs.

## Redis and replicas

Redis is enabled by default so a two-replica installation has one shared policy
counter backend. The bundled Redis instance is ephemeral by default and is
intended for development or a short-lived evaluation. For production, point at
a managed Redis service and disable the bundled StatefulSet:

```sh
helm upgrade --install wayfinder ./deploy/helm/wayfinder-router \
  --set redis.enabled=false \
  --set redis.url=redis://redis.production.example:6379
```

If the bundled instance is used beyond a test, enable its PVC explicitly with
`redis.persistence.enabled=true` and choose a storage class. Redis is not a
provider credential store.

## TLS and network policy

The chart does not terminate TLS in the Rust gateway. Configure TLS on the
Ingress/controller or an external load balancer and keep the gateway Service
internal where possible. Ingress is disabled by default. The default
NetworkPolicy allows traffic to the gateway service from any namespace and
allows DNS, HTTP(S), and bundled Redis egress; set `networkPolicy.ingress` and
`networkPolicy.egress` to the selectors and ports appropriate for the cluster.

## OpenTelemetry

Set `otel.enabled=true` only with a container image built with the gateway
`otel` feature. `otel.endpoint` sets `OTEL_EXPORTER_OTLP_ENDPOINT`, while
`otel.jsonLogs` enables structured JSON logs. The default image/build remains
dependency-free with respect to OpenTelemetry.

## Local validation

```sh
helm lint deploy/helm/wayfinder-router
helm template wayfinder deploy/helm/wayfinder-router --namespace wayfinder
helm template wayfinder deploy/helm/wayfinder-router \
  --set ingress.enabled=true --set redis.enabled=false \
  --set config.existingSecret=router-config
```

See [`docs/managed-gateway-deployment.md`](../../../docs/managed-gateway-deployment.md)
and [WF-ADR-0059](../../../decisions/WF-ADR-0059-helm-deployment.md) for the
managed data-plane boundary and production guidance.
