//! Local HTTP gateway and operational policy.
//!
//! This crate is the HTTP data and control plane: authentication, routing
//! decisions, budgets, rate limits, caching, reliability, and delivery to
//! configured providers. The scored decision itself stays offline and keyless;
//! credentials are resolved and providers called only for delivery.

#![forbid(unsafe_code)]

pub mod access;
pub mod admission;
pub mod audit;
pub mod auth;
pub mod budget;
pub mod cache;
pub mod canary;
pub mod codex_control;
pub mod control_plane;
pub mod decision_policy;
pub mod delivery;
pub mod deployment_selection;
pub mod evidence;
pub mod metrics;
pub mod modalities;
pub mod operator_auth;
pub mod otel;
pub mod outcome_policy;
pub mod project_value;
pub mod rate_limit;
pub mod recent;
pub mod reliability;
pub mod reload;
pub mod responses;
pub mod savings_persistence;
pub mod server;
pub mod shadow;
pub mod state;

mod chat_pipeline;
mod route_mode;

pub use crate::decision_policy::RouteOn;

use std::collections::{BTreeMap, VecDeque};
use std::convert::Infallible;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::{Body, to_bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Extension, State};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tower::ServiceExt;
use uuid::Uuid;
use wayfinder_config::dump_routing_toml;
use wayfinder_config::gateway::{
    CanaryConfig, DeploymentSelectionPolicy, GatewayModel, ProviderKind, ProviderTier, RoutePreset,
    ShadowConfig,
};
use wayfinder_providers::anthropic::{
    MessagesStreamTranslator, anthropic_error, anthropic_to_openai_request,
    openai_to_anthropic_response,
};
use wayfinder_providers::openai_compat::{DEFAULT_MAX_RESPONSE_BYTES, ProviderError};
use wayfinder_providers::reliability::{is_auth_failure, is_retryable, precheck_ok};
use wayfinder_providers::sse::{SseDecoder, SseEvent};
use wayfinder_routing_core::profiles::{LexiconProfile, profiles};
use wayfinder_routing_core::{
    CandidateAssessment, ComplexityScore, DestinationCapabilities, DestinationSnapshot,
    ExclusionReason, ExecutionBoundary, FeatureContribution, Features, InferenceSurface,
    ModalityCapabilities, PrivacyPosture, ProviderReadiness, RoutingConfig, RoutingRequest,
    RoutingRequirements, Tier, assess_destination, explain_score, python_round,
};
use wayfinder_service::pricing::{
    LedgerError, PriceTable, SavingsLedger, SavingsReport, UtcDate, estimate_tokens, price_table,
    table_version, turn_cost, usage_tokens,
};

use crate::access::{
    AccessGrant, AccessPolicy, AccessPolicyError, TokenReservation, TokenReservationDecision,
};
use crate::admission::{AdmissionError, AdmissionPermit, AdmissionPolicy};
use crate::budget::BudgetPolicy;
use crate::cache::{
    CACHE_KEY_SCHEMA_VERSION, CacheError, CacheSettings, CachedResponse, ResponseCache,
    cache_key_with_version, decode_shared_response, encode_shared_response, is_storable,
};
use crate::canary::{CANARY_COHORT_HEADER, CANARY_IDENTITY_HEADER};
use crate::chat_pipeline::{
    CacheLookup, CacheOutcome, RequestAccess, RoutingHeaderInput, apply_budget,
    build_routing_headers, cache_identity, clamp_route_to_key, delivery_candidates,
    enforce_destination_eligibility, lookup_cache, plan_delivery, resolve_delivery_target,
    resolve_request_options, score_request, select_route,
};
use crate::control_plane::{
    BindingContext, PolicyReceipt, ResolvedPolicyProfile, UNMANAGED_POLICY_ID,
    UNMANAGED_SNAPSHOT_ID, ValidatedPolicyVersion,
};
use crate::decision_policy::{
    AUTO_MODEL, PREFER_HIGH, PREFER_HIGH_ALIAS, PREFER_LOW, ROUTE_ON_HEADER,
    STICKY_COOLDOWN_HEADER, STICKY_HEADER, THRESHOLD_HEADER, TUNING_FIELD, apply_scoring_overrides,
    extract_prompt,
};
use crate::delivery::{
    BufferedDelivery, BufferedDeliveryResponse, DeliveryError, StreamingDelivery,
    endpoint_is_literal_local_network, endpoint_is_literal_loopback,
};
use crate::deployment_selection::{
    DeploymentObservation, DeploymentObservationStore, DeploymentSelection,
};
use crate::evidence::{EvidenceError, EvidenceLabel, EvidenceReport, EvidenceStore};
use crate::metrics::{DEFAULT_MAX_LABEL_SERIES, GatewayMetrics};
use crate::outcome_policy::propose_local_threshold;
use crate::project_value::{ProjectAccountingReport, ProjectValueReport};
use crate::rate_limit::{RateResult, RateSnapshot};
use crate::recent::{
    RecentCost, RecentEntry, RecentOutcome, RecentRoutes, UserOutcome, UserOutcomeError,
};
use crate::reliability::{ReliabilityAttempt, ReliabilityError, ReliabilityPolicy, sleep_retry};
use crate::reload::LastGood;
use crate::route_mode::RouteMode;
use crate::shadow::{ShadowJob, ShadowSnapshot, ShadowStore};
use crate::state::{
    SharedBudgetReservation, SharedCachePut, SharedCanaryObservation, SharedCanarySnapshot,
    SharedCostObservation, StateBackend, StateLimitConfig,
};

fn shared_state_identity(config: &wayfinder_config::gateway::StateConfig) -> [u8; 32] {
    let mut digest = Sha256::new();
    for component in [
        config.backend.as_str(),
        config.url.as_deref().unwrap_or_default(),
        config.namespace.as_str(),
    ] {
        digest.update(component.as_bytes());
        digest.update([0]);
    }
    digest.finalize().into()
}

struct CanaryContext {
    state: AppState,
    request_id: String,
    rollout_id: String,
    baseline_route: String,
    candidate_route: String,
    started: Instant,
}

struct CanaryObservationGuard {
    context: Option<CanaryContext>,
}

impl CanaryObservationGuard {
    async fn finish(&mut self, succeeded: bool, route: &str, usage: Option<AccountedUsage>) {
        let Some(context) = self.context.take() else {
            return;
        };
        record_canary_outcome(context, succeeded, route, usage).await;
    }
}

impl Drop for CanaryObservationGuard {
    fn drop(&mut self) {
        let Some(context) = self.context.take() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            record_canary_outcome(context, false, "canary-error", None).await;
        });
    }
}

fn wall_clock_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}

struct CanaryRequest<'a> {
    request_id: &'a str,
    baseline_route: &'a str,
    routing_request: &'a RoutingRequest,
    offline: bool,
    prompt_estimate: u64,
    workspace_id: Option<&'a str>,
    key_id: Option<&'a str>,
    access_grant: Option<AccessGrant>,
    headers: &'a HeaderMap,
}

async fn maybe_assign_canary(
    state: &AppState,
    request: CanaryRequest<'_>,
) -> Option<CanaryObservationGuard> {
    let config = state.canary_config();
    if !config.enabled || config.fraction <= 0.0 {
        return None;
    }
    let backend = state
        .shared_state_backend()
        .filter(|backend| backend.backend_name() == "redis")?;
    let now = wall_clock_seconds();
    let snapshot = backend
        .canary_snapshot(&config.rollout_id, config.window, now)
        .await
        .ok()?;
    if snapshot.tripped_reason.is_some() {
        return None;
    }
    let identity_header = request_header(request.headers, CANARY_IDENTITY_HEADER)
        .ok()
        .flatten();
    let cohort_header = request_header(request.headers, CANARY_COHORT_HEADER)
        .ok()
        .flatten();
    let assignment = crate::canary::assignment_identity(
        config,
        request.request_id,
        request.workspace_id,
        request.key_id,
        identity_header,
        cohort_header,
    );
    let crate::canary::Assignment::Eligible { .. } = assignment else {
        return None;
    };
    let exposure = backend
        .admit(
            &format!("canary-exposure:{}", config.rollout_id),
            StateLimitConfig {
                rpm: Some(config.max_requests),
                tpm: None,
                window_seconds: config.window,
            },
            now,
        )
        .await
        .ok()?;
    if exposure.limited_by.is_some() {
        return None;
    }
    let route = state.route_preset(&config.candidate_route)?;
    let allowed_models = state
        .access_policy()
        .and_then(|policy| {
            request
                .access_grant
                .map(|grant| policy.allowed_models(grant))
        })
        .filter(|allowed| !allowed.is_empty());
    let candidate_route = route
        .models
        .iter()
        .find(|name| {
            let Some(model) = state.models().iter().find(|model| model.name() == *name) else {
                return false;
            };
            assess_model(request.routing_request, model).is_eligible()
                && (!request.offline || model_is_proven_local(model))
                && precheck_ok(request.prompt_estimate, model.context_window())
                && has_eligible_deployment_target(
                    state,
                    name,
                    request.routing_request,
                    request.offline,
                    request.prompt_estimate,
                )
                && allowed_models
                    .as_ref()
                    .is_none_or(|allowed| allowed.iter().any(|allowed| allowed == *name))
        })?
        .clone();
    Some(CanaryObservationGuard {
        context: Some(CanaryContext {
            state: state.clone(),
            request_id: request.request_id.to_owned(),
            rollout_id: config.rollout_id.clone(),
            baseline_route: request.baseline_route.to_owned(),
            candidate_route,
            started: Instant::now(),
        }),
    })
}

async fn record_canary_outcome(
    context: CanaryContext,
    succeeded: bool,
    route: &str,
    usage: Option<AccountedUsage>,
) {
    let Some(backend) = context
        .state
        .shared_state_backend()
        .filter(|backend| backend.backend_name() == "redis")
    else {
        return;
    };
    let cost = usage.and_then(|usage| {
        turn_cost(
            route,
            i64::try_from(usage.prompt_tokens).ok()?,
            i64::try_from(usage.completion_tokens).ok()?,
            &context.state.price_table().costs,
            usage.estimated,
            None,
        )
        .ok()
    });
    let baseline_cost = usage.and_then(|usage| {
        turn_cost(
            &context.baseline_route,
            i64::try_from(usage.prompt_tokens).ok()?,
            i64::try_from(usage.completion_tokens).ok()?,
            &context.state.price_table().costs,
            usage.estimated,
            None,
        )
        .ok()
    });
    let observation = SharedCanaryObservation {
        rollout_id: context.rollout_id.clone(),
        request_id: context.request_id,
        succeeded,
        latency_ms: u64::try_from(context.started.elapsed().as_millis()).unwrap_or(u64::MAX),
        realized_cost: cost.map(|cost| cost.realized),
        baseline_cost: baseline_cost.map(|cost| cost.realized),
        quality_loss: None,
    };
    let now = wall_clock_seconds();
    let _ = backend
        .record_canary(observation, context.state.canary_config().window, now)
        .await;
    let Ok(snapshot) = backend
        .canary_snapshot(
            &context.rollout_id,
            context.state.canary_config().window,
            now,
        )
        .await
    else {
        return;
    };
    if let Some(tripwire) = crate::canary::tripwire(context.state.canary_config(), snapshot) {
        let reason = tripwire.as_str();
        if backend
            .trip_canary(
                &context.rollout_id,
                reason,
                context.state.canary_config().rollback_hold,
                now,
            )
            .await
            .is_ok()
        {
            let _ = context
                .state
                .record_audit(
                    "canary",
                    "canary.rollback",
                    None,
                    Some(json!({
                        "rollout_id": context.rollout_id,
                        "reason": reason,
                        "candidate_route": context.candidate_route,
                    })),
                )
                .await;
        }
    }
}

fn cache_generation(
    config: &wayfinder_config::gateway::GatewayConfig,
) -> Result<String, CacheError> {
    let rendered = wayfinder_config::gateway::dump_gateway_toml(config)
        .map_err(|error| CacheError::Json(error.to_string()))?;
    let mut digest = Sha256::new();
    digest.update(CACHE_KEY_SCHEMA_VERSION.as_bytes());
    digest.update([0]);
    digest.update(rendered.as_bytes());
    let digest = digest.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        encoded.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    Ok(encoded)
}

/// Default maximum request body accepted by body-buffering extractors (1 MiB).
pub const DEFAULT_REQUEST_BODY_LIMIT: usize = 1024 * 1024;

/// Maximum lifetime of one fleet-wide delivery lease. A dropped process can
/// therefore recover its slot without requiring a restart or an operator
/// cleanup, while a normal response releases it immediately.
const SHARED_ADMISSION_TTL_MS: u64 = 6 * 60 * 60 * 1_000;

/// Invalid state for a network-exposed, data-plane-only listener.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum DataPlaneError {
    /// External traffic must never reach an unauthenticated gateway.
    #[error("managed data-plane serving requires at least one configured virtual key")]
    AuthenticationRequired,
    /// A delivery listener without a configured destination cannot serve work.
    #[error("managed data-plane serving requires at least one configured model")]
    ModelRequired,
}

const OFFLINE_HEADER: &str = "x-wayfinder-offline";
const ROUTER_MODEL_HEADER: &str = "x-wayfinder-router-model";
const ROUTER_SCORE_HEADER: &str = "x-wayfinder-router-score";
const ROUTER_MODE_HEADER: &str = "x-wayfinder-router-mode";
const ROUTER_REQUEST_ID_HEADER: &str = "x-wayfinder-router-request-id";
const ROUTER_POLICY_VERSION_HEADER: &str = "x-wayfinder-policy-version";
const ROUTER_SNAPSHOT_ID_HEADER: &str = "x-wayfinder-policy-snapshot";
const ROUTER_POLICY_PROFILE_HEADER: &str = "x-wayfinder-policy-profile";
const ROUTER_WORKSPACE_HEADER: &str = "x-wayfinder-router-workspace";
const ROUTER_OFFLINE_HEADER: &str = "x-wayfinder-router-offline";
const ROUTER_DECISION_ONLY_HEADER: &str = "x-wayfinder-router-decision-only";
const ROUTER_RATE_LIMIT_HEADER: &str = "x-wayfinder-router-rate-limit";
const ROUTER_BUDGET_HEADER: &str = "x-wayfinder-router-budget";
const FAILOVER_HEADER: &str = "x-wayfinder-failover";
const ROUTER_FAILOVER_HEADER: &str = "x-wayfinder-router-failover";
const RATE_LIMIT_LIMIT_HEADER: &str = "x-ratelimit-limit";
const RATE_LIMIT_REMAINING_HEADER: &str = "x-ratelimit-remaining";
const RATE_LIMIT_RESET_HEADER: &str = "x-ratelimit-reset";
const ROUTER_CACHE_HEADER: &str = "x-wayfinder-router-cache";
const ROUTER_OVERLOAD_HEADER: &str = "x-wayfinder-router-overload";
const ROUTER_ROUTE_HEADER: &str = "x-wayfinder-router-route";
const ROUTER_DEPLOYMENT_HEADER: &str = "x-wayfinder-router-deployment";
const ROUTER_DEPLOYMENT_SELECTION_HEADER: &str = "x-wayfinder-router-deployment-selection";
const PRIVACY_POSTURE_HEADER: &str = "x-wayfinder-privacy-posture";
const ROUTER_PRIVACY_POSTURE_HEADER: &str = "x-wayfinder-router-privacy-posture";
const DEBUG_HEADER: &str = "x-wayfinder-debug";
const ROUTE_PRESET_PREFIX: &str = "@route/";
const CHAT_CONTROL_HEADERS: [&str; 8] = [
    OFFLINE_HEADER,
    FAILOVER_HEADER,
    PRIVACY_POSTURE_HEADER,
    DEBUG_HEADER,
    THRESHOLD_HEADER,
    ROUTE_ON_HEADER,
    STICKY_HEADER,
    STICKY_COOLDOWN_HEADER,
];

type MonotonicClock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// Public, non-secret metadata for one configured provider endpoint.
///
/// `api_key_env` is only the environment-variable name. `key_ready` is a
/// caller-resolved presence boolean. This type has no field capable of carrying
/// the credential value itself.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfiguredModel {
    name: String,
    endpoint: String,
    provider_model: String,
    provider: ProviderKind,
    tier: Option<ProviderTier>,
    api_key_env: Option<String>,
    key_ready: bool,
    base_key_ready: bool,
    cost_per_1k: Option<f64>,
    fallbacks: Arc<[String]>,
    context_window: Option<u64>,
    deployments: Arc<[ConfiguredDeployment]>,
    deployment_selection: DeploymentSelectionPolicy,
    deployment_id: Option<String>,
}

/// One concrete delivery target behind a public model alias.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfiguredDeployment {
    id: String,
    endpoint: String,
    provider_model: String,
    api_key_env: Option<String>,
    key_ready: bool,
    weight: u64,
    cost_per_1k: Option<f64>,
}

impl ConfiguredDeployment {
    /// Build a pool member from non-secret startup metadata.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        endpoint: impl Into<String>,
        provider_model: impl Into<String>,
        api_key_env: Option<String>,
        key_ready: bool,
        weight: u64,
    ) -> Self {
        let key_ready = api_key_env.is_none() || key_ready;
        Self {
            id: id.into(),
            endpoint: endpoint.into(),
            provider_model: provider_model.into(),
            api_key_env,
            key_ready,
            weight: weight.max(1),
            cost_per_1k: None,
        }
    }

    /// Attach an optional per-1k-token cost override for this deployment.
    #[must_use]
    pub fn with_cost_per_1k(mut self, cost_per_1k: Option<f64>) -> Self {
        self.cost_per_1k = cost_per_1k.filter(|cost| cost.is_finite() && *cost >= 0.0);
        self
    }
}

impl ConfiguredModel {
    /// Construct endpoint metadata from non-secret values.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        endpoint: impl Into<String>,
        provider_model: impl Into<String>,
        api_key_env: Option<String>,
        key_ready: bool,
    ) -> Self {
        let base_key_ready = api_key_env.is_none() || key_ready;
        Self {
            name: name.into(),
            endpoint: endpoint.into(),
            provider_model: provider_model.into(),
            provider: ProviderKind::OpenAiCompatible,
            tier: None,
            key_ready: base_key_ready,
            base_key_ready,
            api_key_env,
            cost_per_1k: None,
            fallbacks: Arc::from([]),
            context_window: None,
            deployments: Arc::from([]),
            deployment_selection: DeploymentSelectionPolicy::default(),
            deployment_id: None,
        }
    }

    /// Attach safe informational cost metadata used only in decision explanations.
    #[must_use]
    pub fn with_cost_per_1k(mut self, cost_per_1k: Option<f64>) -> Self {
        self.cost_per_1k = cost_per_1k.filter(|cost| cost.is_finite() && *cost >= 0.0);
        self
    }

    /// Attach configured same-tier delivery fallbacks.
    #[must_use]
    pub fn with_fallbacks(mut self, fallbacks: Vec<String>) -> Self {
        self.fallbacks = Arc::from(fallbacks);
        self
    }

    /// Attach the optional deterministic prompt-size precheck bound.
    #[must_use]
    pub const fn with_context_window(mut self, context_window: Option<u64>) -> Self {
        self.context_window = context_window;
        self
    }

    /// Attach parsed provider identity without selecting a delivery implementation.
    #[must_use]
    pub const fn with_provider(
        mut self,
        provider: ProviderKind,
        tier: Option<ProviderTier>,
    ) -> Self {
        self.provider = provider;
        self.tier = tier;
        self
    }

    /// Copy the safe display metadata from parsed gateway configuration.
    ///
    /// Key readiness is supplied by the caller so this crate never reads the
    /// process environment inside a request handler.
    #[must_use]
    pub fn from_gateway_model(
        name: impl Into<String>,
        model: &GatewayModel,
        key_ready: bool,
    ) -> Self {
        Self::new(
            name,
            model.base_url.clone().unwrap_or_default(),
            model.model.clone(),
            model.api_key_env.clone(),
            key_ready,
        )
        .with_cost_per_1k(model.cost_per_1k)
        .with_fallbacks(model.fallbacks.clone())
        .with_context_window(model.context_window)
        .with_provider(model.provider, model.tier)
        .with_deployment_selection(model.deployment_selection.clone())
    }

    /// Attach additional weighted deployments behind this public alias.
    #[must_use]
    pub fn with_deployments(mut self, deployments: Vec<ConfiguredDeployment>) -> Self {
        self.key_ready =
            self.key_ready || deployments.iter().any(|deployment| deployment.key_ready);
        self.deployments = Arc::from(deployments);
        self
    }

    /// Attach the bounded policy used for interchangeable pool members.
    #[must_use]
    pub fn with_deployment_selection(mut self, policy: DeploymentSelectionPolicy) -> Self {
        self.deployment_selection = policy;
        self
    }

    fn for_deployment(&self, deployment: &ConfiguredDeployment) -> Self {
        let mut selected = self.clone();
        selected.endpoint = deployment.endpoint.clone();
        selected.provider_model = deployment.provider_model.clone();
        selected.api_key_env = deployment.api_key_env.clone();
        selected.key_ready = deployment.key_ready;
        selected.cost_per_1k = deployment.cost_per_1k.or(self.cost_per_1k);
        selected.deployments = Arc::from([]);
        selected.deployment_id = Some(deployment.id.clone());
        selected
    }

    fn deployment_targets(&self) -> Vec<ConfiguredDeployment> {
        let mut targets = Vec::with_capacity(self.deployments.len() + 1);
        targets.push(ConfiguredDeployment::new(
            "default",
            self.endpoint.clone(),
            self.provider_model.clone(),
            self.api_key_env.clone(),
            self.base_key_ready,
            1,
        ));
        targets.extend(self.deployments.iter().cloned());
        targets
    }

    /// Stable internal identity used for per-deployment health and metrics.
    #[must_use]
    pub fn delivery_id(&self) -> String {
        self.deployment_id.as_ref().map_or_else(
            || self.name.clone(),
            |deployment| format!("{}#{deployment}", self.name),
        )
    }

    /// Configured routing name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// OpenAI-compatible endpoint base URL.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Provider model identifier.
    #[must_use]
    pub fn provider_model(&self) -> &str {
        &self.provider_model
    }

    /// Typed provider delivery identity.
    #[must_use]
    pub const fn provider(&self) -> ProviderKind {
        self.provider
    }

    /// Explicit native-provider locality.
    #[must_use]
    pub const fn tier(&self) -> Option<ProviderTier> {
        self.tier
    }

    /// Name of the key environment variable, never its value.
    #[must_use]
    pub fn api_key_env(&self) -> Option<&str> {
        self.api_key_env.as_deref()
    }

    /// Whether the separately resolved credential is present.
    #[must_use]
    pub const fn key_ready(&self) -> bool {
        self.key_ready
    }

    /// Optional informational price per one thousand words/tokens.
    #[must_use]
    pub const fn cost_per_1k(&self) -> Option<f64> {
        self.cost_per_1k
    }

    /// Bounded policy used to order interchangeable pool members.
    #[must_use]
    pub const fn deployment_selection(&self) -> &DeploymentSelectionPolicy {
        &self.deployment_selection
    }

    /// Ordered same-tier configured fallback names.
    #[must_use]
    pub fn fallbacks(&self) -> &[String] {
        &self.fallbacks
    }

    /// Optional maximum prompt/context size used for pre-call filtering.
    #[must_use]
    pub const fn context_window(&self) -> Option<u64> {
        self.context_window
    }
}

fn configured_price_table(
    routing: &RoutingConfig,
    models: &[ConfiguredModel],
) -> (PriceTable, String) {
    let model_costs = models
        .iter()
        .map(|model| (model.name().to_owned(), model.cost_per_1k()))
        .collect::<IndexMap<_, _>>();
    let tier_ladder = if routing.classifier.is_none() && !routing.tiers.is_empty() {
        routing
            .tiers
            .iter()
            .map(|tier| tier.model.clone())
            .collect::<Vec<_>>()
    } else {
        models.iter().map(|model| model.name().to_owned()).collect()
    };
    let table = price_table(&model_costs, &tier_ladder).unwrap_or_else(|_| PriceTable {
        costs: IndexMap::new(),
        priced: false,
    });
    let version = table_version(&table.costs).unwrap_or_else(|_| "44136fa355b3".to_owned());
    (table, version)
}

/// Clone-cheap, immutable inputs for the gateway router.
///
/// Configured models retain the caller's insertion order. Clones share the
/// routing configuration, model metadata, and version through reference-counted
/// immutable storage.
#[derive(Clone)]
pub struct AppState {
    routing: Arc<RoutingConfig>,
    models: Arc<[ConfiguredModel]>,
    route_presets: Arc<IndexMap<String, RoutePreset>>,
    deployment_cursors: Arc<Mutex<BTreeMap<String, u64>>>,
    deployment_signals: Arc<DeploymentObservationStore>,
    shadow_config: Arc<ShadowConfig>,
    shadow_store: Arc<ShadowStore>,
    shadow_slots: Arc<tokio::sync::Semaphore>,
    evidence_labels: Arc<EvidenceStore>,
    canary_config: Arc<CanaryConfig>,
    offline: bool,
    dry_run: bool,
    route_on: RouteOn,
    sticky: bool,
    sticky_cooldown: u64,
    slash_directives: bool,
    build_version: Arc<str>,
    policy_version: Arc<str>,
    policy_snapshot_id: Arc<str>,
    policy: Option<Arc<ValidatedPolicyVersion>>,
    request_body_limit: usize,
    admission_policy: Arc<AdmissionPolicy>,
    recent: Arc<RecentRoutes>,
    metrics: Arc<GatewayMetrics>,
    delivery: Option<Arc<dyn BufferedDelivery>>,
    streaming_delivery: Option<Arc<dyn StreamingDelivery>>,
    access_policy: Option<Arc<AccessPolicy>>,
    shared_state_identity: Option<[u8; 32]>,
    shared_state_backend: Option<Arc<dyn state::StateBackend>>,
    budget_policy: Arc<BudgetPolicy>,
    reliability_policy: Arc<ReliabilityPolicy>,
    response_cache: Arc<ResponseCache>,
    cache_settings: CacheSettings,
    shared_cache_enabled: bool,
    cache_generation: Arc<str>,
    cache_clock: MonotonicClock,
    price_table: Arc<PriceTable>,
    price_table_version: Arc<str>,
    savings_ledger: Arc<SavingsLedger>,
    savings_path: Option<Arc<std::path::PathBuf>>,
    savings_persistence: Arc<savings_persistence::SavingsPersistence>,
    codex_control: Option<Arc<dyn codex_control::CodexControl>>,
    operator_auth: Option<Arc<operator_auth::OperatorAuthenticator>>,
    audit_log: Arc<audit::AuditLog>,
}

impl AppState {
    /// Construct state with live mode and the default request-body limit.
    #[must_use]
    pub fn new(
        routing: RoutingConfig,
        models: Vec<ConfiguredModel>,
        offline: bool,
        build_version: impl Into<String>,
    ) -> Self {
        let (price_table, price_table_version) = configured_price_table(&routing, &models);
        // Python starts a fresh ledger in priced/USD mode, then updates the
        // mode immediately before each successfully served turn.
        let savings_ledger = Arc::new(SavingsLedger::default());
        let build_version = Arc::<str>::from(build_version.into());
        let cache_started = Instant::now();
        let deployment_started = Instant::now();
        let shadow_started = Instant::now();
        let metrics = Arc::new(GatewayMetrics::new(
            build_version.to_string(),
            DEFAULT_MAX_LABEL_SERIES,
        ));
        let model_costs = models
            .iter()
            .filter_map(|model| {
                model
                    .cost_per_1k()
                    .map(|cost| (model.name().to_owned(), cost))
            })
            .collect();
        let _ = metrics.set_model_costs(&model_costs);
        Self {
            routing: Arc::new(routing),
            models: Arc::from(models),
            route_presets: Arc::new(IndexMap::new()),
            deployment_cursors: Arc::new(Mutex::new(BTreeMap::new())),
            deployment_signals: Arc::new(DeploymentObservationStore::new(move || {
                deployment_started.elapsed().as_secs_f64()
            })),
            shadow_config: Arc::new(ShadowConfig::default()),
            shadow_store: Arc::new(ShadowStore::new(
                wayfinder_config::gateway::DEFAULT_SHADOW_MAX_RECORDS,
                move || shadow_started.elapsed().as_secs_f64(),
            )),
            shadow_slots: Arc::new(tokio::sync::Semaphore::new(
                usize::try_from(wayfinder_config::gateway::DEFAULT_SHADOW_MAX_IN_FLIGHT)
                    .unwrap_or(1),
            )),
            evidence_labels: Arc::new(EvidenceStore::default()),
            canary_config: Arc::new(CanaryConfig::default()),
            offline,
            dry_run: false,
            route_on: RouteOn::Turn,
            sticky: false,
            sticky_cooldown: 0,
            slash_directives: false,
            build_version,
            policy_version: Arc::from(UNMANAGED_POLICY_ID),
            policy_snapshot_id: Arc::from(UNMANAGED_SNAPSHOT_ID),
            policy: None,
            request_body_limit: DEFAULT_REQUEST_BODY_LIMIT,
            admission_policy: Arc::new(AdmissionPolicy::default()),
            recent: Arc::new(RecentRoutes::default()),
            metrics,
            delivery: None,
            streaming_delivery: None,
            access_policy: None,
            shared_state_identity: None,
            shared_state_backend: None,
            budget_policy: Arc::new(BudgetPolicy::default()),
            reliability_policy: Arc::new(ReliabilityPolicy::default()),
            response_cache: Arc::new(ResponseCache::default()),
            cache_settings: CacheSettings::default(),
            shared_cache_enabled: false,
            cache_generation: Arc::from(CACHE_KEY_SCHEMA_VERSION),
            cache_clock: Arc::new(move || cache_started.elapsed().as_secs_f64()),
            price_table: Arc::new(price_table),
            price_table_version: Arc::from(price_table_version),
            savings_ledger,
            savings_path: None,
            savings_persistence: Arc::default(),
            codex_control: None,
            operator_auth: None,
            audit_log: Arc::new(audit::AuditLog::disabled()),
        }
    }

    /// Set whether the process is explicitly running in decision-only mode.
    #[must_use]
    pub const fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Set the default transcript scope used when no request override is present.
    #[must_use]
    pub const fn with_route_on(mut self, route_on: RouteOn) -> Self {
        self.route_on = route_on;
        self
    }

    /// Set conversation high-water routing and its optional calm-turn decay.
    #[must_use]
    pub const fn with_sticky(mut self, sticky: bool, cooldown: u64) -> Self {
        self.sticky = sticky;
        self.sticky_cooldown = cooldown;
        self
    }

    /// Set whether recognized leading slash directives are enabled.
    #[must_use]
    pub const fn with_slash_directives(mut self, enabled: bool) -> Self {
        self.slash_directives = enabled;
        self
    }

    /// Stamp one immutable control-plane activation on this runtime snapshot.
    #[must_use]
    pub fn with_policy_receipt(mut self, receipt: &PolicyReceipt) -> Self {
        self.policy_version = Arc::from(receipt.policy_version.clone());
        self.policy_snapshot_id = Arc::from(receipt.snapshot_id.clone());
        self
    }

    /// Attach one semantically validated policy to this immutable snapshot.
    #[must_use]
    pub fn with_validated_policy(mut self, policy: ValidatedPolicyVersion) -> Self {
        self.policy = Some(Arc::new(policy));
        self
    }

    /// Attach validated named routing presets without exposing credentials.
    #[must_use]
    pub fn with_route_presets(mut self, presets: IndexMap<String, RoutePreset>) -> Self {
        self.route_presets = Arc::new(presets);
        self
    }

    /// Set the maximum request bytes made available to buffering extractors.
    #[must_use]
    pub const fn with_request_body_limit(mut self, request_body_limit: usize) -> Self {
        self.request_body_limit = request_body_limit;
        self
    }

    /// Attach process-local upstream delivery admission bounds.
    pub fn with_gateway_admission(
        mut self,
        config: &wayfinder_config::gateway::GatewayConfig,
    ) -> Result<Self, AdmissionError> {
        let max_in_flight = usize::try_from(config.concurrency.max_in_flight)
            .map_err(|_| AdmissionError::InvalidInFlightLimit)?;
        let max_queued = usize::try_from(config.concurrency.max_queued)
            .map_err(|_| AdmissionError::InvalidQueueLimit)?;
        let queue_timeout = Duration::try_from_secs_f64(config.concurrency.queue_timeout)
            .map_err(|_| AdmissionError::InvalidQueueTimeout)?;
        self.admission_policy = Arc::new(AdmissionPolicy::new(
            max_in_flight,
            max_queued,
            queue_timeout,
        )?);
        Ok(self)
    }

    /// Attach caller-owned admission state for deterministic tests.
    #[must_use]
    pub fn with_admission_policy(mut self, policy: AdmissionPolicy) -> Self {
        self.admission_policy = Arc::new(policy);
        self
    }

    /// Use a caller-owned recent-route ring, for lifecycle sharing and tests.
    #[must_use]
    pub fn with_recent(mut self, recent: Arc<RecentRoutes>) -> Self {
        self.recent = recent;
        self
    }

    /// Use a caller-owned metrics collector, for lifecycle sharing and tests.
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<GatewayMetrics>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Attach the selected provider-delivery implementation.
    #[must_use]
    pub fn with_delivery(mut self, delivery: Arc<dyn BufferedDelivery>) -> Self {
        self.delivery = Some(delivery);
        self
    }

    /// Attach a streaming-only provider implementation.
    #[must_use]
    pub fn with_streaming_delivery(mut self, delivery: Arc<dyn StreamingDelivery>) -> Self {
        self.streaming_delivery = Some(delivery);
        self
    }

    /// Attach local Codex account control only for an explicitly loopback listener.
    ///
    /// Passing `false` deliberately discards the control, which keeps every
    /// account route absent rather than merely rejecting requests at runtime.
    #[must_use]
    pub fn with_codex_control(
        mut self,
        control: Arc<dyn codex_control::CodexControl>,
        listener_is_literal_loopback: bool,
    ) -> Self {
        self.codex_control = listener_is_literal_loopback.then_some(control);
        self
    }

    /// Attach one provider implementation to both buffered and streaming paths.
    #[must_use]
    pub fn with_provider_delivery<D>(mut self, delivery: Arc<D>) -> Self
    where
        D: BufferedDelivery + StreamingDelivery + 'static,
    {
        self.delivery = Some(delivery.clone());
        self.streaming_delivery = Some(delivery);
        self
    }

    /// Attach shared opt-in authentication and rate-limit state.
    #[must_use]
    pub fn with_access_policy(mut self, policy: AccessPolicy) -> Self {
        self.access_policy = policy.active().then(|| Arc::new(policy));
        self
    }

    /// Build and attach access policy from validated gateway configuration.
    pub fn with_gateway_access(
        self,
        config: &wayfinder_config::gateway::GatewayConfig,
    ) -> Result<Self, AccessPolicyError> {
        Ok(self.with_access_policy(AccessPolicy::from_gateway_config(config)?))
    }

    /// Build access policy with the configured memory or Redis state backend.
    pub async fn with_gateway_shared_access(
        mut self,
        config: &wayfinder_config::gateway::GatewayConfig,
    ) -> Result<Self, AccessPolicyError> {
        self.shared_state_identity = Some(shared_state_identity(&config.state));
        let backend = state::from_config(&config.state)
            .await
            .map_err(AccessPolicyError::from)?;
        let started = Instant::now();
        let policy = AccessPolicy::from_gateway_config_with_backend(
            config,
            move || started.elapsed().as_secs_f64(),
            Arc::clone(&backend),
        )?;
        let mut state = self.with_access_policy(policy);
        state.shared_state_backend = Some(Arc::clone(&backend));
        if config.state.backend == "redis" {
            let audit_log = state
                .audit_log()
                .clone()
                .with_shared_backend(backend, config.state.namespace.clone())
                .map_err(|_| {
                    AccessPolicyError::StateBackend(state::StateBackendError::Unavailable)
                })?;
            state = state.with_audit_log(audit_log);
        }
        Ok(state)
    }

    /// Whether a replacement snapshot uses the same process-lifetime shared
    /// state destination. Endpoint, credential, namespace, and backend changes
    /// require restart so the access and audit authorities cannot diverge.
    #[must_use]
    pub fn shared_state_matches(&self, replacement: &Self) -> bool {
        self.shared_state_identity == replacement.shared_state_identity
    }

    /// Attach the separately authenticated operator surface when configured.
    pub fn with_gateway_operator_auth(
        mut self,
        config: &wayfinder_config::gateway::GatewayConfig,
    ) -> Result<Self, operator_auth::OperatorAuthConfigError> {
        self.operator_auth =
            operator_auth::OperatorAuthenticator::from_config(&config.auth)?.map(Arc::new);
        Ok(self)
    }

    /// Attach an append-only, prompt-free operator audit destination.
    #[must_use]
    pub fn with_audit_log(mut self, audit_log: audit::AuditLog) -> Self {
        self.audit_log = Arc::new(audit_log);
        self
    }

    /// Attach gateway-wide and per-key spend caps from validated configuration.
    #[must_use]
    pub fn with_gateway_budget(
        mut self,
        config: &wayfinder_config::gateway::GatewayConfig,
    ) -> Self {
        self.budget_policy = Arc::new(BudgetPolicy::from_gateway_config(config));
        self
    }

    /// Attach retry, failover, and circuit-breaker state from validated configuration.
    pub fn with_gateway_reliability(
        mut self,
        config: &wayfinder_config::gateway::GatewayConfig,
    ) -> Result<Self, ReliabilityError> {
        self.reliability_policy = Arc::new(ReliabilityPolicy::from_gateway_config(config)?);
        Ok(self)
    }

    /// Attach reliability state with deterministic time and jitter sources.
    pub fn with_gateway_reliability_sources(
        mut self,
        config: &wayfinder_config::gateway::GatewayConfig,
        clock: impl Fn() -> f64 + Send + Sync + 'static,
        jitter: impl Fn() -> f64 + Send + Sync + 'static,
    ) -> Result<Self, ReliabilityError> {
        self.reliability_policy = Arc::new(ReliabilityPolicy::from_gateway_config_with_sources(
            config, clock, jitter,
        )?);
        Ok(self)
    }

    /// Attach bounded, opt-in off-path shadow routing from gateway config.
    pub fn with_gateway_shadow(
        mut self,
        config: &wayfinder_config::gateway::GatewayConfig,
    ) -> Result<Self, String> {
        let shadow = config.shadow.clone();
        let slots = usize::try_from(shadow.max_in_flight)
            .map_err(|_| "gateway.shadow.max_in_flight is too large".to_owned())?;
        if slots == 0 {
            return Err("gateway.shadow.max_in_flight must be positive".to_owned());
        }
        let started = Instant::now();
        self.shadow_store = Arc::new(ShadowStore::new(shadow.max_records, move || {
            started.elapsed().as_secs_f64()
        }));
        self.shadow_store.set_active(shadow.enabled);
        self.shadow_config = Arc::new(shadow);
        self.shadow_slots = Arc::new(tokio::sync::Semaphore::new(slots));
        Ok(self)
    }

    /// Replace shadow policy while retaining an existing record store.
    pub fn with_shadow_config(mut self, config: ShadowConfig) -> Result<Self, String> {
        let slots = usize::try_from(config.max_in_flight)
            .map_err(|_| "gateway.shadow.max_in_flight is too large".to_owned())?;
        if slots == 0 {
            return Err("gateway.shadow.max_in_flight must be positive".to_owned());
        }
        self.shadow_config = Arc::new(config);
        self.shadow_store.set_active(self.shadow_config.enabled);
        self.shadow_slots = Arc::new(tokio::sync::Semaphore::new(slots));
        Ok(self)
    }

    /// Attach a bounded production canary. Enabled rollouts require Redis so
    /// exposure, telemetry, and rollback are shared by every replica.
    pub fn with_gateway_canary(
        mut self,
        config: &wayfinder_config::gateway::GatewayConfig,
    ) -> Result<Self, String> {
        let canary = config.canary.clone();
        if canary.enabled {
            if self
                .shared_state_backend
                .as_ref()
                .is_none_or(|backend| backend.backend_name() != "redis")
            {
                return Err(
                    "gateway.canary.enabled requires gateway.state.backend = redis".to_owned(),
                );
            }
            if canary.fraction <= 0.0 || canary.candidate_route.is_empty() {
                return Err(
                    "gateway.canary.enabled requires a positive fraction and candidate route"
                        .to_owned(),
                );
            }
        }
        self.canary_config = Arc::new(canary);
        Ok(self)
    }

    /// Attach an opt-in bounded exact-response cache and monotonic clock.
    pub fn with_cache_and_clock(
        mut self,
        settings: CacheSettings,
        clock: impl Fn() -> f64 + Send + Sync + 'static,
    ) -> Result<Self, CacheError> {
        self.response_cache = Arc::new(ResponseCache::new(settings)?);
        self.cache_settings = settings;
        self.shared_cache_enabled = false;
        self.cache_clock = Arc::new(clock);
        Ok(self)
    }

    /// Attach cache settings with a process-local monotonic clock.
    pub fn with_cache(self, settings: CacheSettings) -> Result<Self, CacheError> {
        let started = Instant::now();
        self.with_cache_and_clock(settings, move || started.elapsed().as_secs_f64())
    }

    /// Build cache settings from validated gateway configuration.
    pub fn with_gateway_cache(
        mut self,
        config: &wayfinder_config::gateway::GatewayConfig,
    ) -> Result<Self, CacheError> {
        self.cache_generation = Arc::from(cache_generation(config)?);
        let Some(config) = &config.cache else {
            self.shared_cache_enabled = false;
            return self.with_cache(CacheSettings::default());
        };
        if config.backend == "redis" {
            if self
                .shared_state_backend
                .as_ref()
                .is_none_or(|backend| backend.backend_name() != "redis")
            {
                return Err(CacheError::SharedBackendRequired);
            }
            let settings = CacheSettings {
                enabled: config.enabled,
                ttl_seconds: config.ttl,
                max_entries: usize::try_from(config.max_entries)
                    .map_err(|_| CacheError::InvalidBounds)?,
                max_bytes: usize::try_from(config.max_bytes)
                    .map_err(|_| CacheError::InvalidBounds)?,
            };
            self.cache_settings = settings;
            self.shared_cache_enabled = config.enabled;
            self.response_cache = Arc::new(ResponseCache::default());
            return Ok(self);
        }
        self.shared_cache_enabled = false;
        self.with_cache(CacheSettings {
            enabled: config.enabled,
            ttl_seconds: config.ttl,
            max_entries: usize::try_from(config.max_entries)
                .map_err(|_| CacheError::InvalidBounds)?,
            max_bytes: usize::try_from(config.max_bytes).map_err(|_| CacheError::InvalidBounds)?,
        })
    }

    /// Build cache state and purge the shared namespace when shared retention
    /// is disabled. Shared cache failures remain a degraded cache miss rather
    /// than blocking routing or provider delivery.
    pub async fn with_gateway_cache_async(
        self,
        config: &wayfinder_config::gateway::GatewayConfig,
    ) -> Result<Self, CacheError> {
        let state = self.with_gateway_cache(config)?;
        let shared_backend = state
            .shared_state_backend
            .as_ref()
            .filter(|backend| backend.backend_name() == "redis");
        let shared_enabled = config
            .cache
            .as_ref()
            .is_some_and(|cache| cache.backend == "redis" && cache.enabled);
        if let Some(backend) = shared_backend {
            if !shared_enabled {
                let _ = backend.shared_cache_clear(&state.cache_generation).await;
            }
        }
        Ok(state)
    }

    /// Use a caller-owned in-memory savings ledger.
    ///
    /// This supports lifecycle sharing and deterministic tests. Callers should
    /// construct the ledger with the same `priced` mode as the configured model
    /// price table exposed by this state.
    #[must_use]
    pub fn with_savings_ledger(mut self, ledger: Arc<SavingsLedger>) -> Self {
        self.savings_ledger = ledger;
        // A rebound ledger never shares a writer bound to the previous one.
        self.savings_persistence = Arc::default();
        self
    }

    /// Persist realized accounting to this path.
    ///
    /// Completed turns only mark the ledger dirty. Writes happen in the
    /// background once [`Self::start_savings_persister`] has been called (the
    /// `serve` entry point does this), at most once per
    /// [`savings_persistence::SAVINGS_PERSIST_INTERVAL`], with a final write
    /// from [`Self::shutdown_savings_persister`]. Without a started persister,
    /// the owner must call [`Self::persist_savings`] or
    /// [`Self::shutdown_savings_persister`] to write recorded turns.
    #[must_use]
    pub fn with_savings_path(mut self, path: std::path::PathBuf) -> Self {
        self.savings_path = Some(Arc::new(path));
        // A rebound path never shares a writer bound to the previous one.
        self.savings_persistence = Arc::default();
        self
    }

    /// Synchronously write the current ledger when persistence is configured.
    ///
    /// This writes even when nothing is marked dirty, blocks the calling
    /// thread on file I/O, and is intended for tests, embedders without a
    /// background persister, and startup. A failure is logged, counted in
    /// `wayfinder_router_savings_persist_failures_total`, and leaves the
    /// ledger dirty for a running persister to retry.
    pub fn persist_savings(&self) -> Result<(), wayfinder_service::pricing::LedgerError> {
        self.savings_target().map_or(Ok(()), |target| {
            self.savings_persistence.save_now(&target, true).map(|_| ())
        })
    }

    /// Start the single background savings persister for this ledger and path.
    ///
    /// Returns `Ok(false)` when no savings path is configured or a persister
    /// was already started for this process-lifetime persistence authority
    /// (including one carried over by [`Self::with_runtime_state_from`]). The
    /// persister binds the ledger and path configured at call time. Requires a
    /// Tokio runtime; construction of [`AppState`] never starts it.
    pub fn start_savings_persister(
        &self,
    ) -> Result<bool, savings_persistence::SavingsPersistError> {
        self.savings_target()
            .map_or(Ok(false), |target| self.savings_persistence.start(target))
    }

    /// Stop the background persister after a final write of any dirty state.
    ///
    /// When no persister is running, dirty state is still written once on the
    /// blocking pool, so a graceful stop loses nothing either way.
    pub async fn shutdown_savings_persister(
        &self,
    ) -> Result<(), savings_persistence::SavingsPersistError> {
        match self.savings_target() {
            Some(target) => self.savings_persistence.shutdown(target).await,
            None => Ok(()),
        }
    }

    /// Process-lifetime savings persistence state, for diagnostics and tests.
    #[must_use]
    pub fn savings_persistence(&self) -> &savings_persistence::SavingsPersistence {
        &self.savings_persistence
    }

    /// Cheap request-path hook: mark the ledger dirty for the persister.
    fn note_savings_recorded(&self) {
        if self.savings_path.is_some() {
            self.savings_persistence.mark_dirty();
        }
    }

    fn savings_target(&self) -> Option<savings_persistence::SavingsTarget> {
        self.savings_path
            .as_ref()
            .map(|path| savings_persistence::SavingsTarget {
                ledger: Arc::clone(&self.savings_ledger),
                path: Arc::clone(path),
                metrics: Arc::clone(&self.metrics),
            })
    }

    /// Preserve process-lifetime runtime authorities across a config snapshot swap.
    ///
    /// Recent metadata, metrics, realized accounting, and delivery admission
    /// retain continuity. Admission is deliberately one process-lifetime
    /// authority: replacing its semaphores while requests from the previous
    /// snapshot are still in flight would let each generation admit up to its
    /// own limit. Concurrency-limit changes therefore take effect on process
    /// restart rather than weakening the active bound during hot reload.
    /// Savings persistence is likewise one authority: the ledger, its path,
    /// the dirty flag, and the single background writer all carry over.
    #[must_use]
    pub fn with_runtime_state_from(mut self, previous: &Self) -> Self {
        self.admission_policy = Arc::clone(&previous.admission_policy);
        self.shared_state_backend = previous.shared_state_backend.clone();
        self.recent = Arc::clone(&previous.recent);
        self.metrics = Arc::clone(&previous.metrics);
        self.deployment_cursors = Arc::clone(&previous.deployment_cursors);
        self.deployment_signals = Arc::clone(&previous.deployment_signals);
        self.shadow_store = Arc::clone(&previous.shadow_store);
        self.shadow_slots = Arc::clone(&previous.shadow_slots);
        self.shadow_store.set_active(self.shadow_config.enabled);
        self.evidence_labels = Arc::clone(&previous.evidence_labels);
        self.savings_ledger = Arc::clone(&previous.savings_ledger);
        self.savings_path = previous.savings_path.clone();
        self.savings_persistence = Arc::clone(&previous.savings_persistence);
        self.audit_log = Arc::clone(&previous.audit_log);
        self
    }

    /// Active routing configuration.
    #[must_use]
    pub fn routing(&self) -> &RoutingConfig {
        &self.routing
    }

    /// Configured endpoint metadata in source insertion order.
    #[must_use]
    pub fn models(&self) -> &[ConfiguredModel] {
        &self.models
    }

    /// Active immutable policy version identity.
    #[must_use]
    pub fn policy_version(&self) -> &str {
        &self.policy_version
    }

    /// Active immutable runtime snapshot identity.
    #[must_use]
    pub fn policy_snapshot_id(&self) -> &str {
        &self.policy_snapshot_id
    }

    /// Resolve the managed profile for authenticated request identities.
    #[must_use]
    pub fn resolve_policy_profile(
        &self,
        context: BindingContext<'_>,
    ) -> Option<ResolvedPolicyProfile<'_>> {
        self.policy.as_ref().map(|policy| policy.resolve(context))
    }

    /// Configured named routing presets in source insertion order.
    #[must_use]
    pub fn route_presets(&self) -> &IndexMap<String, RoutePreset> {
        &self.route_presets
    }

    /// Resolve one named route without exposing mutable configuration.
    #[must_use]
    pub fn route_preset(&self, name: &str) -> Option<&RoutePreset> {
        self.route_presets.get(name)
    }

    /// Active bounded production canary policy.
    #[must_use]
    pub fn canary_config(&self) -> &CanaryConfig {
        &self.canary_config
    }

    /// Return a deterministic weighted order of concrete targets for an alias.
    ///
    /// The first target rotates according to configured weights. Remaining
    /// targets stay in stable order so a transient failure can fail over within
    /// the same alias before the outer routing ladder is considered.
    pub(crate) fn deployment_candidates(&self, alias: &str) -> Vec<ConfiguredModel> {
        let Some(model) = self.models.iter().find(|model| model.name() == alias) else {
            return Vec::new();
        };
        let targets = model.deployment_targets();
        if targets.len() <= 1 {
            return vec![model.clone()];
        }
        let total_weight = targets.iter().map(|target| target.weight).sum::<u64>();
        let slot = match self.deployment_cursors.lock() {
            Ok(mut cursors) => {
                let cursor = cursors.entry(alias.to_owned()).or_default();
                let slot = *cursor % total_weight;
                *cursor = cursor.saturating_add(1);
                slot
            }
            Err(poisoned) => {
                let mut cursors = poisoned.into_inner();
                let cursor = cursors.entry(alias.to_owned()).or_default();
                let slot = *cursor % total_weight;
                *cursor = cursor.saturating_add(1);
                slot
            }
        };
        let mut start = 0;
        let mut remaining = slot;
        for (index, target) in targets.iter().enumerate() {
            if remaining < target.weight {
                start = index;
                break;
            }
            remaining -= target.weight;
        }
        (0..targets.len())
            .map(|offset| (start + offset) % targets.len())
            .map(|index| model.for_deployment(&targets[index]))
            .collect()
    }

    pub(crate) fn select_deployments(
        &self,
        alias: &str,
        candidates: Vec<ConfiguredModel>,
    ) -> DeploymentSelection {
        let policy = self
            .models
            .iter()
            .find(|model| model.name() == alias)
            .map_or_else(DeploymentSelectionPolicy::default, |model| {
                model.deployment_selection().clone()
            });
        let selection = self.deployment_signals.select(&policy, candidates);
        if let Some(target) = selection.targets.first() {
            let _ = self.metrics.observe_deployment_selection(
                alias,
                &target.delivery_id(),
                selection.reason,
            );
        }
        selection
    }

    pub(crate) fn observe_deployment(
        &self,
        deployment_id: &str,
        observation: DeploymentObservation,
    ) {
        if let Some(ttft) = observation.ttft_seconds {
            let _ = self.metrics.observe_deployment_ttft(deployment_id, ttft);
        }
        if let Some(throughput) = observation.throughput_chars_per_second {
            let _ = self
                .metrics
                .observe_deployment_throughput(deployment_id, throughput);
        }
        if let Some(capacity) = observation.capacity_pressure {
            let _ = self
                .metrics
                .observe_deployment_capacity(deployment_id, capacity);
        }
        self.deployment_signals.observe(deployment_id, observation);
    }

    /// Queue sampled off-path shadow evaluation without affecting delivery.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn schedule_shadow(
        &self,
        request_id: &str,
        prompt: &str,
        routing_request: &RoutingRequest,
        production_model: &str,
        production_score: f64,
        request_body: &serde_json::Map<String, Value>,
        provider_consent: bool,
    ) {
        // Decide on the borrowed config first: with shadow evaluation disabled
        // or the request unsampled, nothing is cloned on the request path.
        let config = self.shadow_config.as_ref();
        if !config.enabled
            || config.candidate_routes.is_empty()
            || !crate::shadow::deterministic_sample(
                request_id,
                &crate::shadow::candidate_fingerprint(&config.candidate_routes),
                config.sample_rate,
            )
        {
            return;
        }
        self.shadow_store.sampled();
        let permit = match Arc::clone(&self.shadow_slots).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                self.shadow_store.dropped_in_flight();
                self.metrics().observe_shadow_event("dropped", "in-flight");
                return;
            }
        };
        let job = ShadowJob {
            request_id: request_id.to_owned(),
            prompt: prompt.to_owned(),
            routing_request: routing_request.clone(),
            production_model: production_model.to_owned(),
            production_score,
            request_body: Value::Object(request_body.clone()),
            provider_consent,
        };
        let config = config.clone();
        let state = self.clone();
        let store = Arc::clone(&self.shadow_store);
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            drop(permit);
            store.error();
            self.metrics().observe_shadow_event("error", "runtime");
            return;
        };
        handle.spawn(crate::shadow::evaluate(state, config, store, job, permit));
    }

    fn configured_deployment_targets(&self, alias: &str) -> Vec<ConfiguredModel> {
        let Some(model) = self.models.iter().find(|model| model.name() == alias) else {
            return Vec::new();
        };
        model
            .deployment_targets()
            .iter()
            .map(|deployment| model.for_deployment(deployment))
            .collect()
    }

    /// Whether the configured router is constrained to offline delivery.
    #[must_use]
    pub const fn offline(&self) -> bool {
        self.offline
    }

    /// Whether explicit dry-run mode is active.
    #[must_use]
    pub const fn dry_run(&self) -> bool {
        self.dry_run
    }

    /// Default transcript scope used for routing decisions.
    #[must_use]
    pub const fn route_on(&self) -> RouteOn {
        self.route_on
    }

    /// Configured conversation-latch default.
    #[must_use]
    pub const fn sticky(&self) -> bool {
        self.sticky
    }

    /// Configured calm-turn count before a sticky latch may decay.
    #[must_use]
    pub const fn sticky_cooldown(&self) -> u64 {
        self.sticky_cooldown
    }

    /// Whether in-message slash directives are enabled.
    #[must_use]
    pub const fn slash_directives(&self) -> bool {
        self.slash_directives
    }

    /// Build version retained for service/capability handshakes.
    ///
    /// It is intentionally absent from the compatibility response bodies.
    #[must_use]
    pub fn build_version(&self) -> &str {
        &self.build_version
    }

    /// Configured request-body limit in bytes.
    #[must_use]
    pub const fn request_body_limit(&self) -> usize {
        self.request_body_limit
    }

    /// Process-local upstream delivery admission policy.
    #[must_use]
    pub fn admission_policy(&self) -> &AdmissionPolicy {
        &self.admission_policy
    }

    /// Prompt-free recent decision metadata retained by this process.
    #[must_use]
    pub fn recent(&self) -> &RecentRoutes {
        &self.recent
    }

    /// Prompt-free process-local metrics collector.
    #[must_use]
    pub fn metrics(&self) -> &GatewayMetrics {
        &self.metrics
    }

    /// Selected provider-delivery implementation, if this build configured one.
    #[must_use]
    pub fn delivery(&self) -> Option<&dyn BufferedDelivery> {
        self.delivery.as_deref()
    }

    /// Clone the configured buffered delivery for a non-blocking shadow task.
    pub(crate) fn delivery_arc(&self) -> Option<Arc<dyn BufferedDelivery>> {
        self.delivery.clone()
    }

    /// Selected streaming provider implementation, if configured.
    #[must_use]
    pub fn streaming_delivery(&self) -> Option<&dyn StreamingDelivery> {
        self.streaming_delivery.as_deref()
    }

    /// Prompt-free shadow evidence retained by this process.
    #[must_use]
    pub fn shadow_snapshot(&self, limit: usize) -> ShadowSnapshot {
        self.shadow_store.snapshot(limit)
    }

    /// Add bounded prompt-free human or versioned automated evidence labels.
    pub fn add_evidence_labels<I>(&self, labels: I) -> Result<usize, EvidenceError>
    where
        I: IntoIterator<Item = EvidenceLabel>,
    {
        self.evidence_labels.insert(labels)
    }

    /// Build a deterministic evidence report from the current bounded stores.
    #[must_use]
    pub fn evidence_report(&self) -> EvidenceReport {
        EvidenceReport::from_snapshot(
            &self.shadow_snapshot(usize::MAX),
            &self.evidence_labels.snapshot(),
        )
    }

    /// Configured opt-in gateway access policy, if any.
    #[must_use]
    pub fn access_policy(&self) -> Option<&AccessPolicy> {
        self.access_policy.as_deref()
    }

    /// Shared policy backend, when the gateway was built from `[gateway.state]`.
    #[must_use]
    pub fn shared_state_backend(&self) -> Option<Arc<dyn state::StateBackend>> {
        self.shared_state_backend.clone()
    }

    /// Whether the shared backend is currently degraded.
    #[must_use]
    pub fn state_degraded(&self) -> bool {
        self.shared_state_backend
            .as_ref()
            .is_some_and(|backend| backend.degraded())
            || self
                .access_policy
                .as_ref()
                .is_some_and(|policy| policy.state_degraded())
    }

    /// Configured operator authenticator, if OIDC is enabled.
    #[must_use]
    pub fn operator_auth(&self) -> Option<&operator_auth::OperatorAuthenticator> {
        self.operator_auth.as_deref()
    }

    /// Prompt-free audit sink shared by immutable request snapshots.
    #[must_use]
    pub fn audit_log(&self) -> &audit::AuditLog {
        &self.audit_log
    }

    /// Record an operator event against this immutable policy snapshot.
    pub async fn record_audit(
        &self,
        actor: impl AsRef<str>,
        action: impl AsRef<str>,
        before: Option<Value>,
        after: Option<Value>,
    ) -> Result<(), audit::AuditError> {
        self.audit_log
            .record_with_policy(
                actor,
                action,
                before,
                after,
                self.policy_version(),
                self.policy_snapshot_id(),
            )
            .await
    }

    /// Configured spend-budget policy.
    #[must_use]
    pub fn budget_policy(&self) -> &BudgetPolicy {
        &self.budget_policy
    }

    /// Shared buffered-delivery reliability policy and circuit state.
    #[must_use]
    pub fn reliability_policy(&self) -> &ReliabilityPolicy {
        &self.reliability_policy
    }

    /// Shared exact-response cache.
    #[must_use]
    pub fn response_cache(&self) -> &ResponseCache {
        &self.response_cache
    }

    fn cache_enabled(&self) -> Result<bool, CacheError> {
        if self.shared_cache_enabled {
            Ok(self.cache_settings.enabled)
        } else {
            self.response_cache.enabled()
        }
    }

    fn cache_generation(&self) -> &str {
        &self.cache_generation
    }

    async fn get_cached_response(
        &self,
        key: &str,
    ) -> Result<Option<Arc<CachedResponse>>, CacheError> {
        if self.shared_cache_enabled {
            let Some(backend) = self.shared_state_backend.as_ref() else {
                return Ok(None);
            };
            let value = backend
                .shared_cache_get(&self.cache_generation, key)
                .await
                .unwrap_or(None);
            return Ok(value
                .and_then(|value| decode_shared_response(&value).ok())
                .map(Arc::new));
        }
        self.response_cache.get_at(key, self.cache_now())
    }

    async fn put_cached_response(
        &self,
        key: String,
        response: CachedResponse,
    ) -> Result<(), CacheError> {
        if self.shared_cache_enabled {
            let Some(backend) = self.shared_state_backend.as_ref() else {
                return Ok(());
            };
            let body_bytes = response.body.len();
            let value = encode_shared_response(&response)?;
            let _ = backend
                .shared_cache_put(SharedCachePut {
                    generation: self.cache_generation.to_string(),
                    key,
                    value,
                    body_bytes,
                    ttl_seconds: self.cache_settings.ttl_seconds,
                    max_entries: self.cache_settings.max_entries,
                    max_bytes: self.cache_settings.max_bytes,
                })
                .await;
            return Ok(());
        }
        self.response_cache.put_at(key, response, self.cache_now())
    }

    fn cache_now(&self) -> f64 {
        (self.cache_clock)()
    }

    /// Stable price table used for in-memory turn accounting.
    #[must_use]
    pub fn price_table(&self) -> &PriceTable {
        &self.price_table
    }

    /// Twelve-hex fingerprint of the active price table.
    #[must_use]
    pub fn price_table_version(&self) -> &str {
        &self.price_table_version
    }

    /// Shared in-memory savings ledger.
    #[must_use]
    pub fn savings_ledger(&self) -> &SavingsLedger {
        &self.savings_ledger
    }

    /// Whether ordered `prefer-local` and `prefer-hosted` directives are valid.
    #[must_use]
    pub fn supports_tier_directives(&self) -> bool {
        self.routing.classifier.is_none() && !self.routing.tiers.is_empty()
    }

    fn codex_control(&self) -> Option<Arc<dyn codex_control::CodexControl>> {
        self.codex_control.clone()
    }
}

impl fmt::Debug for AppState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppState")
            .field("routing", &self.routing)
            .field("models", &self.models)
            .field("route_presets", &self.route_presets)
            .field("offline", &self.offline)
            .field("dry_run", &self.dry_run)
            .field("route_on", &self.route_on)
            .field("sticky", &self.sticky)
            .field("sticky_cooldown", &self.sticky_cooldown)
            .field("slash_directives", &self.slash_directives)
            .field("build_version", &self.build_version)
            .field("policy_version", &self.policy_version)
            .field("policy_snapshot_id", &self.policy_snapshot_id)
            .field("managed_policy", &self.policy.is_some())
            .field("request_body_limit", &self.request_body_limit)
            .field("admission_policy", &self.admission_policy)
            .field("delivery_configured", &self.delivery.is_some())
            .field(
                "streaming_delivery_configured",
                &self.streaming_delivery.is_some(),
            )
            .field("access_policy", &self.access_policy)
            .field("budget_policy", &self.budget_policy)
            .field("reliability_policy", &self.reliability_policy)
            .field("response_cache", &self.response_cache)
            .field("price_table_version", &self.price_table_version)
            .field("savings_priced", &self.savings_ledger.priced())
            .field("codex_control_configured", &self.codex_control.is_some())
            .field("operator_auth_configured", &self.operator_auth.is_some())
            .field("audit_log", &self.audit_log.path())
            .finish_non_exhaustive()
    }
}

/// Build the in-process Axum router without binding a socket or starting a task.
pub fn build_router(state: AppState) -> Router {
    let request_body_limit = state.request_body_limit();
    let codex_control_enabled = state.codex_control.is_some();
    let operator_routes = Router::new()
        .route("/metrics", get(gateway_metrics))
        .route("/router/models", get(router_models))
        .route("/router/routes", get(router_routes))
        .route("/router/profiles", get(router_profiles))
        .route("/router/recent", get(router_recent))
        .route("/router/outcomes", post(record_user_outcome))
        .route("/router/outcomes/policy", get(outcome_policy_report))
        .route("/router/evidence", get(evidence_report_json))
        .route("/router/evidence.txt", get(evidence_report_text))
        .route("/router/evidence/labels", post(evidence_labels))
        .route("/router/canary", get(canary_status))
        .route("/v1/savings", get(savings_report))
        .route("/savings", get(savings_report))
        .route("/v1/value", get(project_value_report))
        .route("/value", get(project_value_report))
        .route("/router/value", get(project_value_report))
        .route("/v1/outcomes", post(record_user_outcome))
        .route("/outcomes", post(record_user_outcome))
        .route("/v1/outcomes/policy", get(outcome_policy_report))
        .route("/outcomes/policy", get(outcome_policy_report))
        .route("/v1/evidence", get(evidence_report_json))
        .route("/evidence", get(evidence_report_json))
        .route("/v1/evidence.txt", get(evidence_report_text))
        .route("/evidence.txt", get(evidence_report_text))
        .route("/v1/evidence/labels", post(evidence_labels))
        .route("/evidence/labels", post(evidence_labels))
        .route("/v1/canary", get(canary_status))
        .route("/canary", get(canary_status))
        .route("/router/config", post(router_config))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            operator_auth::middleware,
        ));
    let application = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(openai_models))
        .route("/models", get(openai_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses::responses))
        .route("/responses", post(responses::responses))
        .route("/v1/messages", post(anthropic_messages))
        .route("/messages", post(anthropic_messages))
        .merge(operator_routes);
    let application = if codex_control_enabled {
        application.merge(codex_control::routes())
    } else {
        application
    };
    application
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn(otel::request_middleware))
        .layer(DefaultBodyLimit::max(request_body_limit))
        .with_state(state)
}

/// Validate the fail-closed contract for a network-exposed data plane.
///
/// Local Desktop and developer serving continue to use [`build_router`]. The
/// managed surface is deliberately narrower and is invalid without virtual-key
/// authentication and at least one configured destination.
pub fn validate_data_plane_state(state: &AppState) -> Result<(), DataPlaneError> {
    if !state
        .access_policy()
        .is_some_and(AccessPolicy::requires_authentication)
    {
        return Err(DataPlaneError::AuthenticationRequired);
    }
    if state.models().is_empty() {
        return Err(DataPlaneError::ModelRequired);
    }
    Ok(())
}

/// Build the authenticated model data plane without local operator surfaces.
///
/// This surface intentionally excludes `/metrics`, `/router/*`, savings, and
/// local account-control routes. It exposes only minimal unauthenticated probe
/// endpoints plus authenticated model discovery and inference compatibility
/// routes.
pub fn build_data_plane_router(state: AppState) -> Result<Router, DataPlaneError> {
    validate_data_plane_state(&state)?;
    let request_body_limit = state.request_body_limit();
    Ok(Router::new()
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
        .route("/v1/models", get(authenticated_openai_models))
        .route("/models", get(authenticated_openai_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses::responses))
        .route("/responses", post(responses::responses))
        .route("/v1/messages", post(anthropic_messages))
        .route("/messages", post(anthropic_messages))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn(otel::request_middleware))
        .layer(DefaultBodyLimit::max(request_body_limit))
        .with_state(state))
}

/// Build a router that selects one immutable last-good state per request.
///
/// Reload installation is deliberately owned by the process boundary. A
/// request can therefore observe either the complete old snapshot or the
/// complete new snapshot, never a partially mutated configuration.
pub fn build_reloadable_router<V>(holder: Arc<LastGood<AppState, V>>) -> Router
where
    V: Clone + Eq + Send + Sync + 'static,
{
    Router::new()
        .fallback(reloadable_dispatch::<V>)
        .with_state(holder)
}

/// Build a managed data plane over immutable last-good configuration snapshots.
pub fn build_reloadable_data_plane_router<V>(holder: Arc<LastGood<AppState, V>>) -> Router
where
    V: Clone + Eq + Send + Sync + 'static,
{
    Router::new()
        .fallback(reloadable_data_plane_dispatch::<V>)
        .with_state(holder)
}

async fn reloadable_dispatch<V>(
    State(holder): State<Arc<LastGood<AppState, V>>>,
    request: Request<Body>,
) -> Response
where
    V: Clone + Eq + Send + Sync + 'static,
{
    let state = match holder.current() {
        Ok(state) => state,
        Err(_) => return internal_error_response(),
    };
    match build_router((*state).clone()).oneshot(request).await {
        Ok(response) => response,
        Err(error) => match error {},
    }
}

async fn reloadable_data_plane_dispatch<V>(
    State(holder): State<Arc<LastGood<AppState, V>>>,
    request: Request<Body>,
) -> Response
where
    V: Clone + Eq + Send + Sync + 'static,
{
    let state = match holder.current() {
        Ok(state) => state,
        Err(_) => return internal_error_response(),
    };
    let application = match build_data_plane_router((*state).clone()) {
        Ok(application) => application,
        Err(_) => return data_plane_unavailable_response(),
    };
    match application.oneshot(request).await {
        Ok(response) => response,
        Err(error) => match error {},
    }
}

fn internal_error_response() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "error": {
                "code": "wayfinder_router_internal_error",
                "message": "gateway state is unavailable"
            }
        })),
    )
        .into_response()
}

fn data_plane_unavailable_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": {
                "code": "wayfinder_router_not_ready",
                "message": "gateway data plane is not ready"
            }
        })),
    )
        .into_response()
}

#[derive(Serialize)]
struct ProbeResponse {
    status: &'static str,
}

async fn livez() -> Json<ProbeResponse> {
    Json(ProbeResponse { status: "ok" })
}

async fn readyz(State(state): State<AppState>) -> Response {
    let ready = state.models().iter().any(ConfiguredModel::key_ready);
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(ProbeResponse {
            status: if ready { "ready" } else { "not_ready" },
        }),
    )
        .into_response()
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    models: Vec<String>,
    offline: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    missing_keys: Vec<String>,
}

async fn healthz(State(state): State<AppState>) -> Json<HealthResponse> {
    let mut models = state
        .models()
        .iter()
        .map(|model| model.name().to_owned())
        .collect::<Vec<_>>();
    models.sort();
    let mut missing_keys = state
        .models()
        .iter()
        .filter(|model| !model.key_ready())
        .map(|model| model.name().to_owned())
        .collect::<Vec<_>>();
    missing_keys.sort();
    Json(HealthResponse {
        status: if missing_keys.is_empty() {
            "ok"
        } else {
            "degraded"
        },
        models,
        offline: state.offline(),
        missing_keys,
    })
}

async fn gateway_metrics(State(state): State<AppState>) -> Response {
    match state.metrics().render() {
        Ok(body) => (
            [(
                header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            body,
        )
            .into_response(),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "wayfinder_router_state_error",
            error.to_string(),
            HeaderMap::new(),
        ),
    }
}

#[derive(Serialize)]
struct OpenAiModelList {
    object: &'static str,
    data: Vec<OpenAiModel>,
}

#[derive(Serialize)]
struct OpenAiModel {
    id: String,
    object: &'static str,
    created: u64,
    owned_by: &'static str,
}

async fn openai_models(State(state): State<AppState>) -> Json<OpenAiModelList> {
    Json(openai_model_list(&state, None))
}

async fn authenticated_openai_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let request_id = new_request_id();
    let Some(policy) = state.access_policy() else {
        return data_plane_unavailable_response();
    };
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let Some(grant) = policy.authenticate(authorization) else {
        let mut response_headers = request_id_headers(&request_id);
        response_headers.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return error_response(
            StatusCode::UNAUTHORIZED,
            "wayfinder_router_unauthorized",
            "missing or invalid API key".to_owned(),
            response_headers,
        );
    };
    Json(openai_model_list(
        &state,
        Some(policy.allowed_models(grant)),
    ))
    .into_response()
}

fn openai_model_list(state: &AppState, allowed_models: Option<&[String]>) -> OpenAiModelList {
    let mut ids = vec![AUTO_MODEL.to_owned()];
    if state.supports_tier_directives() {
        ids.push(PREFER_LOW.to_owned());
        ids.push(PREFER_HIGH.to_owned());
    }
    ids.extend(
        state
            .models()
            .iter()
            .filter(|model| {
                allowed_models.is_none_or(|allowed| {
                    allowed.is_empty() || allowed.iter().any(|name| name == model.name())
                })
            })
            .map(|model| model.name().to_owned()),
    );
    OpenAiModelList {
        object: "list",
        data: ids
            .into_iter()
            .map(|id| OpenAiModel {
                id,
                object: "model",
                created: 0,
                owned_by: "wayfinder",
            })
            .collect(),
    }
}

#[derive(Serialize)]
struct RouterModelsResponse {
    models: Vec<RouterModel>,
    dry_run: bool,
}

#[derive(Serialize)]
struct RouterModel {
    name: String,
    endpoint: String,
    model: String,
    provider: String,
    tier: Option<String>,
    api_key_env: Option<String>,
    key_ok: bool,
}

async fn router_models(State(state): State<AppState>) -> Json<RouterModelsResponse> {
    let models = state
        .models()
        .iter()
        .map(|model| RouterModel {
            name: model.name().to_owned(),
            endpoint: model.endpoint().to_owned(),
            model: model.provider_model().to_owned(),
            provider: model.provider().as_str().to_owned(),
            tier: model.tier().map(|tier| tier.as_str().to_owned()),
            api_key_env: model.api_key_env().map(str::to_owned),
            key_ok: model.key_ready(),
        })
        .collect();
    Json(RouterModelsResponse {
        models,
        dry_run: state.dry_run(),
    })
}

#[derive(Serialize)]
struct RouterProfilesResponse {
    profiles: &'static [LexiconProfile],
}

#[derive(Serialize)]
struct RouterRoutesResponse {
    routes: Vec<RouterRoute>,
}

#[derive(Serialize)]
struct RouterRoute {
    name: String,
    models: Vec<String>,
}

async fn router_routes(State(state): State<AppState>) -> Json<RouterRoutesResponse> {
    Json(RouterRoutesResponse {
        routes: state
            .route_presets()
            .iter()
            .map(|(name, route)| RouterRoute {
                name: name.clone(),
                models: route.models.clone(),
            })
            .collect(),
    })
}

async fn router_profiles() -> Json<RouterProfilesResponse> {
    Json(RouterProfilesResponse {
        profiles: profiles(),
    })
}

const fn default_recent_limit() -> i64 {
    50
}

async fn router_recent(State(state): State<AppState>, uri: Uri) -> Response {
    let limit = match recent_limit(&uri) {
        Ok(limit) => limit,
        Err(input) => {
            return validation_response(json!({
                "type": "int_parsing",
                "loc": ["query", "limit"],
                "msg": "Input should be a valid integer, unable to parse string as an integer",
                "input": input
            }));
        }
    };
    match state.recent().report(limit) {
        Ok(report) => Json(report).into_response(),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "wayfinder_router_state_error",
            error.to_string(),
            HeaderMap::new(),
        ),
    }
}

const OUTCOME_LABEL_SCHEMA_VERSION: &str = "wf-outcome-label-v1";
const MAX_OUTCOME_ID_BYTES: usize = 128;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserOutcomeRequest {
    schema_version: String,
    workspace_id: String,
    request_id: String,
    outcome: UserOutcome,
}

async fn record_user_outcome(
    State(state): State<AppState>,
    actor: Option<Extension<audit::AuditActor>>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(rejection) => return body_rejection_response(&state, rejection),
    };
    let label = match serde_json::from_slice::<UserOutcomeRequest>(&body) {
        Ok(label)
            if label.schema_version == OUTCOME_LABEL_SCHEMA_VERSION
                && !label.workspace_id.is_empty()
                && label.workspace_id.len() <= MAX_OUTCOME_ID_BYTES
                && !label.request_id.is_empty()
                && label.request_id.len() <= MAX_OUTCOME_ID_BYTES =>
        {
            label
        }
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "wayfinder_router_bad_outcome_label",
                "outcome label must use wf-outcome-label-v1 with bounded workspace_id, request_id, and success, correction, or failure outcome"
                    .to_owned(),
                HeaderMap::new(),
            );
        }
    };
    if let Err(error) =
        state
            .recent()
            .label_user_outcome(&label.request_id, &label.workspace_id, label.outcome)
    {
        let status = match error {
            UserOutcomeError::ReceiptNotFound => StatusCode::NOT_FOUND,
            UserOutcomeError::ReceiptNotTerminal => StatusCode::CONFLICT,
            UserOutcomeError::LockPoisoned => StatusCode::INTERNAL_SERVER_ERROR,
        };
        return error_response(
            status,
            "wayfinder_router_outcome_label_rejected",
            error.to_string(),
            HeaderMap::new(),
        );
    }
    let actor = actor.as_ref().map_or("local", |actor| actor.0.0.as_str());
    if let Err(error) = state
        .record_audit(
            actor,
            "outcomes.label",
            None,
            Some(json!({
                "workspace_id": label.workspace_id.clone(),
                "request_id": label.request_id.clone(),
                "outcome": label.outcome,
            })),
        )
        .await
    {
        return audit_unavailable_response(error);
    }
    Json(json!({
        "schema_version": OUTCOME_LABEL_SCHEMA_VERSION,
        "accepted": true,
        "workspace_id": label.workspace_id,
        "request_id": label.request_id,
        "outcome": label.outcome,
        "retention": "process-local-bounded-shared-ring",
    }))
    .into_response()
}

async fn outcome_policy_report(
    State(state): State<AppState>,
    uri: Uri,
    actor: Option<Extension<audit::AuditActor>>,
) -> Response {
    let workspace = match project_value_query(&uri) {
        Ok(query) => query.workspace,
        Err(message) => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "wayfinder_router_bad_outcome_policy_query",
                message,
                HeaderMap::new(),
            );
        }
    };
    let entries = match state.recent().entries_for_workspace(&workspace) {
        Ok(entries) => entries,
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wayfinder_router_state_error",
                error.to_string(),
                HeaderMap::new(),
            );
        }
    };
    let resolved_profile = state.resolve_policy_profile(BindingContext {
        client_id: None,
        workspace_id: Some(&workspace),
        key_id: None,
    });
    let routing = resolved_profile.map_or_else(|| state.routing(), |profile| profile.routing);
    let report = propose_local_threshold(&workspace, routing, state.models(), &entries);
    let actor = actor.as_ref().map_or("local", |actor| actor.0.0.as_str());
    if let Err(error) = state
        .record_audit(
            actor,
            "outcomes.policy.export",
            None,
            Some(json!({"workspace_id": workspace})),
        )
        .await
    {
        return audit_unavailable_response(error);
    }
    Json(report).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceLabelBatch {
    labels: Vec<EvidenceLabel>,
}

async fn evidence_report_json(
    State(state): State<AppState>,
    actor: Option<Extension<audit::AuditActor>>,
) -> Response {
    let report = state.evidence_report();
    if let Err(error) = record_evidence_audit(&state, actor, "evidence.export.json").await {
        return *error;
    }
    Json(report).into_response()
}

async fn evidence_report_text(
    State(state): State<AppState>,
    actor: Option<Extension<audit::AuditActor>>,
) -> Response {
    let report = state.evidence_report();
    if let Err(error) = record_evidence_audit(&state, actor, "evidence.export.text").await {
        return *error;
    }
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        report.render_text(),
    )
        .into_response()
}

async fn canary_status(
    State(state): State<AppState>,
    actor: Option<Extension<audit::AuditActor>>,
) -> Response {
    let config = state.canary_config();
    let snapshot = if config.enabled {
        let Some(backend) = state
            .shared_state_backend()
            .filter(|backend| backend.backend_name() == "redis")
        else {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "wayfinder_router_canary_state_unavailable",
                "enabled canary state requires the configured Redis fleet authority".to_owned(),
                HeaderMap::new(),
            );
        };
        match backend
            .canary_snapshot(&config.rollout_id, config.window, wall_clock_seconds())
            .await
        {
            Ok(snapshot) => snapshot,
            Err(_) => {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "wayfinder_router_canary_state_unavailable",
                    "canary fleet state is unavailable; rollout remains fail-closed".to_owned(),
                    HeaderMap::new(),
                );
            }
        }
    } else {
        SharedCanarySnapshot::default()
    };
    if let Err(error) = record_evidence_audit(&state, actor, "canary.status").await {
        return *error;
    }
    Json(json!({
        "schema_version": "wf-canary-v1",
        "enabled": config.enabled,
        "rollout_id": config.rollout_id,
        "candidate_route": config.candidate_route,
        "fraction": config.fraction,
        "scope": config.scope,
        "max_requests": config.max_requests,
        "window": config.window,
        "rollback_hold": config.rollback_hold,
        "requests": snapshot.requests,
        "errors": snapshot.errors,
        "latency_sum_ms": snapshot.latency_sum_ms,
        "latency_samples": snapshot.latency_samples,
        "realized_cost": snapshot.realized_cost,
        "baseline_cost": snapshot.baseline_cost,
        "cost_samples": snapshot.cost_samples,
        "quality_loss": snapshot.quality_loss,
        "quality_samples": snapshot.quality_samples,
        "tripped_reason": snapshot.tripped_reason,
    }))
    .into_response()
}

async fn evidence_labels(
    State(state): State<AppState>,
    actor: Option<Extension<audit::AuditActor>>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(rejection) => return body_rejection_response(&state, rejection),
    };
    let batch = match serde_json::from_slice::<EvidenceLabelBatch>(&body) {
        Ok(batch) => batch,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "wayfinder_router_bad_evidence",
                "evidence labels must be a bounded JSON object with a labels array".to_owned(),
                HeaderMap::new(),
            );
        }
    };
    let accepted = match state.add_evidence_labels(batch.labels) {
        Ok(accepted) => accepted,
        Err(error) => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "wayfinder_router_bad_evidence",
                error.to_string(),
                HeaderMap::new(),
            );
        }
    };
    if let Err(error) = record_evidence_audit(&state, actor, "evidence.labels").await {
        return *error;
    }
    Json(json!({"accepted": accepted})).into_response()
}

async fn record_evidence_audit(
    state: &AppState,
    actor: Option<Extension<audit::AuditActor>>,
    action: &str,
) -> Result<(), Box<Response>> {
    let actor = actor.as_ref().map_or("local", |actor| actor.0.0.as_str());
    state
        .record_audit(actor, action, None, None)
        .await
        .map_err(|error| Box::new(audit_unavailable_response(error)))
}

#[derive(Serialize)]
struct SavingsResponse {
    #[serde(flatten)]
    report: SavingsReport,
    price_table_version: String,
}

async fn savings_report(
    State(state): State<AppState>,
    uri: Uri,
    actor: Option<Extension<audit::AuditActor>>,
) -> Response {
    let today = match utc_today() {
        Ok(today) => today,
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wayfinder_router_state_error",
                error.to_string(),
                HeaderMap::new(),
            );
        }
    };
    let period = savings_period(&uri);
    match state.savings_ledger().period(period, today) {
        Ok(report) => {
            let actor = actor.as_ref().map_or("local", |actor| actor.0.0.as_str());
            if let Err(error) = state
                .record_audit(
                    actor,
                    "savings.export",
                    None,
                    Some(json!({"period_days": period})),
                )
                .await
            {
                return audit_unavailable_response(error);
            }
            Json(SavingsResponse {
                report,
                price_table_version: state.price_table_version().to_owned(),
            })
            .into_response()
        }
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "wayfinder_router_state_error",
            error.to_string(),
            HeaderMap::new(),
        ),
    }
}

struct ProjectValueQuery {
    workspace: String,
    period_days: Option<u32>,
}

async fn project_value_report(
    State(state): State<AppState>,
    uri: Uri,
    actor: Option<Extension<audit::AuditActor>>,
) -> Response {
    let query = match project_value_query(&uri) {
        Ok(query) => query,
        Err(message) => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "wayfinder_router_bad_value_query",
                message,
                HeaderMap::new(),
            );
        }
    };
    let today = match utc_today() {
        Ok(today) => today,
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wayfinder_router_state_error",
                error.to_string(),
                HeaderMap::new(),
            );
        }
    };
    let accounting = match state.savings_ledger().period_for_workspace(
        &query.workspace,
        query.period_days,
        today,
    ) {
        Ok(report) => ProjectAccountingReport::from_workspace(report, today.to_string()),
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wayfinder_router_state_error",
                error.to_string(),
                HeaderMap::new(),
            );
        }
    };
    let delivery = match state.recent().report_for_workspace(&query.workspace) {
        Ok(report) => report,
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wayfinder_router_state_error",
                error.to_string(),
                HeaderMap::new(),
            );
        }
    };
    let actor = actor.as_ref().map_or("local", |actor| actor.0.0.as_str());
    if let Err(error) = state
        .record_audit(
            actor,
            "project-value.export",
            None,
            Some(json!({
                "workspace_id": query.workspace.clone(),
                "period_days": query.period_days
            })),
        )
        .await
    {
        return audit_unavailable_response(error);
    }
    let generated_at_ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64());
    Json(ProjectValueReport::new(
        query.workspace,
        generated_at_ts,
        accounting,
        delivery,
        state.price_table_version().to_owned(),
        &state.price_table().costs,
    ))
    .into_response()
}

fn project_value_query(uri: &Uri) -> Result<ProjectValueQuery, String> {
    let mut workspace = None;
    let mut period = Some(30_u32);
    for pair in uri.query().unwrap_or_default().split('&') {
        if pair.is_empty() {
            continue;
        }
        let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode_query_component(raw_key)
            .map_err(|_| "project value query contains invalid URL encoding".to_owned())?;
        let value = decode_query_component(raw_value)
            .map_err(|_| "project value query contains invalid URL encoding".to_owned())?;
        match key.as_str() {
            "workspace" => workspace = Some(value),
            "period" => {
                period = match value.as_str() {
                    "today" => Some(1),
                    "7d" => Some(7),
                    "30d" => Some(30),
                    "all" => None,
                    _ => {
                        return Err("period must be one of today, 7d, 30d, or all".to_owned());
                    }
                };
            }
            _ => {}
        }
    }
    let workspace = workspace.ok_or_else(|| "workspace is required".to_owned())?;
    if workspace.is_empty()
        || workspace.len() > 128
        || !workspace.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
    {
        return Err("workspace must be 1..=128 visible ASCII bytes".to_owned());
    }
    Ok(ProjectValueQuery {
        workspace,
        period_days: period,
    })
}

fn savings_period(uri: &Uri) -> Option<u32> {
    let mut period = None;
    for pair in uri.query().unwrap_or_default().split('&') {
        if pair.is_empty() {
            continue;
        }
        let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
        if decode_query_component(raw_key).ok().as_deref() != Some("period") {
            continue;
        }
        period = decode_query_component(raw_value).ok();
    }
    match period.as_deref().unwrap_or("all") {
        "today" => Some(1),
        "7d" => Some(7),
        "30d" => Some(30),
        _ => None,
    }
}

async fn router_config(
    State(state): State<AppState>,
    actor: Option<Extension<audit::AuditActor>>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(rejection) => return body_rejection_response(&state, rejection),
    };
    let body = if body.is_empty() {
        serde_json::Map::new()
    } else {
        match parse_chat_body(&body) {
            Ok(body) => body,
            Err(response) => return *response,
        }
    };
    let body = Value::Object(body);
    let override_value = body.get(TUNING_FIELD).or_else(|| {
        body.as_object()
            .is_some_and(|body| !body.is_empty())
            .then_some(&body)
    });
    let tuned = match apply_scoring_overrides(state.routing(), override_value) {
        Ok(tuned) => tuned,
        Err(error) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "wayfinder_router_bad_override",
                error.to_string(),
                HeaderMap::new(),
            );
        }
    };
    let fields = body
        .as_object()
        .map(|body| body.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let actor = actor.as_ref().map_or("local", |actor| actor.0.0.as_str());
    if let Err(error) = state
        .record_audit(
            actor,
            "config.export",
            None,
            Some(json!({"fields": fields})),
        )
        .await
    {
        return audit_unavailable_response(error);
    }
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        dump_routing_toml(&tuned),
    )
        .into_response()
}

fn audit_unavailable_response(error: audit::AuditError) -> Response {
    tracing::error!(error = %error, "operator audit event was not persisted");
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "wayfinder_router_audit_unavailable",
        "operator audit persistence is unavailable".to_owned(),
        HeaderMap::new(),
    )
}

fn recent_limit(uri: &Uri) -> Result<i64, String> {
    let mut limit = default_recent_limit();
    for pair in uri.query().unwrap_or_default().split('&') {
        if pair.is_empty() {
            continue;
        }
        let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode_query_component(raw_key)?;
        if key != "limit" {
            continue;
        }
        let value = decode_query_component(raw_value)?;
        limit = value.parse::<i64>().map_err(|_| value)?;
    }
    Ok(limit)
}

fn decode_query_component(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                let high = hex_digit(bytes[index + 1]).ok_or_else(|| value.to_owned())?;
                let low = hex_digit(bytes[index + 2]).ok_or_else(|| value.to_owned())?;
                decoded.push((high << 4) | low);
                index += 2;
            }
            b'%' => return Err(value.to_owned()),
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8(decoded).map_err(|_| value.to_owned())
}

const fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    message: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct DecisionEnvelope {
    wayfinder: DecisionResponse,
}

#[derive(Serialize)]
struct DecisionResponse {
    model: String,
    score: f64,
    mode: &'static str,
    offline: bool,
    request_id: String,
    features: Features,
    contributions: Vec<FeatureContribution>,
    tiers: Option<Vec<Tier>>,
    cost: CostResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    dry_run: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision_only: Option<bool>,
}

#[derive(Serialize)]
struct CostResponse {
    per_call: f64,
    baseline: f64,
    saved: f64,
    unit: &'static str,
    estimated: bool,
    word_count: u64,
}

#[cfg_attr(
    feature = "otel",
    tracing::instrument(skip_all, name = "wayfinder.chat")
)]
pub(crate) async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(rejection) => return body_rejection_response(&state, rejection),
    };
    let mut body = match parse_chat_body(&body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    let request_id = new_request_id();
    if let Some(message) = modalities::unsupported_text_surface(&body) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "wayfinder_router_unsupported_modality",
            message,
            request_id_headers(&request_id),
        );
    }
    let access_grant = match preflight_access(&state, &headers, &request_id).await {
        Ok(grant) => grant,
        Err(response) => return *response,
    };
    let key_id = state
        .access_policy()
        .and_then(|policy| access_grant.and_then(|grant| policy.key_id(grant)));
    let workspace_id = state
        .access_policy()
        .and_then(|policy| access_grant.and_then(|grant| policy.workspace_id(grant)));
    let resolved_profile = state.resolve_policy_profile(BindingContext {
        client_id: None,
        workspace_id,
        key_id,
    });
    let policy_profile_id = resolved_profile.map(|profile| profile.profile_id);
    let base_routing = resolved_profile.map_or_else(|| state.routing(), |profile| profile.routing);
    let options =
        match resolve_request_options(&state, &headers, &mut body, base_routing, &request_id) {
            Ok(options) => options,
            Err(response) => return *response,
        };
    let offline = options.offline;
    let privacy_posture = options.privacy_posture;
    let prompt_estimate = options.prompt_estimate;
    let decision_started = Instant::now();
    let decision = match score_request(&options.prompt, &options.routing, &request_id) {
        Ok(decision) => decision,
        Err(response) => return *response,
    };
    let mut route = match select_route(
        &state,
        &headers,
        body.get("model"),
        &options,
        &decision,
        &request_id,
    ) {
        Ok(route) => route,
        Err(response) => return *response,
    };
    if let Err(response) =
        enforce_destination_eligibility(&state, access_grant, &options, &request_id, &mut route)
    {
        return *response;
    }

    let access = RequestAccess {
        grant: access_grant,
        key_id,
        workspace_id,
    };
    let mut budget =
        match apply_budget(&state, access, &options, &decision, &request_id, &mut route).await {
            Ok(budget) => budget,
            Err(response) => return *response,
        };
    clamp_route_to_key(&state, access_grant, &decision, &mut route);
    if budget.rerouted_from.as_deref() == Some(route.chosen.as_str()) {
        budget.state = Some("exhausted");
    }
    let canary_guard =
        if !state.dry_run() && !route.preset_route_active && route.mode == RouteMode::Scored {
            maybe_assign_canary(
                &state,
                CanaryRequest {
                    request_id: &request_id,
                    baseline_route: &route.chosen,
                    routing_request: &options.routing_request,
                    offline,
                    prompt_estimate,
                    workspace_id,
                    key_id,
                    access_grant,
                    headers: &headers,
                },
            )
            .await
        } else {
            None
        };
    if let Some(candidate) = canary_guard.as_ref().and_then(|guard| {
        guard
            .context
            .as_ref()
            .map(|context| context.candidate_route.clone())
    }) {
        route.chosen = candidate;
        route.mode = RouteMode::Canary;
    }
    let routing_headers = match build_routing_headers(
        &state,
        RoutingHeaderInput {
            route: &route,
            score: decision.score,
            request_id: &request_id,
            offline,
            privacy_posture,
            policy_profile_id,
            budget_state: budget.state,
            canary_assigned: canary_guard.is_some(),
            access_grant,
        },
    )
    .await
    {
        Ok(headers) => headers,
        Err(response) => return *response,
    };

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64());
    let _ = state.recent().record(RecentEntry {
        request_id: request_id.clone(),
        model: route.chosen.clone(),
        score: python_round(decision.score, 2),
        mode: route.mode.as_str().to_owned(),
        policy_version: (state.policy_version() != UNMANAGED_POLICY_ID)
            .then(|| state.policy_version().to_owned()),
        snapshot_id: (state.policy_snapshot_id() != UNMANAGED_SNAPSHOT_ID)
            .then(|| state.policy_snapshot_id().to_owned()),
        policy_profile: policy_profile_id.map(str::to_owned),
        served_by: None,
        execution_boundary: None,
        outcome: None,
        user_outcome: None,
        http_status: None,
        error_type: None,
        ts: timestamp,
        cost: None,
        key: key_id.map(str::to_owned),
        workspace: workspace_id.map(str::to_owned),
        cache: None,
    });
    let _ = state.metrics().observe_decision(
        &route.chosen,
        route.mode.as_str(),
        decision_started.elapsed().as_secs_f64(),
    );

    state.schedule_shadow(
        &request_id,
        &options.prompt,
        &options.routing_request,
        &route.chosen,
        decision.score,
        &body,
        crate::shadow::provider_consent(&headers),
    );

    if state.dry_run() || state.models().is_empty() {
        return decision_only_response(
            &state,
            &options.routing,
            decision,
            DecisionMetadata {
                chosen: route.chosen,
                mode: route.mode,
                offline,
                request_id,
                headers: routing_headers,
            },
        );
    }

    let (delivery_name, target) =
        match resolve_delivery_target(&state, &decision, &route, offline, &routing_headers) {
            Ok(resolved) => resolved,
            Err(response) => return *response,
        };

    let request_body = Value::Object(body);
    let debug = request_header(&headers, DEBUG_HEADER)
        .ok()
        .flatten()
        .is_some_and(|value| matches!(value.trim().to_lowercase().as_str(), "1" | "true" | "yes"));
    let cache = cache_identity(key_id, privacy_posture, &route, &delivery_name);
    let cache_state = match lookup_cache(
        &state,
        CacheLookup {
            identity: &cache,
            target,
            delivery_name: &delivery_name,
            request_id: &request_id,
            request_body: &request_body,
            debug,
        },
        &routing_headers,
    )
    .await
    {
        Ok(CacheOutcome::Hit(response)) => return *response,
        Ok(CacheOutcome::Miss) => Some("miss"),
        Ok(CacheOutcome::Bypass) => None,
        Err(response) => return *response,
    };

    let candidates = delivery_candidates(
        &state,
        &headers,
        &decision,
        &mut route,
        &delivery_name,
        target,
        offline,
    );
    let plan = match plan_delivery(
        &state,
        access_grant,
        &options,
        &route,
        &delivery_name,
        &candidates,
        &routing_headers,
    ) {
        Ok(plan) => plan,
        Err(response) => return *response,
    };

    let shared_budget = match reserve_shared_budget(
        &state,
        &request_id,
        prompt_estimate,
        &request_body,
        workspace_id,
        key_id,
        offline,
    )
    .await
    {
        SharedBudgetReservationOutcome::Reserved(lease) => Some(lease),
        SharedBudgetReservationOutcome::Denied => {
            let mut headers = request_id_headers(&request_id);
            headers.insert(ROUTER_BUDGET_HEADER, HeaderValue::from_static("blocked"));
            return error_response(
                StatusCode::PAYMENT_REQUIRED,
                "wayfinder_router_budget_exhausted",
                "shared budget reservation exhausted".to_owned(),
                headers,
            );
        }
        SharedBudgetReservationOutcome::Unavailable => None,
    };

    let admission_permit = match acquire_delivery(&state, &request_id).await {
        Ok(permit) => permit,
        Err(error) => return admission_error_response(&state, error, routing_headers),
    };

    let token_reservation =
        if let (Some(policy), Some(grant)) = (state.access_policy(), access_grant) {
            match policy.token_limits_active(grant) {
                Ok(false) => None,
                Ok(true) => {
                    let reserved_tokens = match conservative_token_reservation(&request_body) {
                        Ok(tokens) => tokens,
                        Err(message) => {
                            return error_response(
                                StatusCode::UNPROCESSABLE_ENTITY,
                                "wayfinder_router_token_bound_required",
                                message.to_owned(),
                                routing_headers,
                            );
                        }
                    };
                    match policy.reserve_tokens(grant, reserved_tokens).await {
                        Ok(TokenReservationDecision::Reserved(reservation)) => Some(reservation),
                        Ok(TokenReservationDecision::Rejected(result)) => {
                            return rate_limited_response(&state, result, &request_id);
                        }
                        Err(error) => return access_state_error(&request_id, &error),
                    }
                }
                Err(error) => return access_state_error(&request_id, &error),
            }
        } else {
            None
        };

    if request_body.get("stream").and_then(Value::as_bool) == Some(true) {
        let initial_event = debug.then(|| {
            let envelope = DecisionEnvelope {
                wayfinder: make_decision_response(
                    &state,
                    &options.routing,
                    &decision,
                    &route.chosen,
                    route.mode,
                    offline,
                    &request_id,
                    false,
                    false,
                ),
            };
            let payload = serde_json::to_string(&envelope).unwrap_or_else(|_| {
                "{\"error\":{\"message\":\"decision metadata unavailable\",\"type\":\"wayfinder_router_state_error\"}}".to_owned()
            });
            Bytes::from(format!("data: {payload}\n\n"))
        });
        return streaming_chat_response(StreamingChatInput {
            state,
            headers: routing_headers,
            plan,
            chosen: route.chosen,
            offline,
            routing_request: options.routing_request,
            prompt_estimate,
            request_body,
            request_id,
            messages: options.messages,
            access_grant,
            initial_event,
            admission_permit,
            token_reservation,
            shared_budget,
            canary_guard,
        })
        .await;
    }

    let _admission_permit = admission_permit;
    let _shared_budget = shared_budget;

    let Some(delivery) = state.delivery() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "wayfinder_router_not_ready",
            format!(
                "provider delivery is not configured in Wayfinder Rust gateway build {}",
                state.build_version()
            ),
            routing_headers,
        );
    };
    let delivery_outcome = match deliver_with_reliability(
        &state,
        delivery,
        &plan,
        &options.routing_request,
        offline,
        prompt_estimate,
        &request_body,
    )
    .await
    {
        Ok(delivered) => delivered,
        Err(ReliableDeliveryFailure::Exhausted(message)) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "wayfinder_router_upstream_error",
                message,
                routing_headers,
            );
        }
        Err(ReliableDeliveryFailure::State(error)) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wayfinder_router_state_error",
                error.to_string(),
                routing_headers,
            );
        }
        Err(ReliableDeliveryFailure::Misconfigured(message)) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wayfinder_router_misconfigured",
                message,
                routing_headers,
            );
        }
        Err(ReliableDeliveryFailure::Fatal(error)) => {
            let (status, kind) = fatal_delivery_status(&error);
            return error_response(status, kind, error.to_string(), routing_headers);
        }
    };
    let served_by = delivery_outcome.served_by;
    let served_by_id = delivery_outcome.served_by_id;
    let execution_boundary = delivery_outcome.execution_boundary;
    let selection_reason = delivery_outcome.selection_reason;
    let response = delivery_outcome.response;
    let status = response.status();
    let _ = state.recent().update_delivery(
        &request_id,
        &served_by,
        execution_boundary,
        if status.is_success() {
            RecentOutcome::Succeeded
        } else {
            RecentOutcome::Failed
        },
        Some(status.as_u16()),
        (!status.is_success()).then_some("wayfinder_router_upstream_error"),
    );
    let response_content_type = response.content_type().to_owned();
    let content_type = HeaderValue::from_str(&response_content_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/json"));
    let body = response.into_body();
    let accounted = account_buffered_success(
        &state,
        BufferedAccounting {
            route: &served_by,
            request_id: &request_id,
            messages: &options.messages,
            access_grant,
            status,
            content_type: &response_content_type,
            body: &body,
            token_reservation,
            shared_budget: _shared_budget,
        },
    )
    .await;
    if let Some(mut guard) = canary_guard {
        guard
            .finish(status.is_success(), &served_by, accounted)
            .await;
    }
    if let (Some("miss"), Some(usage)) = (cache_state, accounted) {
        let response_value = if !body.is_empty() && response_content_type.contains("json") {
            serde_json::from_slice(&body).unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        if is_storable(status.as_u16(), &response_content_type, &response_value) {
            if let Some(served_target) = state
                .models()
                .iter()
                .find(|model| model.name() == served_by)
            {
                if served_target.provider() != ProviderKind::CodexAppServer {
                    if let Ok(key) = cache_key_with_version(
                        state.cache_generation(),
                        &cache.partition,
                        &cache.route,
                        served_target.provider_model(),
                        &request_body,
                    ) {
                        let _ = state
                            .put_cached_response(
                                key,
                                CachedResponse {
                                    status: status.as_u16(),
                                    content_type: response_content_type.clone(),
                                    body: body.to_vec(),
                                    prompt_tokens: usage.prompt_tokens,
                                    completion_tokens: usage.completion_tokens,
                                    estimated: usage.estimated,
                                },
                            )
                            .await;
                    }
                }
            }
        }
    }
    let body = if debug {
        inject_debug_decision(
            body,
            make_decision_response(
                &state,
                &options.routing,
                &decision,
                &route.chosen,
                route.mode,
                offline,
                &request_id,
                false,
                false,
            ),
        )
    } else {
        body
    };
    let mut response_headers = routing_headers;
    if let Err(message) = insert_header(
        &mut response_headers,
        "x-wayfinder-router-served-by",
        &served_by,
    ) {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "wayfinder_router_misconfigured",
            message,
            request_id_headers(&request_id),
        );
    }
    if let Err(message) = insert_header(
        &mut response_headers,
        ROUTER_DEPLOYMENT_HEADER,
        &served_by_id,
    ) {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "wayfinder_router_misconfigured",
            message,
            request_id_headers(&request_id),
        );
    }
    if let Err(message) = insert_header(
        &mut response_headers,
        ROUTER_DEPLOYMENT_SELECTION_HEADER,
        selection_reason,
    ) {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "wayfinder_router_misconfigured",
            message,
            request_id_headers(&request_id),
        );
    }
    if let Some(cache_state) = cache_state {
        response_headers.insert(ROUTER_CACHE_HEADER, HeaderValue::from_static(cache_state));
    }
    if served_by != route.chosen && !offline {
        response_headers.insert(ROUTER_FAILOVER_HEADER, HeaderValue::from_static("true"));
    }
    response_headers.insert(header::CONTENT_TYPE, content_type);
    (status, response_headers, body).into_response()
}

fn inject_debug_decision(body: Bytes, decision: DecisionResponse) -> Bytes {
    let Ok(mut value) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };
    let Some(object) = value.as_object_mut() else {
        return body;
    };
    let Ok(decision) = serde_json::to_value(decision) else {
        return body;
    };
    object.insert("wayfinder".to_owned(), decision);
    serde_json::to_vec(&value).map_or(body, Bytes::from)
}

struct ObservedAdmissionPermit {
    _permit: AdmissionPermit,
    metrics: Arc<GatewayMetrics>,
    _shared: Option<SharedAdmissionLease>,
}

impl Drop for ObservedAdmissionPermit {
    fn drop(&mut self) {
        let _ = self.metrics.observe_admission_release();
    }
}

struct SharedAdmissionLease {
    backend: Arc<dyn StateBackend>,
    lease_id: String,
}

impl Drop for SharedAdmissionLease {
    fn drop(&mut self) {
        let backend = Arc::clone(&self.backend);
        let lease_id = self.lease_id.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            let _ = backend.release_admission(&lease_id).await;
        });
    }
}

/// A fleet budget reservation held until the request reaches a terminal
/// accounting state. Failed or cancelled requests release it; successful
/// requests settle it immediately after writing realized usage.
struct SharedBudgetLease {
    backend: Arc<dyn StateBackend>,
    reservation: Option<SharedBudgetReservation>,
    settled: bool,
}

enum SharedBudgetReservationOutcome {
    Reserved(SharedBudgetLease),
    Denied,
    Unavailable,
}

impl SharedBudgetLease {
    async fn settle(mut self) {
        if let Some(reservation) = self.reservation.take() {
            let _ = self.backend.settle_budget(reservation).await;
        }
        self.settled = true;
    }
}

impl Drop for SharedBudgetLease {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let Some(reservation) = self.reservation.take() else {
            return;
        };
        let backend = Arc::clone(&self.backend);
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            let _ = backend.release_budget(reservation).await;
        });
    }
}

async fn reserve_shared_budget(
    state: &AppState,
    request_id: &str,
    prompt_estimate: u64,
    request_body: &Value,
    workspace_id: Option<&str>,
    key_id: Option<&str>,
    offline: bool,
) -> SharedBudgetReservationOutcome {
    let Some(backend) = state
        .shared_state_backend()
        .filter(|backend| backend.backend_name() == "redis")
    else {
        return SharedBudgetReservationOutcome::Unavailable;
    };
    if !state.price_table().priced {
        return SharedBudgetReservationOutcome::Unavailable;
    }
    let scopes = state
        .budget_policy()
        .shared_scopes(workspace_id, key_id, offline);
    if scopes.is_empty() {
        return SharedBudgetReservationOutcome::Unavailable;
    }
    let completions = request_body
        .get("n")
        .and_then(Value::as_u64)
        .filter(|count| *count > 0)
        .unwrap_or(1);
    let requested_output = requested_output_tokens(request_body).max(1_024);
    let output_tokens = requested_output.saturating_mul(completions);
    // Reserve against the configured frontier rate rather than the first
    // route: a later provider failover must not bypass a hard cap.
    let Ok(cost) = turn_cost(
        "",
        i64::try_from(prompt_estimate).unwrap_or(i64::MAX),
        i64::try_from(output_tokens).unwrap_or(i64::MAX),
        &state.price_table().costs,
        true,
        None,
    ) else {
        return SharedBudgetReservationOutcome::Unavailable;
    };
    let Ok(date) = utc_today() else {
        return SharedBudgetReservationOutcome::Unavailable;
    };
    let reservation = SharedBudgetReservation {
        request_id: request_id.to_owned(),
        date: date.to_string(),
        amount: cost.realized,
        scopes,
    };
    match backend.reserve_budget(reservation.clone()).await {
        Ok(true) => SharedBudgetReservationOutcome::Reserved(SharedBudgetLease {
            backend,
            reservation: Some(reservation),
            settled: false,
        }),
        Ok(false) => SharedBudgetReservationOutcome::Denied,
        Err(_) => {
            let _ = state.metrics().set_state_degraded(true);
            SharedBudgetReservationOutcome::Unavailable
        }
    }
}

async fn acquire_delivery(
    state: &AppState,
    request_id: &str,
) -> Result<ObservedAdmissionPermit, AdmissionError> {
    let permit = state.admission_policy().acquire().await?;
    let shared = if let Some(backend) = state
        .shared_state_backend()
        .filter(|backend| backend.backend_name() == "redis")
    {
        match backend
            .acquire_admission(
                state.admission_policy().max_in_flight() as u64,
                request_id,
                SHARED_ADMISSION_TTL_MS,
            )
            .await
        {
            Ok(true) => Some(SharedAdmissionLease {
                backend,
                lease_id: request_id.to_owned(),
            }),
            Ok(false) => {
                drop(permit);
                return Err(AdmissionError::SharedLimit);
            }
            Err(_) => {
                let _ = state.metrics().set_state_degraded(true);
                None
            }
        }
    } else {
        None
    };
    let metrics = Arc::clone(&state.metrics);
    let _ = metrics.observe_admission(permit.waited().as_secs_f64());
    Ok(ObservedAdmissionPermit {
        _permit: permit,
        metrics,
        _shared: shared,
    })
}

/// Fleet-wide circuit lease paired with the process-local reliability attempt.
/// Redis failures degrade to local state, but never make a request look
/// healthy: an unfinished lease is conservatively failed when it is dropped.
struct SharedHealthGuard {
    backend: Arc<dyn StateBackend>,
    target: String,
    threshold: u64,
    cooldown_ms: u64,
    probe_id: String,
    completed: bool,
}

impl SharedHealthGuard {
    async fn complete(mut self, succeeded: bool) {
        self.completed = true;
        let _ = self
            .backend
            .complete_provider_health(
                &self.target,
                self.threshold,
                self.cooldown_ms,
                &self.probe_id,
                succeeded,
            )
            .await;
    }

    async fn release(mut self) {
        self.completed = true;
        let _ = self
            .backend
            .release_provider_health(&self.target, &self.probe_id)
            .await;
    }

    fn release_in_background(mut self) {
        self.completed = true;
        let backend = Arc::clone(&self.backend);
        let target = self.target.clone();
        let probe_id = self.probe_id.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            let _ = backend.release_provider_health(&target, &probe_id).await;
        });
    }

    fn complete_in_background(mut self, succeeded: bool) {
        self.completed = true;
        let backend = Arc::clone(&self.backend);
        let target = self.target.clone();
        let threshold = self.threshold;
        let cooldown_ms = self.cooldown_ms;
        let probe_id = self.probe_id.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            let _ = backend
                .complete_provider_health(&target, threshold, cooldown_ms, &probe_id, succeeded)
                .await;
        });
    }
}

impl Drop for SharedHealthGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let backend = Arc::clone(&self.backend);
        let target = self.target.clone();
        let threshold = self.threshold;
        let cooldown_ms = self.cooldown_ms;
        let probe_id = self.probe_id.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            let _ = backend
                .complete_provider_health(&target, threshold, cooldown_ms, &probe_id, false)
                .await;
        });
    }
}

struct CombinedReliabilityAttempt {
    local: Option<ReliabilityAttempt>,
    shared: Option<SharedHealthGuard>,
}

impl CombinedReliabilityAttempt {
    async fn complete(mut self, succeeded: bool) -> Result<(), ReliabilityError> {
        let local = self.local.take().ok_or(ReliabilityError::LockPoisoned)?;
        let result = local.complete(succeeded);
        if let Some(shared) = self.shared.take() {
            shared.complete(succeeded).await;
        }
        result
    }

    fn complete_in_background(mut self, succeeded: bool) -> Result<(), ReliabilityError> {
        let local = self.local.take().ok_or(ReliabilityError::LockPoisoned)?;
        let result = local.complete(succeeded);
        if let Some(shared) = self.shared.take() {
            shared.complete_in_background(succeeded);
        }
        result
    }

    fn release_in_background(mut self) {
        self.local.take();
        if let Some(shared) = self.shared.take() {
            shared.release_in_background();
        }
    }
}

/// Acquire both the local and Redis circuit leases. The local breaker remains
/// authoritative when Redis is unavailable, with degraded state exposed via
/// metrics and health endpoints.
async fn begin_reliability_attempt(
    state: &AppState,
    target: &str,
    probe_id: &str,
) -> Result<Option<CombinedReliabilityAttempt>, ReliableDeliveryFailure> {
    let shared = if let Some(backend) = state
        .shared_state_backend()
        .filter(|backend| backend.backend_name() == "redis")
    {
        match backend
            .begin_provider_health(
                target,
                state.reliability_policy().breaker_threshold(),
                state.reliability_policy().breaker_cooldown_ms(),
                probe_id,
            )
            .await
        {
            Ok(true) => Some(SharedHealthGuard {
                backend,
                target: target.to_owned(),
                threshold: state.reliability_policy().breaker_threshold(),
                cooldown_ms: state.reliability_policy().breaker_cooldown_ms(),
                probe_id: probe_id.to_owned(),
                completed: false,
            }),
            Ok(false) => return Ok(None),
            Err(_) => {
                let _ = state.metrics().set_state_degraded(true);
                None
            }
        }
    } else {
        None
    };

    let local = match state.reliability_policy().begin_attempt(target) {
        Ok(Some(attempt)) => attempt,
        Ok(None) => {
            if let Some(shared) = shared {
                shared.release().await;
            }
            return Ok(None);
        }
        Err(error) => {
            if let Some(shared) = shared {
                shared.release().await;
            }
            return Err(ReliableDeliveryFailure::State(error));
        }
    };
    Ok(Some(CombinedReliabilityAttempt {
        local: Some(local),
        shared,
    }))
}

struct StreamRelayContext {
    state: AppState,
    target_name: String,
    target_id: String,
    request_id: String,
    messages: Value,
    access_grant: Option<AccessGrant>,
    decoder: SseDecoder,
    completion_chars: u64,
    ttft_seconds: Option<f64>,
    started: Option<Instant>,
    signal_recorded: bool,
    initial_event: Option<Bytes>,
    token_reservation: Option<TokenReservation>,
    reliability_attempt: Option<CombinedReliabilityAttempt>,
    _admission_permit: ObservedAdmissionPermit,
    shared_budget: Option<SharedBudgetLease>,
    canary_guard: Option<CanaryObservationGuard>,
}

impl Drop for StreamRelayContext {
    fn drop(&mut self) {
        if !self.signal_recorded {
            let _ = self.state.recent().update_outcome(
                &self.request_id,
                RecentOutcome::Cancelled,
                None,
                Some("wayfinder_router_cancelled"),
            );
        }
    }
}

struct StreamingChatInput {
    state: AppState,
    headers: HeaderMap,
    plan: Vec<String>,
    chosen: String,
    offline: bool,
    routing_request: RoutingRequest,
    prompt_estimate: u64,
    request_body: Value,
    request_id: String,
    messages: Value,
    access_grant: Option<AccessGrant>,
    initial_event: Option<Bytes>,
    admission_permit: ObservedAdmissionPermit,
    token_reservation: Option<TokenReservation>,
    shared_budget: Option<SharedBudgetLease>,
    canary_guard: Option<CanaryObservationGuard>,
}

#[cfg_attr(
    feature = "otel",
    tracing::instrument(skip_all, name = "wayfinder.delivery.stream")
)]
async fn streaming_chat_response(input: StreamingChatInput) -> Response {
    let StreamingChatInput {
        state,
        mut headers,
        plan,
        chosen,
        offline,
        routing_request,
        prompt_estimate,
        request_body,
        request_id,
        messages,
        access_grant,
        initial_event,
        admission_permit,
        token_reservation,
        shared_budget,
        canary_guard,
    } = input;
    if state.streaming_delivery().is_none() {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "wayfinder_router_streaming_not_ready",
            format!(
                "streaming provider delivery is not configured in Wayfinder Rust gateway build {}",
                state.build_version()
            ),
            headers,
        );
    }
    let mut last_error = "upstream stream could not be established".to_owned();
    let mut last_failure = (StatusCode::BAD_GATEWAY, "wayfinder_router_upstream_error");
    let mut established = None;
    for target_name in &plan {
        let configured_targets = state.deployment_candidates(target_name);
        if configured_targets.is_empty() {
            last_error = format!("configured delivery target '{target_name}' is missing");
            continue;
        }
        let eligible_targets = eligible_deployment_targets(
            configured_targets,
            &routing_request,
            offline,
            prompt_estimate,
        );
        if eligible_targets.is_empty() {
            last_error = format!(
                "configured delivery target '{target_name}' has no eligible deployment for this request"
            );
            continue;
        }
        let selection = state.select_deployments(target_name, eligible_targets);
        let targets = selection.targets;
        if targets.is_empty() {
            last_error = format!(
                "configured delivery target '{target_name}' is excluded by its deployment price ceiling"
            );
            continue;
        }
        let Some(delivery) = state.streaming_delivery() else {
            break;
        };
        for target in targets {
            let target_id = target.delivery_id();
            if target.deployment_id.is_some() && !target.key_ready() {
                last_error = format!("deployment '{target_id}' credential is unavailable");
                continue;
            }
            let reliability_attempt =
                match begin_reliability_attempt(&state, &target_id, &request_id).await {
                    Ok(Some(attempt)) => attempt,
                    Ok(None) => continue,
                    Err(ReliableDeliveryFailure::State(error)) => {
                        return error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "wayfinder_router_state_error",
                            error.to_string(),
                            request_id_headers(&request_id),
                        );
                    }
                    Err(_) => continue,
                };
            let mut reliability_attempt = Some(reliability_attempt);
            let delays = state.reliability_policy().retry_delays();
            for attempt_index in 0..=state.reliability_policy().retries() {
                let attempt_started = Instant::now();
                match delivery.send_stream(&target, request_body.clone()).await {
                    Ok(response) if response.status().is_success() => {
                        let attempt = match reliability_attempt.take() {
                            Some(attempt) => attempt,
                            None => {
                                return error_response(
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    "wayfinder_router_state_error",
                                    "gateway reliability attempt state is unavailable".to_owned(),
                                    request_id_headers(&request_id),
                                );
                            }
                        };
                        established = Some((
                            target_name.clone(),
                            target_id.clone(),
                            model_execution_boundary(&target),
                            selection.reason,
                            response,
                            attempt,
                            attempt_started,
                        ));
                        break;
                    }
                    Ok(response) if is_retryable(Some(response.status().as_u16())) => {
                        last_error = format!("upstream returned {}", response.status().as_u16());
                        state.observe_deployment(
                            &target_id,
                            DeploymentObservation::failure(
                                Some(attempt_started.elapsed().as_secs_f64()),
                                Some(response.status().as_u16()),
                            ),
                        );
                        let _ = state.metrics().observe_upstream_error(&target_id);
                    }
                    Ok(response) => {
                        let status = response.status();
                        let _ = state.recent().update_delivery(
                            &request_id,
                            target_name,
                            model_execution_boundary(&target),
                            RecentOutcome::Failed,
                            Some(status.as_u16()),
                            Some("wayfinder_router_upstream_error"),
                        );
                        state.observe_deployment(
                            &target_id,
                            DeploymentObservation::failure(
                                Some(attempt_started.elapsed().as_secs_f64()),
                                Some(status.as_u16()),
                            ),
                        );
                        let succeeded = !is_auth_failure(Some(status.as_u16()));
                        if let Some(attempt) = reliability_attempt.take() {
                            if let Err(error) = attempt.complete(succeeded).await {
                                return error_response(
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    "wayfinder_router_state_error",
                                    error.to_string(),
                                    request_id_headers(&request_id),
                                );
                            }
                        }
                        let content_type = HeaderValue::from_str(response.content_type())
                            .unwrap_or_else(|_| {
                                HeaderValue::from_static("application/octet-stream")
                            });
                        headers.insert(header::CONTENT_TYPE, content_type);
                        if let Err(message) =
                            insert_header(&mut headers, "x-wayfinder-router-served-by", target_name)
                        {
                            return error_response(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "wayfinder_router_misconfigured",
                                message,
                                request_id_headers(&request_id),
                            );
                        }
                        return (status, headers, Body::from_stream(response.into_stream()))
                            .into_response();
                    }
                    Err(error) => {
                        last_error = error.to_string();
                        last_failure = fatal_delivery_status(&error);
                        state.observe_deployment(
                            &target_id,
                            DeploymentObservation::failure(
                                Some(attempt_started.elapsed().as_secs_f64()),
                                None,
                            ),
                        );
                        let _ = state.metrics().observe_upstream_error(&target_id);
                        if !matches!(error, DeliveryError::Provider(ProviderError::Transport)) {
                            if stream_failure_affects_reliability(&error) {
                                if let Some(attempt) = reliability_attempt.take() {
                                    if let Err(error) = attempt.complete(false).await {
                                        return error_response(
                                            StatusCode::INTERNAL_SERVER_ERROR,
                                            "wayfinder_router_state_error",
                                            error.to_string(),
                                            request_id_headers(&request_id),
                                        );
                                    }
                                }
                            } else if let Some(attempt) = reliability_attempt.take() {
                                attempt.release_in_background();
                            }
                            return error_response(
                                last_failure.0,
                                last_failure.1,
                                last_error,
                                headers,
                            );
                        }
                    }
                }
                if attempt_index < state.reliability_policy().retries() {
                    if let Some(delay) = delays.get(attempt_index) {
                        sleep_retry(*delay).await;
                    }
                } else if let Some(attempt) = reliability_attempt.take() {
                    if let Err(error) = attempt.complete(false).await {
                        return error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "wayfinder_router_state_error",
                            error.to_string(),
                            request_id_headers(&request_id),
                        );
                    }
                }
            }
            // A stream is already open on this deployment: do not call the
            // remaining deployments of the pool as well.
            if established.is_some() {
                break;
            }
        }
        if established.is_some() {
            break;
        }
    }
    let Some((
        served_by,
        served_by_id,
        execution_boundary,
        selection_reason,
        response,
        reliability_attempt,
        attempt_started,
    )) = established
    else {
        let _ = state.recent().update_outcome(
            &request_id,
            RecentOutcome::Failed,
            None,
            Some(last_failure.1),
        );
        return error_response(last_failure.0, last_failure.1, last_error, headers);
    };
    let _ = state.recent().update_delivery(
        &request_id,
        &served_by,
        execution_boundary,
        RecentOutcome::Streaming,
        Some(StatusCode::OK.as_u16()),
        None,
    );
    if let Err(message) = insert_header(&mut headers, "x-wayfinder-router-served-by", &served_by) {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "wayfinder_router_misconfigured",
            message,
            request_id_headers(&request_id),
        );
    }
    if let Err(message) = insert_header(&mut headers, ROUTER_DEPLOYMENT_HEADER, &served_by_id) {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "wayfinder_router_misconfigured",
            message,
            request_id_headers(&request_id),
        );
    }
    if let Err(message) = insert_header(
        &mut headers,
        ROUTER_DEPLOYMENT_SELECTION_HEADER,
        selection_reason,
    ) {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "wayfinder_router_misconfigured",
            message,
            request_id_headers(&request_id),
        );
    }
    if served_by != chosen && !offline {
        headers.insert(ROUTER_FAILOVER_HEADER, HeaderValue::from_static("true"));
    }
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    let context = StreamRelayContext {
        state,
        target_name: served_by,
        target_id: served_by_id,
        request_id,
        messages,
        access_grant,
        decoder: SseDecoder::default(),
        completion_chars: 0,
        ttft_seconds: None,
        started: Some(attempt_started),
        signal_recorded: false,
        initial_event,
        token_reservation,
        reliability_attempt: Some(reliability_attempt),
        _admission_permit: admission_permit,
        shared_budget,
        canary_guard,
    };
    let body_stream = stream::unfold(
        Some((context, response.into_stream())),
        |phase| async move {
            let (mut context, mut upstream) = phase?;
            if let Some(event) = context.initial_event.take() {
                return Some((Ok::<Bytes, Infallible>(event), Some((context, upstream))));
            }
            match upstream.next().await {
                Some(Ok(chunk)) => match context.decoder.push(&chunk) {
                    Ok(events) => {
                        observe_stream_events(
                            &events,
                            &mut context.completion_chars,
                            &mut context.ttft_seconds,
                            context.started,
                        );
                        Some((Ok::<Bytes, Infallible>(chunk), Some((context, upstream))))
                    }
                    Err(error) => {
                        let chunk = stream_failure_chunk(
                            &mut context,
                            &error.to_string(),
                            "wayfinder_router_upstream_error",
                            true,
                        );
                        Some((Ok::<Bytes, Infallible>(chunk), None))
                    }
                },
                Some(Err(error)) => {
                    let chunk = stream_failure_chunk(
                        &mut context,
                        &error.to_string(),
                        stream_failure_type(&error),
                        stream_failure_affects_reliability(&error),
                    );
                    Some((Ok::<Bytes, Infallible>(chunk), None))
                }
                None => match context.decoder.finish() {
                    Ok(events) => {
                        observe_stream_events(
                            &events,
                            &mut context.completion_chars,
                            &mut context.ttft_seconds,
                            context.started,
                        );
                        finish_stream_success(&mut context).await;
                        None
                    }
                    Err(error) => {
                        let chunk = stream_failure_chunk(
                            &mut context,
                            &error.to_string(),
                            "wayfinder_router_upstream_error",
                            true,
                        );
                        Some((Ok::<Bytes, Infallible>(chunk), None))
                    }
                },
            }
        },
    );
    (StatusCode::OK, headers, Body::from_stream(body_stream)).into_response()
}

fn observe_stream_events(
    events: &[SseEvent],
    completion_chars: &mut u64,
    ttft_seconds: &mut Option<f64>,
    started: Option<Instant>,
) {
    for event in events {
        if event.data.trim() == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        let Some(choices) = value.get("choices").and_then(Value::as_array) else {
            continue;
        };
        for content in choices.iter().filter_map(|choice| {
            choice
                .get("delta")
                .and_then(Value::as_object)
                .and_then(|delta| delta.get("content"))
                .and_then(Value::as_str)
        }) {
            if !content.is_empty() && ttft_seconds.is_none() {
                *ttft_seconds = started.map(|started| started.elapsed().as_secs_f64());
            }
            *completion_chars = completion_chars
                .saturating_add(u64::try_from(content.chars().count()).unwrap_or(u64::MAX));
        }
    }
}

async fn finish_stream_success(context: &mut StreamRelayContext) {
    let elapsed = context
        .started
        .map_or(0.0, |started| started.elapsed().as_secs_f64());
    let _ = context
        .state
        .metrics()
        .observe_upstream(&context.target_id, elapsed);
    let throughput = (context.completion_chars > 0 && elapsed > 0.0)
        .then(|| context.completion_chars as f64 / elapsed);
    context.state.observe_deployment(
        &context.target_id,
        DeploymentObservation::success(elapsed, context.ttft_seconds, throughput),
    );
    context.signal_recorded = true;
    if let Some(attempt) = context.reliability_attempt.take() {
        let _ = attempt.complete(true).await;
    }
    let usage = account_stream_success(
        &context.state,
        StreamAccounting {
            route: &context.target_name,
            request_id: &context.request_id,
            messages: &context.messages,
            access_grant: context.access_grant,
            completion_chars: context.completion_chars,
            token_reservation: context.token_reservation.take(),
            shared_budget: context.shared_budget.take(),
        },
    )
    .await;
    if let Some(mut guard) = context.canary_guard.take() {
        guard.finish(true, &context.target_name, usage).await;
    }
    let _ = context.state.recent().update_outcome(
        &context.request_id,
        RecentOutcome::Succeeded,
        Some(StatusCode::OK.as_u16()),
        None,
    );
}

fn stream_failure_type(error: &DeliveryError) -> &'static str {
    match error {
        DeliveryError::Codex(
            delivery::CodexDeliveryError::Unavailable
            | delivery::CodexDeliveryError::AuthenticationRequired
            | delivery::CodexDeliveryError::ModelUnavailable,
        ) => "wayfinder_router_not_ready",
        DeliveryError::Codex(delivery::CodexDeliveryError::InvalidRequest) => {
            "wayfinder_router_unsupported_request"
        }
        DeliveryError::Codex(delivery::CodexDeliveryError::Busy) => "wayfinder_router_busy",
        DeliveryError::Codex(delivery::CodexDeliveryError::TurnFailed) => {
            "wayfinder_router_turn_failed"
        }
        DeliveryError::Codex(delivery::CodexDeliveryError::Interrupted) => {
            "wayfinder_router_interrupted"
        }
        DeliveryError::Codex(delivery::CodexDeliveryError::UsageLimitReached) => {
            "wayfinder_router_usage_limited"
        }
        _ => "wayfinder_router_upstream_error",
    }
}

fn stream_failure_affects_reliability(error: &DeliveryError) -> bool {
    !matches!(
        error,
        DeliveryError::Codex(
            delivery::CodexDeliveryError::AuthenticationRequired
                | delivery::CodexDeliveryError::ModelUnavailable
                | delivery::CodexDeliveryError::Busy
                | delivery::CodexDeliveryError::UsageLimitReached
                | delivery::CodexDeliveryError::InvalidRequest
                | delivery::CodexDeliveryError::Interrupted
        )
    )
}

fn stream_failure_chunk(
    context: &mut StreamRelayContext,
    message: &str,
    error_type: &str,
    affects_reliability: bool,
) -> Bytes {
    let _ = context.state.recent().update_outcome(
        &context.request_id,
        RecentOutcome::Failed,
        None,
        Some(error_type),
    );
    if !context.signal_recorded {
        let elapsed = context
            .started
            .map(|started| started.elapsed().as_secs_f64());
        context.state.observe_deployment(
            &context.target_id,
            DeploymentObservation::failure(elapsed, None),
        );
        context.signal_recorded = true;
    }
    let _ = context
        .state
        .metrics()
        .observe_upstream_error(&context.target_id);
    if affects_reliability {
        if let Some(attempt) = context.reliability_attempt.take() {
            let _ = attempt.complete_in_background(false);
        }
    } else if let Some(attempt) = context.reliability_attempt.take() {
        attempt.release_in_background();
    }
    let payload = serde_json::to_string(&json!({
        "error": {
            "message": message,
            "type": error_type
        }
    }))
    .unwrap_or_else(|_| {
        "{\"error\":{\"message\":\"upstream error\",\"type\":\"wayfinder_router_upstream_error\"}}"
            .to_owned()
    });
    Bytes::from(format!("data: {payload}\n\ndata: [DONE]\n\n"))
}

async fn anthropic_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(rejection) => return body_rejection_response(&state, rejection),
    };
    let body = match parse_chat_body(&body) {
        Ok(body) => Value::Object(body),
        Err(response) => return *response,
    };
    let model_echo = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let openai_body = anthropic_to_openai_request(&body);
    let prompt_text = extract_prompt(
        openai_body.get("messages").unwrap_or(&Value::Null),
        RouteOn::All,
    );
    let encoded = match serde_json::to_vec(&openai_body) {
        Ok(encoded) => encoded,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(anthropic_error(500, "request translation failed")),
            )
                .into_response();
        }
    };
    let mut inner_headers = chat_control_headers(&headers);
    if let Some(authorization) = headers.get(header::AUTHORIZATION) {
        inner_headers.insert(header::AUTHORIZATION, authorization.clone());
    }
    let streaming = openai_body.get("stream").and_then(Value::as_bool) == Some(true);
    let inner = chat_completions(State(state), inner_headers, Ok(Bytes::from(encoded))).await;
    let status = inner.status();
    let mut output_headers = HeaderMap::new();
    for (name, value) in inner.headers() {
        if name.as_str().starts_with("x-wayfinder") {
            output_headers.insert(name.clone(), value.clone());
        }
    }
    let request_id = output_headers
        .get(ROUTER_REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let message_id = if request_id.is_empty() {
        "msg_unknown".to_owned()
    } else {
        format!("msg_{request_id}")
    };
    if streaming && status.is_success() {
        output_headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
        let input_tokens = if prompt_text.is_empty() {
            0
        } else {
            u64::try_from(prompt_text.chars().count() / 4)
                .unwrap_or(u64::MAX)
                .max(1)
        };
        let mut translator = MessagesStreamTranslator::new(model_echo, message_id, input_tokens);
        let pending = VecDeque::from(translator.start());
        let relay = AnthropicRelay {
            upstream: inner.into_body().into_data_stream(),
            translator,
            pending,
            finished: false,
        };
        let translated = stream::unfold(relay, |mut relay| async move {
            loop {
                if let Some(frame) = relay.pending.pop_front() {
                    return Some((Ok::<Bytes, Infallible>(Bytes::from(frame)), relay));
                }
                if relay.finished {
                    return None;
                }
                match relay.upstream.next().await {
                    Some(Ok(chunk)) => match relay.translator.push(&chunk) {
                        Ok(frames) => relay.pending.extend(frames),
                        Err(error) => {
                            relay
                                .pending
                                .push_back(anthropic_stream_error_frame(&error.to_string()));
                            relay.finished = true;
                        }
                    },
                    Some(Err(error)) => {
                        relay
                            .pending
                            .push_back(anthropic_stream_error_frame(&error.to_string()));
                        relay.finished = true;
                    }
                    None => {
                        match relay.translator.finish() {
                            Ok(frames) => relay.pending.extend(frames),
                            Err(error) => relay
                                .pending
                                .push_back(anthropic_stream_error_frame(&error.to_string())),
                        }
                        relay.finished = true;
                    }
                }
            }
        });
        return (status, output_headers, Body::from_stream(translated)).into_response();
    }
    let raw = match to_bytes(inner.into_body(), DEFAULT_MAX_RESPONSE_BYTES).await {
        Ok(raw) => raw,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                output_headers,
                Json(anthropic_error(500, "upstream error")),
            )
                .into_response();
        }
    };
    let parsed = (!raw.is_empty())
        .then(|| serde_json::from_slice::<Value>(&raw).ok())
        .flatten();
    if status.as_u16() >= 400 || !parsed.as_ref().is_some_and(Value::is_object) {
        let message = parsed
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(Value::as_object)
            .and_then(|error| error.get("message"))
            .map_or_else(
                || {
                    let decoded = String::from_utf8_lossy(&raw);
                    let truncated = decoded.chars().take(500).collect::<String>();
                    if truncated.is_empty() {
                        "upstream error".to_owned()
                    } else {
                        truncated
                    }
                },
                |message| {
                    message
                        .as_str()
                        .map_or_else(|| message.to_string(), str::to_owned)
                },
            );
        return (
            status,
            output_headers,
            Json(anthropic_error(status.as_u16(), &message)),
        )
            .into_response();
    }
    let parsed = parsed.unwrap_or(Value::Null);
    if parsed.get("choices").is_none() {
        return (status, output_headers, Json(parsed)).into_response();
    }
    (
        status,
        output_headers,
        Json(openai_to_anthropic_response(
            &parsed,
            &model_echo,
            &message_id,
            &prompt_text,
        )),
    )
        .into_response()
}

fn chat_control_headers(source: &HeaderMap) -> HeaderMap {
    let mut forwarded = HeaderMap::new();
    for name in CHAT_CONTROL_HEADERS {
        if let Some(value) = source.get(name) {
            forwarded.insert(name, value.clone());
        }
    }
    forwarded
}

struct AnthropicRelay {
    upstream: axum::body::BodyDataStream,
    translator: MessagesStreamTranslator,
    pending: VecDeque<Vec<u8>>,
    finished: bool,
}

fn anthropic_stream_error_frame(message: &str) -> Vec<u8> {
    let payload = serde_json::to_string(&anthropic_error(500, message)).unwrap_or_else(|_| {
        "{\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"upstream error\"}}"
            .to_owned()
    });
    format!("event: error\ndata: {payload}\n\n").into_bytes()
}

enum ReliableDeliveryFailure {
    Exhausted(String),
    Fatal(DeliveryError),
    Misconfigured(String),
    State(ReliabilityError),
}

struct BufferedDeliveryOutcome {
    served_by: String,
    served_by_id: String,
    execution_boundary: ExecutionBoundary,
    selection_reason: &'static str,
    response: BufferedDeliveryResponse,
}

fn buffered_observation(
    response: &BufferedDeliveryResponse,
    elapsed_seconds: f64,
) -> DeploymentObservation {
    let completion_chars = if response.content_type().contains("json") {
        serde_json::from_slice::<Value>(response.body())
            .ok()
            .map(|value| {
                u64::try_from(first_choice_text(&value).chars().count()).unwrap_or(u64::MAX)
            })
            .filter(|chars| *chars > 0)
    } else {
        None
    };
    let throughput = completion_chars
        .filter(|_| elapsed_seconds > 0.0)
        .map(|chars| chars as f64 / elapsed_seconds);
    if response.status().is_success() {
        DeploymentObservation::success(elapsed_seconds, None, throughput)
    } else {
        DeploymentObservation::failure(Some(elapsed_seconds), Some(response.status().as_u16()))
    }
}

#[cfg_attr(
    feature = "otel",
    tracing::instrument(skip_all, name = "wayfinder.delivery")
)]
async fn deliver_with_reliability(
    state: &AppState,
    delivery: &dyn BufferedDelivery,
    plan: &[String],
    routing_request: &RoutingRequest,
    offline: bool,
    prompt_estimate: u64,
    request_body: &Value,
) -> Result<BufferedDeliveryOutcome, ReliableDeliveryFailure> {
    let mut last_error = "no upstream available".to_owned();
    for target_name in plan {
        let configured_targets = state.deployment_candidates(target_name);
        if configured_targets.is_empty() {
            return Err(ReliableDeliveryFailure::Misconfigured(format!(
                "no gateway endpoint configured for model '{target_name}'"
            )));
        }
        let eligible_targets = eligible_deployment_targets(
            configured_targets,
            routing_request,
            offline,
            prompt_estimate,
        );
        if eligible_targets.is_empty() {
            last_error = format!(
                "configured delivery target '{target_name}' has no eligible deployment for this request"
            );
            continue;
        }
        let selection = state.select_deployments(target_name, eligible_targets);
        let targets = selection.targets;
        if targets.is_empty() {
            last_error = format!(
                "configured delivery target '{target_name}' is excluded by its deployment price ceiling"
            );
            continue;
        }
        for target in targets {
            let target_id = target.delivery_id();
            if target.deployment_id.is_some() && !target.key_ready() {
                last_error = format!("deployment '{target_id}' credential is unavailable");
                continue;
            }
            let Some(reliability_attempt) =
                begin_reliability_attempt(state, &target_id, &routing_request.request_id).await?
            else {
                continue;
            };
            let mut reliability_attempt = Some(reliability_attempt);
            let delays = state.reliability_policy().retry_delays();
            for attempt in 0..=state.reliability_policy().retries() {
                let started = Instant::now();
                match delivery.send(&target, request_body.clone()).await {
                    Ok(response) => {
                        let status = response.status().as_u16();
                        if !is_retryable(Some(status)) {
                            if is_auth_failure(Some(status)) {
                                state.observe_deployment(
                                    &target_id,
                                    DeploymentObservation::failure(
                                        Some(started.elapsed().as_secs_f64()),
                                        Some(status),
                                    ),
                                );
                                let _ = state.metrics().observe_upstream_error(&target_id);
                                reliability_attempt
                                    .take()
                                    .ok_or(ReliableDeliveryFailure::State(
                                        ReliabilityError::LockPoisoned,
                                    ))?
                                    .complete(false)
                                    .await
                                    .map_err(ReliableDeliveryFailure::State)?;
                            } else {
                                let observation = buffered_observation(
                                    &response,
                                    started.elapsed().as_secs_f64(),
                                );
                                state.observe_deployment(&target_id, observation);
                                let _ = state
                                    .metrics()
                                    .observe_upstream(&target_id, started.elapsed().as_secs_f64());
                                reliability_attempt
                                    .take()
                                    .ok_or(ReliableDeliveryFailure::State(
                                        ReliabilityError::LockPoisoned,
                                    ))?
                                    .complete(true)
                                    .await
                                    .map_err(ReliableDeliveryFailure::State)?;
                            }
                            return Ok(BufferedDeliveryOutcome {
                                served_by: target_name.clone(),
                                served_by_id: target_id,
                                execution_boundary: model_execution_boundary(&target),
                                selection_reason: selection.reason,
                                response,
                            });
                        }
                        last_error = format!("upstream returned {status}");
                        state.observe_deployment(
                            &target_id,
                            DeploymentObservation::failure(
                                Some(started.elapsed().as_secs_f64()),
                                Some(status),
                            ),
                        );
                        let _ = state.metrics().observe_upstream_error(&target_id);
                    }
                    Err(error) => {
                        state.observe_deployment(
                            &target_id,
                            DeploymentObservation::failure(
                                Some(started.elapsed().as_secs_f64()),
                                None,
                            ),
                        );
                        let _ = state.metrics().observe_upstream_error(&target_id);
                        if !matches!(error, DeliveryError::Provider(ProviderError::Transport)) {
                            if let Some(attempt) = reliability_attempt.take() {
                                attempt.release_in_background();
                            }
                            return Err(ReliableDeliveryFailure::Fatal(error));
                        }
                        last_error = error.to_string();
                    }
                }
                if attempt < state.reliability_policy().retries() {
                    if let Some(delay) = delays.get(attempt) {
                        sleep_retry(*delay).await;
                    }
                }
            }
            reliability_attempt
                .take()
                .ok_or(ReliableDeliveryFailure::State(
                    ReliabilityError::LockPoisoned,
                ))?
                .complete(false)
                .await
                .map_err(ReliableDeliveryFailure::State)?;
        }
    }
    Err(ReliableDeliveryFailure::Exhausted(last_error))
}

fn fatal_delivery_status(error: &DeliveryError) -> (StatusCode, &'static str) {
    match error {
        DeliveryError::InvalidEndpoint
        | DeliveryError::Provider(ProviderError::InvalidEndpoint) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "wayfinder_router_misconfigured",
        ),
        DeliveryError::CredentialUnavailable
        | DeliveryError::Provider(ProviderError::InvalidCredential) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "wayfinder_router_not_ready",
        ),
        DeliveryError::Apple(
            delivery::AppleDeliveryError::Unsupported
            | delivery::AppleDeliveryError::NotReady
            | delivery::AppleDeliveryError::Unavailable,
        ) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "wayfinder_router_not_ready",
        ),
        DeliveryError::Apple(delivery::AppleDeliveryError::InvalidRequest) => (
            StatusCode::BAD_REQUEST,
            "wayfinder_router_unsupported_request",
        ),
        DeliveryError::Apple(delivery::AppleDeliveryError::InvalidResponse) => {
            (StatusCode::BAD_GATEWAY, "wayfinder_router_upstream_error")
        }
        DeliveryError::Anthropic(delivery::AnthropicDeliveryError::InvalidRequest) => (
            StatusCode::BAD_REQUEST,
            "wayfinder_router_unsupported_request",
        ),
        DeliveryError::Anthropic(delivery::AnthropicDeliveryError::InvalidResponse) => {
            (StatusCode::BAD_GATEWAY, "wayfinder_router_upstream_error")
        }
        DeliveryError::Codex(
            delivery::CodexDeliveryError::Unavailable
            | delivery::CodexDeliveryError::AuthenticationRequired
            | delivery::CodexDeliveryError::ModelUnavailable,
        ) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "wayfinder_router_not_ready",
        ),
        DeliveryError::Codex(delivery::CodexDeliveryError::UsageLimitReached) => (
            StatusCode::TOO_MANY_REQUESTS,
            "wayfinder_router_usage_limited",
        ),
        DeliveryError::Codex(delivery::CodexDeliveryError::Busy) => {
            (StatusCode::CONFLICT, "wayfinder_router_busy")
        }
        DeliveryError::Codex(delivery::CodexDeliveryError::InvalidRequest) => (
            StatusCode::BAD_REQUEST,
            "wayfinder_router_unsupported_request",
        ),
        DeliveryError::Codex(delivery::CodexDeliveryError::InvalidResponse) => {
            (StatusCode::BAD_GATEWAY, "wayfinder_router_upstream_error")
        }
        DeliveryError::Codex(delivery::CodexDeliveryError::TurnFailed) => {
            (StatusCode::BAD_GATEWAY, "wayfinder_router_upstream_error")
        }
        DeliveryError::Codex(delivery::CodexDeliveryError::Interrupted) => {
            (StatusCode::CONFLICT, "wayfinder_router_interrupted")
        }
        DeliveryError::Provider(_) => (StatusCode::BAD_GATEWAY, "wayfinder_router_upstream_error"),
    }
}

fn model_is_proven_local(model: &ConfiguredModel) -> bool {
    (model.provider() == ProviderKind::AppleFoundationModels
        && model.tier() == Some(ProviderTier::Local))
        || (model.provider() == ProviderKind::OpenAiCompatible
            && endpoint_is_literal_loopback(model.endpoint()))
}

fn model_execution_boundary(model: &ConfiguredModel) -> ExecutionBoundary {
    if model_is_proven_local(model) {
        return ExecutionBoundary::OnDevice;
    }
    if model.provider() == ProviderKind::OpenAiCompatible
        && endpoint_is_literal_local_network(model.endpoint())
    {
        return ExecutionBoundary::LocalNetwork;
    }
    ExecutionBoundary::Hosted
}

fn model_capabilities(model: &ConfiguredModel) -> DestinationCapabilities {
    match model.provider() {
        // The OpenAI-compatible adapter currently supports text, tools, and
        // streaming. Non-text surfaces remain explicit opt-ins and are not
        // advertised until their adapters and parity fixtures ship.
        ProviderKind::OpenAiCompatible | ProviderKind::Anthropic => DestinationCapabilities {
            text: true,
            streaming: true,
            image_input: false,
            tools: true,
            modalities: ModalityCapabilities::default(),
        },
        // The bounded native adapters currently accept text-only turns. They
        // both expose ordered streaming, but reject images and tools before
        // any provider call is attempted.
        ProviderKind::AppleFoundationModels | ProviderKind::CodexAppServer => {
            DestinationCapabilities {
                text: true,
                streaming: true,
                image_input: false,
                tools: false,
                modalities: ModalityCapabilities::default(),
            }
        }
    }
}

fn destination_snapshot(model: &ConfiguredModel) -> DestinationSnapshot {
    DestinationSnapshot {
        id: model.name().to_owned(),
        provider_id: model.provider().as_str().to_owned(),
        model_id: model.provider_model().to_owned(),
        display_name: model.name().to_owned(),
        route_tier: model
            .tier()
            .map_or_else(|| model.name().to_owned(), |tier| tier.as_str().to_owned()),
        execution_boundary: model_execution_boundary(model),
        readiness: if model.key_ready() {
            ProviderReadiness::Ready
        } else {
            ProviderReadiness::Unavailable
        },
        billing_class: match model.provider() {
            ProviderKind::AppleFoundationModels => wayfinder_routing_core::BillingClass::OnDevice,
            ProviderKind::CodexAppServer => wayfinder_routing_core::BillingClass::Subscription,
            ProviderKind::OpenAiCompatible | ProviderKind::Anthropic => {
                wayfinder_routing_core::BillingClass::ApiMetered
            }
        },
        context_window: model.context_window(),
        capabilities: model_capabilities(model),
        automatic_eligible: true,
    }
}

fn assess_model(request: &RoutingRequest, model: &ConfiguredModel) -> CandidateAssessment {
    let mut assessment = assess_destination(request, &destination_snapshot(model));
    // The legacy gateway permits models without an advertised context window
    // and lets the existing prompt precheck decide what the provider can take.
    // Preserve that compatibility while still enforcing every known hard
    // capability and boundary exclusion here.
    assessment
        .exclusions
        .retain(|reason| *reason != ExclusionReason::ContextWindowUnknown);
    assessment
}

fn eligible_deployment_targets(
    targets: Vec<ConfiguredModel>,
    request: &RoutingRequest,
    offline: bool,
    prompt_estimate: u64,
) -> Vec<ConfiguredModel> {
    targets
        .into_iter()
        .filter(|target| {
            assess_model(request, target).is_eligible()
                && (!offline || model_is_proven_local(target))
                && precheck_ok(prompt_estimate, target.context_window())
        })
        .collect()
}

fn has_eligible_deployment_target(
    state: &AppState,
    alias: &str,
    request: &RoutingRequest,
    offline: bool,
    prompt_estimate: u64,
) -> bool {
    state
        .configured_deployment_targets(alias)
        .into_iter()
        .any(|target| {
            assess_model(request, &target).is_eligible()
                && (!offline || model_is_proven_local(&target))
                && precheck_ok(prompt_estimate, target.context_window())
        })
}

fn privacy_posture_name(posture: PrivacyPosture) -> &'static str {
    match posture {
        PrivacyPosture::OnDeviceOnly => "on-device-only",
        PrivacyPosture::LocalDevices => "local-devices",
        PrivacyPosture::HostedAllowed => "hosted-allowed",
    }
}

fn parse_privacy_posture(headers: &HeaderMap, offline: bool) -> Result<PrivacyPosture, String> {
    if offline {
        return Ok(PrivacyPosture::OnDeviceOnly);
    }
    let value = request_header(headers, PRIVACY_POSTURE_HEADER)?;
    match value.map(str::trim) {
        None => Ok(PrivacyPosture::HostedAllowed),
        Some("on-device-only") => Ok(PrivacyPosture::OnDeviceOnly),
        Some("local-devices") => Ok(PrivacyPosture::LocalDevices),
        Some("hosted-allowed") => Ok(PrivacyPosture::HostedAllowed),
        Some(value) => Err(format!(
            "{PRIVACY_POSTURE_HEADER}: expected on-device-only, local-devices, or hosted-allowed; got {value:?}"
        )),
    }
}

fn request_requires_image_input(messages: &Value) -> bool {
    fn contains_image(value: &Value) -> bool {
        match value {
            Value::Array(values) => values.iter().any(contains_image),
            Value::Object(object) => {
                object
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| matches!(kind, "image_url" | "input_image" | "image"))
                    || object.values().any(contains_image)
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
        }
    }

    contains_image(messages)
}

fn request_requires_tools(body: &serde_json::Map<String, Value>) -> bool {
    ["tools", "functions"].iter().any(|field| {
        body.get(*field)
            .and_then(Value::as_array)
            .is_some_and(|v| !v.is_empty())
    })
}

fn request_requirements(
    body: &serde_json::Map<String, Value>,
    messages: &Value,
    prompt_estimate: u64,
) -> RoutingRequirements {
    let requested_output = requested_output_tokens_from_map(body);
    RoutingRequirements {
        // Existing gateway model entries may intentionally omit a context
        // window. Preserve that compatibility when the caller did not make an
        // explicit output-size promise; the delivery precheck still applies
        // the prompt estimate to every configured window that is known.
        context_tokens: (requested_output > 0)
            .then_some(prompt_estimate.saturating_add(requested_output)),
        image_input: request_requires_image_input(messages),
        tools: request_requires_tools(body),
        streaming: body.get("stream").and_then(Value::as_bool) == Some(true),
        surface: InferenceSurface::Text,
    }
}

fn requested_output_tokens(body: &Value) -> u64 {
    body.as_object().map_or(0, requested_output_tokens_from_map)
}

fn requested_output_tokens_from_map(body: &serde_json::Map<String, Value>) -> u64 {
    ["max_tokens", "max_completion_tokens"]
        .iter()
        .filter_map(|field| body.get(*field).and_then(Value::as_u64))
        .max()
        .unwrap_or(0)
}

fn conservative_token_reservation(body: &Value) -> Result<u64, &'static str> {
    let requested_output = requested_output_tokens(body);
    if requested_output == 0 {
        return Err("TPM-limited requests must set max_tokens or max_completion_tokens");
    }
    let completions = match body.get("n") {
        None => 1,
        Some(value) => value
            .as_u64()
            .filter(|count| *count > 0)
            .ok_or("TPM-limited requests require n to be a positive integer")?,
    };
    let request_bytes = serde_json::to_vec(body)
        .map(|encoded| u64::try_from(encoded.len()).unwrap_or(u64::MAX))
        .map_err(|_| "TPM-limited request could not be bounded")?;
    Ok(request_bytes.saturating_add(requested_output.saturating_mul(completions)))
}

fn exclusion_reason_name(reason: ExclusionReason) -> &'static str {
    match reason {
        ExclusionReason::ProviderNotReady => "provider-not-ready",
        ExclusionReason::PrivacyBoundaryDenied => "privacy-boundary-denied",
        ExclusionReason::TextUnsupported => "text-unsupported",
        ExclusionReason::ContextWindowUnknown => "context-window-unknown",
        ExclusionReason::ContextWindowTooSmall => "context-window-too-small",
        ExclusionReason::ImageInputUnsupported => "image-input-unsupported",
        ExclusionReason::ToolsUnsupported => "tools-unsupported",
        ExclusionReason::StreamingUnsupported => "streaming-unsupported",
        ExclusionReason::EmbeddingsUnsupported => "embeddings-unsupported",
        ExclusionReason::ImageGenerationUnsupported => "image-generation-unsupported",
        ExclusionReason::AudioUnsupported => "audio-unsupported",
        ExclusionReason::BatchUnsupported => "batch-unsupported",
        ExclusionReason::AutomaticNotAllowed => "automatic-not-allowed",
    }
}

fn destination_ineligible_response(
    model: &str,
    assessment: &CandidateAssessment,
    headers: HeaderMap,
    offline: bool,
) -> Response {
    let reasons = assessment
        .exclusions
        .iter()
        .map(|reason| exclusion_reason_name(*reason))
        .collect::<Vec<_>>()
        .join(", ");
    let offline_boundary_error = offline
        && assessment.exclusions.len() == 1
        && assessment.exclusions[0] == ExclusionReason::PrivacyBoundaryDenied;
    if offline_boundary_error {
        error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "wayfinder_router_offline_unavailable",
            "offline delivery requires a literal loopback provider endpoint".to_owned(),
            headers,
        )
    } else {
        error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "wayfinder_router_destination_ineligible",
            format!("destination '{model}' is not eligible for this request: {reasons}"),
            headers,
        )
    }
}

fn privacy_response_headers(request_id: &str, posture: PrivacyPosture) -> HeaderMap {
    let mut headers = request_id_headers(request_id);
    let _ = insert_header(
        &mut headers,
        ROUTER_PRIVACY_POSTURE_HEADER,
        privacy_posture_name(posture),
    );
    headers
}

fn parse_chat_body(bytes: &[u8]) -> Result<serde_json::Map<String, Value>, Box<Response>> {
    if bytes.is_empty() {
        return Err(Box::new(validation_response(json!({
            "type": "missing",
            "loc": ["body"],
            "msg": "Field required",
            "input": null
        }))));
    }
    let value = serde_json::from_slice::<Value>(bytes).map_err(|error| {
        let diagnostic = if error.is_eof() && bytes == b"{" {
            "Expecting property name enclosed in double quotes".to_owned()
        } else {
            error.to_string()
        };
        Box::new(validation_response(json!({
            "type": "json_invalid",
            "loc": ["body", error.column()],
            "msg": "JSON decode error",
            "input": {},
            "ctx": {"error": diagnostic}
        })))
    })?;
    match value {
        Value::Object(body) => Ok(body),
        Value::Null => Err(Box::new(validation_response(json!({
            "type": "missing",
            "loc": ["body"],
            "msg": "Field required",
            "input": null
        })))),
        input => Err(Box::new(validation_response(json!({
            "type": "dict_type",
            "loc": ["body"],
            "msg": "Input should be a valid dictionary",
            "input": input
        })))),
    }
}

pub(crate) fn body_rejection_response(state: &AppState, rejection: BytesRejection) -> Response {
    if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "wayfinder_router_request_too_large",
            format!(
                "request body exceeds the configured {} byte limit",
                state.request_body_limit()
            ),
            HeaderMap::new(),
        );
    }
    validation_response(json!({
        "type": "json_invalid",
        "loc": ["body"],
        "msg": "Request body could not be read",
        "input": null,
        "ctx": {"error": rejection.body_text()}
    }))
}

fn validation_response(detail: Value) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"detail": [detail]})),
    )
        .into_response()
}

fn bad_override_response(request_id: &str, message: String) -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "wayfinder_router_bad_override",
        message,
        request_id_headers(request_id),
    )
}

fn error_response(
    status: StatusCode,
    kind: &'static str,
    message: String,
    headers: HeaderMap,
) -> Response {
    (
        status,
        headers,
        Json(ErrorEnvelope {
            error: ErrorBody { message, kind },
        }),
    )
        .into_response()
}

fn request_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<Option<&'a str>, String> {
    headers
        .get(name)
        .map(|value| {
            value
                .to_str()
                .map_err(|_| format!("{name}: must contain valid visible ASCII"))
        })
        .transpose()
}

async fn preflight_access(
    state: &AppState,
    headers: &HeaderMap,
    request_id: &str,
) -> Result<Option<AccessGrant>, Box<Response>> {
    let Some(policy) = state.access_policy() else {
        return Ok(None);
    };
    if !policy.uses_shared_state() {
        match policy.admit_global() {
            Ok(Some(result)) if !result.allowed() => {
                return Err(Box::new(rate_limited_response(state, result, request_id)));
            }
            Ok(_) => {}
            Err(error) => return Err(Box::new(access_state_error(request_id, &error))),
        }
    } else {
        match policy.admit_shared_global().await {
            Ok(Some(result)) if !result.allowed() => {
                return Err(Box::new(rate_limited_response(state, result, request_id)));
            }
            Ok(_) => {}
            Err(error) => return Err(Box::new(access_state_error(request_id, &error))),
        }
    }

    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let Some(grant) = policy.authenticate(authorization) else {
        let mut response_headers = request_id_headers(request_id);
        response_headers.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return Err(Box::new(error_response(
            StatusCode::UNAUTHORIZED,
            "wayfinder_router_unauthorized",
            "missing or invalid API key".to_owned(),
            response_headers,
        )));
    };
    if let Some(key_id) = policy.key_id(grant) {
        let _ = state.metrics().observe_key_request(key_id);
    }
    let result = if policy.uses_shared_state() {
        policy.admit_shared_key(grant).await
    } else {
        policy.admit_key(grant)
    };
    let _ = state.metrics().set_state_degraded(state.state_degraded());
    match result {
        Ok(Some(result)) if !result.allowed() => {
            Err(Box::new(rate_limited_response(state, result, request_id)))
        }
        Ok(_) => Ok(Some(grant)),
        Err(error) => Err(Box::new(access_state_error(request_id, &error))),
    }
}

fn access_state_error(request_id: &str, error: &AccessPolicyError) -> Response {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "wayfinder_router_state_error",
        error.to_string(),
        request_id_headers(request_id),
    )
}

fn rate_limited_response(state: &AppState, result: RateResult, request_id: &str) -> Response {
    let limit = result
        .limited_by
        .map_or("rpm", crate::rate_limit::LimitKind::as_str);
    let _ = state.metrics().observe_rate_limited(limit);
    let mut headers = request_id_headers(request_id);
    headers.insert(ROUTER_RATE_LIMIT_HEADER, HeaderValue::from_static(limit));
    if let Ok(value) = HeaderValue::from_str(&result.retry_after_seconds.to_string()) {
        headers.insert(header::RETRY_AFTER, value);
    }
    error_response(
        StatusCode::TOO_MANY_REQUESTS,
        "wayfinder_router_rate_limited",
        format!("{limit} rate limit exceeded"),
        headers,
    )
}

fn admission_error_response(
    state: &AppState,
    error: AdmissionError,
    mut headers: HeaderMap,
) -> Response {
    let (reason, retry_after) = match error {
        AdmissionError::QueueFull => ("queue-full", 1_u64),
        AdmissionError::QueueTimedOut => (
            "queue-timeout",
            state.admission_policy().queue_timeout().as_secs().max(1),
        ),
        AdmissionError::SharedLimit => ("fleet-limit", 1),
        AdmissionError::InvalidInFlightLimit
        | AdmissionError::InvalidQueueLimit
        | AdmissionError::InvalidQueueTimeout
        | AdmissionError::Unavailable => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wayfinder_router_state_error",
                error.to_string(),
                headers,
            );
        }
    };
    let _ = state.metrics().observe_admission_rejected(reason);
    headers.insert(ROUTER_OVERLOAD_HEADER, HeaderValue::from_static(reason));
    if let Ok(value) = HeaderValue::from_str(&retry_after.to_string()) {
        headers.insert(header::RETRY_AFTER, value);
    }
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "wayfinder_router_overloaded",
        error.to_string(),
        headers,
    )
}

fn insert_rate_snapshot_headers(
    headers: &mut HeaderMap,
    snapshot: RateSnapshot,
) -> Result<(), String> {
    insert_header(
        headers,
        RATE_LIMIT_LIMIT_HEADER,
        &snapshot.limit.to_string(),
    )?;
    insert_header(
        headers,
        RATE_LIMIT_REMAINING_HEADER,
        &snapshot.remaining.to_string(),
    )?;
    insert_header(
        headers,
        RATE_LIMIT_RESET_HEADER,
        &snapshot.reset_seconds.to_string(),
    )
}

fn clamp_to_allowed(chosen: &str, ladder: &[&str], allowed: &[String]) -> String {
    if allowed.is_empty() || allowed.iter().any(|model| model == chosen) {
        return chosen.to_owned();
    }
    let in_ladder = ladder
        .iter()
        .copied()
        .filter(|model| allowed.iter().any(|allowed| allowed == model))
        .collect::<Vec<_>>();
    if in_ladder.is_empty() {
        return allowed
            .iter()
            .min()
            .cloned()
            .unwrap_or_else(|| chosen.to_owned());
    }
    if let Some(chosen_index) = ladder.iter().position(|model| *model == chosen) {
        if let Some(model) = in_ladder.iter().rev().find(|model| {
            ladder
                .iter()
                .position(|candidate| candidate == *model)
                .is_some_and(|index| index <= chosen_index)
        }) {
            return (*model).to_owned();
        }
    }
    in_ladder[0].to_owned()
}

enum RequestSelection {
    Model(String),
    Preset { name: String, models: Vec<String> },
}

fn resolve_request_selection(
    model_field: Option<&Value>,
    state: &AppState,
) -> Result<Option<RequestSelection>, String> {
    let Some(model_field) = model_field else {
        return Ok(None);
    };
    let Some(name) = model_field.as_str() else {
        return Ok(None);
    };
    let name = name.trim();
    if name.is_empty() || name == AUTO_MODEL {
        return Ok(None);
    }
    if let Some(route_name) = name.strip_prefix(ROUTE_PRESET_PREFIX) {
        let Some(route) = state.route_preset(route_name) else {
            return Err(format!(
                "unknown routing preset '{name}'; configure [gateway.routes.{route_name}] first"
            ));
        };
        return Ok(Some(RequestSelection::Preset {
            name: route_name.to_owned(),
            models: route.models.clone(),
        }));
    }
    if state.supports_tier_directives() {
        if name == PREFER_LOW {
            return Ok(state
                .routing()
                .tiers
                .first()
                .map(|tier| RequestSelection::Model(tier.model.clone())));
        }
        if name == PREFER_HIGH || name == PREFER_HIGH_ALIAS {
            return Ok(state
                .routing()
                .tiers
                .last()
                .map(|tier| RequestSelection::Model(tier.model.clone())));
        }
    }
    Ok(state
        .models()
        .iter()
        .any(|model| model.name() == name)
        .then(|| RequestSelection::Model(name.to_owned())))
}

fn offline_override(headers: &HeaderMap) -> bool {
    request_header(headers, OFFLINE_HEADER)
        .ok()
        .flatten()
        .is_some_and(|value| matches!(value.trim().to_lowercase().as_str(), "1" | "true" | "yes"))
}

pub(crate) fn new_request_id() -> String {
    Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(12)
        .collect()
}

struct BufferedAccounting<'a> {
    route: &'a str,
    request_id: &'a str,
    messages: &'a Value,
    access_grant: Option<AccessGrant>,
    status: StatusCode,
    content_type: &'a str,
    body: &'a Bytes,
    token_reservation: Option<TokenReservation>,
    shared_budget: Option<SharedBudgetLease>,
}

#[derive(Clone, Copy)]
struct AccountedUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    estimated: bool,
}

struct UsageAccounting<'a> {
    route: &'a str,
    request_id: &'a str,
    access_grant: Option<AccessGrant>,
    prompt_tokens: i64,
    completion_tokens: i64,
    estimated: bool,
    token_reservation: Option<TokenReservation>,
    shared_budget: Option<SharedBudgetLease>,
}

struct StreamAccounting<'a> {
    route: &'a str,
    request_id: &'a str,
    messages: &'a Value,
    access_grant: Option<AccessGrant>,
    completion_chars: u64,
    token_reservation: Option<TokenReservation>,
    shared_budget: Option<SharedBudgetLease>,
}

async fn account_buffered_success(
    state: &AppState,
    context: BufferedAccounting<'_>,
) -> Option<AccountedUsage> {
    let BufferedAccounting {
        route,
        request_id,
        messages,
        access_grant,
        status,
        content_type,
        body,
        token_reservation,
        shared_budget,
    } = context;
    if status.as_u16() >= 400 {
        return None;
    }
    let response = if !body.is_empty() && content_type.contains("json") {
        serde_json::from_slice(body).unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let prompt = extract_prompt(messages, RouteOn::All);
    let usage = usage_tokens(&response, &prompt, first_choice_text(&response));
    record_accounted_usage(
        state,
        UsageAccounting {
            route,
            request_id,
            access_grant,
            prompt_tokens: usage.prompt,
            completion_tokens: usage.completion,
            estimated: usage.estimated,
            token_reservation,
            shared_budget,
        },
    )
    .await
}

async fn account_stream_success(
    state: &AppState,
    context: StreamAccounting<'_>,
) -> Option<AccountedUsage> {
    let StreamAccounting {
        route,
        request_id,
        messages,
        access_grant,
        completion_chars,
        token_reservation,
        shared_budget,
    } = context;
    let prompt_tokens = estimate_tokens(&extract_prompt(messages, RouteOn::All));
    let completion_tokens = if completion_chars == 0 {
        0
    } else {
        (completion_chars / 4).max(1)
    };
    record_accounted_usage(
        state,
        UsageAccounting {
            route,
            request_id,
            access_grant,
            prompt_tokens: i64::try_from(prompt_tokens).unwrap_or(i64::MAX),
            completion_tokens: i64::try_from(completion_tokens).unwrap_or(i64::MAX),
            estimated: true,
            token_reservation,
            shared_budget,
        },
    )
    .await
}

async fn record_accounted_usage(
    state: &AppState,
    accounting: UsageAccounting<'_>,
) -> Option<AccountedUsage> {
    let UsageAccounting {
        route,
        request_id,
        access_grant,
        prompt_tokens,
        completion_tokens,
        estimated,
        token_reservation,
        shared_budget,
    } = accounting;
    let Ok(cost) = turn_cost(
        route,
        prompt_tokens,
        completion_tokens,
        &state.price_table().costs,
        estimated,
        None,
    ) else {
        return None;
    };
    let Ok(today) = utc_today() else {
        return None;
    };
    state
        .savings_ledger()
        .set_priced(state.price_table().priced);
    let key_id = state
        .access_policy()
        .and_then(|policy| access_grant.and_then(|grant| policy.key_id(grant)));
    let workspace_id = state
        .access_policy()
        .and_then(|policy| access_grant.and_then(|grant| policy.workspace_id(grant)));
    if state
        .savings_ledger()
        .record_attributed(&cost, today, key_id, workspace_id)
        .is_ok()
    {
        // Only mark dirty here; the background persister writes the file.
        state.note_savings_recorded();
    }
    if let Some(backend) = state
        .shared_state_backend()
        .filter(|backend| backend.backend_name() == "redis")
    {
        let mut scopes = vec!["global".to_owned(), format!("route:{route}")];
        if let Some(key_id) = key_id {
            scopes.push(format!("key:{key_id}"));
        }
        if let Some(workspace_id) = workspace_id {
            scopes.push(format!("workspace:{workspace_id}"));
        }
        let observation =
            SharedCostObservation::from_turn(request_id, today.to_string(), &cost, scopes);
        let _ = backend.record_cost(observation).await;
        let _ = state.metrics().set_state_degraded(state.state_degraded());
    }
    if let Some(lease) = shared_budget {
        lease.settle().await;
    }
    let tokens = cost.prompt_tokens.saturating_add(cost.completion_tokens);
    if let Some(reservation) = token_reservation {
        if !estimated {
            let policy = state.access_policy()?;
            policy.reconcile_tokens(reservation, tokens).await.ok()?;
        }
    } else if let (Some(policy), Some(grant)) = (state.access_policy(), access_grant) {
        let _ = policy.add_tokens(grant, tokens);
        if policy.uses_shared_state() {
            let _ = policy.add_shared_tokens(grant, tokens).await;
        }
    }
    let _ = state.metrics().observe_cost(cost.realized, cost.baseline);
    let _ = state.recent().update_cost(
        request_id,
        RecentCost {
            realized: cost.realized,
            baseline: cost.baseline,
            saved: cost.savings,
            tokens: cost.prompt_tokens.saturating_add(cost.completion_tokens),
            unit: if state.price_table().priced {
                "usd"
            } else {
                "relative"
            }
            .to_owned(),
            estimated: cost.estimated,
        },
    );
    Some(AccountedUsage {
        prompt_tokens: cost.prompt_tokens,
        completion_tokens: cost.completion_tokens,
        estimated: cost.estimated,
    })
}

fn first_choice_text(response: &Value) -> &str {
    response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(Value::as_object)
        .and_then(|choice| choice.get("message"))
        .and_then(Value::as_object)
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn utc_today() -> Result<UtcDate, LedgerError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| LedgerError::InvalidDate("system clock is before Unix epoch".to_owned()))?
        .as_secs();
    let days = i64::try_from(seconds / 86_400)
        .map_err(|_| LedgerError::InvalidDate("system clock exceeds UTC date range".to_owned()))?;
    utc_date_from_unix_days(days)
}

fn utc_date_from_unix_days(days: i64) -> Result<UtcDate, LedgerError> {
    // Howard Hinnant's civil-from-days transform, with day zero at 1970-01-01.
    let shifted = days.saturating_add(719_468);
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted.saturating_sub(146_096)
    } / 146_097;
    let day_of_era = shifted.saturating_sub(era.saturating_mul(146_097));
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era.saturating_add(era.saturating_mul(400));
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    UtcDate::new(
        i32::try_from(year).unwrap_or(i32::MAX),
        u8::try_from(month).unwrap_or(u8::MAX),
        u8::try_from(day).unwrap_or(u8::MAX),
    )
}

#[derive(Clone, Copy)]
struct DecisionPolicyReceipt<'a> {
    version: &'a str,
    snapshot_id: &'a str,
    profile_id: Option<&'a str>,
}

fn decision_headers(
    chosen: &str,
    score: f64,
    mode: RouteMode,
    request_id: &str,
    offline: bool,
    policy: DecisionPolicyReceipt<'_>,
) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    insert_header(&mut headers, ROUTER_MODEL_HEADER, chosen)?;
    insert_header(&mut headers, ROUTER_SCORE_HEADER, &format!("{score:.2}"))?;
    insert_header(&mut headers, ROUTER_MODE_HEADER, mode.as_str())?;
    insert_header(&mut headers, ROUTER_REQUEST_ID_HEADER, request_id)?;
    if policy.version != UNMANAGED_POLICY_ID {
        insert_header(&mut headers, ROUTER_POLICY_VERSION_HEADER, policy.version)?;
        insert_header(&mut headers, ROUTER_SNAPSHOT_ID_HEADER, policy.snapshot_id)?;
        if let Some(profile_id) = policy.profile_id {
            insert_header(&mut headers, ROUTER_POLICY_PROFILE_HEADER, profile_id)?;
        }
    }
    if offline {
        headers.insert(ROUTER_OFFLINE_HEADER, HeaderValue::from_static("true"));
    }
    Ok(headers)
}

fn request_id_headers(request_id: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(request_id) {
        headers.insert(ROUTER_REQUEST_ID_HEADER, value);
    }
    headers
}

fn insert_header(headers: &mut HeaderMap, name: &'static str, value: &str) -> Result<(), String> {
    let value = HeaderValue::from_str(value)
        .map_err(|_| format!("configured routing value for {name} is not a valid HTTP header"))?;
    headers.insert(name, value);
    Ok(())
}

struct DecisionMetadata {
    chosen: String,
    mode: RouteMode,
    offline: bool,
    request_id: String,
    headers: HeaderMap,
}

fn decision_only_response(
    state: &AppState,
    routing: &RoutingConfig,
    decision: ComplexityScore,
    metadata: DecisionMetadata,
) -> Response {
    let dry_run = state.dry_run();
    let mut headers = metadata.headers;
    if !dry_run {
        headers.insert(
            ROUTER_DECISION_ONLY_HEADER,
            HeaderValue::from_static("true"),
        );
    }
    (
        StatusCode::OK,
        headers,
        Json(DecisionEnvelope {
            wayfinder: make_decision_response(
                state,
                routing,
                &decision,
                &metadata.chosen,
                metadata.mode,
                metadata.offline,
                &metadata.request_id,
                dry_run,
                !dry_run,
            ),
        }),
    )
        .into_response()
}

#[allow(clippy::too_many_arguments)]
fn make_decision_response(
    state: &AppState,
    routing: &RoutingConfig,
    decision: &ComplexityScore,
    chosen: &str,
    mode: RouteMode,
    offline: bool,
    request_id: &str,
    dry_run: bool,
    decision_only: bool,
) -> DecisionResponse {
    DecisionResponse {
        model: chosen.to_owned(),
        score: decision.score,
        mode: mode.as_str(),
        offline,
        request_id: request_id.to_owned(),
        features: decision.features,
        contributions: explain_score(&decision.features, &routing.weights),
        tiers: decision.tiers.clone(),
        cost: cost_response(state, decision, chosen),
        dry_run: dry_run.then_some(true),
        decision_only: decision_only.then_some(true),
    }
}

fn cost_response(state: &AppState, decision: &ComplexityScore, chosen: &str) -> CostResponse {
    let mut costs = BTreeMap::new();
    for model in state.models() {
        if let Some(cost) = model.cost_per_1k() {
            costs.insert(model.name().to_owned(), cost);
        }
    }
    if let Some(tiers) = &decision.tiers {
        for tier in tiers {
            if let Some(cost) = tier.cost.filter(|cost| cost.is_finite() && *cost >= 0.0) {
                costs.entry(tier.model.clone()).or_insert(cost);
            }
        }
    }
    let estimated = costs.is_empty();
    if estimated {
        let ladder = decision
            .tiers
            .as_deref()
            .map(|tiers| {
                tiers
                    .iter()
                    .map(|tier| tier.model.clone())
                    .collect::<Vec<_>>()
            })
            .filter(|ladder| !ladder.is_empty())
            .unwrap_or_else(|| vec![chosen.to_owned()]);
        let denominator = ladder.len().saturating_sub(1).max(1) as f64;
        for (index, model) in ladder.into_iter().enumerate() {
            let cost = python_round(0.2 + (0.8 * index as f64 / denominator), 3);
            costs.insert(model, cost);
        }
    }
    let baseline_per_1k = costs.values().copied().fold(0.0_f64, f64::max);
    let chosen_per_1k = costs.get(chosen).copied().unwrap_or(baseline_per_1k);
    let word_count = decision.features.word_count;
    let scale = word_count as f64 / 1000.0;
    let per_call = python_round(chosen_per_1k * scale, 6);
    let baseline = python_round(baseline_per_1k * scale, 6);
    CostResponse {
        per_call,
        baseline,
        saved: python_round(baseline - per_call, 6),
        unit: if estimated {
            "relative units / 1k words"
        } else {
            "$ / 1k words"
        },
        estimated,
        word_count,
    }
}

#[derive(Serialize)]
struct DetailResponse {
    detail: &'static str,
}

async fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(DetailResponse {
            detail: "Not Found",
        }),
    )
        .into_response()
}

async fn method_not_allowed(uri: Uri) -> Response {
    let allowed = if matches!(
        uri.path(),
        "/v1/chat/completions"
            | "/chat/completions"
            | "/v1/responses"
            | "/responses"
            | "/v1/messages"
            | "/messages"
            | "/router/config"
    ) {
        "POST"
    } else {
        "GET"
    };
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, allowed)],
        Json(DetailResponse {
            detail: "Method Not Allowed",
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests;
