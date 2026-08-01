//! Optional OpenTelemetry tracing and W3C trace-context propagation.
//!
//! The default gateway build keeps the existing dependency and logging
//! posture. Compile with `--features otel` and opt in at runtime with
//! `WAYFINDER_ROUTER_OTEL=1` (and, optionally, the standard
//! `OTEL_EXPORTER_OTLP_ENDPOINT`) to install the exporter and JSON logging
//! layers.

#![allow(clippy::missing_const_for_fn)]

#[cfg(feature = "otel")]
use std::future::Future;

use axum::body::Body;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use http::HeaderMap;
#[cfg(feature = "otel")]
use http::{HeaderName, HeaderValue};
use tracing::Instrument;

#[cfg(feature = "otel")]
const ENABLE_ENV: &str = "WAYFINDER_ROUTER_OTEL";
#[cfg(feature = "otel")]
const JSON_LOGS_ENV: &str = "WAYFINDER_ROUTER_JSON_LOGS";
#[cfg(feature = "otel")]
const EXPORTER_ENV: &str = "WAYFINDER_ROUTER_OTEL_EXPORTER";

/// Runtime guard for an installed telemetry provider.
#[cfg(feature = "otel")]
pub struct TelemetryGuard {
    provider: opentelemetry_sdk::trace::SdkTracerProvider,
}

#[cfg(feature = "otel")]
impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        let _ = self.provider.shutdown();
    }
}

/// Zero-sized default-build guard. It makes the CLI integration compile
/// without pulling any OpenTelemetry crates into the ordinary gateway.
#[cfg(not(feature = "otel"))]
pub struct TelemetryGuard;

/// Install optional tracing/exporter layers from environment configuration.
pub fn init_from_env() -> Result<Option<TelemetryGuard>, String> {
    #[cfg(feature = "otel")]
    {
        let enabled = env_truthy(ENABLE_ENV);
        let json_logs = env_truthy(JSON_LOGS_ENV);
        if !enabled && !json_logs {
            return Ok(None);
        }

        use opentelemetry::trace::TracerProvider as _;
        use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};
        use tracing_subscriber::layer::SubscriberExt as _;
        use tracing_subscriber::util::SubscriberInitExt as _;

        let resource = Resource::builder()
            .with_service_name("wayfinder-router")
            .build();
        let provider = if enabled && exporter_requested() {
            let exporter = opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .build()
                .map_err(|error| format!("OpenTelemetry exporter setup failed: {error}"))?;
            SdkTracerProvider::builder()
                .with_simple_exporter(exporter)
                .with_resource(resource)
                .build()
        } else {
            SdkTracerProvider::builder().with_resource(resource).build()
        };
        let tracer = provider.tracer("wayfinder-router");
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
        if json_logs {
            tracing_subscriber::registry()
                .with(otel_layer)
                .with(
                    tracing_subscriber::fmt::layer()
                        .json()
                        .with_target(true)
                        .with_current_span(true),
                )
                .try_init()
                .map_err(|error| format!("tracing subscriber setup failed: {error}"))?;
        } else {
            tracing_subscriber::registry()
                .with(otel_layer)
                .try_init()
                .map_err(|error| format!("tracing subscriber setup failed: {error}"))?;
        }
        Ok(Some(TelemetryGuard { provider }))
    }

    #[cfg(not(feature = "otel"))]
    {
        Ok(None)
    }
}

/// Add a bounded request span around every gateway request.
pub async fn request_middleware(request: Request<Body>, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let span = tracing::info_span!(
        "wayfinder.request",
        http.method = %method,
        http.target = %path,
        http.status_code = tracing::field::Empty,
    );

    #[cfg(feature = "otel")]
    {
        set_remote_parent(&span, request.headers());
    }
    #[cfg(feature = "otel")]
    let incoming = incoming_traceparent(request.headers());

    let future = next.run(request).instrument(span.clone());
    #[cfg(feature = "otel")]
    let response = scope_incoming(incoming, future).await;
    #[cfg(not(feature = "otel"))]
    let response = future.await;
    span.record("http.status_code", response.status().as_u16());
    response
}

/// Inject the current W3C trace context into a provider request.
///
/// Only `traceparent` and `tracestate` can be written. Credentials and prompt
/// content never enter this propagation seam.
pub fn inject_traceparent(headers: &mut HeaderMap) {
    #[cfg(feature = "otel")]
    {
        use opentelemetry::propagation::TextMapPropagator as _;
        use opentelemetry::trace::TraceContextExt as _;
        use tracing_opentelemetry::OpenTelemetrySpanExt as _;

        let context = tracing::Span::current().context();
        let valid = context.span().span_context().is_valid();
        if valid {
            let propagator = opentelemetry_sdk::propagation::TraceContextPropagator::new();
            let mut injector = HeaderInjector(headers);
            propagator.inject_context(&context, &mut injector);
            return;
        }

        // A feature-enabled library can still be used without installing the
        // process subscriber (for example, in an embedding test). Preserve a
        // validated inbound traceparent in that case.
        let _ = INCOMING_TRACEPARENT.try_with(|incoming| {
            if let Some(value) = incoming.as_deref() {
                if let Ok(value) = HeaderValue::from_str(value) {
                    headers.insert(HeaderName::from_static("traceparent"), value);
                }
            }
        });
    }

    #[cfg(not(feature = "otel"))]
    let _ = headers;
}

#[cfg(feature = "otel")]
tokio::task_local! {
    static INCOMING_TRACEPARENT: Option<String>;
}

#[cfg(feature = "otel")]
async fn scope_incoming<F>(incoming: Option<String>, future: F) -> Response
where
    F: Future<Output = Response>,
{
    INCOMING_TRACEPARENT.scope(incoming, future).await
}

#[cfg(feature = "otel")]
fn set_remote_parent(span: &tracing::Span, headers: &HeaderMap) {
    use opentelemetry::propagation::TextMapPropagator as _;
    use opentelemetry::trace::TraceContextExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let propagator = opentelemetry_sdk::propagation::TraceContextPropagator::new();
    let context = propagator.extract(&HeaderExtractor(headers));
    if context.span().span_context().is_valid() {
        let _ = span.set_parent(context);
    }
}

#[cfg(feature = "otel")]
fn incoming_traceparent(headers: &HeaderMap) -> Option<String> {
    use opentelemetry::propagation::TextMapPropagator as _;
    use opentelemetry::trace::TraceContextExt as _;
    let value = headers.get("traceparent")?.to_str().ok()?;
    let propagator = opentelemetry_sdk::propagation::TraceContextPropagator::new();
    let context = propagator.extract(&HeaderExtractor(headers));
    context
        .span()
        .span_context()
        .is_valid()
        .then(|| value.to_owned())
}

#[cfg(feature = "otel")]
struct HeaderExtractor<'a>(&'a HeaderMap);

#[cfg(feature = "otel")]
impl opentelemetry::propagation::Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        vec!["traceparent", "tracestate"]
    }
}

#[cfg(feature = "otel")]
struct HeaderInjector<'a>(&'a mut HeaderMap);

#[cfg(feature = "otel")]
impl opentelemetry::propagation::Injector for HeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        let Ok(name) = HeaderName::from_bytes(key.as_bytes()) else {
            return;
        };
        let Ok(value) = HeaderValue::from_str(&value) else {
            return;
        };
        self.0.insert(name, value);
    }
}

#[cfg(feature = "otel")]
fn exporter_requested() -> bool {
    env_truthy(EXPORTER_ENV)
        || std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").is_some_and(|value| !value.is_empty())
}

#[cfg(any(feature = "otel", test))]
fn env_truthy(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_build_does_not_enable_telemetry_from_absent_environment() {
        assert!(!env_truthy("WAYFINDER_ROUTER_TEST_ENV_THAT_IS_NOT_SET"));
    }

    #[cfg(feature = "otel")]
    #[test]
    fn invalid_traceparent_is_not_retained() {
        let mut headers = HeaderMap::new();
        headers.insert("traceparent", HeaderValue::from_static("not-a-trace"));
        assert!(incoming_traceparent(&headers).is_none());
    }

    #[cfg(feature = "otel")]
    #[tokio::test]
    async fn validated_parent_is_forwarded_without_a_subscriber() {
        let parent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let response = scope_incoming(Some(parent.to_owned()), async {
            let mut headers = HeaderMap::new();
            inject_traceparent(&mut headers);
            assert_eq!(
                headers
                    .get("traceparent")
                    .and_then(|value| value.to_str().ok()),
                Some(parent)
            );
            Response::new(Body::empty())
        })
        .await;
        assert_eq!(response.status(), http::StatusCode::OK);
    }
}
