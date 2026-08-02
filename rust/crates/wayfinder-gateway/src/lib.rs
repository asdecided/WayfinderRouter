//! Local HTTP gateway and operational policy.
//!
//! This bounded gateway surface exposes configuration-derived health/model
//! discovery and deterministic decision-only chat. It never reads environment
//! variables, resolves secrets, probes or calls providers, or starts a process.

#![forbid(unsafe_code)]

pub mod access;
pub mod admission;
pub mod audit;
pub mod auth;
pub mod budget;
pub mod cache;
pub mod codex_control;
pub mod decision_policy;
pub mod delivery;
pub mod deployment_selection;
pub mod metrics;
pub mod operator_auth;
pub mod otel;
pub mod rate_limit;
pub mod recent;
pub mod reliability;
pub mod reload;
pub mod server;
pub mod state;

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
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tower::ServiceExt;
use uuid::Uuid;
use wayfinder_config::dump_routing_toml;
use wayfinder_config::gateway::{
    DeploymentSelectionPolicy, GatewayModel, ProviderKind, ProviderTier, RoutePreset,
};
use wayfinder_providers::anthropic::{
    MessagesStreamTranslator, anthropic_error, anthropic_to_openai_request,
    openai_to_anthropic_response,
};
use wayfinder_providers::openai_compat::{DEFAULT_MAX_RESPONSE_BYTES, ProviderError};
use wayfinder_providers::reliability::{
    failover_candidates, is_auth_failure, is_retryable, precheck_ok,
};
use wayfinder_providers::sse::{SseDecoder, SseEvent};
use wayfinder_routing_core::profiles::{LexiconProfile, profiles};
use wayfinder_routing_core::{
    CandidateAssessment, ComplexityScore, DestinationCapabilities, DestinationSnapshot,
    ExclusionReason, ExecutionBoundary, FeatureContribution, Features, PrivacyPosture,
    ProviderReadiness, RoutingConfig, RoutingRequest, RoutingRequirements, Tier,
    assess_destination, explain_score, python_round, recommend_tier, score_complexity,
};
use wayfinder_service::pricing::{
    LedgerError, PriceTable, SavingsLedger, SavingsReport, UtcDate, estimate_tokens, price_table,
    table_version, turn_cost, usage_tokens,
};

use crate::access::{
    AccessGrant, AccessPolicy, AccessPolicyError, TokenReservation, TokenReservationDecision,
};
use crate::admission::{AdmissionError, AdmissionPermit, AdmissionPolicy};
use crate::budget::{BudgetDecision, BudgetPolicy, format_limit};
use crate::cache::{
    CACHE_KEY_SCHEMA_VERSION, CacheError, CacheSettings, CachedResponse, ResponseCache,
    cache_key_with_version, decode_shared_response, encode_shared_response, is_cacheable,
    is_storable,
};
use crate::decision_policy::{
    AUTO_MODEL, PREFER_HIGH, PREFER_HIGH_ALIAS, PREFER_LOW, ROUTE_ON_HEADER,
    STICKY_COOLDOWN_HEADER, STICKY_HEADER, THRESHOLD_HEADER, TUNING_FIELD, apply_scoring_overrides,
    conversation_high_water, extract_prompt, parse_route_on_header, parse_threshold_header,
    resolve_slash_directive_for_names, resolve_sticky, resolve_sticky_cooldown, threshold_tiers,
};
use crate::delivery::{
    BufferedDelivery, BufferedDeliveryResponse, DeliveryError, StreamingDelivery,
    endpoint_is_literal_local_network, endpoint_is_literal_loopback,
};
use crate::deployment_selection::{
    DeploymentObservation, DeploymentObservationStore, DeploymentSelection,
};
use crate::metrics::{DEFAULT_MAX_LABEL_SERIES, GatewayMetrics};
use crate::rate_limit::{RateResult, RateSnapshot};
use crate::recent::{RecentCost, RecentEntry, RecentRoutes};
use crate::reliability::{ReliabilityAttempt, ReliabilityError, ReliabilityPolicy, sleep_retry};
use crate::reload::LastGood;
use crate::state::{SharedBudgetReservation, SharedCachePut, SharedCostObservation, StateBackend};

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
    offline: bool,
    dry_run: bool,
    route_on: RouteOn,
    sticky: bool,
    sticky_cooldown: u64,
    slash_directives: bool,
    build_version: Arc<str>,
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
            offline,
            dry_run: false,
            route_on: RouteOn::Turn,
            sticky: false,
            sticky_cooldown: 0,
            slash_directives: false,
            build_version,
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
        self
    }

    /// Persist realized accounting to this path after each completed turn.
    #[must_use]
    pub fn with_savings_path(mut self, path: std::path::PathBuf) -> Self {
        self.savings_path = Some(Arc::new(path));
        self
    }

    /// Flush the current ledger when persistence is configured.
    pub fn persist_savings(&self) -> Result<(), wayfinder_service::pricing::LedgerError> {
        self.savings_path
            .as_deref()
            .map_or(Ok(()), |path| self.savings_ledger.save(path))
    }

    /// Preserve process-lifetime runtime authorities across a config snapshot swap.
    ///
    /// Recent metadata, metrics, realized accounting, and delivery admission
    /// retain continuity. Admission is deliberately one process-lifetime
    /// authority: replacing its semaphores while requests from the previous
    /// snapshot are still in flight would let each generation admit up to its
    /// own limit. Concurrency-limit changes therefore take effect on process
    /// restart rather than weakening the active bound during hot reload.
    #[must_use]
    pub fn with_runtime_state_from(mut self, previous: &Self) -> Self {
        self.admission_policy = Arc::clone(&previous.admission_policy);
        self.shared_state_backend = previous.shared_state_backend.clone();
        self.recent = Arc::clone(&previous.recent);
        self.metrics = Arc::clone(&previous.metrics);
        self.deployment_cursors = Arc::clone(&previous.deployment_cursors);
        self.deployment_signals = Arc::clone(&previous.deployment_signals);
        self.savings_ledger = Arc::clone(&previous.savings_ledger);
        self.savings_path = previous.savings_path.clone();
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

    /// Selected streaming provider implementation, if configured.
    #[must_use]
    pub fn streaming_delivery(&self) -> Option<&dyn StreamingDelivery> {
        self.streaming_delivery.as_deref()
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
        .route("/v1/savings", get(savings_report))
        .route("/savings", get(savings_report))
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
pub fn build_reloadable_router(holder: Arc<LastGood<AppState, u128>>) -> Router {
    Router::new()
        .fallback(reloadable_dispatch)
        .with_state(holder)
}

/// Build a managed data plane over immutable last-good configuration snapshots.
pub fn build_reloadable_data_plane_router(holder: Arc<LastGood<AppState, u128>>) -> Router {
    Router::new()
        .fallback(reloadable_data_plane_dispatch)
        .with_state(holder)
}

async fn reloadable_dispatch(
    State(holder): State<Arc<LastGood<AppState, u128>>>,
    request: Request<Body>,
) -> Response {
    let state = match holder.current() {
        Ok(state) => state,
        Err(_) => return internal_error_response(),
    };
    match build_router((*state).clone()).oneshot(request).await {
        Ok(response) => response,
        Err(error) => match error {},
    }
}

async fn reloadable_data_plane_dispatch(
    State(holder): State<Arc<LastGood<AppState, u128>>>,
    request: Request<Body>,
) -> Response {
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
                .audit_log()
                .record(
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
        .audit_log()
        .record(
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
async fn chat_completions(
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
    let access_grant = match preflight_access(&state, &headers, &request_id).await {
        Ok(grant) => grant,
        Err(response) => return *response,
    };
    let tuning = body.remove(TUNING_FIELD);
    let routing = match apply_scoring_overrides(state.routing(), tuning.as_ref()) {
        Ok(routing) => routing,
        Err(error) => return bad_override_response(&request_id, error.to_string()),
    };

    let route_on_header = match request_header(&headers, ROUTE_ON_HEADER) {
        Ok(value) => value,
        Err(message) => return bad_override_response(&request_id, message),
    };
    let route_on = match parse_route_on_header(route_on_header) {
        Ok(value) => value.unwrap_or(state.route_on()),
        Err(error) => return bad_override_response(&request_id, error.to_string()),
    };
    let sticky_header = match request_header(&headers, STICKY_HEADER) {
        Ok(value) => value,
        Err(message) => return bad_override_response(&request_id, message),
    };
    let sticky = match resolve_sticky(sticky_header, state.sticky()) {
        Ok(value) => value,
        Err(error) => return bad_override_response(&request_id, error.to_string()),
    };
    let sticky_cooldown_header = match request_header(&headers, STICKY_COOLDOWN_HEADER) {
        Ok(value) => value,
        Err(message) => return bad_override_response(&request_id, message),
    };
    let sticky_cooldown =
        match resolve_sticky_cooldown(sticky_cooldown_header, state.sticky_cooldown()) {
            Ok(value) => value,
            Err(error) => return bad_override_response(&request_id, error.to_string()),
        };

    let mut messages = body.get("messages").cloned().unwrap_or(Value::Null);
    let slash_resolution = state.slash_directives().then(|| {
        resolve_slash_directive_for_names(
            &messages,
            &routing,
            state.models().iter().map(ConfiguredModel::name),
        )
    });
    let slash_resolution = slash_resolution.flatten();
    if let Some(resolution) = &slash_resolution {
        messages = resolution.messages.clone();
        body.insert("messages".to_owned(), messages.clone());
    }
    let prompt = extract_prompt(&messages, route_on);
    let offline = state.offline() || offline_override(&headers);
    let privacy_posture = match parse_privacy_posture(&headers, offline) {
        Ok(value) => value,
        Err(message) => return bad_override_response(&request_id, message),
    };
    let prompt_estimate = estimate_tokens(&extract_prompt(&messages, RouteOn::All));
    let requirements = request_requirements(&body, &messages, prompt_estimate);
    let routing_request = RoutingRequest {
        schema_version: wayfinder_routing_core::RUNTIME_CONTRACT_VERSION,
        request_id: request_id.clone(),
        prompt: prompt.clone(),
        privacy_posture,
        requirements,
    };
    let decision_started = Instant::now();
    let decision_span = tracing::info_span!("wayfinder.decision");
    let decision = match decision_span.in_scope(|| score_complexity(&prompt, &routing)) {
        Ok(decision) => decision,
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wayfinder_router_misconfigured",
                error.to_string(),
                request_id_headers(&request_id),
            );
        }
    };

    let selection = match resolve_request_selection(body.get("model"), &state) {
        Ok(selection) => selection,
        Err(message) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "wayfinder_router_unknown_route",
                message,
                request_id_headers(&request_id),
            );
        }
    };
    let (mut chosen, mut mode, mut preset_models, mut preset_name) =
        if let Some(selection) = selection {
            match selection {
                RequestSelection::Model(pin) => (pin, "pinned", None, None),
                RequestSelection::Preset { name, models } => {
                    let chosen = models[0].clone();
                    (chosen, "preset", Some(models), Some(name))
                }
            }
        } else if let Some(slash_pin) = slash_resolution.and_then(|resolution| resolution.pin) {
            (slash_pin, "slash-pinned", None, None)
        } else {
            let threshold_header = match request_header(&headers, THRESHOLD_HEADER) {
                Ok(value) => value,
                Err(message) => return bad_override_response(&request_id, message),
            };
            let threshold = match parse_threshold_header(threshold_header) {
                Ok(value) => value,
                Err(error) => return bad_override_response(&request_id, error.to_string()),
            };
            let (mut chosen, mut mode, effective_tiers) = if let Some(threshold) = threshold {
                let tiers = match threshold_tiers(&routing, threshold) {
                    Ok(tiers) => tiers,
                    Err(error) => return bad_override_response(&request_id, error.to_string()),
                };
                let Some(model) = recommend_tier(decision.score, &tiers) else {
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "wayfinder_router_misconfigured",
                        "routing configuration contains no selectable tier".to_owned(),
                        request_id_headers(&request_id),
                    );
                };
                (model.to_owned(), "threshold-override", tiers)
            } else {
                (
                    decision.recommendation.clone(),
                    "scored",
                    routing.tiers.clone(),
                )
            };
            if sticky && routing.classifier.is_none() && effective_tiers.len() >= 2 {
                let latched = match conversation_high_water(
                    &messages,
                    &routing,
                    &effective_tiers,
                    sticky_cooldown,
                ) {
                    Ok(value) => value,
                    Err(error) => return bad_override_response(&request_id, error.to_string()),
                };
                if let Some(latched) = latched {
                    let current_rank = effective_tiers
                        .iter()
                        .position(|tier| tier.model == chosen)
                        .unwrap_or(0);
                    let latched_rank = effective_tiers
                        .iter()
                        .position(|tier| tier.model == latched)
                        .unwrap_or(0);
                    if latched_rank > current_rank {
                        chosen = latched;
                        mode = "sticky";
                    }
                }
            }
            (chosen, mode, None, None)
        };
    let mut preset_route_active = preset_name.is_some();

    if let Some(preset_models) = preset_models.as_mut() {
        let allowed_models = state
            .access_policy()
            .and_then(|policy| access_grant.map(|grant| policy.allowed_models(grant)))
            .filter(|allowed| !allowed.is_empty());
        let mut reasons = Vec::new();
        preset_models.retain(|name| {
            if allowed_models.is_some_and(|allowed| !allowed.iter().any(|model| model == name)) {
                reasons.push(format!("{name}: not-allowed-by-key"));
                return false;
            }
            if state.dry_run() {
                return true;
            }
            let Some(model) = state.models().iter().find(|model| model.name() == name) else {
                reasons.push(format!("{name}: unknown-model"));
                return false;
            };
            let assessment = assess_model(&routing_request, model);
            if assessment.is_eligible() {
                true
            } else {
                reasons.push(format!(
                    "{name}: {}",
                    assessment
                        .exclusions
                        .iter()
                        .map(|reason| exclusion_reason_name(*reason))
                        .collect::<Vec<_>>()
                        .join(",")
                ));
                false
            }
        });
        let Some(first) = preset_models.first().cloned() else {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "wayfinder_router_destination_ineligible",
                format!(
                    "routing preset '{}' has no eligible destination for this request: {}",
                    preset_name.as_deref().unwrap_or("unknown"),
                    reasons.join("; ")
                ),
                privacy_response_headers(&request_id, privacy_posture),
            );
        };
        chosen = first;
    } else if !state.dry_run() && matches!(mode, "pinned" | "slash-pinned") {
        if let Some(model) = state.models().iter().find(|model| model.name() == chosen) {
            let assessment = assess_model(&routing_request, model);
            if !assessment.is_eligible() {
                return destination_ineligible_response(
                    &chosen,
                    &assessment,
                    privacy_response_headers(&request_id, privacy_posture),
                    offline,
                );
            }
        }
    }

    let key_id = state
        .access_policy()
        .and_then(|policy| access_grant.and_then(|grant| policy.key_id(grant)));
    let workspace_id = state
        .access_policy()
        .and_then(|policy| access_grant.and_then(|grant| policy.workspace_id(grant)));
    let mut budget_state = None;
    if state.budget_policy().active() && state.price_table().priced {
        let today = match utc_today() {
            Ok(today) => today,
            Err(error) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "wayfinder_router_state_error",
                    error.to_string(),
                    request_id_headers(&request_id),
                );
            }
        };
        let budget_decision = if let Some(backend) = state
            .shared_state_backend()
            .filter(|backend| backend.backend_name() == "redis")
        {
            match state
                .budget_policy()
                .evaluate_shared_for(backend.as_ref(), true, workspace_id, key_id, today, offline)
                .await
            {
                Ok(decision) => Ok(decision),
                Err(_) => state
                    .budget_policy()
                    .evaluate_for(
                        state.savings_ledger(),
                        true,
                        workspace_id,
                        key_id,
                        today,
                        offline,
                    )
                    .map_err(|error| error.to_string()),
            }
        } else {
            state
                .budget_policy()
                .evaluate_for(
                    state.savings_ledger(),
                    true,
                    workspace_id,
                    key_id,
                    today,
                    offline,
                )
                .map_err(|error| error.to_string())
        };
        match budget_decision {
            Ok(BudgetDecision::Allow) => {}
            Ok(BudgetDecision::Degrade) => {
                budget_state = Some("degraded");
                if !offline {
                    let cheapest = decision
                        .tiers
                        .as_ref()
                        .and_then(|tiers| {
                            tiers
                                .iter()
                                .min_by(|left, right| left.min_score.total_cmp(&right.min_score))
                        })
                        .map(|tier| tier.model.clone());
                    if let Some(cheapest) = cheapest {
                        if cheapest != chosen {
                            chosen = cheapest;
                            mode = "budget-degraded";
                            preset_models = None;
                            preset_name = None;
                            preset_route_active = false;
                        }
                    }
                }
            }
            Ok(BudgetDecision::Block { window, limit }) => {
                let mut headers = request_id_headers(&request_id);
                headers.insert(ROUTER_BUDGET_HEADER, HeaderValue::from_static("blocked"));
                return error_response(
                    StatusCode::PAYMENT_REQUIRED,
                    "wayfinder_router_budget_exhausted",
                    format!("{window} budget of {} reached", format_limit(limit)),
                    headers,
                );
            }
            Err(error) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "wayfinder_router_state_error",
                    error,
                    request_id_headers(&request_id),
                );
            }
        }
    }
    if let (Some(policy), Some(grant)) = (state.access_policy(), access_grant) {
        let allowed = policy.allowed_models(grant);
        if !allowed.is_empty() {
            let mut ladder = decision
                .tiers
                .as_ref()
                .map(|tiers| tiers.iter().collect::<Vec<_>>())
                .unwrap_or_default();
            ladder.sort_by(|left, right| left.min_score.total_cmp(&right.min_score));
            let ladder = ladder
                .into_iter()
                .map(|tier| tier.model.as_str())
                .collect::<Vec<_>>();
            let clamped = clamp_to_allowed(&chosen, &ladder, allowed);
            if clamped != chosen {
                chosen = clamped;
                mode = "key-scoped";
            }
        }
    }
    let mut routing_headers =
        match decision_headers(&chosen, decision.score, mode, &request_id, offline) {
            Ok(headers) => headers,
            Err(message) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "wayfinder_router_misconfigured",
                    message,
                    request_id_headers(&request_id),
                );
            }
        };
    if let Err(message) = insert_header(
        &mut routing_headers,
        ROUTER_PRIVACY_POSTURE_HEADER,
        privacy_posture_name(privacy_posture),
    ) {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "wayfinder_router_misconfigured",
            message,
            request_id_headers(&request_id),
        );
    }
    if let Some(budget_state) = budget_state {
        routing_headers.insert(ROUTER_BUDGET_HEADER, HeaderValue::from_static(budget_state));
    }
    if let Some(route_name) = preset_name.as_deref() {
        if let Err(message) = insert_header(
            &mut routing_headers,
            ROUTER_ROUTE_HEADER,
            &format!("{ROUTE_PRESET_PREFIX}{route_name}"),
        ) {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wayfinder_router_misconfigured",
                message,
                request_id_headers(&request_id),
            );
        }
    }
    if let (Some(policy), Some(grant)) = (state.access_policy(), access_grant) {
        if let Some(workspace) = policy.workspace_id(grant) {
            let value = match HeaderValue::from_str(workspace) {
                Ok(value) => value,
                Err(_) => {
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "wayfinder_router_misconfigured",
                        "workspace id cannot be represented as a response header".to_owned(),
                        request_id_headers(&request_id),
                    );
                }
            };
            routing_headers.insert(ROUTER_WORKSPACE_HEADER, value);
        }
        match policy.tightest_snapshot_shared(grant).await {
            Ok(Some(snapshot)) => {
                let _ = state.metrics().set_state_degraded(state.state_degraded());
                if let Err(message) = insert_rate_snapshot_headers(&mut routing_headers, snapshot) {
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "wayfinder_router_state_error",
                        message,
                        request_id_headers(&request_id),
                    );
                }
            }
            Ok(None) => {
                let _ = state.metrics().set_state_degraded(state.state_degraded());
            }
            Err(error) => {
                let _ = state.metrics().set_state_degraded(state.state_degraded());
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "wayfinder_router_state_error",
                    error.to_string(),
                    request_id_headers(&request_id),
                );
            }
        }
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64());
    let _ = state.recent().record(RecentEntry {
        request_id: request_id.clone(),
        model: chosen.clone(),
        score: python_round(decision.score, 2),
        mode: mode.to_owned(),
        ts: timestamp,
        cost: None,
        key: key_id.map(str::to_owned),
        cache: None,
    });
    let _ =
        state
            .metrics()
            .observe_decision(&chosen, mode, decision_started.elapsed().as_secs_f64());

    if state.dry_run() || state.models().is_empty() {
        return decision_only_response(
            &state,
            &routing,
            decision,
            DecisionMetadata {
                chosen,
                mode,
                offline,
                request_id,
                headers: routing_headers,
            },
        );
    }

    let request_is_pinned = matches!(mode, "pinned" | "slash-pinned");
    let request_is_preset = preset_route_active;
    let delivery_name = if offline && !request_is_pinned && !request_is_preset {
        decision
            .tiers
            .as_ref()
            .and_then(|tiers| {
                tiers
                    .iter()
                    .min_by(|left, right| left.min_score.total_cmp(&right.min_score))
            })
            .map_or_else(|| chosen.clone(), |tier| tier.model.clone())
    } else {
        chosen.clone()
    };
    let Some(target) = state
        .models()
        .iter()
        .find(|model| model.name() == delivery_name)
    else {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "wayfinder_router_misconfigured",
            format!("no gateway endpoint configured for model '{delivery_name}'"),
            routing_headers,
        );
    };
    if offline && !model_is_proven_local(target) {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "wayfinder_router_offline_unavailable",
            "offline delivery requires a literal loopback provider endpoint".to_owned(),
            routing_headers,
        );
    }

    let request_body = Value::Object(body);
    let debug = request_header(&headers, DEBUG_HEADER)
        .ok()
        .flatten()
        .is_some_and(|value| matches!(value.trim().to_lowercase().as_str(), "1" | "true" | "yes"));
    let cache_enabled = match state.cache_enabled() {
        Ok(enabled) => enabled,
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wayfinder_router_state_error",
                error.to_string(),
                routing_headers,
            );
        }
    };
    // ChatGPT subscription replies are account-scoped while the normalized
    // boundary intentionally exposes no stable account identifier suitable for
    // a cache key. Never replay or retain them across sign-in state changes.
    let cache_partition = format!(
        "{}:{}",
        key_id.unwrap_or("local"),
        privacy_posture_name(privacy_posture)
    );
    let cache_route = preset_name.as_deref().map_or_else(
        || format!("{mode}:{delivery_name}"),
        |name| format!("{ROUTE_PRESET_PREFIX}{name}"),
    );
    let cacheable = cache_enabled
        && target.provider() != ProviderKind::CodexAppServer
        && !debug
        && is_cacheable(&request_body);
    let mut cache_state = None;
    if cacheable {
        let key = match cache_key_with_version(
            state.cache_generation(),
            &cache_partition,
            &cache_route,
            target.provider_model(),
            &request_body,
        ) {
            Ok(key) => key,
            Err(error) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "wayfinder_router_state_error",
                    error.to_string(),
                    routing_headers,
                );
            }
        };
        let cached = match state.get_cached_response(&key).await {
            Ok(cached) => cached,
            Err(error) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "wayfinder_router_state_error",
                    error.to_string(),
                    routing_headers,
                );
            }
        };
        if let Some(cached) = cached {
            let avoided = turn_cost(
                &delivery_name,
                i64::try_from(cached.prompt_tokens).unwrap_or(i64::MAX),
                i64::try_from(cached.completion_tokens).unwrap_or(i64::MAX),
                &state.price_table().costs,
                cached.estimated,
                None,
            )
            .map_or(0.0, |cost| cost.realized);
            let _ = state.metrics().observe_cache_hit(avoided);
            let _ = state.recent().update_cache(&request_id, "hit");

            let status = match StatusCode::from_u16(cached.status) {
                Ok(status) => status,
                Err(error) => {
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "wayfinder_router_state_error",
                        error.to_string(),
                        routing_headers,
                    );
                }
            };
            let content_type = HeaderValue::from_str(&cached.content_type)
                .unwrap_or_else(|_| HeaderValue::from_static("application/json"));
            let mut response_headers = routing_headers;
            if let Err(message) = insert_header(
                &mut response_headers,
                "x-wayfinder-router-served-by",
                &delivery_name,
            ) {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "wayfinder_router_misconfigured",
                    message,
                    request_id_headers(&request_id),
                );
            }
            response_headers.insert(ROUTER_CACHE_HEADER, HeaderValue::from_static("hit"));
            response_headers.insert(header::CONTENT_TYPE, content_type);
            return (
                status,
                response_headers,
                Bytes::copy_from_slice(&cached.body),
            )
                .into_response();
        }
        let _ = state.metrics().observe_cache_miss();
        cache_state = Some("miss");
    }

    let mut ladder = decision
        .tiers
        .as_ref()
        .map(|tiers| tiers.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    ladder.sort_by(|left, right| left.min_score.total_cmp(&right.min_score));
    let ladder = ladder
        .into_iter()
        .map(|tier| tier.model.clone())
        .collect::<Vec<_>>();
    let failover_header = request_header(&headers, FAILOVER_HEADER).ok().flatten();
    let mut candidates = if request_is_pinned {
        Vec::new()
    } else if let Some(preset_models) = preset_models.take() {
        preset_models
            .into_iter()
            .filter(|model| model != &delivery_name)
            .collect()
    } else {
        target.fallbacks().to_vec()
    };
    if !request_is_pinned && !request_is_preset && (!offline || ladder.is_empty()) {
        candidates.extend(failover_candidates(
            &chosen,
            &ladder,
            state
                .reliability_policy()
                .effective_failover(failover_header),
        ));
    }
    let allowed_models = state
        .access_policy()
        .and_then(|policy| access_grant.map(|grant| policy.allowed_models(grant)))
        .filter(|allowed| !allowed.is_empty());
    let plan = match state
        .reliability_policy()
        .delivery_plan(&delivery_name, &candidates, |name| {
            state
                .models()
                .iter()
                .find(|model| model.name() == name)
                .is_some_and(|model| {
                    assess_model(&routing_request, model).is_eligible()
                        && (!offline || model_is_proven_local(model))
                        && precheck_ok(prompt_estimate, model.context_window())
                        && has_eligible_deployment_target(
                            &state,
                            name,
                            &routing_request,
                            offline,
                            prompt_estimate,
                        )
                        && allowed_models
                            .is_none_or(|allowed| allowed.iter().any(|allowed| allowed == name))
                })
        }) {
        Ok(plan) => plan,
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wayfinder_router_state_error",
                error.to_string(),
                routing_headers,
            );
        }
    };
    if plan.is_empty() {
        if let Some(model) = state.models().iter().find(|model| model.name() == chosen) {
            let assessment = assess_model(&routing_request, model);
            if !assessment.is_eligible() {
                return destination_ineligible_response(
                    &chosen,
                    &assessment,
                    routing_headers,
                    offline,
                );
            }
            if !has_eligible_deployment_target(
                &state,
                &chosen,
                &routing_request,
                offline,
                prompt_estimate,
            ) {
                return error_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "wayfinder_router_destination_ineligible",
                    format!(
                        "destination '{chosen}' has no eligible concrete deployment for this request"
                    ),
                    routing_headers,
                );
            }
        }
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "wayfinder_router_circuit_open",
            format!("no available upstream for '{chosen}' (cooling down or context too small)"),
            routing_headers,
        );
    }

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
                    &routing,
                    &decision,
                    &chosen,
                    mode,
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
        &routing_request,
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
    let selection_reason = delivery_outcome.selection_reason;
    let response = delivery_outcome.response;
    let status = response.status();
    let response_content_type = response.content_type().to_owned();
    let content_type = HeaderValue::from_str(&response_content_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/json"));
    let body = response.into_body();
    let accounted = account_buffered_success(
        &state,
        BufferedAccounting {
            route: &served_by,
            request_id: &request_id,
            messages: &messages,
            access_grant,
            status,
            content_type: &response_content_type,
            body: &body,
            token_reservation,
            shared_budget: _shared_budget,
        },
    )
    .await;
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
                        &cache_partition,
                        &cache_route,
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
                &routing,
                &decision,
                &chosen,
                mode,
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
    if served_by != chosen && !offline {
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
        }
        if established.is_some() {
            break;
        }
    }
    let Some((
        served_by,
        served_by_id,
        selection_reason,
        response,
        reliability_attempt,
        attempt_started,
    )) = established
    else {
        return error_response(last_failure.0, last_failure.1, last_error, headers);
    };
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
    let _ = account_stream_success(
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
        // The OpenAI-compatible adapter is deliberately a pass-through. Its
        // request contract supports multimodal messages and tool declarations;
        // provider-specific rejection still remains a delivery failure.
        ProviderKind::OpenAiCompatible => DestinationCapabilities {
            text: true,
            streaming: true,
            image_input: true,
            tools: true,
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
            ProviderKind::OpenAiCompatible => wayfinder_routing_core::BillingClass::ApiMetered,
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

fn body_rejection_response(state: &AppState, rejection: BytesRejection) -> Response {
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

fn new_request_id() -> String {
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
    let _ = state.savings_ledger().record(&cost, today, key_id);
    let _ = state.persist_savings();
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

fn decision_headers(
    chosen: &str,
    score: f64,
    mode: &str,
    request_id: &str,
    offline: bool,
) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    insert_header(&mut headers, ROUTER_MODEL_HEADER, chosen)?;
    insert_header(&mut headers, ROUTER_SCORE_HEADER, &format!("{score:.2}"))?;
    insert_header(&mut headers, ROUTER_MODE_HEADER, mode)?;
    insert_header(&mut headers, ROUTER_REQUEST_ID_HEADER, request_id)?;
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
    mode: &'static str,
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
    mode: &'static str,
    offline: bool,
    request_id: &str,
    dry_run: bool,
    decision_only: bool,
) -> DecisionResponse {
    DecisionResponse {
        model: chosen.to_owned(),
        score: decision.score,
        mode,
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
mod tests {
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
        Budget as GatewayBudget, DeploymentSelectionPolicy, DeploymentSelectionStrategy,
        GatewayConfig, ProviderKind, ProviderTier, RateLimit, RoutePreset, VirtualKey, Workspace,
    };
    use wayfinder_routing_core::{
        ClassifierModel, PrivacyPosture, RoutingConfig, RoutingRequest, RoutingRequirements,
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
        BufferedDelivery, BufferedDeliveryResponse, DeliveryError, DeliveryFuture,
        StreamingDelivery, StreamingDeliveryFuture, StreamingDeliveryResponse,
    };
    use crate::{
        access::AccessPolicy,
        admission::{AdmissionError, AdmissionPolicy},
        auth,
        cache::{CacheSettings, CachedResponse, cache_key},
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
                    ScriptedStreamOutcome::Cancellable(dropped) => {
                        Ok(StreamingDeliveryResponse::new(
                            StatusCode::OK,
                            "text/event-stream",
                            Box::pin(PendingDropStream { dropped }),
                        ))
                    }
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
        let cost = turn_cost("cloud", 1_000, 0, &state.price_table().costs, false, None)?;
        state.savings_ledger().record(&cost, utc_today()?, key_id)?;
        Ok(())
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
    async fn twenty_workspace_users_send_multi_turn_chats_without_router_serialization()
    -> TestResult {
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
            .audit_log()
            .record("system", "before.reload", None, None)
            .await?;
        current
            .audit_log()
            .record("system", "after.reload", None, None)
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
                    && id.chars().all(|character| {
                        character.is_ascii_digit() || ('a'..='f').contains(&character)
                    })
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
            initial_text
                .contains("wayfinder_router_build_info{version=\"test-build-secret-free\"} 1")
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
        assert!(
            text.contains("wayfinder_router_requests_total{model=\"local\",mode=\"scored\"} 1")
        );
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
                ConfiguredModel::new("apple-local", "", "system-default", None, true)
                    .with_provider(
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

        let captured = seen
            .lock()
            .map_err(|_| std::io::Error::other("fake delivery lock poisoned"))?;
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, "upstream-small");
        assert_eq!(captured[0].1["messages"][0]["content"], "hi");
        assert!(captured[0].1.get("wayfinder_tuning").is_none());
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
    async fn codex_account_routes_never_replay_or_store_exact_response_cache_entries() -> TestResult
    {
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
        assert_eq!(
            degraded
                .headers()
                .get("x-wayfinder-router-budget")
                .and_then(|value| value.to_str().ok()),
            Some("degraded")
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
    async fn streaming_relay_establishes_before_commit_and_accounts_only_on_completion()
    -> TestResult {
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
        assert!(recent["recent"][1].get("cost").is_none());
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
            let text =
                String::from_utf8(to_bytes(response.into_body(), usize::MAX).await?.to_vec())?;
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
        let failed_body =
            String::from_utf8(to_bytes(failed.into_body(), usize::MAX).await?.to_vec())?;
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
                    ScriptedStreamOutcome::Chunks(vec![Ok(Bytes::from_static(
                        b"data: [DONE]\n\n",
                    ))]),
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
        let unproven_apple =
            ConfiguredModel::new("apple-unproven", "", "system-default", None, true)
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
    async fn workspace_scopes_a_multi_turn_request_without_storing_the_conversation() -> TestResult
    {
        let mut config = GatewayConfig::default();
        config.workspaces.insert(
            "mobile".to_owned(),
            Workspace {
                models: vec!["local".to_owned()],
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
}
