use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use bytes::Bytes;
use indexmap::IndexMap;
use serde_json::{Value, json};
use tokio::sync::{Barrier, Semaphore, oneshot};
use tower::ServiceExt;
use wayfinder_config::gateway::{
    Budget as GatewayBudget, DeploymentSelectionPolicy, DeploymentSelectionStrategy, GatewayConfig,
    ProviderKind, ProviderTier, RateLimit, RoutePreset, VirtualKey, Workspace,
};
use wayfinder_routing_core::{
    ClassifierModel, ExecutionBoundary, PrivacyPosture, RoutingConfig, RoutingRequest,
    RoutingRequirements,
};
use wayfinder_service::pricing::{SavingsLedger, UtcDate, turn_cost};

use super::{
    AppState, ConfiguredDeployment, ConfiguredModel, DataPlaneError, OFFLINE_HEADER,
    PRIVACY_POSTURE_HEADER, ROUTER_CACHE_HEADER, ROUTER_OVERLOAD_HEADER,
    ROUTER_PRIVACY_POSTURE_HEADER, ROUTER_ROUTE_HEADER, ROUTER_WORKSPACE_HEADER, RouteOn,
    build_data_plane_router, build_reloadable_router, build_router, deliver_with_reliability,
    model_is_proven_local, utc_date_from_unix_days, utc_today, validate_data_plane_state,
};
use crate::delivery::{
    BufferedDelivery, BufferedDeliveryResponse, DeliveryError, DeliveryFuture, StreamingDelivery,
    StreamingDeliveryFuture, StreamingDeliveryResponse,
};
use crate::responses;
use crate::{
    access::AccessPolicy,
    admission::{AdmissionError, AdmissionPolicy},
    audit::AuditLog,
    auth,
    cache::{CacheSettings, CachedResponse, cache_key},
    control_plane::{
        AdministrativeIdentity, BindingKind, PolicyBinding, PolicyDocument, PolicyLifecycle,
        PolicyProfile,
    },
    recent::{RecentEntry, RecentOutcome},
    server::serve_with_shutdown,
};

type TestResult = Result<(), Box<dyn Error>>;
type SeenTargets = Arc<Mutex<Vec<String>>>;

#[test]
fn deployment_pool_rotates_weighted_targets_without_changing_the_public_alias() {
    let model = ConfiguredModel::new(
        "cloud",
        "https://primary.example/v1",
        "gpt-primary",
        None,
        true,
    )
    .with_deployments(vec![
        ConfiguredDeployment::new("eu", "https://eu.example/v1", "gpt-eu", None, true, 2),
        ConfiguredDeployment::new(
            "backup",
            "https://backup.example/v1",
            "gpt-backup",
            None,
            true,
            1,
        ),
    ]);
    let state = AppState::new(RoutingConfig::binary(0.5), vec![model], false, "test");

    let first = state.deployment_candidates("cloud");
    let second = state.deployment_candidates("cloud");
    let third = state.deployment_candidates("cloud");
    let fourth = state.deployment_candidates("cloud");
    assert_eq!(first[0].name(), "cloud");
    assert_eq!(first[0].delivery_id(), "cloud#default");
    assert_eq!(second[0].delivery_id(), "cloud#eu");
    assert_eq!(third[0].delivery_id(), "cloud#eu");
    assert_eq!(fourth[0].delivery_id(), "cloud#backup");
    assert_eq!(second[1].delivery_id(), "cloud#backup");
    assert_eq!(second[2].delivery_id(), "cloud#default");
}

#[test]
fn runtime_signals_reorder_only_members_of_the_public_alias() {
    let model = ConfiguredModel::new(
        "cloud",
        "https://primary.example/v1",
        "gpt-primary",
        None,
        true,
    )
    .with_deployments(vec![
        ConfiguredDeployment::new("slow", "https://slow.example/v1", "gpt-slow", None, true, 1),
        ConfiguredDeployment::new("fast", "https://fast.example/v1", "gpt-fast", None, true, 1),
    ])
    .with_deployment_selection(DeploymentSelectionPolicy {
        strategy: DeploymentSelectionStrategy::Latency,
        ..DeploymentSelectionPolicy::default()
    });
    let state = AppState::new(RoutingConfig::binary(0.5), vec![model], false, "test");
    for (deployment, latency) in [
        ("cloud#default", 0.3),
        ("cloud#slow", 0.8),
        ("cloud#fast", 0.1),
    ] {
        state.observe_deployment(
            deployment,
            crate::deployment_selection::DeploymentObservation::success(
                latency,
                Some(latency / 2.0),
                Some(100.0),
            ),
        );
    }
    let selection = state.select_deployments("cloud", state.deployment_candidates("cloud"));
    assert_eq!(selection.reason, "latency");
    assert_eq!(selection.targets[0].delivery_id(), "cloud#fast");
    assert!(
        selection
            .targets
            .iter()
            .all(|target| target.name() == "cloud")
    );
}

struct FakeDelivery {
    seen: Arc<Mutex<Vec<(String, Value)>>>,
    fail: bool,
}

impl BufferedDelivery for FakeDelivery {
    fn send<'a>(&'a self, model: &'a ConfiguredModel, body: Value) -> DeliveryFuture<'a> {
        Box::pin(async move {
            self.seen
                .lock()
                .map_err(|_| {
                    DeliveryError::Provider(
                        wayfinder_providers::openai_compat::ProviderError::Transport,
                    )
                })?
                .push((model.provider_model().to_owned(), body));
            if self.fail {
                return Err(DeliveryError::Provider(
                    wayfinder_providers::openai_compat::ProviderError::Transport,
                ));
            }
            Ok(BufferedDeliveryResponse::new(
                StatusCode::CREATED,
                "application/json",
                Bytes::from_static(b"{\"id\":\"provider-response\"}"),
            ))
        })
    }
}

struct PoolBoundaryDelivery {
    seen: SeenTargets,
}

impl BufferedDelivery for PoolBoundaryDelivery {
    fn send<'a>(&'a self, model: &'a ConfiguredModel, _body: Value) -> DeliveryFuture<'a> {
        Box::pin(async move {
            self.seen
                .lock()
                .map_err(|_| {
                    DeliveryError::Provider(
                        wayfinder_providers::openai_compat::ProviderError::Transport,
                    )
                })?
                .push(model.provider_model().to_owned());
            if model.provider_model() == "local-default" {
                return Err(DeliveryError::Provider(
                    wayfinder_providers::openai_compat::ProviderError::Transport,
                ));
            }
            Ok(BufferedDeliveryResponse::new(
                StatusCode::OK,
                "application/json",
                Bytes::from_static(EXACT_USAGE_BODY),
            ))
        })
    }
}

struct PoolBoundaryStreamingDelivery {
    seen: SeenTargets,
}

impl StreamingDelivery for PoolBoundaryStreamingDelivery {
    fn send_stream<'a>(
        &'a self,
        model: &'a ConfiguredModel,
        _body: Value,
    ) -> StreamingDeliveryFuture<'a> {
        Box::pin(async move {
            self.seen
                .lock()
                .map_err(|_| {
                    DeliveryError::Provider(
                        wayfinder_providers::openai_compat::ProviderError::Transport,
                    )
                })?
                .push(model.provider_model().to_owned());
            if model.provider_model() == "local-default" {
                return Err(DeliveryError::Provider(
                    wayfinder_providers::openai_compat::ProviderError::Transport,
                ));
            }
            Ok(StreamingDeliveryResponse::new(
                StatusCode::OK,
                "text/event-stream",
                Box::pin(futures_util::stream::iter([Ok(Bytes::from_static(
                    b"data: [DONE]\n\n",
                ))])),
            ))
        })
    }
}

#[tokio::test]
async fn buffered_pool_delivery_uses_member_but_returns_public_alias() -> TestResult {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let model = ConfiguredModel::new(
        "cloud",
        "https://primary.example/v1",
        "gpt-primary",
        None,
        true,
    )
    .with_deployments(vec![ConfiguredDeployment::new(
        "eu",
        "https://eu.example/v1",
        "gpt-eu",
        None,
        true,
        1,
    )]);
    let state = AppState::new(RoutingConfig::binary(0.5), vec![model], false, "test");
    let delivery = FakeDelivery {
        seen: Arc::clone(&seen),
        fail: false,
    };
    let outcome = deliver_with_reliability(
        &state,
        &delivery,
        &["cloud".to_owned()],
        &RoutingRequest {
            schema_version: wayfinder_routing_core::RUNTIME_CONTRACT_VERSION,
            request_id: "test-request".to_owned(),
            prompt: "hello".to_owned(),
            privacy_posture: PrivacyPosture::HostedAllowed,
            requirements: RoutingRequirements::default(),
        },
        false,
        1,
        &json!({"model": "cloud"}),
    )
    .await
    .map_err(|_| std::io::Error::other("delivery failed"))?;
    assert_eq!(outcome.served_by, "cloud");
    assert_eq!(outcome.served_by_id, "cloud#default");
    assert_eq!(outcome.selection_reason, "weighted");
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("seen lock poisoned"))?[0]
            .0,
        "gpt-primary"
    );
    Ok(())
}

#[tokio::test]
async fn buffered_pool_never_crosses_the_requested_privacy_boundary() -> TestResult {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let model = ConfiguredModel::new(
        "local",
        "http://127.0.0.1:11434/v1",
        "local-default",
        None,
        true,
    )
    .with_deployments(vec![ConfiguredDeployment::new(
        "hosted",
        "https://hosted.example/v1",
        "hosted-child",
        None,
        true,
        1,
    )]);
    let state = AppState::new(RoutingConfig::binary(0.5), vec![model], false, "test")
        .with_gateway_reliability(&GatewayConfig {
            retries: 0,
            ..GatewayConfig::default()
        })?
        .with_delivery(Arc::new(PoolBoundaryDelivery {
            seen: Arc::clone(&seen),
        }));

    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "local",
            "messages": [{"role": "user", "content": "keep this here"}]
        }),
        &[(PRIVACY_POSTURE_HEADER, "on-device-only")],
    )
    .await?;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("seen lock poisoned"))?
            .as_slice(),
        ["local-default"]
    );
    Ok(())
}

#[tokio::test]
async fn streaming_pool_never_crosses_the_offline_boundary() -> TestResult {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let model = ConfiguredModel::new(
        "local",
        "http://127.0.0.1:11434/v1",
        "local-default",
        None,
        true,
    )
    .with_deployments(vec![ConfiguredDeployment::new(
        "hosted",
        "https://hosted.example/v1",
        "hosted-child",
        None,
        true,
        1,
    )]);
    let state = AppState::new(RoutingConfig::binary(0.5), vec![model], false, "test")
        .with_gateway_reliability(&GatewayConfig {
            retries: 0,
            ..GatewayConfig::default()
        })?
        .with_streaming_delivery(Arc::new(PoolBoundaryStreamingDelivery {
            seen: Arc::clone(&seen),
        }));

    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "local",
            "stream": true,
            "messages": [{"role": "user", "content": "stay offline"}]
        }),
        &[(OFFLINE_HEADER, "true")],
    )
    .await?;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("seen lock poisoned"))?
            .as_slice(),
        ["local-default"]
    );
    Ok(())
}

struct AcceptingStreamingDelivery {
    seen: SeenTargets,
}

impl StreamingDelivery for AcceptingStreamingDelivery {
    fn send_stream<'a>(
        &'a self,
        model: &'a ConfiguredModel,
        _body: Value,
    ) -> StreamingDeliveryFuture<'a> {
        Box::pin(async move {
            self.seen
                .lock()
                .map_err(|_| {
                    DeliveryError::Provider(
                        wayfinder_providers::openai_compat::ProviderError::Transport,
                    )
                })?
                .push(model.provider_model().to_owned());
            Ok(StreamingDeliveryResponse::new(
                StatusCode::OK,
                "text/event-stream",
                Box::pin(futures_util::stream::iter([Ok(Bytes::from_static(
                    b"data: [DONE]\n\n",
                ))])),
            ))
        })
    }
}

#[tokio::test]
async fn streaming_pool_stops_at_the_first_established_deployment() -> TestResult {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let model = ConfiguredModel::new(
        "local",
        "http://127.0.0.1:11434/v1",
        "local-default",
        None,
        true,
    )
    .with_deployments(vec![ConfiguredDeployment::new(
        "second",
        "http://127.0.0.1:11435/v1",
        "local-second",
        None,
        true,
        1,
    )]);
    let state = AppState::new(RoutingConfig::binary(0.5), vec![model], false, "test")
        .with_gateway_reliability(&GatewayConfig {
            retries: 0,
            ..GatewayConfig::default()
        })?
        .with_streaming_delivery(Arc::new(AcceptingStreamingDelivery {
            seen: Arc::clone(&seen),
        }));

    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "local",
            "stream": true,
            "messages": [{"role": "user", "content": "one upstream call"}]
        }),
        &[],
    )
    .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let _ = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("seen lock poisoned"))?
            .len(),
        1,
        "an established stream must not be followed by calls to the pool's other deployments"
    );
    Ok(())
}

type TestStateFuture<'a, T> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<T, crate::state::StateBackendError>> + Send + 'a>,
>;

/// The in-process backend under the name the canary path requires, so canary
/// accounting can be exercised without a Redis server.
#[derive(Clone, Debug, Default)]
struct CanaryTestBackend(crate::state::MemoryStateBackend);

impl crate::state::StateBackend for CanaryTestBackend {
    fn backend_name(&self) -> &'static str {
        "redis"
    }

    fn admit<'a>(
        &'a self,
        key: &'a str,
        config: crate::state::StateLimitConfig,
        now_seconds: f64,
    ) -> TestStateFuture<'a, crate::rate_limit::RateResult> {
        self.0.admit(key, config, now_seconds)
    }

    fn reserve<'a>(
        &'a self,
        key: &'a str,
        config: crate::state::StateLimitConfig,
        tokens: u64,
        now_seconds: f64,
    ) -> TestStateFuture<'a, crate::rate_limit::ReservationResult> {
        self.0.reserve(key, config, tokens, now_seconds)
    }

    fn reserve_tokens<'a>(
        &'a self,
        key: &'a str,
        config: crate::state::StateLimitConfig,
        tokens: u64,
        now_seconds: f64,
    ) -> TestStateFuture<'a, crate::rate_limit::ReservationResult> {
        self.0.reserve_tokens(key, config, tokens, now_seconds)
    }

    fn reconcile<'a>(
        &'a self,
        key: &'a str,
        config: crate::state::StateLimitConfig,
        reservation: crate::rate_limit::RateReservation,
        actual_tokens: u64,
        now_seconds: f64,
    ) -> TestStateFuture<'a, bool> {
        self.0
            .reconcile(key, config, reservation, actual_tokens, now_seconds)
    }

    fn rollback<'a>(
        &'a self,
        key: &'a str,
        config: crate::state::StateLimitConfig,
        reservation: crate::rate_limit::RateReservation,
        now_seconds: f64,
    ) -> TestStateFuture<'a, bool> {
        self.0.rollback(key, config, reservation, now_seconds)
    }

    fn add_tokens<'a>(
        &'a self,
        key: &'a str,
        config: crate::state::StateLimitConfig,
        tokens: u64,
        now_seconds: f64,
    ) -> TestStateFuture<'a, ()> {
        self.0.add_tokens(key, config, tokens, now_seconds)
    }

    fn record_canary(
        &self,
        observation: crate::state::SharedCanaryObservation,
        window_seconds: f64,
        now_seconds: f64,
    ) -> TestStateFuture<'_, bool> {
        self.0
            .record_canary(observation, window_seconds, now_seconds)
    }

    fn canary_snapshot<'a>(
        &'a self,
        rollout_id: &'a str,
        window_seconds: f64,
        now_seconds: f64,
    ) -> TestStateFuture<'a, crate::state::SharedCanarySnapshot> {
        self.0
            .canary_snapshot(rollout_id, window_seconds, now_seconds)
    }

    fn snapshot<'a>(
        &'a self,
        key: &'a str,
        config: crate::state::StateLimitConfig,
        now_seconds: f64,
    ) -> TestStateFuture<'a, Option<crate::rate_limit::RateSnapshot>> {
        self.0.snapshot(key, config, now_seconds)
    }

    fn degraded(&self) -> bool {
        false
    }
}

#[tokio::test]
async fn canary_cache_hit_is_not_recorded_as_a_canary_failure() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let backend = Arc::new(CanaryTestBackend::default());
    let mut state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "upstream-local",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.0)),
            ConfiguredModel::new(
                "cloud",
                "https://cloud.example/v1",
                "upstream-cloud",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.01)),
        ],
        false,
        "test",
    )
    .with_delivery(Arc::new(CountingUsageDelivery {
        calls: Arc::clone(&calls),
    }))
    .with_cache_and_clock(
        CacheSettings {
            enabled: true,
            ttl_seconds: 300.0,
            max_entries: 16,
            max_bytes: 1024 * 1024,
        },
        || 1_000.0,
    )?
    .with_route_presets(
        [(
            "candidate".to_owned(),
            RoutePreset {
                models: vec!["cloud".to_owned()],
            },
        )]
        .into_iter()
        .collect(),
    );
    state.shared_state_backend = Some(backend.clone());
    state.canary_config = Arc::new(wayfinder_config::gateway::CanaryConfig {
        enabled: true,
        rollout_id: "rollout-1".to_owned(),
        candidate_route: "candidate".to_owned(),
        fraction: 1.0,
        ..wayfinder_config::gateway::CanaryConfig::default()
    });
    let window = state.canary_config().window;
    let body = json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "same deterministic prompt"}]
    });

    let first = post_json(&state, "/v1/chat/completions", &body, &[]).await?;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        first.headers().get("x-wayfinder-router-mode"),
        Some(&axum::http::HeaderValue::from_static("canary"))
    );
    let _ = to_bytes(first.into_body(), usize::MAX).await?;

    let second = post_json(&state, "/v1/chat/completions", &body, &[]).await?;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        second.headers().get(ROUTER_CACHE_HEADER),
        Some(&axum::http::HeaderValue::from_static("hit"))
    );
    let _ = to_bytes(second.into_body(), usize::MAX).await?;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the second request is a cache hit"
    );

    // Let any outcome recorded from a dropped guard run before reading.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    let snapshot = crate::state::StateBackend::canary_snapshot(
        backend.as_ref(),
        "rollout-1",
        window,
        crate::wall_clock_seconds(),
    )
    .await
    .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    assert_eq!(
        snapshot.requests, 1,
        "only the delivered request is observed"
    );
    assert_eq!(
        snapshot.errors, 0,
        "a cache hit says nothing about the candidate"
    );
    Ok(())
}

#[tokio::test]
async fn buffered_fatal_fault_follows_the_streaming_breaker_classification() -> TestResult {
    let config = GatewayConfig {
        retries: 0,
        breaker_threshold: 1,
        breaker_cooldown: 60.0,
        ..GatewayConfig::default()
    };
    let body = json!({
        "model": "chatgpt",
        "messages": [{"role": "user", "content": "hello"}]
    });
    // A provider fault opens the breaker; an account-state fault does not.
    for (error, opens) in [
        (crate::delivery::CodexDeliveryError::Unavailable, true),
        (crate::delivery::CodexDeliveryError::Busy, false),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let state = AppState::new(
            RoutingConfig::binary(0.5),
            vec![
                ConfiguredModel::new("chatgpt", "", "shared-provider-model", None, true)
                    .with_provider(ProviderKind::CodexAppServer, None),
            ],
            false,
            "test",
        )
        .with_gateway_reliability(&config)?
        .with_delivery(Arc::new(CodexFailureDelivery {
            calls: Arc::clone(&calls),
            error,
        }));
        let _ = post_json(&state, "/v1/chat/completions", &body, &[]).await?;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        let _ = post_json(&state, "/v1/chat/completions", &body, &[]).await?;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            if opens { 1 } else { 2 },
            "{error:?}: second request reaches the provider only if the breaker stayed closed"
        );
    }
    Ok(())
}

#[tokio::test]
async fn failover_served_response_is_not_stored_in_the_cache() -> TestResult {
    let config = GatewayConfig {
        retries: 0,
        ..GatewayConfig::default()
    };
    let outcomes = BTreeMap::from([
        (
            "cloud".to_owned(),
            VecDeque::from([ScriptedOutcome::Transport]),
        ),
        (
            "cloud-backup".to_owned(),
            VecDeque::from([ScriptedOutcome::Response(StatusCode::OK, EXACT_USAGE_BODY)]),
        ),
    ]);
    let (state, seen) = reliability_live_state(&config, Vec::new(), outcomes)?;
    let state = state
        .with_route_presets(IndexMap::from([(
            "hosted".to_owned(),
            RoutePreset {
                models: vec!["cloud".to_owned(), "cloud-backup".to_owned()],
            },
        )]))
        .with_cache_and_clock(
            CacheSettings {
                enabled: true,
                ttl_seconds: 300.0,
                max_entries: 16,
                max_bytes: 1024 * 1024,
            },
            || 1_000.0,
        )?;

    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "@route/hosted",
            "messages": [{"role": "user", "content": "served by the fallback"}]
        }),
        &[],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-served-by")
            .and_then(|value| value.to_str().ok()),
        Some("cloud-backup")
    );
    let _ = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("seen lock poisoned"))?
            .as_slice(),
        ["cloud", "cloud-backup"]
    );
    assert_eq!(
        state.response_cache().stats()?.entries,
        0,
        "a failover response would be stored under a key no lookup computes"
    );
    Ok(())
}

#[test]
fn anthropic_surface_forwards_canary_and_shadow_consent_headers() {
    let mut source = axum::http::HeaderMap::new();
    for name in [
        crate::canary::CANARY_IDENTITY_HEADER,
        crate::canary::CANARY_COHORT_HEADER,
        wayfinder_config::gateway::SHADOW_PROVIDER_CONSENT_HEADER,
        "x-unrelated",
    ] {
        source.insert(name, axum::http::HeaderValue::from_static("value"));
    }
    let forwarded = crate::chat_control_headers(&source);
    assert!(forwarded.contains_key(crate::canary::CANARY_IDENTITY_HEADER));
    assert!(forwarded.contains_key(crate::canary::CANARY_COHORT_HEADER));
    assert!(forwarded.contains_key(wayfinder_config::gateway::SHADOW_PROVIDER_CONSENT_HEADER));
    assert!(!forwarded.contains_key("x-unrelated"));
}

#[test]
fn reloadable_router_is_built_once_per_snapshot() -> TestResult {
    let state = || AppState::new(RoutingConfig::binary(0.5), Vec::new(), false, "test");
    let holder = Arc::new(crate::reload::LastGood::new(state(), 1_u64));
    let dispatch = crate::ReloadableDispatch::new(Arc::clone(&holder));
    let builds = AtomicUsize::new(0);
    let build = |state: AppState| {
        builds.fetch_add(1, Ordering::SeqCst);
        Ok::<_, std::convert::Infallible>(build_router(state))
    };

    let first = holder.current()?;
    for _ in 0..5 {
        assert!(dispatch.router_for(&first, build).is_ok());
    }
    assert_eq!(
        builds.load(Ordering::SeqCst),
        1,
        "one build for one snapshot"
    );

    // A reload installs a different snapshot, which gets its own router.
    let _ = holder.refresh(2_u64, || Ok::<_, std::convert::Infallible>(state()))?;
    let second = holder.current()?;
    assert!(!Arc::ptr_eq(&first, &second));
    for _ in 0..5 {
        assert!(dispatch.router_for(&second, build).is_ok());
    }
    assert_eq!(builds.load(Ordering::SeqCst), 2);

    // A failed build is reported and never cached.
    let _ = holder.refresh(3_u64, || Ok::<_, std::convert::Infallible>(state()))?;
    let third = holder.current()?;
    assert!(matches!(
        dispatch.router_for(&third, |_| Err::<axum::Router, _>("not ready")),
        Err(Some("not ready"))
    ));
    assert!(dispatch.router_for(&third, build).is_ok());
    assert_eq!(builds.load(Ordering::SeqCst), 3);
    Ok(())
}

const EXACT_USAGE_BODY: &[u8] =
    br#"{"object":"chat.completion","usage":{"prompt_tokens":1000,"completion_tokens":25},"choices":[{"message":{"content":"ok"}}]}"#;

struct ExactUsageDelivery;

impl BufferedDelivery for ExactUsageDelivery {
    fn send<'a>(&'a self, _model: &'a ConfiguredModel, _body: Value) -> DeliveryFuture<'a> {
        Box::pin(async {
            Ok(BufferedDeliveryResponse::new(
                StatusCode::OK,
                "application/json",
                Bytes::from_static(EXACT_USAGE_BODY),
            ))
        })
    }
}

struct CountingUsageDelivery {
    calls: Arc<AtomicUsize>,
}

struct ConcurrentDelivery {
    barrier: Arc<Barrier>,
    current: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl BufferedDelivery for ConcurrentDelivery {
    fn send<'a>(&'a self, _model: &'a ConfiguredModel, _body: Value) -> DeliveryFuture<'a> {
        Box::pin(async move {
            let current = self.current.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(current, Ordering::SeqCst);
            self.barrier.wait().await;
            self.current.fetch_sub(1, Ordering::SeqCst);
            Ok(BufferedDeliveryResponse::new(
                StatusCode::OK,
                "application/json",
                Bytes::from_static(EXACT_USAGE_BODY),
            ))
        })
    }
}

struct GatedDelivery {
    entered: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
}

impl BufferedDelivery for GatedDelivery {
    fn send<'a>(&'a self, _model: &'a ConfiguredModel, _body: Value) -> DeliveryFuture<'a> {
        Box::pin(async move {
            self.entered.fetch_add(1, Ordering::SeqCst);
            self.release
                .acquire()
                .await
                .map_err(|_| {
                    DeliveryError::Provider(
                        wayfinder_providers::openai_compat::ProviderError::Transport,
                    )
                })?
                .forget();
            Ok(BufferedDeliveryResponse::new(
                StatusCode::OK,
                "application/json",
                Bytes::from_static(EXACT_USAGE_BODY),
            ))
        })
    }
}

impl BufferedDelivery for CountingUsageDelivery {
    fn send<'a>(&'a self, _model: &'a ConfiguredModel, _body: Value) -> DeliveryFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(BufferedDeliveryResponse::new(
                StatusCode::OK,
                "application/json",
                Bytes::from_static(EXACT_USAGE_BODY),
            ))
        })
    }
}

struct CodexFailureDelivery {
    calls: Arc<AtomicUsize>,
    error: crate::delivery::CodexDeliveryError,
}

impl BufferedDelivery for CodexFailureDelivery {
    fn send<'a>(&'a self, _model: &'a ConfiguredModel, _body: Value) -> DeliveryFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let error = self.error;
        Box::pin(async move { Err(DeliveryError::Codex(error)) })
    }
}

enum ScriptedOutcome {
    Response(StatusCode, &'static [u8]),
    Transport,
}

struct ScriptedDelivery {
    outcomes: Arc<Mutex<BTreeMap<String, VecDeque<ScriptedOutcome>>>>,
    seen: SeenTargets,
}

enum ScriptedStreamOutcome {
    Chunks(Vec<Result<Bytes, DeliveryError>>),
    Response(StatusCode, Vec<Result<Bytes, DeliveryError>>),
    Cancellable(Arc<AtomicBool>),
    EstablishError(DeliveryError),
}

struct PendingDropStream {
    dropped: Arc<AtomicBool>,
}

impl futures_util::Stream for PendingDropStream {
    type Item = Result<Bytes, DeliveryError>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Pending
    }
}

impl Drop for PendingDropStream {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

struct ScriptedStreamingDelivery {
    outcomes: Arc<Mutex<BTreeMap<String, VecDeque<ScriptedStreamOutcome>>>>,
    seen: SeenTargets,
}

impl StreamingDelivery for ScriptedStreamingDelivery {
    fn send_stream<'a>(
        &'a self,
        model: &'a ConfiguredModel,
        _body: Value,
    ) -> StreamingDeliveryFuture<'a> {
        Box::pin(async move {
            self.seen
                .lock()
                .map_err(|_| {
                    DeliveryError::Provider(
                        wayfinder_providers::openai_compat::ProviderError::Transport,
                    )
                })?
                .push(model.name().to_owned());
            let outcome = self
                .outcomes
                .lock()
                .map_err(|_| {
                    DeliveryError::Provider(
                        wayfinder_providers::openai_compat::ProviderError::Transport,
                    )
                })?
                .get_mut(model.name())
                .and_then(VecDeque::pop_front)
                .ok_or(DeliveryError::Provider(
                    wayfinder_providers::openai_compat::ProviderError::Transport,
                ))?;
            match outcome {
                ScriptedStreamOutcome::Chunks(chunks) => Ok(StreamingDeliveryResponse::new(
                    StatusCode::OK,
                    "text/event-stream",
                    Box::pin(futures_util::stream::iter(chunks)),
                )),
                ScriptedStreamOutcome::Response(status, chunks) => {
                    Ok(StreamingDeliveryResponse::new(
                        status,
                        "application/json",
                        Box::pin(futures_util::stream::iter(chunks)),
                    ))
                }
                ScriptedStreamOutcome::Cancellable(dropped) => Ok(StreamingDeliveryResponse::new(
                    StatusCode::OK,
                    "text/event-stream",
                    Box::pin(PendingDropStream { dropped }),
                )),
                ScriptedStreamOutcome::EstablishError(error) => Err(error),
            }
        })
    }
}

impl BufferedDelivery for ScriptedDelivery {
    fn send<'a>(&'a self, model: &'a ConfiguredModel, _body: Value) -> DeliveryFuture<'a> {
        Box::pin(async move {
            self.seen
                .lock()
                .map_err(|_| {
                    DeliveryError::Provider(
                        wayfinder_providers::openai_compat::ProviderError::Transport,
                    )
                })?
                .push(model.name().to_owned());
            let outcome = self
                .outcomes
                .lock()
                .map_err(|_| {
                    DeliveryError::Provider(
                        wayfinder_providers::openai_compat::ProviderError::Transport,
                    )
                })?
                .get_mut(model.name())
                .and_then(VecDeque::pop_front)
                .ok_or(DeliveryError::Provider(
                    wayfinder_providers::openai_compat::ProviderError::Transport,
                ))?;
            match outcome {
                ScriptedOutcome::Response(status, body) => Ok(BufferedDeliveryResponse::new(
                    status,
                    "application/json",
                    Bytes::from_static(body),
                )),
                ScriptedOutcome::Transport => Err(DeliveryError::Provider(
                    wayfinder_providers::openai_compat::ProviderError::Transport,
                )),
            }
        })
    }
}

fn configured_state() -> AppState {
    AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new(
                "zeta",
                "https://zeta.example/v1",
                "provider-zeta",
                Some("ZETA_API_KEY".to_owned()),
                false,
            ),
            ConfiguredModel::new(
                "alpha",
                "http://127.0.0.1:11434/v1",
                "provider-alpha",
                None,
                false,
            ),
        ],
        true,
        "test-build-secret-free",
    )
    .with_dry_run(true)
    .with_request_body_limit(4096)
}

fn managed_data_plane_state(model_ready: bool) -> Result<AppState, DataPlaneError> {
    let mut config = GatewayConfig::default();
    config.keys.insert(
        "team-a".to_owned(),
        VirtualKey {
            hash: auth::hash_key("wf-secret"),
            tags: vec!["engineering".to_owned()],
            workspace: None,
            budget: None,
            rate_limit: None,
            models: vec!["local".to_owned()],
        },
    );
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "provider-local",
                Some("LOCAL_API_KEY".to_owned()),
                model_ready,
            ),
            ConfiguredModel::new(
                "cloud",
                "https://cloud.example/v1",
                "provider-cloud",
                Some("CLOUD_API_KEY".to_owned()),
                false,
            ),
        ],
        false,
        "test",
    )
    .with_dry_run(true)
    .with_gateway_access(&config)
    .map_err(|_| DataPlaneError::AuthenticationRequired)?;
    validate_data_plane_state(&state)?;
    Ok(state)
}

fn cached_live_state(calls: Arc<AtomicUsize>) -> Result<AppState, crate::cache::CacheError> {
    AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "upstream-local",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.0)),
            ConfiguredModel::new(
                "cloud",
                "https://cloud.example/v1",
                "upstream-cloud",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.01)),
        ],
        false,
        "test",
    )
    .with_delivery(Arc::new(CountingUsageDelivery { calls }))
    .with_cache_and_clock(
        CacheSettings {
            enabled: true,
            ttl_seconds: 300.0,
            max_entries: 16,
            max_bytes: 1024 * 1024,
        },
        || 1_000.0,
    )
}

fn budget_live_state(
    config: &GatewayConfig,
    offline: bool,
    calls: Arc<AtomicUsize>,
) -> Result<AppState, crate::access::AccessPolicyError> {
    AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "upstream-local",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.0)),
            ConfiguredModel::new(
                "cloud",
                "https://cloud.example/v1",
                "upstream-cloud",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.01)),
        ],
        offline,
        "test",
    )
    .with_gateway_budget(config)
    .with_gateway_access(config)
    .map(|state| state.with_delivery(Arc::new(CountingUsageDelivery { calls })))
}

fn seed_budget_spend(state: &AppState, key_id: Option<&str>) -> TestResult {
    seed_budget_spend_on(state, "cloud", key_id)
}

fn seed_budget_spend_on(state: &AppState, route: &str, key_id: Option<&str>) -> TestResult {
    let cost = turn_cost(route, 1_000, 0, &state.price_table().costs, false, None)?;
    state.savings_ledger().record(&cost, utc_today()?, key_id)?;
    Ok(())
}

/// Classifier-mode live state over `large` (dearest), `spare` (unpriced),
/// `small`, and `tied` (both cheapest). `intercepts` steer the
/// zero-weight classifier's recommendation in `[large, spare, small, tied]`
/// order.
fn classifier_budget_live_state(
    config: &GatewayConfig,
    intercepts: Vec<f64>,
    calls: Arc<AtomicUsize>,
) -> Result<AppState, Box<dyn Error>> {
    let classifier = ClassifierModel::new(
        vec![
            "large".to_owned(),
            "spare".to_owned(),
            "small".to_owned(),
            "tied".to_owned(),
        ],
        BTreeMap::new(),
        intercepts,
    )?;
    let model = |name: &str, cost: Option<f64>| {
        ConfiguredModel::new(
            name,
            format!("https://{name}.example/v1"),
            format!("upstream-{name}"),
            None,
            true,
        )
        .with_cost_per_1k(cost)
    };
    Ok(AppState::new(
        RoutingConfig {
            classifier: Some(classifier),
            ..RoutingConfig::default()
        },
        vec![
            model("large", Some(0.01)),
            model("spare", None),
            model("small", Some(0.002)),
            model("tied", Some(0.002)),
        ],
        false,
        "test",
    )
    .with_gateway_budget(config)
    .with_gateway_access(config)?
    .with_delivery(Arc::new(CountingUsageDelivery { calls })))
}

fn degrade_budget_config() -> GatewayConfig {
    GatewayConfig {
        budget: Some(GatewayBudget {
            limit: 0.001,
            window: "day".to_owned(),
            on_breach: "degrade".to_owned(),
        }),
        ..GatewayConfig::default()
    }
}

fn header_text<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}

fn reliability_live_state(
    config: &GatewayConfig,
    cloud_fallbacks: Vec<String>,
    outcomes: BTreeMap<String, VecDeque<ScriptedOutcome>>,
) -> Result<(AppState, SeenTargets), crate::reliability::ReliabilityError> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let delivery = Arc::new(ScriptedDelivery {
        outcomes: Arc::new(Mutex::new(outcomes)),
        seen: Arc::clone(&seen),
    });
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "upstream-local",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.0)),
            ConfiguredModel::new(
                "cloud",
                "https://cloud.example/v1",
                "upstream-cloud",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.01))
            .with_fallbacks(cloud_fallbacks),
            ConfiguredModel::new(
                "cloud-backup",
                "https://backup.example/v1",
                "upstream-cloud-backup",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.01)),
        ],
        false,
        "test",
    )
    .with_gateway_reliability_sources(config, || 1_000.0, || 0.0)?
    .with_delivery(delivery);
    Ok((state, seen))
}

fn streaming_live_state(
    config: &GatewayConfig,
    outcomes: BTreeMap<String, VecDeque<ScriptedStreamOutcome>>,
) -> Result<(AppState, SeenTargets), crate::reliability::ReliabilityError> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let delivery = Arc::new(ScriptedStreamingDelivery {
        outcomes: Arc::new(Mutex::new(outcomes)),
        seen: Arc::clone(&seen),
    });
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "upstream-local",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.0)),
            ConfiguredModel::new(
                "cloud",
                "https://cloud.example/v1",
                "upstream-cloud",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.01)),
        ],
        false,
        "test",
    )
    .with_gateway_reliability_sources(config, || 1_000.0, || 0.0)?
    .with_streaming_delivery(delivery);
    Ok((state, seen))
}

async fn get(state: &AppState, path: &str) -> Result<Response, Box<dyn Error>> {
    let request = Request::builder().uri(path).body(Body::empty())?;
    Ok(build_router(state.clone()).oneshot(request).await?)
}

async fn post_raw(
    state: &AppState,
    path: &str,
    body: impl Into<Body>,
    headers: &[(&str, &str)],
) -> Result<Response, Box<dyn Error>> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(body.into())?;
    Ok(build_router(state.clone()).oneshot(request).await?)
}

async fn post_json(
    state: &AppState,
    path: &str,
    body: &Value,
    headers: &[(&str, &str)],
) -> Result<Response, Box<dyn Error>> {
    post_raw(state, path, serde_json::to_vec(body)?, headers).await
}

async fn json_body(response: Response) -> Result<Value, Box<dyn Error>> {
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[tokio::test]
async fn health_matches_python_schema_and_sorts_names() -> TestResult {
    let state = configured_state();
    let response = get(&state, "/healthz").await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    assert!(
        response
            .headers()
            .get("x-wayfinder-router-request-id")
            .is_none()
    );
    assert_eq!(
        json_body(response).await?,
        json!({
            "status": "degraded",
            "models": ["alpha", "zeta"],
            "offline": true,
            "missing_keys": ["zeta"]
        })
    );
    Ok(())
}

#[tokio::test]
async fn zero_models_is_healthy_and_omits_missing_keys() -> TestResult {
    let state = AppState::new(RoutingConfig::default(), Vec::new(), false, "test");
    let response = get(&state, "/healthz").await?;
    assert_eq!(
        json_body(response).await?,
        json!({"status": "ok", "models": [], "offline": false})
    );
    Ok(())
}

#[tokio::test]
async fn managed_data_plane_fails_closed_and_exposes_only_delivery_routes() -> TestResult {
    let unauthenticated = AppState::new(
        RoutingConfig::binary(0.5),
        vec![ConfiguredModel::new(
            "local",
            "http://127.0.0.1:11434/v1",
            "provider-local",
            None,
            true,
        )],
        false,
        "test",
    );
    assert_eq!(
        validate_data_plane_state(&unauthenticated),
        Err(DataPlaneError::AuthenticationRequired)
    );

    let state = managed_data_plane_state(true)?;
    let app = build_data_plane_router(state)?;
    for hidden in [
        "/healthz",
        "/metrics",
        "/router/models",
        "/router/recent",
        "/router/config",
        "/v1/savings",
    ] {
        let request = Request::builder().uri(hidden).body(Body::empty())?;
        assert_eq!(
            app.clone().oneshot(request).await?.status(),
            StatusCode::NOT_FOUND,
            "{hidden} must not be exposed on the managed data plane"
        );
    }

    let live = Request::builder().uri("/livez").body(Body::empty())?;
    assert_eq!(app.clone().oneshot(live).await?.status(), StatusCode::OK);
    let ready = Request::builder().uri("/readyz").body(Body::empty())?;
    assert_eq!(app.clone().oneshot(ready).await?.status(), StatusCode::OK);

    let models = Request::builder().uri("/v1/models").body(Body::empty())?;
    let unauthorized = app.clone().oneshot(models).await?;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        unauthorized
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer")
    );

    let models = Request::builder()
        .uri("/v1/models")
        .header(header::AUTHORIZATION, "Bearer wf-secret")
        .body(Body::empty())?;
    let authorized = app.oneshot(models).await?;
    assert_eq!(authorized.status(), StatusCode::OK);
    let body = json_body(authorized).await?;
    let ids = body["data"]
        .as_array()
        .ok_or("model data must be an array")?
        .iter()
        .filter_map(|model| model["id"].as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"local"));
    assert!(!ids.contains(&"cloud"));
    Ok(())
}

#[tokio::test]
async fn managed_readiness_is_minimal_and_requires_one_ready_destination() -> TestResult {
    let state = managed_data_plane_state(false)?;
    let app = build_data_plane_router(state)?;
    let request = Request::builder().uri("/readyz").body(Body::empty())?;
    let response = app.oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_body(response).await?, json!({"status": "not_ready"}));
    Ok(())
}

#[tokio::test]
async fn twenty_workspace_users_send_multi_turn_chats_without_router_serialization() -> TestResult {
    const USERS: usize = 20;
    let barrier = Arc::new(Barrier::new(USERS + 1));
    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let delivery = Arc::new(ConcurrentDelivery {
        barrier: Arc::clone(&barrier),
        current: Arc::clone(&current),
        peak: Arc::clone(&peak),
    });
    let mut config = GatewayConfig::default();
    config.workspaces.insert(
        "production".to_owned(),
        Workspace {
            models: vec!["local".to_owned()],
            profile: None,
            budget: None,
            rate_limit: Some(RateLimit {
                rpm: Some(100),
                tpm: None,
                window: 60.0,
            }),
        },
    );
    for user in 0..USERS {
        config.keys.insert(
            format!("user-{user}"),
            VirtualKey {
                hash: auth::hash_key(&format!("wf-throughput-{user}")),
                tags: vec!["load-test".to_owned()],
                workspace: Some("production".to_owned()),
                budget: None,
                rate_limit: None,
                models: Vec::new(),
            },
        );
    }
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![ConfiguredModel::new(
            "local",
            "http://127.0.0.1:11434/v1",
            "provider-local",
            None,
            true,
        )],
        false,
        "test",
    )
    .with_gateway_access(&config)?
    .with_delivery(delivery);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let (shutdown_sender, shutdown_receiver) = oneshot::channel();
    let server = tokio::spawn(serve_with_shutdown(
        listener,
        build_data_plane_router(state.clone())?,
        async move {
            let _: Result<(), oneshot::error::RecvError> = shutdown_receiver.await;
        },
        Duration::from_secs(2),
    ));
    let client = reqwest::Client::new();
    let url = format!("http://{address}/v1/chat/completions");
    let mut tasks = Vec::with_capacity(USERS);
    for user in 0..USERS {
        let client = client.clone();
        let url = url.clone();
        tasks.push(tokio::spawn(async move {
            client
                .post(url)
                .bearer_auth(format!("wf-throughput-{user}"))
                .json(&json!({
                    "model": "local",
                    "messages": [
                        {"role": "user", "content": format!("user {user} first turn")},
                        {"role": "assistant", "content": "first reply"},
                        {"role": "user", "content": "follow up"}
                    ]
                }))
                .send()
                .await
        }));
    }
    tokio::time::timeout(Duration::from_secs(5), barrier.wait()).await?;
    for task in tasks {
        assert_eq!(task.await??.status(), StatusCode::OK);
    }
    let _: Result<(), ()> = shutdown_sender.send(());
    server.await??;
    assert_eq!(peak.load(Ordering::SeqCst), USERS);
    assert_eq!(current.load(Ordering::SeqCst), 0);
    let metrics = state.metrics().render()?;
    assert!(metrics.contains("wayfinder_router_peak_in_flight_deliveries 20"));
    assert!(metrics.contains("wayfinder_router_in_flight_deliveries 0"));
    Ok(())
}

#[tokio::test]
async fn full_delivery_capacity_returns_bounded_overload_response() -> TestResult {
    let entered = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Semaphore::new(0));
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![ConfiguredModel::new(
            "local",
            "http://127.0.0.1:11434/v1",
            "provider-local",
            None,
            true,
        )],
        false,
        "test",
    )
    .with_admission_policy(AdmissionPolicy::new(1, 0, Duration::from_secs(1))?)
    .with_delivery(Arc::new(GatedDelivery {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    }));
    let app = build_router(state.clone());
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"model":"local","messages":[{"role":"user","content":"hello"}]}"#,
            ))
    };
    let first_app = app.clone();
    let first_request = request()?;
    let first = tokio::spawn(async move { first_app.oneshot(first_request).await });
    while entered.load(Ordering::SeqCst) != 1 {
        tokio::task::yield_now().await;
    }

    let overloaded = app.oneshot(request()?).await?;
    assert_eq!(overloaded.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        overloaded
            .headers()
            .get(ROUTER_OVERLOAD_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("queue-full")
    );
    assert_eq!(
        json_body(overloaded).await?["error"]["type"],
        "wayfinder_router_overloaded"
    );
    assert!(
        state
            .metrics()
            .render()?
            .contains("wayfinder_router_admission_rejected_total{reason=\"queue-full\"} 1")
    );

    release.add_permits(1);
    assert_eq!(first.await??.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn hot_reload_snapshots_share_one_admission_authority() -> TestResult {
    let previous = AppState::new(RoutingConfig::default(), Vec::new(), false, "before")
        .with_admission_policy(AdmissionPolicy::new(1, 0, Duration::from_secs(1))?);
    let old_generation = previous.admission_policy().acquire().await?;

    // A freshly parsed snapshot owns fresh semaphores and may even carry a
    // different configured limit. Runtime adoption must retain the one
    // process-lifetime authority while old-generation requests can exist.
    let current = AppState::new(RoutingConfig::default(), Vec::new(), false, "after")
        .with_admission_policy(AdmissionPolicy::new(8, 0, Duration::from_secs(1))?)
        .with_runtime_state_from(&previous);
    assert_eq!(current.admission_policy().max_in_flight(), 1);
    assert!(matches!(
        current.admission_policy().acquire().await,
        Err(AdmissionError::QueueFull)
    ));

    drop(old_generation);
    let new_generation = current.admission_policy().acquire().await?;
    assert!(matches!(
        previous.admission_policy().acquire().await,
        Err(AdmissionError::QueueFull)
    ));
    drop(new_generation);
    assert_eq!(previous.admission_policy().in_flight(), 0);
    assert_eq!(current.admission_policy().in_flight(), 0);
    Ok(())
}

#[tokio::test]
async fn hot_reload_snapshots_preserve_one_ordered_audit_pipeline() -> TestResult {
    let previous_path = std::env::temp_dir().join(format!(
        "wayfinder-reload-audit-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let replacement_path = std::env::temp_dir().join(format!(
        "wayfinder-replacement-audit-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let previous = AppState::new(RoutingConfig::default(), Vec::new(), false, "before")
        .with_audit_log(crate::audit::AuditLog::from_path(&previous_path)?);
    let current = AppState::new(RoutingConfig::default(), Vec::new(), false, "after")
        .with_audit_log(crate::audit::AuditLog::from_path(&replacement_path)?)
        .with_runtime_state_from(&previous);

    previous
        .record_audit("system", "before.reload", None, None)
        .await?;
    current
        .record_audit("system", "after.reload", None, None)
        .await?;
    current.audit_log().shutdown().await?;

    let events = std::fs::read_to_string(&previous_path)?;
    assert!(matches!(
        (events.find("before.reload"), events.find("after.reload")),
        (Some(before), Some(after)) if before < after
    ));
    assert!(std::fs::read_to_string(&replacement_path)?.is_empty());
    let _ = std::fs::remove_file(previous_path);
    let _ = std::fs::remove_file(replacement_path);
    Ok(())
}

#[test]
fn shared_state_destination_changes_are_restart_only() {
    let mut previous = AppState::new(RoutingConfig::default(), Vec::new(), false, "before");
    let mut replacement = AppState::new(RoutingConfig::default(), Vec::new(), false, "after");
    let first = wayfinder_config::gateway::StateConfig::default();
    let mut second = first.clone();
    previous.shared_state_identity = Some(super::shared_state_identity(&first));
    replacement.shared_state_identity = Some(super::shared_state_identity(&first));
    assert!(previous.shared_state_matches(&replacement));

    second.namespace = "replacement".to_owned();
    replacement.shared_state_identity = Some(super::shared_state_identity(&second));
    assert!(!previous.shared_state_matches(&replacement));
}

#[tokio::test]
async fn streaming_delivery_holds_capacity_until_response_body_is_dropped() -> TestResult {
    let dropped = Arc::new(AtomicBool::new(false));
    let outcomes = Arc::new(Mutex::new(BTreeMap::from([(
        "local".to_owned(),
        VecDeque::from([ScriptedStreamOutcome::Cancellable(Arc::clone(&dropped))]),
    )])));
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![ConfiguredModel::new(
            "local",
            "http://127.0.0.1:11434/v1",
            "provider-local",
            None,
            true,
        )],
        false,
        "test",
    )
    .with_admission_policy(AdmissionPolicy::new(1, 0, Duration::from_secs(1))?)
    .with_streaming_delivery(Arc::new(ScriptedStreamingDelivery {
        outcomes,
        seen: Arc::new(Mutex::new(Vec::new())),
    }));
    let app = build_router(state.clone());
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"model":"local","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
            ))
    };

    let first = app.clone().oneshot(request()?).await?;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(state.admission_policy().in_flight(), 1);
    let overloaded = app.oneshot(request()?).await?;
    assert_eq!(overloaded.status(), StatusCode::SERVICE_UNAVAILABLE);

    drop(first);
    tokio::task::yield_now().await;
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(state.admission_policy().in_flight(), 0);
    Ok(())
}

#[tokio::test]
async fn openai_model_aliases_preserve_directive_and_config_order() -> TestResult {
    let state = configured_state();
    let canonical = get(&state, "/v1/models").await?;
    let alias = get(&state, "/models").await?;
    assert_eq!(canonical.status(), StatusCode::OK);
    assert_eq!(alias.status(), StatusCode::OK);
    assert_eq!(
        json_body(canonical).await?,
        json!({
            "object": "list",
            "data": [
                {"id": "auto", "object": "model", "created": 0, "owned_by": "wayfinder"},
                {"id": "prefer-local", "object": "model", "created": 0, "owned_by": "wayfinder"},
                {"id": "prefer-hosted", "object": "model", "created": 0, "owned_by": "wayfinder"},
                {"id": "zeta", "object": "model", "created": 0, "owned_by": "wayfinder"},
                {"id": "alpha", "object": "model", "created": 0, "owned_by": "wayfinder"}
            ]
        })
    );
    assert_eq!(
        json_body(alias).await?,
        json!({
            "object": "list",
            "data": [
                {"id": "auto", "object": "model", "created": 0, "owned_by": "wayfinder"},
                {"id": "prefer-local", "object": "model", "created": 0, "owned_by": "wayfinder"},
                {"id": "prefer-hosted", "object": "model", "created": 0, "owned_by": "wayfinder"},
                {"id": "zeta", "object": "model", "created": 0, "owned_by": "wayfinder"},
                {"id": "alpha", "object": "model", "created": 0, "owned_by": "wayfinder"}
            ]
        })
    );
    Ok(())
}

#[tokio::test]
async fn classifier_model_list_omits_ordered_directives() -> TestResult {
    let classifier = ClassifierModel::new(
        vec!["small".to_owned(), "large".to_owned()],
        BTreeMap::new(),
        vec![0.0, 0.0],
    )?;
    let routing = RoutingConfig {
        classifier: Some(classifier),
        ..RoutingConfig::default()
    };
    let state = AppState::new(routing, Vec::new(), false, "test");
    let response = get(&state, "/v1/models").await?;
    assert_eq!(
        json_body(response).await?,
        json!({
            "object": "list",
            "data": [
                {"id": "auto", "object": "model", "created": 0, "owned_by": "wayfinder"}
            ]
        })
    );
    Ok(())
}

#[tokio::test]
async fn router_models_exposes_only_metadata_and_presence() -> TestResult {
    let state = configured_state();
    assert_eq!(state.build_version(), "test-build-secret-free");
    assert_eq!(state.request_body_limit(), 4096);
    let response = get(&state, "/router/models").await?;
    assert!(
        response
            .headers()
            .get("x-wayfinder-router-request-id")
            .is_none()
    );
    let body = json_body(response).await?;
    assert_eq!(
        body,
        json!({
            "models": [
                {
                    "name": "zeta",
                    "endpoint": "https://zeta.example/v1",
                    "model": "provider-zeta",
                    "provider": "openai-compatible",
                    "tier": null,
                    "api_key_env": "ZETA_API_KEY",
                    "key_ok": false
                },
                {
                    "name": "alpha",
                    "endpoint": "http://127.0.0.1:11434/v1",
                    "model": "provider-alpha",
                    "provider": "openai-compatible",
                    "tier": null,
                    "api_key_env": null,
                    "key_ok": true
                }
            ],
            "dry_run": true
        })
    );
    let encoded = serde_json::to_string(&body)?;
    assert!(!encoded.contains("credential-value"));
    assert!(!encoded.contains("test-build-secret-free"));
    Ok(())
}

#[tokio::test]
async fn savings_aliases_match_python_empty_ledger_mode_and_exact_version() -> TestResult {
    let state = AppState::new(RoutingConfig::binary(0.5), Vec::new(), false, "test");
    let expected = json!({
        "period_days": null,
        "unit": "usd",
        "priced": true,
        "requests": 0,
        "estimated_requests": 0,
        "tokens": 0,
        "realized": 0.0,
        "baseline": 0.0,
        "saved": 0.0,
        "saved_pct": 0.0,
        "by_route": {},
        "by_key": {},
        "price_table_version": "441085a4f80a"
    });
    for path in ["/v1/savings", "/savings"] {
        let response = get(&state, path).await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await?, expected);
    }
    let today = json_body(get(&state, "/v1/savings?period=today").await?).await?;
    assert_eq!(today["period_days"], 1);
    Ok(())
}

#[tokio::test]
async fn project_value_report_combines_attributed_accounting_and_bounded_delivery() -> TestResult {
    let ledger = Arc::new(SavingsLedger::new(400, true));
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "provider-local",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.0))
            .with_provider(ProviderKind::OpenAiCompatible, Some(ProviderTier::Local)),
            ConfiguredModel::new(
                "cloud",
                "https://cloud.example/v1",
                "provider-cloud",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.01)),
        ],
        false,
        "test",
    )
    .with_savings_ledger(Arc::clone(&ledger));
    let cost = turn_cost("local", 1_000, 0, &state.price_table().costs, false, None)?;
    ledger.record_attributed(
        &cost,
        utc_today()?,
        Some("project-key"),
        Some("project one"),
    )?;
    state.recent().record(RecentEntry {
        request_id: "abcdef123456".to_owned(),
        model: "local".to_owned(),
        score: 0.1,
        mode: "scored".to_owned(),
        policy_version: None,
        snapshot_id: None,
        policy_profile: Some("developer-starter".to_owned()),
        served_by: Some("local".to_owned()),
        execution_boundary: Some(ExecutionBoundary::OnDevice),
        outcome: Some(RecentOutcome::Succeeded),
        user_outcome: None,
        http_status: Some(200),
        error_type: None,
        ts: 1_777_680_000.0,
        cost: None,
        key: Some("project-key".to_owned()),
        workspace: Some("project one".to_owned()),
        cache: None,
    })?;

    let response = get(&state, "/v1/value?workspace=project%20one&period=30d").await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await?;
    assert_eq!(body["schema_version"], "wf-project-value-v1");
    assert_eq!(body["workspace_id"], "project one");
    assert_eq!(body["accounting"]["requests"], 1);
    assert_eq!(body["accounting"]["saved"], 0.01);
    assert!(body["accounting"].get("by_key").is_none());
    assert_eq!(body["delivery"]["retained"], 1);
    assert_eq!(body["delivery"]["boundaries"]["on_device"], 1);
    assert_eq!(body["quality"]["status"], "not-collected");
    assert_eq!(body["quality"]["coverage_pct"], 0.0);
    assert!(body["quality"]["correction_rate_pct"].is_null());
    assert_eq!(body["baseline"]["kind"], "dearest-configured-rate");
    assert_eq!(body["baseline"]["routes"], json!(["cloud"]));
    assert_eq!(body["baseline"]["rate_per_1k"], 0.01);

    let labelled = post_json(
        &state,
        "/v1/outcomes",
        &json!({
            "schema_version": "wf-outcome-label-v1",
            "workspace_id": "project one",
            "request_id": "abcdef123456",
            "outcome": "correction"
        }),
        &[],
    )
    .await?;
    assert_eq!(labelled.status(), StatusCode::OK);
    let labelled = json_body(labelled).await?;
    assert_eq!(labelled["accepted"], true);
    assert!(labelled.get("prompt").is_none());

    let value = json_body(get(&state, "/v1/value?workspace=project%20one").await?).await?;
    assert_eq!(value["quality"]["status"], "collected");
    assert_eq!(value["quality"]["labelled_receipts"], 1);
    assert_eq!(value["quality"]["coverage_pct"], 100.0);
    assert_eq!(value["quality"]["correction_rate_pct"], 100.0);
    assert_eq!(value["quality"]["failure_rate_pct"], 0.0);

    let proposal =
        json_body(get(&state, "/v1/outcomes/policy?workspace=project%20one").await?).await?;
    assert_eq!(proposal["schema_version"], "wf-local-outcome-policy-v1");
    assert_eq!(proposal["disposition"], "propose-narrower-local-range");
    assert_eq!(proposal["current_threshold"], 0.5);
    assert_eq!(proposal["proposed_threshold"], 0.1);

    let invalid = get(&state, "/v1/value?workspace=project-one&period=year").await?;
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json_body(invalid).await?["error"]["type"],
        "wayfinder_router_bad_value_query"
    );
    Ok(())
}

#[tokio::test]
async fn operator_endpoint_surfaces_unacknowledged_audit_write() -> TestResult {
    let audit_log = crate::audit::AuditLog::from_shared_backend(
        Arc::new(crate::state::MemoryStateBackend::default()),
        "test",
    )?;
    let state = AppState::new(RoutingConfig::binary(0.5), Vec::new(), false, "test")
        .with_audit_log(audit_log);

    let response = get(&state, "/v1/savings").await?;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        json_body(response).await?["error"]["type"],
        "wayfinder_router_audit_unavailable"
    );
    state.audit_log().shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn evidence_endpoints_are_bounded_and_reject_content_fields() -> TestResult {
    let state = AppState::new(RoutingConfig::binary(0.5), Vec::new(), false, "test");
    let empty = get(&state, "/v1/evidence").await?;
    assert_eq!(empty.status(), StatusCode::OK);
    assert_eq!(json_body(empty).await?["schema_version"], "wf-evidence-v1");

    let invalid = post_json(
        &state,
        "/v1/evidence/labels",
        &json!({
            "labels": [{
                "request_id": "r",
                "candidate_route": "c",
                "label": "win",
                "evaluator": {"kind": "human"},
                "prompt": "must not be retained"
            }]
        }),
        &[],
    )
    .await?;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let accepted = post_json(
        &state,
        "/v1/evidence/labels",
        &json!({
            "labels": [{
                "request_id": "r",
                "candidate_route": "c",
                "label": "win",
                "evaluator": {"kind": "automated", "version": "heuristic-1"}
            }]
        }),
        &[],
    )
    .await?;
    assert_eq!(accepted.status(), StatusCode::OK);
    assert_eq!(json_body(accepted).await?["accepted"], 1);

    let text = get(&state, "/router/evidence.txt").await?;
    assert_eq!(text.status(), StatusCode::OK);
    let text = String::from_utf8(to_bytes(text.into_body(), usize::MAX).await?.to_vec())?;
    assert!(text.contains("Wayfinder evidence report wf-evidence-v1"));
    assert!(!text.contains("must not be retained"));

    let canary = get(&state, "/v1/canary").await?;
    assert_eq!(canary.status(), StatusCode::OK);
    assert_eq!(json_body(canary).await?["schema_version"], "wf-canary-v1");
    Ok(())
}

#[tokio::test]
async fn injected_shared_ledger_drives_relative_report() -> TestResult {
    let ledger = Arc::new(SavingsLedger::new(400, false));
    let state = AppState::new(RoutingConfig::binary(0.5), Vec::new(), false, "test")
        .with_savings_ledger(Arc::clone(&ledger));
    let cost = turn_cost("local", 1_000, 0, &state.price_table().costs, true, None)?;
    ledger.record(&cost, utc_today()?, None)?;
    let report = json_body(get(&state, "/v1/savings").await?).await?;
    assert_eq!(report["priced"], false);
    assert_eq!(report["unit"], "relative");
    assert_eq!(report["requests"], 1);
    assert_eq!(report["estimated_requests"], 1);
    assert_eq!(report["realized"], 0.2);
    assert_eq!(report["baseline"], 1.0);
    assert_eq!(report["saved"], 0.8);
    assert_eq!(report["by_route"]["local"]["tokens"], 1_000);
    Ok(())
}

#[test]
fn utc_day_conversion_covers_epoch_and_leap_day() -> TestResult {
    assert_eq!(utc_date_from_unix_days(0)?, UtcDate::new(1970, 1, 1)?);
    assert_eq!(utc_date_from_unix_days(19_782)?, UtcDate::new(2024, 2, 29)?);
    Ok(())
}

#[tokio::test]
async fn router_profiles_matches_python_order_and_provenance() -> TestResult {
    let response = get(&configured_state(), "/router/profiles").await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await?;
    let profiles = body["profiles"]
        .as_array()
        .ok_or("profiles response must contain an array")?;
    assert_eq!(profiles.len(), 10);
    assert_eq!(profiles[0]["id"], "proofs-math");
    assert_eq!(profiles[3]["id"], "science-medicine");
    assert_eq!(profiles[4]["id"], "mined-science");
    assert_eq!(profiles[9]["id"], "mined-multilingual");
    assert!(
        profiles[..4]
            .iter()
            .all(|profile| profile["source"] == "curated")
    );
    assert!(
        profiles[4..]
            .iter()
            .all(|profile| profile["source"] == "mined")
    );
    assert_eq!(
        profiles[0]["constraint_terms"],
        json!(["exactly", "minimize", "maximize", "subject"])
    );
    assert_eq!(profiles[9]["reasoning_terms"][9], "subject's");
    Ok(())
}

#[tokio::test]
async fn chat_aliases_return_full_dry_run_decisions() -> TestResult {
    let state = configured_state();
    let payload = json!({"messages": [{"role": "user", "content": "hello"}]});
    for path in ["/v1/chat/completions", "/chat/completions"] {
        let response = post_json(&state, path, &payload, &[]).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let request_id = response
            .headers()
            .get("x-wayfinder-router-request-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let score = response
            .headers()
            .get("x-wayfinder-router-score")
            .and_then(|value| value.to_str().ok());
        assert_eq!(score, Some("0.00"));
        assert_eq!(
            response
                .headers()
                .get("x-wayfinder-router-mode")
                .and_then(|value| value.to_str().ok()),
            Some("scored")
        );
        assert!(
            response
                .headers()
                .get("x-wayfinder-router-decision-only")
                .is_none()
        );
        let body = json_body(response).await?;
        let body_id = body
            .get("wayfinder")
            .and_then(|wayfinder| wayfinder.get("request_id"))
            .and_then(Value::as_str);
        assert_eq!(body_id, request_id.as_deref());
        assert!(body_id.is_some_and(|id| {
            id.len() == 12
                && id
                    .chars()
                    .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character))
        }));
        assert_eq!(body["wayfinder"]["dry_run"], true);
        assert!(body["wayfinder"].get("decision_only").is_none());
        assert!(body["wayfinder"]["features"].is_object());
        assert!(body["wayfinder"]["contributions"].is_array());
        assert!(body["wayfinder"]["tiers"].is_array());
        assert_eq!(body["wayfinder"]["cost"]["estimated"], true);
        assert_eq!(
            body["wayfinder"]["cost"]["unit"],
            "relative units / 1k words"
        );
    }
    Ok(())
}

#[tokio::test]
async fn responses_aliases_normalize_buffered_chat_results_and_usage() -> TestResult {
    let state = cached_live_state(Arc::new(AtomicUsize::new(0)))?;
    for path in ["/v1/responses", "/responses"] {
        let response = post_json(
            &state,
            path,
            &json!({"model": "local", "input": "hello"}),
            &[],
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        let body = json_body(response).await?;
        assert_eq!(body["object"], "response");
        assert_eq!(body["status"], "completed");
        assert_eq!(body["output"][0]["type"], "message");
        assert_eq!(body["output"][0]["content"][0]["text"], "ok");
        assert_eq!(
            body["usage"],
            json!({"input_tokens": 1000, "output_tokens": 25, "total_tokens": 1025})
        );
    }
    Ok(())
}

#[tokio::test]
async fn responses_preserve_the_existing_decision_only_contract() -> TestResult {
    let state = configured_state();
    for stream in [false, true] {
        let response = post_json(
            &state,
            "/v1/responses",
            &json!({"model": "auto", "input": "route only", "stream": stream}),
            &[],
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await?;
        assert!(body.get("wayfinder").is_some());
        assert!(body.get("output").is_none());
    }
    Ok(())
}

#[tokio::test]
async fn responses_stream_has_one_terminal_event_and_bounded_translation() -> TestResult {
    let chunks = vec![
        Ok(Bytes::from_static(
            br#"data: {"choices":[{"delta":{"content":"Hel"}}]}"#,
        )),
        Ok(Bytes::from_static(
            b"\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2}}\n\n",
        )),
        Ok(Bytes::from_static(b"data: [DONE]\n\ndata: [DONE]\n\n")),
    ];
    let (state, _) = streaming_live_state(
        &GatewayConfig::default(),
        BTreeMap::from([(
            "local".to_owned(),
            VecDeque::from([ScriptedStreamOutcome::Chunks(chunks)]),
        )]),
    )?;
    let response = post_json(
        &state,
        "/v1/responses",
        &json!({"model": "local", "input": "hello", "stream": true}),
        &[],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let stream_text =
        String::from_utf8(to_bytes(response.into_body(), usize::MAX).await?.to_vec())?;
    assert!(stream_text.contains("event: response.created"));
    assert!(stream_text.contains("\"delta\":\"Hel\""));
    assert!(stream_text.contains("\"delta\":\"lo\""));
    assert_eq!(stream_text.matches("event: response.completed").count(), 1);
    assert_eq!(stream_text.matches("event: response.failed").count(), 0);
    Ok(())
}

#[tokio::test]
async fn responses_stream_restores_codex_custom_tool_calls() -> TestResult {
    let chunks = vec![
        Ok(Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_patch\",\"function\":{\"name\":\"apply_patch\",\"arguments\":\"{\\\"input\\\":\\\"*** Begin\"}}]}}]}\n\n",
        )),
        Ok(Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\" Patch\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n",
        )),
    ];
    let (state, _) = streaming_live_state(
        &GatewayConfig::default(),
        BTreeMap::from([(
            "local".to_owned(),
            VecDeque::from([ScriptedStreamOutcome::Chunks(chunks)]),
        )]),
    )?;
    let response = post_json(
        &state,
        "/v1/responses",
        &json!({
            "model": "local",
            "input": [{"type": "message", "role": "user", "content": "edit it"}],
            "tools": [{
                "type": "custom",
                "name": "apply_patch",
                "description": "Apply a patch",
                "format": {"type": "grammar", "syntax": "lark", "definition": "start: /.+/"}
            }],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "stream": true
        }),
        &[],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let stream_text =
        String::from_utf8(to_bytes(response.into_body(), usize::MAX).await?.to_vec())?;
    assert_eq!(
        stream_text
            .matches("event: response.output_item.done")
            .count(),
        1
    );
    assert!(stream_text.contains("\"type\":\"custom_tool_call\""));
    assert!(stream_text.contains("\"name\":\"apply_patch\""));
    assert!(stream_text.contains("*** Begin Patch"));
    assert_eq!(stream_text.matches("event: response.completed").count(), 1);
    assert_eq!(stream_text.matches("event: response.failed").count(), 0);
    Ok(())
}

#[tokio::test]
async fn responses_reject_unknown_fields_and_oversized_history_specifically() -> TestResult {
    let state = configured_state();
    let unknown = post_json(
        &state,
        "/v1/responses",
        &json!({"model": "auto", "input": "hello", "background": true}),
        &[],
    )
    .await?;
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    let unknown_body = json_body(unknown).await?;
    assert_eq!(
        unknown_body["error"]["type"],
        "wayfinder_router_unsupported_request"
    );
    assert!(
        unknown_body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("background"))
    );

    let oversized_state = AppState::new(RoutingConfig::binary(0.5), Vec::new(), false, "test")
        .with_dry_run(true)
        .with_request_body_limit(responses::MAX_INPUT_CHARS + 1024);
    let oversized = post_json(
        &oversized_state,
        "/v1/responses",
        &json!({
            "model": "auto",
            "input": "x".repeat(responses::MAX_INPUT_CHARS + 1)
        }),
        &[],
    )
    .await?;
    assert_eq!(oversized.status(), StatusCode::BAD_REQUEST);
    let oversized_body = json_body(oversized).await?;
    assert!(
        oversized_body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("history"))
    );
    Ok(())
}

#[tokio::test]
async fn anthropic_aliases_pass_decision_only_payload_and_headers_through() -> TestResult {
    let state = configured_state();
    let payload = json!({
        "model": "claude-x",
        "max_tokens": 32,
        "messages": [{"role": "user", "content": "hello"}]
    });
    for path in ["/v1/messages", "/messages"] {
        let response = post_json(&state, path, &payload, &[]).await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get("x-wayfinder-router-model").is_some());
        let body = json_body(response).await?;
        assert!(body.get("wayfinder").is_some());
        assert!(body.get("choices").is_none());
        assert!(body.get("type").is_none());
    }
    Ok(())
}

#[tokio::test]
async fn buffered_and_streaming_anthropic_round_trip_reuse_chat_decision() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let state = budget_live_state(&GatewayConfig::default(), false, Arc::clone(&calls))?;
    let text = "hello from Claude Code";
    let payload = json!({
        "model": "claude-opus-4",
        "max_tokens": 64,
        "messages": [{"role": "user", "content": text}]
    });
    let response = post_json(&state, "/v1/messages", &payload, &[]).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let request_id = response
        .headers()
        .get("x-wayfinder-router-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .ok_or("Anthropic response omitted request id")?;
    let decision_headers = [
        "x-wayfinder-router-model",
        "x-wayfinder-router-score",
        "x-wayfinder-router-mode",
    ]
    .map(|name| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    });
    let body = json_body(response).await?;
    assert_eq!(body["id"], format!("msg_{request_id}"));
    assert_eq!(body["type"], "message");
    assert_eq!(body["role"], "assistant");
    assert_eq!(body["model"], "claude-opus-4");
    assert_eq!(body["content"], json!([{"type": "text", "text": "ok"}]));
    assert_eq!(body["stop_reason"], "end_turn");
    assert_eq!(
        body["usage"],
        json!({"input_tokens": 1000, "output_tokens": 25})
    );

    let direct = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "auto",
            "messages": [{"role": "user", "content": text}]
        }),
        &[],
    )
    .await?;
    for (name, expected) in [
        ("x-wayfinder-router-model", &decision_headers[0]),
        ("x-wayfinder-router-score", &decision_headers[1]),
        ("x-wayfinder-router-mode", &decision_headers[2]),
    ] {
        assert_eq!(
            direct
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok()),
            Some(expected.as_str())
        );
    }

    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let chunks = vec![
        Ok(Bytes::from_static(
            br#"data: {"choices":[{"delta":{"content":"Hel"}}]}"#,
        )),
        Ok(Bytes::from_static(
            b"\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2}}\n\n",
        )),
        Ok(Bytes::from_static(b"data: [DONE]\n\n")),
    ];
    let (stream_state, _) = streaming_live_state(
        &GatewayConfig::default(),
        BTreeMap::from([(
            "local".to_owned(),
            VecDeque::from([ScriptedStreamOutcome::Chunks(chunks)]),
        )]),
    )?;
    let streaming = post_json(
        &stream_state,
        "/v1/messages",
        &json!({
            "model": "claude-opus-4",
            "max_tokens": 64,
            "stream": true,
            "messages": [{"role": "user", "content": text}]
        }),
        &[],
    )
    .await?;
    assert_eq!(streaming.status(), StatusCode::OK);
    assert_eq!(
        streaming
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    let stream_body = to_bytes(streaming.into_body(), usize::MAX).await?;
    let stream_text = String::from_utf8(stream_body.to_vec())?;
    assert!(stream_text.contains("event: message_start"));
    assert!(stream_text.contains("event: content_block_delta"));
    assert!(stream_text.contains("\"text\":\"Hel\""));
    assert!(stream_text.contains("\"text\":\"lo\""));
    assert!(stream_text.contains("event: message_stop"));
    Ok(())
}

#[tokio::test]
async fn anthropic_adapter_reshapes_upstream_and_gateway_auth_errors() -> TestResult {
    let reliability = GatewayConfig {
        retries: 0,
        ..GatewayConfig::default()
    };
    let (error_state, _) = reliability_live_state(
        &reliability,
        Vec::new(),
        BTreeMap::from([(
            "cloud".to_owned(),
            VecDeque::from([ScriptedOutcome::Response(
                StatusCode::BAD_REQUEST,
                br#"{"error":{"message":"bad request","type":"x"}}"#,
            )]),
        )]),
    )?;
    let payload = json!({
        "model": "cloud",
        "max_tokens": 8,
        "messages": [{"role": "user", "content": "hi"}]
    });
    let error = post_json(&error_state, "/v1/messages", &payload, &[]).await?;
    assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        error
            .headers()
            .get("x-wayfinder-router-served-by")
            .and_then(|value| value.to_str().ok()),
        Some("cloud")
    );
    assert_eq!(
        json_body(error).await?,
        json!({
            "type": "error",
            "error": {"type": "invalid_request_error", "message": "bad request"}
        })
    );

    let mut key = access_key("wf-secret");
    key.models = vec!["local".to_owned()];
    let mut config = GatewayConfig::default();
    config.keys.insert("team-a".to_owned(), key);
    let calls = Arc::new(AtomicUsize::new(0));
    let auth_state = budget_live_state(&config, false, Arc::clone(&calls))?;
    let unauthorized = post_json(&auth_state, "/v1/messages", &payload, &[]).await?;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(unauthorized).await?["error"]["type"],
        "authentication_error"
    );
    let authorized = post_json(
        &auth_state,
        "/v1/messages",
        &payload,
        &[("authorization", "Bearer wf-secret")],
    )
    .await?;
    assert_eq!(authorized.status(), StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn anthropic_adapter_preserves_privacy_and_offline_controls() -> TestResult {
    for (name, value, expected_status) in [
        (
            PRIVACY_POSTURE_HEADER,
            "on-device-only",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (OFFLINE_HEADER, "true", StatusCode::SERVICE_UNAVAILABLE),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let state = budget_live_state(&GatewayConfig::default(), false, Arc::clone(&calls))?;
        let response = post_json(
            &state,
            "/v1/messages",
            &json!({
                "model": "cloud",
                "max_tokens": 32,
                "messages": [{"role": "user", "content": "do not send this remotely"}]
            }),
            &[(name, value)],
        )
        .await?;

        assert_eq!(response.status(), expected_status);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        if name == PRIVACY_POSTURE_HEADER {
            assert_eq!(
                response
                    .headers()
                    .get(ROUTER_PRIVACY_POSTURE_HEADER)
                    .and_then(|header| header.to_str().ok()),
                Some("on-device-only")
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn recent_route_is_shared_bounded_and_prompt_free() -> TestResult {
    let state = configured_state();
    let secret_prompt = "a secret prompt body that must never enter recent metadata";
    for model in ["zeta", "alpha"] {
        let response = post_json(
            &state,
            "/v1/chat/completions",
            &json!({
                "model": model,
                "messages": [{"role": "user", "content": secret_prompt}]
            }),
            &[],
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = get(&state, "/router/recent?limit=1").await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await?;
    assert_eq!(body["total"], 2);
    assert_eq!(body["by_model"], json!({"alpha": 1, "zeta": 1}));
    assert_eq!(body["recent"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["recent"][0]["model"], "alpha");
    assert_eq!(
        body["recent"][0].as_object().map(|entry| entry
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>()),
        Some(std::collections::BTreeSet::from([
            "mode",
            "model",
            "request_id",
            "score",
            "ts"
        ]))
    );
    assert!(!serde_json::to_string(&body)?.contains(secret_prompt));
    Ok(())
}

#[tokio::test]
async fn metrics_endpoint_counts_decisions_without_prompt_content() -> TestResult {
    let state = configured_state();
    let initial = get(&state, "/metrics").await?;
    assert_eq!(initial.status(), StatusCode::OK);
    assert_eq!(
        initial
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/plain; version=0.0.4; charset=utf-8")
    );
    let initial_bytes = to_bytes(initial.into_body(), usize::MAX).await?;
    let initial_text = String::from_utf8(initial_bytes.to_vec())?;
    assert!(
        initial_text.contains("wayfinder_router_build_info{version=\"test-build-secret-free\"} 1")
    );
    assert!(initial_text.contains("wayfinder_router_decision_latency_seconds_count 0"));

    let secret_prompt = "a secret prompt body that metrics must never retain";
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({"messages": [{"role": "user", "content": secret_prompt}]}),
        &[],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let metrics = get(&state, "/metrics").await?;
    let bytes = to_bytes(metrics.into_body(), usize::MAX).await?;
    let text = String::from_utf8(bytes.to_vec())?;
    assert!(text.contains("wayfinder_router_requests_total{model=\"local\",mode=\"scored\"} 1"));
    assert!(text.contains("wayfinder_router_decision_latency_seconds_count 1"));
    assert!(!text.contains(secret_prompt));
    Ok(())
}

#[tokio::test]
async fn zero_model_live_is_distinct_from_explicit_dry_run() -> TestResult {
    let state = AppState::new(RoutingConfig::binary(0.2), Vec::new(), false, "test");
    let payload = json!({"messages": [{"role": "user", "content": "hello"}]});
    let response = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-decision-only")
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
    let body = json_body(response).await?;
    assert_eq!(body["wayfinder"]["decision_only"], true);
    assert!(body["wayfinder"].get("dry_run").is_none());
    Ok(())
}

#[tokio::test]
async fn malformed_wrong_shape_and_oversized_bodies_are_structured() -> TestResult {
    let state = AppState::new(RoutingConfig::default(), Vec::new(), false, "test")
        .with_request_body_limit(64);

    let missing = post_raw(&state, "/v1/chat/completions", Body::empty(), &[]).await?;
    assert_eq!(missing.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        missing
            .headers()
            .get("x-wayfinder-router-request-id")
            .is_none()
    );
    assert_eq!(json_body(missing).await?["detail"][0]["type"], "missing");

    let malformed = post_raw(&state, "/v1/chat/completions", "{", &[]).await?;
    assert_eq!(malformed.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json_body(malformed).await?["detail"][0]["type"],
        "json_invalid"
    );

    let wrong_shape = post_json(&state, "/v1/chat/completions", &json!([]), &[]).await?;
    assert_eq!(wrong_shape.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json_body(wrong_shape).await?["detail"][0]["type"],
        "dict_type"
    );

    let oversized = post_raw(
        &state,
        "/v1/chat/completions",
        format!("{{\"messages\":\"{}\"}}", "x".repeat(80)),
        &[],
    )
    .await?;
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        json_body(oversized).await?["error"]["type"],
        "wayfinder_router_request_too_large"
    );
    Ok(())
}

#[tokio::test]
async fn unsupported_modalities_fail_before_provider_delivery() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![ConfiguredModel::new(
            "local",
            "http://127.0.0.1:11434/v1",
            "provider-local",
            None,
            true,
        )],
        false,
        "test",
    )
    .with_delivery(Arc::new(CountingUsageDelivery {
        calls: Arc::clone(&calls),
    }));

    for payload in [
        json!({"input": "embed this"}),
        json!({"messages": [{"role": "user", "content": [{"type": "image_url", "image_url": {"url": "data:"}}]}]}),
        json!({"messages": [{"role": "user", "content": [{"type": "input_audio", "input_audio": {"data": "..."}}]}]}),
        json!({"batch": {"id": "batch-1"}}),
    ] {
        let response = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            json_body(response).await?["error"]["type"],
            "wayfinder_router_unsupported_modality"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn pins_threshold_route_scope_and_offline_override_are_applied() -> TestResult {
    let state = AppState::new(
        RoutingConfig::binary(0.2),
        vec![ConfiguredModel::new(
            "named",
            "https://never-contact.invalid/v1",
            "provider-model",
            None,
            true,
        )],
        false,
        "test",
    )
    .with_dry_run(true)
    .with_route_on(RouteOn::All);
    let long_turn = (0..450).map(|_| "word").collect::<Vec<_>>().join(" ");
    let payload = json!({
        "messages": [
            {"role": "user", "content": long_turn},
            {"role": "user", "content": "hi"}
        ]
    });

    let scoped = post_json(
        &state,
        "/v1/chat/completions",
        &payload,
        &[
            ("x-wayfinder-route-on", "last_user"),
            ("x-wayfinder-offline", "true"),
        ],
    )
    .await?;
    assert_eq!(
        scoped
            .headers()
            .get("x-wayfinder-router-model")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    assert_eq!(
        scoped
            .headers()
            .get("x-wayfinder-router-offline")
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
    assert_eq!(json_body(scoped).await?["wayfinder"]["offline"], true);

    let pinned = post_json(
        &state,
        "/v1/chat/completions",
        &json!({"model": "named", "messages": []}),
        &[("x-wayfinder-threshold", "not-a-number")],
    )
    .await?;
    assert_eq!(pinned.status(), StatusCode::OK);
    assert_eq!(
        pinned
            .headers()
            .get("x-wayfinder-router-mode")
            .and_then(|value| value.to_str().ok()),
        Some("pinned")
    );

    let directive = post_json(
        &state,
        "/v1/chat/completions",
        &json!({"model": "prefer-hosted", "messages": []}),
        &[],
    )
    .await?;
    assert_eq!(
        directive
            .headers()
            .get("x-wayfinder-router-model")
            .and_then(|value| value.to_str().ok()),
        Some("cloud")
    );
    assert_eq!(
        directive
            .headers()
            .get("x-wayfinder-router-mode")
            .and_then(|value| value.to_str().ok()),
        Some("pinned")
    );

    let threshold = post_json(
        &state,
        "/v1/chat/completions",
        &json!({"messages": [{"role": "user", "content": "hi"}]}),
        &[("x-wayfinder-threshold", "1.0")],
    )
    .await?;
    assert_eq!(
        threshold
            .headers()
            .get("x-wayfinder-router-mode")
            .and_then(|value| value.to_str().ok()),
        Some("threshold-override")
    );
    assert_eq!(
        threshold
            .headers()
            .get("x-wayfinder-router-model")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    Ok(())
}

#[tokio::test]
async fn named_route_selects_ordered_models_and_exposes_route_metadata() -> TestResult {
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new("local", "http://127.0.0.1:11434/v1", "local", None, true),
            ConfiguredModel::new("cloud", "https://cloud.example/v1", "cloud", None, true),
        ],
        false,
        "test",
    )
    .with_dry_run(true)
    .with_route_presets(IndexMap::from([(
        "coding".to_owned(),
        RoutePreset {
            models: vec!["cloud".to_owned(), "local".to_owned()],
        },
    )]));

    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "@route/coding",
            "messages": [{"role": "user", "content": "write a function"}]
        }),
        &[],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-model")
            .and_then(|value| value.to_str().ok()),
        Some("cloud")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-mode")
            .and_then(|value| value.to_str().ok()),
        Some("preset")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-route")
            .and_then(|value| value.to_str().ok()),
        Some("@route/coding")
    );
    assert_eq!(json_body(response).await?["wayfinder"]["mode"], "preset");

    let routes = get(&state, "/router/routes").await?;
    assert_eq!(routes.status(), StatusCode::OK);
    assert_eq!(
        json_body(routes).await?,
        json!({"routes": [{"name": "coding", "models": ["cloud", "local"]}]})
    );
    Ok(())
}

#[tokio::test]
async fn unknown_named_route_fails_closed_without_falling_back_to_automatic() -> TestResult {
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![ConfiguredModel::new(
            "local",
            "http://127.0.0.1:11434/v1",
            "local",
            None,
            true,
        )],
        false,
        "test",
    );
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "@route/missing",
            "messages": [{"role": "user", "content": "hello"}]
        }),
        &[],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(response).await?["error"]["type"],
        "wayfinder_router_unknown_route"
    );
    Ok(())
}

#[tokio::test]
async fn named_route_failover_stays_within_its_ordered_aliases() -> TestResult {
    let config = GatewayConfig {
        retries: 0,
        ..GatewayConfig::default()
    };
    let (state, seen) = reliability_live_state(
        &config,
        Vec::new(),
        BTreeMap::from([
            (
                "cloud".to_owned(),
                VecDeque::from([ScriptedOutcome::Transport]),
            ),
            (
                "cloud-backup".to_owned(),
                VecDeque::from([ScriptedOutcome::Response(
                    StatusCode::OK,
                    br#"{"id":"route-ok","choices":[{"message":{"content":"ok"}}]}"#,
                )]),
            ),
        ]),
    )?;
    let state = state.with_route_presets(IndexMap::from([(
        "coding".to_owned(),
        RoutePreset {
            models: vec!["cloud".to_owned(), "cloud-backup".to_owned()],
        },
    )]));
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "@route/coding",
            "messages": [{"role": "user", "content": "write code"}]
        }),
        &[],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-served-by")
            .and_then(|value| value.to_str().ok()),
        Some("cloud-backup")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-route")
            .and_then(|value| value.to_str().ok()),
        Some("@route/coding")
    );
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("seen lock poisoned"))?
            .as_slice(),
        ["cloud", "cloud-backup"]
    );
    Ok(())
}

#[tokio::test]
async fn named_route_intersects_the_virtual_key_allowlist() -> TestResult {
    let mut config = GatewayConfig::default();
    let mut key = access_key("wf-secret");
    key.models = vec!["local".to_owned()];
    config.keys.insert("team-a".to_owned(), key);
    let policy = AccessPolicy::from_gateway_config_with_clock(&config, || 1_000.0)?;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "local-upstream",
                None,
                true,
            ),
            ConfiguredModel::new(
                "cloud",
                "https://cloud.example/v1",
                "cloud-upstream",
                None,
                true,
            ),
        ],
        false,
        "test",
    )
    .with_access_policy(policy)
    .with_route_presets(IndexMap::from([(
        "coding".to_owned(),
        RoutePreset {
            models: vec!["cloud".to_owned(), "local".to_owned()],
        },
    )]))
    .with_delivery(Arc::new(FakeDelivery {
        seen: Arc::clone(&seen),
        fail: false,
    }));

    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "@route/coding",
            "messages": [{"role": "user", "content": "use my allowed route"}]
        }),
        &[("authorization", "Bearer wf-secret")],
    )
    .await?;

    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response
            .headers()
            .get(ROUTER_ROUTE_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("@route/coding")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-served-by")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("seen lock poisoned"))?[0]
            .0,
        "local-upstream"
    );
    Ok(())
}

#[tokio::test]
async fn named_route_with_empty_allowlist_intersection_fails_closed() -> TestResult {
    let mut config = GatewayConfig::default();
    let mut key = access_key("wf-secret");
    key.models = vec!["local".to_owned()];
    config.keys.insert("team-a".to_owned(), key);
    let policy = AccessPolicy::from_gateway_config_with_clock(&config, || 1_000.0)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "local-upstream",
                None,
                true,
            ),
            ConfiguredModel::new(
                "cloud",
                "https://cloud.example/v1",
                "cloud-upstream",
                None,
                true,
            ),
        ],
        false,
        "test",
    )
    .with_access_policy(policy)
    .with_route_presets(IndexMap::from([(
        "cloud-only".to_owned(),
        RoutePreset {
            models: vec!["cloud".to_owned()],
        },
    )]))
    .with_delivery(Arc::new(CountingUsageDelivery {
        calls: Arc::clone(&calls),
    }));

    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "@route/cloud-only",
            "messages": [{"role": "user", "content": "do not escape this preset"}]
        }),
        &[("authorization", "Bearer wf-secret")],
    )
    .await?;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json_body(response).await?["error"]["type"],
        "wayfinder_router_destination_ineligible"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn privacy_posture_rejects_hosted_pin_before_delivery() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let state = budget_live_state(&GatewayConfig::default(), false, Arc::clone(&calls))?;
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "cloud",
            "messages": [{"role": "user", "content": "keep this local"}]
        }),
        &[("x-wayfinder-privacy-posture", "on-device-only")],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-privacy-posture")
            .and_then(|value| value.to_str().ok()),
        Some("on-device-only")
    );
    assert_eq!(
        json_body(response).await?["error"]["type"],
        "wayfinder_router_destination_ineligible"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn local_privacy_preset_filters_hosted_members_before_failover() -> TestResult {
    let config = GatewayConfig {
        retries: 0,
        ..GatewayConfig::default()
    };
    let (state, seen) = reliability_live_state(
        &config,
        Vec::new(),
        BTreeMap::from([(
            "local".to_owned(),
            VecDeque::from([ScriptedOutcome::Response(
                StatusCode::OK,
                br#"{"id":"local-ok","choices":[{"message":{"content":"ok"}}]}"#,
            )]),
        )]),
    )?;
    let state = state.with_route_presets(IndexMap::from([(
        "private".to_owned(),
        RoutePreset {
            models: vec!["cloud".to_owned(), "local".to_owned()],
        },
    )]));
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "@route/private",
            "messages": [{"role": "user", "content": "stay nearby"}]
        }),
        &[("x-wayfinder-privacy-posture", "local-devices")],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-served-by")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-privacy-posture")
            .and_then(|value| value.to_str().ok()),
        Some("local-devices")
    );
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("seen lock poisoned"))?
            .as_slice(),
        ["local"]
    );
    Ok(())
}

#[tokio::test]
async fn native_text_only_provider_rejects_tool_request_before_delivery() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new("apple-local", "", "system-default", None, true).with_provider(
                ProviderKind::AppleFoundationModels,
                Some(ProviderTier::Local),
            ),
        ],
        false,
        "test",
    )
    .with_delivery(Arc::new(CountingUsageDelivery {
        calls: Arc::clone(&calls),
    }));
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "apple-local",
            "tools": [{"type": "function", "function": {"name": "lookup"}}],
            "messages": [{"role": "user", "content": "use a tool"}]
        }),
        &[],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json_body(response).await?["error"]["type"],
        "wayfinder_router_destination_ineligible"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn slash_directive_is_stripped_and_explicit_pin_still_wins() -> TestResult {
    let state = AppState::new(
        RoutingConfig::binary(0.2),
        vec![ConfiguredModel::new(
            "named",
            "https://never-contact.invalid/v1",
            "provider-model",
            None,
            true,
        )],
        false,
        "test",
    )
    .with_dry_run(true)
    .with_slash_directives(true);
    let payload = json!({
        "messages": [{"role": "user", "content": "/prefer-hosted hi"}]
    });
    let response = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await?;
    assert_eq!(body["wayfinder"]["model"], "cloud");
    assert_eq!(body["wayfinder"]["mode"], "slash-pinned");
    assert_eq!(body["wayfinder"]["features"]["word_count"], 1);

    let pinned = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "named",
            "messages": [{"role": "user", "content": "/prefer-hosted hi"}]
        }),
        &[],
    )
    .await?;
    let pinned = json_body(pinned).await?;
    assert_eq!(pinned["wayfinder"]["model"], "named");
    assert_eq!(pinned["wayfinder"]["mode"], "pinned");

    let automatic = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "messages": [{"role": "user", "content": "/auto hi"}]
        }),
        &[],
    )
    .await?;
    let automatic = json_body(automatic).await?;
    assert_eq!(automatic["wayfinder"]["model"], "local");
    assert_eq!(automatic["wayfinder"]["mode"], "scored");
    assert_eq!(automatic["wayfinder"]["features"]["word_count"], 1);
    Ok(())
}

#[tokio::test]
async fn sticky_header_latches_a_hard_conversation_and_validates_when_pinned() -> TestResult {
    let state =
        AppState::new(RoutingConfig::binary(0.2), Vec::new(), false, "test").with_dry_run(true);
    let hard_turn = (0..450).map(|_| "word").collect::<Vec<_>>().join(" ");
    let payload = json!({
        "messages": [
            {"role": "user", "content": hard_turn},
            {"role": "assistant", "content": "done"},
            {"role": "user", "content": "hi"}
        ]
    });
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &payload,
        &[("x-wayfinder-sticky", "true")],
    )
    .await?;
    let body = json_body(response).await?;
    assert_eq!(body["wayfinder"]["model"], "cloud");
    assert_eq!(body["wayfinder"]["mode"], "sticky");

    let invalid = post_json(
        &state,
        "/v1/chat/completions",
        &json!({"model": "prefer-local", "messages": []}),
        &[("x-wayfinder-sticky", "maybe")],
    )
    .await?;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(invalid).await?["error"]["type"],
        "wayfinder_router_bad_override"
    );
    Ok(())
}

#[tokio::test]
async fn request_scoring_tuning_changes_decision_and_bad_tuning_is_400() -> TestResult {
    let state =
        AppState::new(RoutingConfig::binary(0.2), Vec::new(), false, "test").with_dry_run(true);
    let prompt = "Prove the halting problem is undecidable.";
    let base = post_json(
        &state,
        "/v1/chat/completions",
        &json!({"messages": [{"role": "user", "content": prompt}]}),
        &[],
    )
    .await?;
    assert_eq!(json_body(base).await?["wayfinder"]["model"], "local");

    let tuned = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "messages": [{"role": "user", "content": prompt}],
            "wayfinder_tuning": {"weights": {"reasoning_term_count": 6.0}}
        }),
        &[],
    )
    .await?;
    let tuned = json_body(tuned).await?;
    assert_eq!(tuned["wayfinder"]["model"], "cloud");
    assert!(
        tuned["wayfinder"]["contributions"]
            .as_array()
            .is_some_and(|contributions| contributions.iter().any(|contribution| {
                contribution["name"] == "reasoning_term_count"
                    && contribution["contribution"].as_f64().unwrap_or(0.0) > 0.0
            }))
    );

    let invalid = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "messages": [],
            "wayfinder_tuning": {"weights": {"ghost": 1}}
        }),
        &[],
    )
    .await?;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    let body = json_body(invalid).await?;
    assert_eq!(body["error"]["type"], "wayfinder_router_bad_override");
    assert_eq!(
        body["error"]["message"],
        "wayfinder_tuning.weights: unknown feature 'ghost'"
    );
    Ok(())
}

#[tokio::test]
async fn router_config_exports_round_trippable_tuning_without_mutating_state() -> TestResult {
    let state = AppState::new(RoutingConfig::binary(0.2), Vec::new(), false, "test");
    let response = post_json(
        &state,
        "/router/config",
        &json!({
            "weights": {"reasoning_term_count": 6.0},
            "lexicon": {"reasoning_terms": ["prove", "qed"]}
        }),
        &[],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/plain; charset=utf-8")
    );
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let rendered = String::from_utf8(bytes.to_vec())?;
    let parsed = wayfinder_config::routing_config_from_toml(
        &rendered,
        "exported",
        None,
        wayfinder_config::TierOrderPolicy::StrictInput,
    )?;
    assert_eq!(parsed.weights.get("reasoning_term_count"), Some(6.0));
    assert_eq!(
        parsed.lexicon.reasoning_terms().collect::<Vec<_>>(),
        ["prove", "qed"]
    );
    assert_eq!(
        state.routing().weights.get("reasoning_term_count"),
        Some(0.0)
    );

    let invalid = post_json(
        &state,
        "/router/config",
        &json!({"weights": {"word_count": -3}}),
        &[],
    )
    .await?;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(invalid).await?["error"]["type"],
        "wayfinder_router_bad_override"
    );
    Ok(())
}

#[tokio::test]
async fn bad_override_has_only_request_id_metadata() -> TestResult {
    let state = AppState::new(RoutingConfig::default(), Vec::new(), false, "test");
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({}),
        &[("x-wayfinder-route-on", "somewhere")],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        response
            .headers()
            .get("x-wayfinder-router-request-id")
            .is_some()
    );
    assert!(response.headers().get("x-wayfinder-router-model").is_none());
    assert_eq!(
        json_body(response).await?["error"]["type"],
        "wayfinder_router_bad_override"
    );
    Ok(())
}

#[tokio::test]
async fn configured_live_mode_is_truthfully_not_ready_and_leaks_no_input() -> TestResult {
    let state = AppState::new(
        RoutingConfig::default(),
        vec![ConfiguredModel::new(
            "local",
            "https://never-contact.invalid/v1",
            "provider-model",
            Some("PROVIDER_KEY_NAME".to_owned()),
            true,
        )],
        false,
        "phase-two",
    );
    let secret_prompt = "prompt-value-that-must-not-be-returned";
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({"messages": [{"role": "user", "content": secret_prompt}]}),
        &[],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-model")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    let body = json_body(response).await?;
    assert_eq!(body["error"]["type"], "wayfinder_router_not_ready");
    let encoded = serde_json::to_string(&body)?;
    assert!(!encoded.contains(secret_prompt));
    assert!(!encoded.contains("PROVIDER_KEY_NAME"));
    Ok(())
}

#[tokio::test]
async fn buffered_live_delivery_is_byte_clean_and_receives_sanitized_body() -> TestResult {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let delivery = Arc::new(FakeDelivery {
        seen: Arc::clone(&seen),
        fail: false,
    });
    let state = AppState::new(
        RoutingConfig::binary(0.2),
        vec![ConfiguredModel::new(
            "local",
            "http://127.0.0.1:11434/v1",
            "upstream-small",
            None,
            true,
        )],
        false,
        "test",
    )
    .with_slash_directives(true)
    .with_delivery(delivery);
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "/auto hi"}],
            "wayfinder_tuning": {"weights": {"word_count": 4.0}}
        }),
        &[],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-served-by")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-model")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(bytes, Bytes::from_static(b"{\"id\":\"provider-response\"}"));
    assert!(!String::from_utf8_lossy(&bytes).contains("wayfinder"));

    {
        let captured = seen
            .lock()
            .map_err(|_| std::io::Error::other("fake delivery lock poisoned"))?;
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, "upstream-small");
        assert_eq!(captured[0].1["messages"][0]["content"], "hi");
        assert!(captured[0].1.get("wayfinder_tuning").is_none());
    }
    let recent = json_body(get(&state, "/router/recent?limit=1").await?).await?;
    assert_eq!(recent["recent"][0]["served_by"], "local");
    assert_eq!(recent["recent"][0]["execution_boundary"], "on-device");
    assert_eq!(recent["recent"][0]["outcome"], "succeeded");
    assert_eq!(recent["recent"][0]["http_status"], 201);
    Ok(())
}

#[tokio::test]
async fn live_exact_usage_updates_priced_savings_metrics_and_recent() -> TestResult {
    let state = AppState::new(
        RoutingConfig::binary(0.2),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "upstream-local",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.0)),
            ConfiguredModel::new(
                "cloud",
                "https://cloud.example/v1",
                "upstream-cloud",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.01)),
        ],
        false,
        "test",
    )
    .with_delivery(Arc::new(ExactUsageDelivery));
    assert_eq!(state.price_table_version(), "a98334b82067");

    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({"model": "auto", "messages": [{"role": "user", "content": "hi"}]}),
        &[],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(
        bytes,
        Bytes::from_static(
            br#"{"object":"chat.completion","usage":{"prompt_tokens":1000,"completion_tokens":25},"choices":[{"message":{"content":"ok"}}]}"#
        )
    );

    let report = json_body(get(&state, "/v1/savings").await?).await?;
    assert_eq!(report["priced"], true);
    assert_eq!(report["unit"], "usd");
    assert_eq!(report["requests"], 1);
    assert_eq!(report["estimated_requests"], 0);
    assert_eq!(report["tokens"], 1_025);
    assert_eq!(report["realized"], 0.0);
    assert_eq!(report["baseline"], 0.01025);
    assert_eq!(report["saved"], 0.01025);
    assert_eq!(report["saved_pct"], 100.0);
    assert_eq!(report["price_table_version"], "a98334b82067");
    assert_eq!(report["by_route"]["local"]["tokens"], 1_025);

    let recent = json_body(get(&state, "/router/recent?limit=1").await?).await?;
    assert_eq!(recent["recent"][0]["cost"]["realized"], 0.0);
    assert_eq!(recent["recent"][0]["cost"]["baseline"], 0.01025);
    assert_eq!(recent["recent"][0]["cost"]["saved"], 0.01025);
    assert_eq!(recent["recent"][0]["cost"]["tokens"], 1_025);
    assert_eq!(recent["recent"][0]["cost"]["unit"], "usd");
    assert_eq!(recent["recent"][0]["cost"]["estimated"], false);

    let metrics = to_bytes(get(&state, "/metrics").await?.into_body(), usize::MAX).await?;
    let metrics = String::from_utf8(metrics.to_vec())?;
    assert!(metrics.contains("wayfinder_router_baseline_cost_total 0.01025"));
    assert!(metrics.contains("wayfinder_router_savings_cost_total 0.01025"));
    Ok(())
}

#[tokio::test]
async fn live_cache_replays_exact_bytes_without_double_accounting() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let state = cached_live_state(Arc::clone(&calls))?;
    let payload = json!({
        "model": "cloud",
        "messages": [{"role": "user", "content": "deterministic request"}]
    });

    let first = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        first
            .headers()
            .get("x-wayfinder-router-cache")
            .and_then(|value| value.to_str().ok()),
        Some("miss")
    );
    let first_body = to_bytes(first.into_body(), usize::MAX).await?;

    let second = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        second
            .headers()
            .get("x-wayfinder-router-cache")
            .and_then(|value| value.to_str().ok()),
        Some("hit")
    );
    assert_eq!(
        second
            .headers()
            .get("x-wayfinder-router-served-by")
            .and_then(|value| value.to_str().ok()),
        Some("cloud")
    );
    let second_body = to_bytes(second.into_body(), usize::MAX).await?;
    assert_eq!(second_body, first_body);
    assert_eq!(second_body, Bytes::from_static(EXACT_USAGE_BODY));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let savings = json_body(get(&state, "/v1/savings").await?).await?;
    assert_eq!(savings["requests"], 1);
    assert_eq!(savings["realized"], 0.01025);

    let recent = json_body(get(&state, "/router/recent?limit=2").await?).await?;
    assert_eq!(recent["recent"][0]["cache"], "hit");
    assert!(recent["recent"][0].get("cost").is_none());
    assert!(recent["recent"][1].get("cache").is_none());
    assert_eq!(recent["recent"][1]["cost"]["realized"], 0.01025);

    let metrics = to_bytes(get(&state, "/metrics").await?.into_body(), usize::MAX).await?;
    let metrics = String::from_utf8(metrics.to_vec())?;
    assert!(metrics.contains("wayfinder_router_cache_hits_total 1"));
    assert!(metrics.contains("wayfinder_router_cache_misses_total 1"));
    assert!(metrics.contains("wayfinder_router_cache_avoided_cost_total 0.01025"));
    assert!(metrics.contains("wayfinder_router_realized_cost_total 0.01025"));

    let cache = state.response_cache().stats()?;
    assert_eq!(cache.entries, 1);
    assert_eq!(cache.hits, 1);
    assert_eq!(cache.misses, 1);
    Ok(())
}

#[tokio::test]
async fn live_cache_never_replays_across_virtual_keys() -> TestResult {
    let mut config = GatewayConfig::default();
    for (id, secret) in [("tenant-a", "secret-a"), ("tenant-b", "secret-b")] {
        let mut key = access_key(secret);
        key.models = vec!["cloud".to_owned()];
        config.keys.insert(id.to_owned(), key);
    }
    let policy = AccessPolicy::from_gateway_config_with_clock(&config, || 1_000.0)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let state = cached_live_state(Arc::clone(&calls))?.with_access_policy(policy);
    let payload = json!({
        "model": "cloud",
        "messages": [{"role": "user", "content": "same deterministic request"}]
    });

    let first = post_json(
        &state,
        "/v1/chat/completions",
        &payload,
        &[("authorization", "Bearer secret-a")],
    )
    .await?;
    assert_eq!(
        first
            .headers()
            .get(ROUTER_CACHE_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("miss")
    );

    let second = post_json(
        &state,
        "/v1/chat/completions",
        &payload,
        &[("authorization", "Bearer secret-b")],
    )
    .await?;
    assert_eq!(
        second
            .headers()
            .get(ROUTER_CACHE_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("miss")
    );

    let replay = post_json(
        &state,
        "/v1/chat/completions",
        &payload,
        &[("authorization", "Bearer secret-a")],
    )
    .await?;
    assert_eq!(
        replay
            .headers()
            .get(ROUTER_CACHE_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("hit")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn live_cache_never_replays_across_privacy_postures_or_routes() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let state = cached_live_state(Arc::clone(&calls))?.with_route_presets(IndexMap::from([(
        "local-only".to_owned(),
        RoutePreset {
            models: vec!["local".to_owned()],
        },
    )]));
    let direct = json!({
        "model": "local",
        "messages": [{"role": "user", "content": "same deterministic request"}]
    });

    let hosted = post_json(
        &state,
        "/v1/chat/completions",
        &direct,
        &[(PRIVACY_POSTURE_HEADER, "hosted-allowed")],
    )
    .await?;
    assert_eq!(
        hosted
            .headers()
            .get(ROUTER_CACHE_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("miss")
    );

    let on_device = post_json(
        &state,
        "/v1/chat/completions",
        &direct,
        &[(PRIVACY_POSTURE_HEADER, "on-device-only")],
    )
    .await?;
    assert_eq!(
        on_device
            .headers()
            .get(ROUTER_CACHE_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("miss")
    );

    let preset = json!({
        "model": "@route/local-only",
        "messages": [{"role": "user", "content": "same deterministic request"}]
    });
    let preset_miss = post_json(
        &state,
        "/v1/chat/completions",
        &preset,
        &[(PRIVACY_POSTURE_HEADER, "on-device-only")],
    )
    .await?;
    assert_eq!(
        preset_miss
            .headers()
            .get(ROUTER_CACHE_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("miss")
    );

    let preset_hit = post_json(
        &state,
        "/v1/chat/completions",
        &preset,
        &[(PRIVACY_POSTURE_HEADER, "on-device-only")],
    )
    .await?;
    assert_eq!(
        preset_hit
            .headers()
            .get(ROUTER_CACHE_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("hit")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    Ok(())
}

#[tokio::test]
async fn codex_account_routes_never_replay_or_store_exact_response_cache_entries() -> TestResult {
    let payload = json!({
        "model": "chatgpt",
        "messages": [{"role": "user", "content": "account-scoped request"}]
    });

    for error in [
        crate::delivery::CodexDeliveryError::AuthenticationRequired,
        crate::delivery::CodexDeliveryError::Unavailable,
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let state = AppState::new(
            RoutingConfig::binary(0.5),
            vec![
                ConfiguredModel::new("chatgpt", "", "shared-provider-model", None, true)
                    .with_provider(ProviderKind::CodexAppServer, None),
            ],
            false,
            "test",
        )
        .with_delivery(Arc::new(CodexFailureDelivery {
            calls: Arc::clone(&calls),
            error,
        }))
        .with_cache_and_clock(
            CacheSettings {
                enabled: true,
                ttl_seconds: 300.0,
                max_entries: 16,
                max_bytes: 1024 * 1024,
            },
            || 1_000.0,
        )?;

        state.response_cache().put_at(
            cache_key("local", "chatgpt", "shared-provider-model", &payload)?,
            CachedResponse {
                status: StatusCode::OK.as_u16(),
                content_type: "application/json".to_owned(),
                body: br#"{"choices":[{"message":{"content":"old account reply"}}]}"#.to_vec(),
                prompt_tokens: 1,
                completion_tokens: 1,
                estimated: true,
            },
            state.cache_now(),
        )?;

        let response = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = json_body(response).await?;
        assert_eq!(body["error"]["type"], "wayfinder_router_not_ready");
        assert!(!body.to_string().contains("old account reply"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let cache = state.response_cache().stats()?;
        assert_eq!(cache.entries, 1);
        assert_eq!(cache.hits, 0);
        assert_eq!(cache.misses, 0);
    }
    Ok(())
}

#[tokio::test]
async fn openai_cache_miss_never_stores_a_codex_fallback_response() -> TestResult {
    let config = GatewayConfig {
        retries: 0,
        ..GatewayConfig::default()
    };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let delivery = Arc::new(ScriptedDelivery {
        outcomes: Arc::new(Mutex::new(BTreeMap::from([
            (
                "local".to_owned(),
                VecDeque::from([ScriptedOutcome::Transport]),
            ),
            (
                "chatgpt".to_owned(),
                VecDeque::from([ScriptedOutcome::Response(StatusCode::OK, EXACT_USAGE_BODY)]),
            ),
        ]))),
        seen: Arc::clone(&seen),
    });
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "shared-provider-model",
                None,
                true,
            )
            .with_fallbacks(vec!["chatgpt".to_owned()]),
            ConfiguredModel::new("chatgpt", "", "shared-provider-model", None, true)
                .with_provider(ProviderKind::CodexAppServer, None),
        ],
        false,
        "test",
    )
    .with_gateway_reliability(&config)?
    .with_delivery(delivery)
    .with_cache_and_clock(
        CacheSettings {
            enabled: true,
            ttl_seconds: 300.0,
            max_entries: 16,
            max_bytes: 1024 * 1024,
        },
        || 1_000.0,
    )?;

    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "fallback request"}]
        }),
        &[],
    )
    .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-served-by")
            .and_then(|value| value.to_str().ok()),
        Some("chatgpt")
    );
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("scripted delivery lock poisoned"))?
            .as_slice(),
        ["local", "chatgpt"]
    );
    let cache = state.response_cache().stats()?;
    assert_eq!(cache.entries, 0);
    assert_eq!(cache.hits, 0);
    assert_eq!(cache.misses, 1);
    Ok(())
}

#[tokio::test]
async fn nondeterministic_and_debug_requests_bypass_live_cache() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let state = cached_live_state(Arc::clone(&calls))?;
    let sampled = json!({
        "model": "cloud",
        "temperature": 0.7,
        "messages": [{"role": "user", "content": "sample this"}]
    });
    for _ in 0..2 {
        let response = post_json(&state, "/v1/chat/completions", &sampled, &[]).await?;
        assert!(response.headers().get("x-wayfinder-router-cache").is_none());
    }

    let deterministic = json!({
        "model": "cloud",
        "messages": [{"role": "user", "content": "debug this"}]
    });
    for _ in 0..2 {
        let response = post_json(
            &state,
            "/v1/chat/completions",
            &deterministic,
            &[("x-wayfinder-debug", "true")],
        )
        .await?;
        assert!(response.headers().get("x-wayfinder-router-cache").is_none());
        let body = json_body(response).await?;
        assert_eq!(body["wayfinder"]["model"], "cloud");
        assert_eq!(body["wayfinder"]["mode"], "pinned");
        assert!(body["choices"].is_array());
    }

    assert_eq!(calls.load(Ordering::SeqCst), 4);
    let cache = state.response_cache().stats()?;
    assert_eq!(cache.entries, 0);
    assert_eq!(cache.hits, 0);
    assert_eq!(cache.misses, 0);
    Ok(())
}

#[tokio::test]
async fn exhausted_global_budget_degrades_route_without_rescoring() -> TestResult {
    let config = GatewayConfig {
        budget: Some(GatewayBudget {
            limit: 0.001,
            window: "day".to_owned(),
            on_breach: "degrade".to_owned(),
        }),
        ..GatewayConfig::default()
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let state = budget_live_state(&config, false, Arc::clone(&calls))?;
    seed_budget_spend(&state, None)?;
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "cloud",
            "messages": [{"role": "user", "content": "hi"}]
        }),
        &[],
    )
    .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-budget")
            .and_then(|value| value.to_str().ok()),
        Some("degraded")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-model")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-served-by")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-mode")
            .and_then(|value| value.to_str().ok()),
        Some("budget-degraded")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-score")
            .and_then(|value| value.to_str().ok()),
        Some("0.00")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let savings = json_body(get(&state, "/v1/savings").await?).await?;
    assert_eq!(savings["requests"], 2);
    assert_eq!(savings["realized"], 0.01);
    let recent = json_body(get(&state, "/router/recent").await?).await?;
    assert_eq!(recent["recent"][0]["model"], "local");
    assert_eq!(recent["recent"][0]["mode"], "budget-degraded");
    Ok(())
}

#[tokio::test]
async fn unpriced_live_gateway_ignores_dollar_budget() -> TestResult {
    let config = GatewayConfig {
        budget: Some(GatewayBudget {
            limit: 0.001,
            window: "day".to_owned(),
            on_breach: "block".to_owned(),
        }),
        ..GatewayConfig::default()
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "upstream-local",
                None,
                true,
            ),
            ConfiguredModel::new(
                "cloud",
                "https://cloud.example/v1",
                "upstream-cloud",
                None,
                true,
            ),
        ],
        false,
        "test",
    )
    .with_gateway_budget(&config)
    .with_delivery(Arc::new(CountingUsageDelivery {
        calls: Arc::clone(&calls),
    }));
    assert!(!state.price_table().priced);
    seed_budget_spend(&state, None)?;

    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "cloud",
            "messages": [{"role": "user", "content": "hi"}]
        }),
        &[],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get("x-wayfinder-router-budget")
            .is_none()
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-served-by")
            .and_then(|value| value.to_str().ok()),
        Some("cloud")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn hard_budget_blocks_online_but_offline_softens_to_delivery_degrade() -> TestResult {
    let config = GatewayConfig {
        budget: Some(GatewayBudget {
            limit: 0.001,
            window: "day".to_owned(),
            on_breach: "block".to_owned(),
        }),
        ..GatewayConfig::default()
    };
    let payload = json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hi"}]
    });

    let online_calls = Arc::new(AtomicUsize::new(0));
    let online = budget_live_state(&config, false, Arc::clone(&online_calls))?;
    seed_budget_spend(&online, None)?;
    let blocked = post_json(&online, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(blocked.status(), StatusCode::PAYMENT_REQUIRED);
    assert_eq!(
        blocked
            .headers()
            .get("x-wayfinder-router-budget")
            .and_then(|value| value.to_str().ok()),
        Some("blocked")
    );
    assert!(blocked.headers().get("x-wayfinder-router-model").is_none());
    assert_eq!(
        json_body(blocked).await?,
        json!({
            "error": {
                "message": "day budget of 0.001 reached",
                "type": "wayfinder_router_budget_exhausted"
            }
        })
    );
    assert_eq!(online_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        json_body(get(&online, "/router/recent").await?).await?["total"],
        0
    );

    let offline_calls = Arc::new(AtomicUsize::new(0));
    let offline = budget_live_state(&config, true, Arc::clone(&offline_calls))?;
    seed_budget_spend(&offline, None)?;
    let degraded = post_json(&offline, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(degraded.status(), StatusCode::OK);
    // The budget softens to a degrade offline but does not reroute: offline
    // delivery already serves the cheapest local tier, so it is `exhausted`.
    assert_eq!(
        degraded
            .headers()
            .get("x-wayfinder-router-budget")
            .and_then(|value| value.to_str().ok()),
        Some("exhausted")
    );
    assert_eq!(
        degraded
            .headers()
            .get("x-wayfinder-router-offline")
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
    assert_eq!(
        degraded
            .headers()
            .get("x-wayfinder-router-model")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    assert_eq!(
        degraded
            .headers()
            .get("x-wayfinder-router-mode")
            .and_then(|value| value.to_str().ok()),
        Some("scored")
    );
    assert_eq!(
        degraded
            .headers()
            .get("x-wayfinder-router-served-by")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    assert_eq!(offline_calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn exhausted_budget_on_the_cheapest_tier_reports_exhausted() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let state = budget_live_state(&degrade_budget_config(), false, Arc::clone(&calls))?;
    seed_budget_spend(&state, None)?;
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hi"}]
        }),
        &[],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_text(&response, "x-wayfinder-router-budget"),
        Some("exhausted")
    );
    assert_eq!(
        header_text(&response, "x-wayfinder-router-model"),
        Some("local")
    );
    assert_eq!(
        header_text(&response, "x-wayfinder-router-mode"),
        Some("scored")
    );
    assert_eq!(
        header_text(&response, "x-wayfinder-router-served-by"),
        Some("local")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn budget_degrade_undone_by_key_allowlist_reports_exhausted() -> TestResult {
    let mut config = degrade_budget_config();
    let mut key = access_key("wf-secret");
    key.models = vec!["cloud".to_owned()];
    config.keys.insert("team-a".to_owned(), key);
    let calls = Arc::new(AtomicUsize::new(0));
    let state = budget_live_state(&config, false, Arc::clone(&calls))?;
    seed_budget_spend(&state, None)?;
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "cloud",
            "messages": [{"role": "user", "content": "hi"}]
        }),
        &[("authorization", "Bearer wf-secret")],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    // The cheapest tier is not allowed for this key, so the clamp puts the
    // request back on `cloud`: the budget changed nothing.
    assert_eq!(
        header_text(&response, "x-wayfinder-router-budget"),
        Some("exhausted")
    );
    assert_eq!(
        header_text(&response, "x-wayfinder-router-model"),
        Some("cloud")
    );
    assert_eq!(
        header_text(&response, "x-wayfinder-router-mode"),
        Some("key-scoped")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn classifier_budget_degrade_reroutes_to_cheapest_priced_model() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let state = classifier_budget_live_state(
        &degrade_budget_config(),
        vec![1.0, 0.0, 0.0, 0.0],
        Arc::clone(&calls),
    )?;
    seed_budget_spend_on(&state, "large", None)?;
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hi"}]
        }),
        &[],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_text(&response, "x-wayfinder-router-budget"),
        Some("degraded")
    );
    // `spare` is unpriced and never a candidate; `small` and `tied` share
    // the lowest cost and `small` comes first in classifier order.
    assert_eq!(
        header_text(&response, "x-wayfinder-router-model"),
        Some("small")
    );
    assert_eq!(
        header_text(&response, "x-wayfinder-router-mode"),
        Some("budget-degraded")
    );
    assert_eq!(
        header_text(&response, "x-wayfinder-router-served-by"),
        Some("small")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let recent = json_body(get(&state, "/router/recent").await?).await?;
    assert_eq!(recent["recent"][0]["model"], "small");
    assert_eq!(recent["recent"][0]["mode"], "budget-degraded");
    Ok(())
}

#[tokio::test]
async fn classifier_budget_on_the_cheapest_model_reports_exhausted() -> TestResult {
    for (intercepts, expected) in [
        // Already on the first cheapest model.
        (vec![0.0, 0.0, 1.0, 0.0], "small"),
        // An equal-cost model is not a downgrade.
        (vec![0.0, 0.0, 0.0, 1.0], "tied"),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let state =
            classifier_budget_live_state(&degrade_budget_config(), intercepts, Arc::clone(&calls))?;
        seed_budget_spend_on(&state, "large", None)?;
        let response = post_json(
            &state,
            "/v1/chat/completions",
            &json!({
                "model": "auto",
                "messages": [{"role": "user", "content": "hi"}]
            }),
            &[],
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            header_text(&response, "x-wayfinder-router-budget"),
            Some("exhausted")
        );
        assert_eq!(
            header_text(&response, "x-wayfinder-router-model"),
            Some(expected)
        );
        assert_eq!(
            header_text(&response, "x-wayfinder-router-mode"),
            Some("scored")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    Ok(())
}

#[tokio::test]
async fn authenticated_key_budget_reads_only_its_attributed_spend() -> TestResult {
    let mut key = access_key("wf-secret");
    key.budget = Some(GatewayBudget {
        limit: 0.001,
        window: "day".to_owned(),
        on_breach: "block".to_owned(),
    });
    let mut config = GatewayConfig::default();
    config.keys.insert("team-a".to_owned(), key);
    let calls = Arc::new(AtomicUsize::new(0));
    let state = budget_live_state(&config, false, Arc::clone(&calls))?;
    seed_budget_spend(&state, Some("team-a"))?;

    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "Prove the halting problem is undecidable."}],
            "wayfinder_tuning": {"weights": {"reasoning_term_count": 6.0}}
        }),
        &[("authorization", "Bearer wf-secret")],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-budget")
            .and_then(|value| value.to_str().ok()),
        Some("blocked")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let savings = json_body(get(&state, "/v1/savings").await?).await?;
    assert_eq!(savings["by_key"]["team-a"]["requests"], 1);
    Ok(())
}

#[tokio::test]
async fn denied_fallback_is_never_delivered_after_primary_failure() -> TestResult {
    let mut config = GatewayConfig {
        retries: 0,
        failover: "same-tier".to_owned(),
        ..GatewayConfig::default()
    };
    let mut key = access_key("wf-secret");
    key.models = vec!["cloud".to_owned()];
    config.keys.insert("team-a".to_owned(), key);
    let (state, seen) = reliability_live_state(
        &config,
        vec!["cloud-backup".to_owned()],
        BTreeMap::from([
            (
                "cloud".to_owned(),
                VecDeque::from([ScriptedOutcome::Transport]),
            ),
            (
                "cloud-backup".to_owned(),
                VecDeque::from([ScriptedOutcome::Response(StatusCode::OK, EXACT_USAGE_BODY)]),
            ),
        ]),
    )?;
    let state = state.with_access_policy(AccessPolicy::from_gateway_config_with_clock(
        &config,
        || 1_000.0,
    )?);
    let response = post_json(
        &state, "/v1/chat/completions",
        &json!({"model": "auto", "messages": [{"role": "user", "content": "Prove the halting problem is undecidable."}]}),
        &[("authorization", "Bearer wf-secret"), ("x-wayfinder-threshold", "0.1")],
    ).await?;
    assert!(!response.status().is_success());
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("seen lock poisoned"))?
            .as_slice(),
        ["cloud"]
    );
    Ok(())
}

#[tokio::test]
async fn transient_exhaustion_retries_then_uses_ordered_fallback() -> TestResult {
    let config = GatewayConfig {
        retries: 1,
        breaker_threshold: 5,
        breaker_cooldown: 30.0,
        failover: "same-tier".to_owned(),
        ..GatewayConfig::default()
    };
    let (state, seen) = reliability_live_state(
        &config,
        vec!["cloud-backup".to_owned()],
        BTreeMap::from([
            (
                "cloud".to_owned(),
                VecDeque::from([ScriptedOutcome::Transport, ScriptedOutcome::Transport]),
            ),
            (
                "cloud-backup".to_owned(),
                VecDeque::from([ScriptedOutcome::Response(StatusCode::OK, EXACT_USAGE_BODY)]),
            ),
        ]),
    )?;
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "Prove the halting problem is undecidable."}],
            "wayfinder_tuning": {"weights": {"reasoning_term_count": 6.0}}
        }),
        &[("x-wayfinder-threshold", "0.1")],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-model")
            .and_then(|value| value.to_str().ok()),
        Some("cloud")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-served-by")
            .and_then(|value| value.to_str().ok()),
        Some("cloud-backup")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-failover")
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("scripted delivery lock poisoned"))?
            .as_slice(),
        ["cloud", "cloud", "cloud-backup"]
    );
    Ok(())
}

#[tokio::test]
async fn exhausted_target_opens_breaker_and_next_request_fails_before_delivery() -> TestResult {
    let config = GatewayConfig {
        retries: 0,
        breaker_threshold: 1,
        breaker_cooldown: 30.0,
        failover: "same-tier".to_owned(),
        ..GatewayConfig::default()
    };
    let (state, seen) = reliability_live_state(
        &config,
        Vec::new(),
        BTreeMap::from([(
            "cloud".to_owned(),
            VecDeque::from([ScriptedOutcome::Transport]),
        )]),
    )?;
    let payload = json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "Prove the halting problem is undecidable."}],
        "wayfinder_tuning": {"weights": {"reasoning_term_count": 6.0}}
    });

    let exhausted = post_json(
        &state,
        "/v1/chat/completions",
        &payload,
        &[("x-wayfinder-threshold", "0.1")],
    )
    .await?;
    assert_eq!(exhausted.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        json_body(exhausted).await?["error"]["type"],
        "wayfinder_router_upstream_error"
    );
    let open = post_json(
        &state,
        "/v1/chat/completions",
        &payload,
        &[("x-wayfinder-threshold", "0.1")],
    )
    .await?;
    assert_eq!(open.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        json_body(open).await?["error"]["type"],
        "wayfinder_router_circuit_open"
    );
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("scripted delivery lock poisoned"))?
            .as_slice(),
        ["cloud"]
    );
    Ok(())
}

#[tokio::test]
async fn upstream_auth_failure_is_terminal_then_opens_fallback_on_next_request() -> TestResult {
    let config = GatewayConfig {
        retries: 2,
        breaker_threshold: 1,
        breaker_cooldown: 30.0,
        failover: "same-tier".to_owned(),
        ..GatewayConfig::default()
    };
    let (state, seen) = reliability_live_state(
        &config,
        vec!["cloud-backup".to_owned()],
        BTreeMap::from([
            (
                "cloud".to_owned(),
                VecDeque::from([ScriptedOutcome::Response(
                    StatusCode::UNAUTHORIZED,
                    br#"{"error":{"message":"bad key"}}"#,
                )]),
            ),
            (
                "cloud-backup".to_owned(),
                VecDeque::from([ScriptedOutcome::Response(StatusCode::OK, EXACT_USAGE_BODY)]),
            ),
        ]),
    )?;
    let payload = json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "Prove the halting problem is undecidable."}],
        "wayfinder_tuning": {"weights": {"reasoning_term_count": 6.0}}
    });

    let auth = post_json(
        &state,
        "/v1/chat/completions",
        &payload,
        &[("x-wayfinder-threshold", "0.1")],
    )
    .await?;
    assert_eq!(auth.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        auth.headers()
            .get("x-wayfinder-router-served-by")
            .and_then(|value| value.to_str().ok()),
        Some("cloud")
    );
    assert!(auth.headers().get("x-wayfinder-router-failover").is_none());
    assert_eq!(json_body(auth).await?["error"]["message"], "bad key");

    let fallback = post_json(
        &state,
        "/v1/chat/completions",
        &payload,
        &[("x-wayfinder-threshold", "0.1")],
    )
    .await?;
    assert_eq!(fallback.status(), StatusCode::OK);
    assert_eq!(
        fallback
            .headers()
            .get("x-wayfinder-router-served-by")
            .and_then(|value| value.to_str().ok()),
        Some("cloud-backup")
    );
    assert_eq!(
        fallback
            .headers()
            .get("x-wayfinder-router-failover")
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("scripted delivery lock poisoned"))?
            .as_slice(),
        ["cloud", "cloud-backup"]
    );
    Ok(())
}

#[tokio::test]
async fn streaming_relay_establishes_before_commit_and_accounts_only_on_completion() -> TestResult {
    let config = GatewayConfig {
        retries: 2,
        ..GatewayConfig::default()
    };
    let chunks = [
        Bytes::from_static(br#"data: {"choices":[{"delta":{"content":"Hel"#),
        Bytes::from_static(b"lo\"}}]}\n\n"),
        Bytes::from_static(b"data: [DONE]\n\n"),
    ];
    let (state, seen) = streaming_live_state(
        &config,
        BTreeMap::from([(
            "local".to_owned(),
            VecDeque::from([
                ScriptedStreamOutcome::Chunks(chunks.iter().cloned().map(Ok).collect()),
                ScriptedStreamOutcome::Chunks(chunks.iter().cloned().map(Ok).collect()),
            ]),
        )]),
    )?;
    let payload = json!({
        "model": "auto",
        "stream": true,
        "messages": [{"role": "user", "content": "hi"}]
    });

    let abandoned = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(abandoned.status(), StatusCode::OK);
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("stream delivery lock poisoned"))?
            .as_slice(),
        ["local"]
    );
    drop(abandoned);
    assert_eq!(
        json_body(get(&state, "/v1/savings").await?).await?["requests"],
        0
    );

    let response = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-served-by")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(
        body,
        chunks.iter().fold(Vec::new(), |mut output, chunk| {
            output.extend_from_slice(chunk);
            output
        })
    );
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("stream delivery lock poisoned"))?
            .as_slice(),
        ["local", "local"]
    );
    let savings = json_body(get(&state, "/v1/savings").await?).await?;
    assert_eq!(savings["requests"], 1);
    assert_eq!(savings["estimated_requests"], 1);
    assert_eq!(savings["tokens"], 2);
    let recent = json_body(get(&state, "/router/recent?limit=2").await?).await?;
    assert!(recent["recent"][0]["cost"].is_object());
    assert_eq!(recent["recent"][0]["served_by"], "local");
    assert_eq!(recent["recent"][0]["execution_boundary"], "on-device");
    assert_eq!(recent["recent"][0]["outcome"], "succeeded");
    assert!(recent["recent"][1].get("cost").is_none());
    assert_eq!(recent["recent"][1]["outcome"], "cancelled");
    assert_eq!(
        recent["recent"][1]["error_type"],
        "wayfinder_router_cancelled"
    );
    Ok(())
}

#[tokio::test]
async fn debug_stream_prepends_authoritative_decision_metadata() -> TestResult {
    let chunks = vec![
        Ok(Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
        )),
        Ok(Bytes::from_static(b"data: [DONE]\n\n")),
    ];
    let (state, _) = streaming_live_state(
        &GatewayConfig::default(),
        BTreeMap::from([(
            "local".to_owned(),
            VecDeque::from([ScriptedStreamOutcome::Chunks(chunks)]),
        )]),
    )?;
    let payload = json!({
        "model": "auto",
        "stream": true,
        "messages": [{"role": "user", "content": "hi"}]
    });

    let response = post_json(
        &state,
        "/v1/chat/completions",
        &payload,
        &[("x-wayfinder-debug", "true")],
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8(body.to_vec())?;
    let first_event = text
        .split("\n\n")
        .next()
        .and_then(|event| event.strip_prefix("data: "))
        .ok_or_else(|| std::io::Error::other("missing debug metadata event"))?;
    let metadata: Value = serde_json::from_str(first_event)?;

    assert_eq!(metadata["wayfinder"]["model"], "local");
    assert_eq!(metadata["wayfinder"]["mode"], "scored");
    assert!(metadata["wayfinder"]["score"].is_number());
    assert!(metadata["wayfinder"]["features"].is_object());
    assert!(text.contains("\"content\":\"hello\""));
    assert!(text.ends_with("data: [DONE]\n\n"));
    Ok(())
}

#[tokio::test]
async fn established_codex_stream_preserves_distinct_terminal_categories() -> TestResult {
    for (error, expected_type) in [
        (
            DeliveryError::Codex(crate::delivery::CodexDeliveryError::Unavailable),
            "wayfinder_router_not_ready",
        ),
        (
            DeliveryError::Codex(crate::delivery::CodexDeliveryError::AuthenticationRequired),
            "wayfinder_router_not_ready",
        ),
        (
            DeliveryError::Codex(crate::delivery::CodexDeliveryError::ModelUnavailable),
            "wayfinder_router_not_ready",
        ),
        (
            DeliveryError::Codex(crate::delivery::CodexDeliveryError::Busy),
            "wayfinder_router_busy",
        ),
        (
            DeliveryError::Codex(crate::delivery::CodexDeliveryError::InvalidRequest),
            "wayfinder_router_unsupported_request",
        ),
        (
            DeliveryError::Codex(crate::delivery::CodexDeliveryError::TurnFailed),
            "wayfinder_router_turn_failed",
        ),
        (
            DeliveryError::Codex(crate::delivery::CodexDeliveryError::Interrupted),
            "wayfinder_router_interrupted",
        ),
        (
            DeliveryError::Codex(crate::delivery::CodexDeliveryError::UsageLimitReached),
            "wayfinder_router_usage_limited",
        ),
    ] {
        let (state, _) = streaming_live_state(
            &GatewayConfig::default(),
            BTreeMap::from([(
                "local".to_owned(),
                VecDeque::from([ScriptedStreamOutcome::Chunks(vec![Err(error)])]),
            )]),
        )?;
        let response = post_json(
            &state,
            "/v1/chat/completions",
            &json!({
                "model": "auto",
                "stream": true,
                "messages": [{"role": "user", "content": "hi"}]
            }),
            &[("x-wayfinder-debug", "true")],
        )
        .await?;

        assert_eq!(response.status(), StatusCode::OK);
        let text = String::from_utf8(to_bytes(response.into_body(), usize::MAX).await?.to_vec())?;
        assert!(text.contains(expected_type));
        assert!(text.ends_with("data: [DONE]\n\n"));
        assert_eq!(
            json_body(get(&state, "/v1/savings").await?).await?["requests"],
            0
        );
    }
    Ok(())
}

#[tokio::test]
async fn codex_readiness_and_user_stream_failures_do_not_open_breaker() -> TestResult {
    let config = GatewayConfig {
        retries: 0,
        breaker_threshold: 1,
        breaker_cooldown: 30.0,
        ..GatewayConfig::default()
    };
    let success = vec![
        Ok(Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"ready\"}}]}\n\n",
        )),
        Ok(Bytes::from_static(b"data: [DONE]\n\n")),
    ];
    for error in [
        crate::delivery::CodexDeliveryError::AuthenticationRequired,
        crate::delivery::CodexDeliveryError::ModelUnavailable,
        crate::delivery::CodexDeliveryError::Busy,
        crate::delivery::CodexDeliveryError::UsageLimitReached,
        crate::delivery::CodexDeliveryError::InvalidRequest,
        crate::delivery::CodexDeliveryError::Interrupted,
    ] {
        let (state, seen) = streaming_live_state(
            &config,
            BTreeMap::from([(
                "local".to_owned(),
                VecDeque::from([
                    ScriptedStreamOutcome::Chunks(vec![Err(DeliveryError::Codex(error))]),
                    ScriptedStreamOutcome::Chunks(success.clone()),
                ]),
            )]),
        )?;
        let payload = json!({
            "model": "auto",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        });

        let failed = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
        assert_eq!(failed.status(), StatusCode::OK);
        let failed_body =
            String::from_utf8(to_bytes(failed.into_body(), usize::MAX).await?.to_vec())?;
        assert!(failed_body.ends_with("data: [DONE]\n\n"));

        let recovered = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
        assert_eq!(recovered.status(), StatusCode::OK);
        let recovered_body =
            String::from_utf8(to_bytes(recovered.into_body(), usize::MAX).await?.to_vec())?;
        assert!(recovered_body.contains("ready"));
        assert_eq!(
            seen.lock()
                .map_err(|_| std::io::Error::other("stream delivery lock poisoned"))?
                .as_slice(),
            ["local", "local"]
        );
    }
    Ok(())
}

#[tokio::test]
async fn codex_busy_before_stream_establishment_does_not_open_breaker() -> TestResult {
    let config = GatewayConfig {
        retries: 0,
        breaker_threshold: 1,
        breaker_cooldown: 30.0,
        ..GatewayConfig::default()
    };
    let success = vec![
        Ok(Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"ready\"}}]}\n\n",
        )),
        Ok(Bytes::from_static(b"data: [DONE]\n\n")),
    ];
    let (state, seen) = streaming_live_state(
        &config,
        BTreeMap::from([(
            "local".to_owned(),
            VecDeque::from([
                ScriptedStreamOutcome::EstablishError(DeliveryError::Codex(
                    crate::delivery::CodexDeliveryError::Busy,
                )),
                ScriptedStreamOutcome::Chunks(success),
            ]),
        )]),
    )?;
    let payload = json!({
        "model": "auto",
        "stream": true,
        "messages": [{"role": "user", "content": "hi"}]
    });

    let busy = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(busy.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(busy).await?["error"]["type"],
        "wayfinder_router_busy"
    );

    let recovered = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(recovered.status(), StatusCode::OK);
    let recovered_body =
        String::from_utf8(to_bytes(recovered.into_body(), usize::MAX).await?.to_vec())?;
    assert!(recovered_body.contains("ready"));
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("stream delivery lock poisoned"))?
            .as_slice(),
        ["local", "local"]
    );
    Ok(())
}

#[tokio::test]
async fn codex_runtime_stream_failure_opens_breaker() -> TestResult {
    let config = GatewayConfig {
        retries: 0,
        breaker_threshold: 1,
        breaker_cooldown: 30.0,
        ..GatewayConfig::default()
    };
    let (state, seen) = streaming_live_state(
        &config,
        BTreeMap::from([(
            "local".to_owned(),
            VecDeque::from([ScriptedStreamOutcome::Chunks(vec![Err(
                DeliveryError::Codex(crate::delivery::CodexDeliveryError::Unavailable),
            )])]),
        )]),
    )?;
    let payload = json!({
        "model": "auto",
        "stream": true,
        "messages": [{"role": "user", "content": "hi"}]
    });

    let failed = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(failed.status(), StatusCode::OK);
    let failed_body = String::from_utf8(to_bytes(failed.into_body(), usize::MAX).await?.to_vec())?;
    assert!(failed_body.contains("wayfinder_router_not_ready"));

    let open = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(open.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        json_body(open).await?["error"]["type"],
        "wayfinder_router_circuit_open"
    );
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("stream delivery lock poisoned"))?
            .as_slice(),
        ["local"]
    );
    Ok(())
}

#[test]
fn codex_usage_limit_has_a_distinct_bounded_http_contract() {
    assert_eq!(
        crate::fatal_delivery_status(&DeliveryError::Codex(
            crate::delivery::CodexDeliveryError::UsageLimitReached,
        )),
        (
            StatusCode::TOO_MANY_REQUESTS,
            "wayfinder_router_usage_limited",
        )
    );
}

#[test]
fn codex_busy_has_a_distinct_bounded_http_contract() {
    assert_eq!(
        crate::fatal_delivery_status(&DeliveryError::Codex(
            crate::delivery::CodexDeliveryError::Busy,
        )),
        (StatusCode::CONFLICT, "wayfinder_router_busy")
    );
}

#[tokio::test]
async fn streaming_precommit_transport_failure_is_502_and_opens_breaker() -> TestResult {
    let config = GatewayConfig {
        retries: 2,
        breaker_threshold: 1,
        breaker_cooldown: 30.0,
        ..GatewayConfig::default()
    };
    let (state, seen) = streaming_live_state(
        &config,
        BTreeMap::from([(
            "local".to_owned(),
            VecDeque::from([ScriptedStreamOutcome::EstablishError(
                DeliveryError::Provider(
                    wayfinder_providers::openai_compat::ProviderError::Transport,
                ),
            )]),
        )]),
    )?;
    let payload = json!({
        "model": "auto",
        "stream": true,
        "messages": [{"role": "user", "content": "hi"}]
    });

    let response = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        json_body(response).await?["error"]["type"],
        "wayfinder_router_upstream_error"
    );
    let open = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(open.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        json_body(open).await?["error"]["type"],
        "wayfinder_router_circuit_open"
    );
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("stream delivery lock poisoned"))?
            .as_slice(),
        ["local", "local", "local"]
    );
    assert_eq!(
        json_body(get(&state, "/v1/savings").await?).await?["requests"],
        0
    );
    Ok(())
}

#[tokio::test]
async fn streaming_retryable_status_recovers_before_response_establishment() -> TestResult {
    let config = GatewayConfig {
        retries: 1,
        ..GatewayConfig::default()
    };
    let (state, seen) = streaming_live_state(
        &config,
        BTreeMap::from([(
            "local".to_owned(),
            VecDeque::from([
                ScriptedStreamOutcome::Response(StatusCode::SERVICE_UNAVAILABLE, vec![]),
                ScriptedStreamOutcome::Chunks(vec![Ok(Bytes::from_static(b"data: [DONE]\n\n"))]),
            ]),
        )]),
    )?;
    let payload = json!({
        "model": "auto",
        "stream": true,
        "messages": [{"role": "user", "content": "hi"}]
    });

    let response = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), 1024).await?,
        Bytes::from_static(b"data: [DONE]\n\n")
    );
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("stream delivery lock poisoned"))?
            .as_slice(),
        ["local", "local"]
    );
    Ok(())
}

#[tokio::test]
async fn streaming_precommit_apple_not_ready_is_truthful_503() -> TestResult {
    let (state, _) = streaming_live_state(
        &GatewayConfig::default(),
        BTreeMap::from([(
            "local".to_owned(),
            VecDeque::from([ScriptedStreamOutcome::EstablishError(DeliveryError::Apple(
                crate::delivery::AppleDeliveryError::NotReady,
            ))]),
        )]),
    )?;
    let payload = json!({
        "model": "auto",
        "stream": true,
        "messages": [{"role": "user", "content": "hi"}]
    });
    let response = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        json_body(response).await?["error"]["type"],
        "wayfinder_router_not_ready"
    );
    Ok(())
}

#[tokio::test]
async fn streaming_pre_first_byte_failure_uses_ordered_plan_fallback() -> TestResult {
    let config = GatewayConfig {
        failover: "escalate".to_owned(),
        ..GatewayConfig::default()
    };
    let chunks = vec![
        Ok(Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
        )),
        Ok(Bytes::from_static(b"data: [DONE]\n\n")),
    ];
    let (state, seen) = streaming_live_state(
        &config,
        BTreeMap::from([
            (
                "local".to_owned(),
                VecDeque::from([ScriptedStreamOutcome::EstablishError(
                    DeliveryError::Provider(
                        wayfinder_providers::openai_compat::ProviderError::Transport,
                    ),
                )]),
            ),
            (
                "cloud".to_owned(),
                VecDeque::from([ScriptedStreamOutcome::Chunks(chunks)]),
            ),
        ]),
    )?;
    let payload = json!({
        "model": "auto", "stream": true,
        "messages": [{"role": "user", "content": "hi"}]
    });
    let response = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-served-by")
            .and_then(|value| value.to_str().ok()),
        Some("cloud")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-failover")
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
    let _ = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("stream delivery lock poisoned"))?
            .as_slice(),
        ["local", "local", "local", "cloud"]
    );
    Ok(())
}

#[tokio::test]
async fn pinned_stream_never_falls_back_after_transport_failure() -> TestResult {
    let config = GatewayConfig {
        failover: "escalate".to_owned(),
        ..GatewayConfig::default()
    };
    let (state, seen) = streaming_live_state(
        &config,
        BTreeMap::from([
            (
                "local".to_owned(),
                VecDeque::from([ScriptedStreamOutcome::EstablishError(
                    DeliveryError::Provider(
                        wayfinder_providers::openai_compat::ProviderError::Transport,
                    ),
                )]),
            ),
            (
                "cloud".to_owned(),
                VecDeque::from([ScriptedStreamOutcome::Chunks(Vec::new())]),
            ),
        ]),
    )?;
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "local", "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }),
        &[],
    )
    .await?;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("stream delivery lock poisoned"))?
            .as_slice(),
        ["local", "local", "local"]
    );
    Ok(())
}

#[tokio::test]
async fn fatal_codex_stream_readiness_error_never_falls_back() -> TestResult {
    let config = GatewayConfig {
        failover: "escalate".to_owned(),
        ..GatewayConfig::default()
    };
    let (state, seen) = streaming_live_state(
        &config,
        BTreeMap::from([
            (
                "local".to_owned(),
                VecDeque::from([ScriptedStreamOutcome::EstablishError(DeliveryError::Codex(
                    crate::delivery::CodexDeliveryError::AuthenticationRequired,
                ))]),
            ),
            (
                "cloud".to_owned(),
                VecDeque::from([ScriptedStreamOutcome::Chunks(Vec::new())]),
            ),
        ]),
    )?;
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "auto", "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }),
        &[],
    )
    .await?;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        json_body(response).await?["error"]["type"],
        "wayfinder_router_not_ready"
    );
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("stream delivery lock poisoned"))?
            .as_slice(),
        ["local"]
    );
    Ok(())
}

#[tokio::test]
async fn streaming_upstream_http_error_is_not_mislabeled_as_200() -> TestResult {
    let (state, _) = streaming_live_state(
        &GatewayConfig::default(),
        BTreeMap::from([(
            "local".to_owned(),
            VecDeque::from([ScriptedStreamOutcome::Response(
                StatusCode::UNAUTHORIZED,
                vec![Ok(Bytes::from_static(b"{\"error\":\"bad key\"}"))],
            )]),
        )]),
    )?;
    let payload = json!({
        "model": "auto", "stream": true,
        "messages": [{"role": "user", "content": "hi"}]
    });
    let response = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        to_bytes(response.into_body(), 1024).await?,
        Bytes::from_static(b"{\"error\":\"bad key\"}")
    );
    Ok(())
}

#[tokio::test]
async fn downstream_stream_cancellation_drops_upstream_without_accounting_or_breaker_failure()
-> TestResult {
    let dropped = Arc::new(AtomicBool::new(false));
    let config = GatewayConfig {
        breaker_threshold: 1,
        ..GatewayConfig::default()
    };
    let (state, _) = streaming_live_state(
        &config,
        BTreeMap::from([(
            "local".to_owned(),
            VecDeque::from([
                ScriptedStreamOutcome::Cancellable(Arc::clone(&dropped)),
                ScriptedStreamOutcome::Cancellable(Arc::clone(&dropped)),
            ]),
        )]),
    )?;
    let payload = json!({
        "model": "auto", "stream": true,
        "messages": [{"role": "user", "content": "hi"}]
    });
    let response = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(response.status(), StatusCode::OK);
    drop(response);
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(
        json_body(get(&state, "/v1/savings").await?).await?["requests"],
        0
    );
    let second = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(second.status(), StatusCode::OK);
    drop(second);
    Ok(())
}

#[tokio::test]
async fn downstream_stream_cancellation_retains_conservative_tpm_reservation() -> TestResult {
    let dropped = Arc::new(AtomicBool::new(false));
    let mut config = GatewayConfig {
        rate_limit: Some(RateLimit {
            rpm: None,
            tpm: Some(150),
            window: 60.0,
        }),
        ..GatewayConfig::default()
    };
    config
        .keys
        .insert("team-a".to_owned(), access_key("wf-secret"));
    let (state, seen) = streaming_live_state(
        &config,
        BTreeMap::from([(
            "local".to_owned(),
            VecDeque::from([ScriptedStreamOutcome::Cancellable(Arc::clone(&dropped))]),
        )]),
    )?;
    let policy = AccessPolicy::from_gateway_config_with_clock(&config, || 1_000.0)?;
    let state = state.with_access_policy(policy);
    let payload = json!({
        "model": "auto", "stream": true, "max_tokens": 8,
        "messages": [{"role": "user", "content": "hi"}]
    });
    let headers = [("authorization", "Bearer wf-secret")];

    let response = post_json(&state, "/v1/chat/completions", &payload, &headers).await?;
    assert_eq!(response.status(), StatusCode::OK);
    drop(response);
    assert!(dropped.load(Ordering::SeqCst));

    let limited = post_json(&state, "/v1/chat/completions", &payload, &headers).await?;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        limited
            .headers()
            .get("x-wayfinder-router-rate-limit")
            .and_then(|value| value.to_str().ok()),
        Some("tpm")
    );
    assert_eq!(
        seen.lock()
            .map_err(|_| std::io::Error::other("stream delivery lock poisoned"))?
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn offline_remote_delivery_fails_closed_and_transport_errors_are_502() -> TestResult {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let remote_state = AppState::new(
        RoutingConfig::binary(0.2),
        vec![ConfiguredModel::new(
            "local",
            "https://remote.example/v1",
            "remote-small",
            None,
            true,
        )],
        true,
        "test",
    )
    .with_delivery(Arc::new(FakeDelivery {
        seen: Arc::clone(&seen),
        fail: false,
    }));
    let blocked = post_json(
        &remote_state,
        "/v1/chat/completions",
        &json!({"messages": [{"role": "user", "content": "hi"}]}),
        &[],
    )
    .await?;
    assert_eq!(blocked.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        json_body(blocked).await?["error"]["type"],
        "wayfinder_router_offline_unavailable"
    );
    assert!(
        seen.lock()
            .map_err(|_| std::io::Error::other("fake delivery lock poisoned"))?
            .is_empty()
    );

    let failing_state = AppState::new(
        RoutingConfig::binary(0.2),
        vec![ConfiguredModel::new(
            "local",
            "http://127.0.0.1:11434/v1",
            "local-small",
            None,
            true,
        )],
        false,
        "test",
    )
    .with_delivery(Arc::new(FakeDelivery { seen, fail: true }));
    let failed = post_json(
        &failing_state,
        "/v1/chat/completions",
        &json!({"messages": [{"role": "user", "content": "hi"}]}),
        &[],
    )
    .await?;
    assert_eq!(failed.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        failed
            .headers()
            .get("x-wayfinder-router-model")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    assert_eq!(
        json_body(failed).await?["error"]["type"],
        "wayfinder_router_upstream_error"
    );
    Ok(())
}

#[tokio::test]
async fn offline_buffered_pin_never_rewrites_chatgpt_account_route_to_local() -> TestResult {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let delivery = Arc::new(ScriptedDelivery {
        outcomes: Arc::new(Mutex::new(BTreeMap::from([
            (
                "local".to_owned(),
                VecDeque::from([ScriptedOutcome::Response(StatusCode::OK, EXACT_USAGE_BODY)]),
            ),
            (
                "cloud".to_owned(),
                VecDeque::from([ScriptedOutcome::Response(StatusCode::OK, EXACT_USAGE_BODY)]),
            ),
        ]))),
        seen: Arc::clone(&seen),
    });
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "local-model",
                None,
                true,
            ),
            ConfiguredModel::new("cloud", "", "gpt-5.6-sol", None, true)
                .with_provider(ProviderKind::CodexAppServer, None),
        ],
        true,
        "test",
    )
    .with_delivery(delivery);
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "cloud",
            "messages": [{"role": "user", "content": "hello"}]
        }),
        &[],
    )
    .await?;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        json_body(response).await?["error"]["type"],
        "wayfinder_router_offline_unavailable"
    );
    assert!(
        seen.lock()
            .map_err(|_| std::io::Error::other("delivery lock poisoned"))?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn offline_streaming_pin_never_rewrites_chatgpt_account_route_to_local() -> TestResult {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let chunks = vec![Ok(Bytes::from_static(b"data: [DONE]\n\n"))];
    let delivery = Arc::new(ScriptedStreamingDelivery {
        outcomes: Arc::new(Mutex::new(BTreeMap::from([
            (
                "local".to_owned(),
                VecDeque::from([ScriptedStreamOutcome::Chunks(chunks.clone())]),
            ),
            (
                "cloud".to_owned(),
                VecDeque::from([ScriptedStreamOutcome::Chunks(chunks)]),
            ),
        ]))),
        seen: Arc::clone(&seen),
    });
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "local-model",
                None,
                true,
            ),
            ConfiguredModel::new("cloud", "", "gpt-5.6-sol", None, true)
                .with_provider(ProviderKind::CodexAppServer, None),
        ],
        true,
        "test",
    )
    .with_streaming_delivery(delivery);
    let response = post_json(
        &state,
        "/v1/chat/completions",
        &json!({
            "model": "cloud",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}]
        }),
        &[],
    )
    .await?;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        json_body(response).await?["error"]["type"],
        "wayfinder_router_offline_unavailable"
    );
    assert!(
        seen.lock()
            .map_err(|_| std::io::Error::other("stream delivery lock poisoned"))?
            .is_empty()
    );
    Ok(())
}

#[test]
fn offline_locality_accepts_only_loopback_http_or_typed_apple_local() {
    let loopback = ConfiguredModel::new(
        "local-http",
        "http://127.0.0.1:11434/v1",
        "local",
        None,
        true,
    );
    let remote = ConfiguredModel::new(
        "remote-http",
        "https://api.example.test/v1",
        "remote",
        None,
        true,
    );
    let apple = ConfiguredModel::new("apple-local", "", "system-default", None, true)
        .with_provider(
            ProviderKind::AppleFoundationModels,
            Some(ProviderTier::Local),
        );
    let unproven_apple = ConfiguredModel::new("apple-unproven", "", "system-default", None, true)
        .with_provider(ProviderKind::AppleFoundationModels, None);

    assert!(model_is_proven_local(&loopback));
    assert!(!model_is_proven_local(&remote));
    assert!(model_is_proven_local(&apple));
    assert!(!model_is_proven_local(&unproven_apple));
}

fn access_key(secret: &str) -> VirtualKey {
    VirtualKey {
        hash: auth::hash_key(secret),
        tags: Vec::new(),
        workspace: None,
        budget: None,
        rate_limit: None,
        models: Vec::new(),
    }
}

#[tokio::test]
async fn managed_policy_resolves_key_then_workspace_then_default_profile() -> TestResult {
    let mut config = GatewayConfig::default();
    config
        .workspaces
        .insert("engineering".to_owned(), Workspace::default());
    config
        .keys
        .insert("default-key".to_owned(), access_key("wf-default"));

    let mut workspace_key = access_key("wf-workspace");
    workspace_key.workspace = Some("engineering".to_owned());
    config
        .keys
        .insert("workspace-key".to_owned(), workspace_key);

    let mut direct_key = access_key("wf-direct");
    direct_key.workspace = Some("engineering".to_owned());
    config.keys.insert("direct-key".to_owned(), direct_key);

    let access_policy = AccessPolicy::from_gateway_config(&config)?;
    let document = PolicyDocument::new_with_default(
        vec![
            PolicyProfile::new("default", &RoutingConfig::binary(1.0))?,
            PolicyProfile::new("workspace", &RoutingConfig::binary(0.2))?,
            PolicyProfile::new("key", &RoutingConfig::binary(1.0))?,
        ],
        vec![
            PolicyBinding {
                kind: BindingKind::Workspace,
                subject: "engineering".to_owned(),
                profile_id: "workspace".to_owned(),
            },
            PolicyBinding {
                kind: BindingKind::Key,
                subject: "direct-key".to_owned(),
                profile_id: "key".to_owned(),
            },
        ],
        Some("default".to_owned()),
    )?;
    let actor = AdministrativeIdentity::new("test", "operator")?;
    let policy = PolicyLifecycle::draft(document, actor.clone())?.validate()?;
    let base = AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new("local", "http://127.0.0.1:11434/v1", "local", None, true),
            ConfiguredModel::new("cloud", "https://cloud.example/v1", "cloud", None, true),
        ],
        false,
        "test",
    )
    .with_access_policy(access_policy)
    .with_dry_run(true);
    let lifecycle =
        PolicyLifecycle::bootstrap(&policy, |_| Ok(base), &actor, AuditLog::disabled()).await?;
    let state = lifecycle.runtime_holder().current()?;
    let long_turn = (0..450).map(|_| "word").collect::<Vec<_>>().join(" ");
    let payload = json!({"messages": [{"role": "user", "content": long_turn}]});

    for (secret, profile, model) in [
        ("wf-default", "default", "local"),
        ("wf-workspace", "workspace", "cloud"),
        ("wf-direct", "key", "local"),
    ] {
        let authorization = format!("Bearer {secret}");
        let response = post_json(
            &state,
            "/v1/chat/completions",
            &payload,
            &[("authorization", authorization.as_str())],
        )
        .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-wayfinder-policy-profile")
                .and_then(|value| value.to_str().ok()),
            Some(profile)
        );
        assert_eq!(
            response
                .headers()
                .get("x-wayfinder-router-model")
                .and_then(|value| value.to_str().ok()),
            Some(model)
        );
        assert!(
            response
                .headers()
                .get("x-wayfinder-policy-version")
                .is_some()
        );
        assert!(
            response
                .headers()
                .get("x-wayfinder-policy-snapshot")
                .is_some()
        );
    }
    Ok(())
}

#[tokio::test]
async fn global_rate_limit_precedes_virtual_key_auth() -> TestResult {
    let mut config = GatewayConfig {
        rate_limit: Some(RateLimit {
            rpm: Some(1),
            tpm: None,
            window: 60.0,
        }),
        ..GatewayConfig::default()
    };
    config
        .keys
        .insert("team-a".to_owned(), access_key("wf-secret"));
    let policy = AccessPolicy::from_gateway_config_with_clock(&config, || 1_000.0)?;
    let state = configured_state().with_access_policy(policy);
    let payload = json!({
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 1
    });

    let unauthorized = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        unauthorized
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer")
    );
    assert_eq!(
        json_body(unauthorized).await?["error"]["type"],
        "wayfinder_router_unauthorized"
    );

    let limited = post_json(&state, "/v1/chat/completions", &payload, &[]).await?;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        limited
            .headers()
            .get("x-wayfinder-router-rate-limit")
            .and_then(|value| value.to_str().ok()),
        Some("rpm")
    );
    assert!(limited.headers().get("retry-after").is_some());
    assert_eq!(
        json_body(limited).await?["error"]["type"],
        "wayfinder_router_rate_limited"
    );
    Ok(())
}

#[tokio::test]
async fn valid_key_clamps_model_reports_tight_limit_and_attributes_usage() -> TestResult {
    let mut config = GatewayConfig {
        rate_limit: Some(RateLimit {
            rpm: Some(100),
            tpm: Some(200),
            window: 60.0,
        }),
        ..GatewayConfig::default()
    };
    let mut key = access_key("wf-secret");
    key.models = vec!["cloud".to_owned()];
    key.rate_limit = Some(RateLimit {
        rpm: Some(3),
        tpm: None,
        window: 60.0,
    });
    config.keys.insert("team-a".to_owned(), key);
    let policy = AccessPolicy::from_gateway_config_with_clock(&config, || 1_000.0)?;
    let state = AppState::new(
        RoutingConfig::binary(0.5),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "upstream-local",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.0)),
            ConfiguredModel::new(
                "cloud",
                "http://127.0.0.1:11435/v1",
                "upstream-cloud",
                None,
                true,
            )
            .with_cost_per_1k(Some(0.01)),
        ],
        false,
        "test",
    )
    .with_access_policy(policy)
    .with_delivery(Arc::new(ExactUsageDelivery));
    let payload = json!({
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 1
    });
    let headers = [("authorization", "Bearer wf-secret")];

    let missing_bound = json!({"messages": [{"role": "user", "content": "hi"}]});
    let rejected = post_json(&state, "/v1/chat/completions", &missing_bound, &headers).await?;
    assert_eq!(rejected.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json_body(rejected).await?["error"]["type"],
        "wayfinder_router_token_bound_required"
    );

    let response = post_json(&state, "/v1/chat/completions", &payload, &headers).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-model")
            .and_then(|value| value.to_str().ok()),
        Some("cloud")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-mode")
            .and_then(|value| value.to_str().ok()),
        Some("key-scoped")
    );
    assert_eq!(
        response
            .headers()
            .get("x-ratelimit-limit")
            .and_then(|value| value.to_str().ok()),
        Some("3")
    );
    assert_eq!(
        response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|value| value.to_str().ok()),
        Some("1")
    );

    let savings = json_body(get(&state, "/v1/savings").await?).await?;
    assert_eq!(savings["by_key"]["team-a"]["requests"], 1);
    let recent = json_body(get(&state, "/router/recent").await?).await?;
    assert_eq!(recent["recent"][0]["key"], "team-a");
    let metrics = to_bytes(get(&state, "/metrics").await?.into_body(), usize::MAX).await?;
    assert!(
        String::from_utf8(metrics.to_vec())?
            .contains("wayfinder_router_key_requests_total{key=\"team-a\"} 2")
    );

    let tpm_limited = post_json(&state, "/v1/chat/completions", &payload, &headers).await?;
    assert_eq!(tpm_limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        tpm_limited
            .headers()
            .get("x-wayfinder-router-rate-limit")
            .and_then(|value| value.to_str().ok()),
        Some("tpm")
    );
    Ok(())
}

#[tokio::test]
async fn workspace_scopes_a_multi_turn_request_without_storing_the_conversation() -> TestResult {
    let mut config = GatewayConfig::default();
    config.workspaces.insert(
        "mobile".to_owned(),
        Workspace {
            models: vec!["local".to_owned()],
            profile: None,
            budget: None,
            rate_limit: None,
        },
    );
    let mut key = access_key("mobile-secret");
    key.workspace = Some("mobile".to_owned());
    config.keys.insert("alice".to_owned(), key);
    let policy = AccessPolicy::from_gateway_config_with_clock(&config, || 1_000.0)?;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let state = AppState::new(
        RoutingConfig::binary(0.2),
        vec![
            ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "qwen-upstream",
                None,
                true,
            ),
            ConfiguredModel::new(
                "cloud",
                "https://cloud.example/v1",
                "gpt-upstream",
                None,
                true,
            ),
        ],
        false,
        "test",
    )
    .with_sticky(true, 0)
    .with_access_policy(policy)
    .with_delivery(Arc::new(FakeDelivery {
        seen: Arc::clone(&seen),
        fail: false,
    }));
    let hard_turn = (0..450).map(|_| "word").collect::<Vec<_>>().join(" ");
    let payload = json!({
        "model": "auto",
        "messages": [
            {"role": "user", "content": hard_turn},
            {"role": "assistant", "content": "Let us work through it."},
            {"role": "user", "content": "Now summarize that in one sentence."}
        ]
    });
    let headers = [("authorization", "Bearer mobile-secret")];

    let response = post_json(&state, "/v1/chat/completions", &payload, &headers).await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response
            .headers()
            .get(ROUTER_WORKSPACE_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("mobile")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-model")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    assert_eq!(
        response
            .headers()
            .get("x-wayfinder-router-mode")
            .and_then(|value| value.to_str().ok()),
        Some("key-scoped")
    );
    let seen = seen
        .lock()
        .map_err(|_| std::io::Error::other("delivery lock poisoned"))?;
    assert_eq!(seen[0].0, "qwen-upstream");
    assert_eq!(seen[0].1["messages"], payload["messages"]);
    Ok(())
}

#[tokio::test]
async fn missing_routes_and_wrong_methods_are_structured() -> TestResult {
    let state = configured_state();
    let missing = get(&state, "/not-a-route").await?;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_body(missing).await?, json!({"detail": "Not Found"}));

    let request = Request::builder()
        .method("POST")
        .uri("/healthz")
        .body(Body::empty())?;
    let wrong_method = build_router(state).oneshot(request).await?;
    assert_eq!(wrong_method.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        wrong_method
            .headers()
            .get("allow")
            .and_then(|value| value.to_str().ok()),
        Some("GET")
    );
    assert_eq!(
        json_body(wrong_method).await?,
        json!({"detail": "Method Not Allowed"})
    );

    let wrong_chat_method = get(&configured_state(), "/chat/completions").await?;
    assert_eq!(wrong_chat_method.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        wrong_chat_method
            .headers()
            .get("allow")
            .and_then(|value| value.to_str().ok()),
        Some("POST")
    );
    Ok(())
}

#[tokio::test]
async fn reloadable_router_switches_complete_snapshots_and_retains_last_good() -> TestResult {
    let first = AppState::new(RoutingConfig::binary(0.9), Vec::new(), false, "test");
    let holder = Arc::new(crate::reload::LastGood::new(first, 1_u128));
    let router = build_reloadable_router(Arc::clone(&holder));
    let body = Body::from(r#"{"model":"auto","messages":[{"role":"user","content":"hello"}]}"#);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(body)?;
    let first_response = router.clone().oneshot(request).await?;
    assert_eq!(first_response.status(), StatusCode::OK);

    let second = AppState::new(RoutingConfig::binary(0.0), Vec::new(), true, "test");
    let outcome = holder.refresh(2, || Ok::<_, ()>(second))?;
    assert!(matches!(outcome, crate::reload::ReloadOutcome::Reloaded(_)));
    let request = Request::builder()
        .method("GET")
        .uri("/healthz")
        .body(Body::empty())?;
    let health = json_body(router.clone().oneshot(request).await?).await?;
    assert_eq!(health.get("offline").and_then(Value::as_bool), Some(true));

    let retained = holder.refresh(3, || Err::<AppState, _>("invalid config"))?;
    assert!(matches!(
        retained,
        crate::reload::ReloadOutcome::Retained { .. }
    ));
    let request = Request::builder()
        .method("GET")
        .uri("/healthz")
        .body(Body::empty())?;
    let retained_health = json_body(router.oneshot(request).await?).await?;
    assert_eq!(retained_health, health);
    Ok(())
}
