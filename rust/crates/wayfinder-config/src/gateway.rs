//! Semantic parsing and validation for the optional gateway configuration.
//!
//! This module deliberately stores credential references only. It does not read
//! environment variables, execute `api_key_cmd`, or otherwise resolve provider
//! secret values.

use std::fmt;

use indexmap::IndexMap;
use toml::Value;

use super::{ConfigError, format_number, invalid};

/// Supported portions of a chat transcript that may be scored.
pub const ROUTE_ON_SCOPES: [&str; 4] = ["turn", "last_user", "user", "all"];
/// Supported cross-tier failover policies.
pub const FAILOVER_POLICIES: [&str; 3] = ["same-tier", "degrade", "escalate"];
/// Supported spend-budget windows.
pub const BUDGET_WINDOWS: [&str; 3] = ["day", "month", "all"];
/// Supported actions when a spend budget is exhausted.
pub const BUDGET_BREACH_ACTIONS: [&str; 2] = ["degrade", "block"];

/// Default exact-match cache lifetime, in seconds.
pub const DEFAULT_CACHE_TTL: f64 = 300.0;
/// Default maximum number of exact-match cache entries.
pub const DEFAULT_CACHE_MAX_ENTRIES: u64 = 1_024;
/// Default maximum aggregate cache body size (64 MiB).
pub const DEFAULT_CACHE_MAX_BYTES: u64 = 64 * 1_024 * 1_024;
/// Default rate-limit window, in seconds.
pub const DEFAULT_RATE_LIMIT_WINDOW: f64 = 60.0;
/// Default simultaneous upstream deliveries accepted by one process.
pub const DEFAULT_MAX_IN_FLIGHT: u64 = 32;
/// Default requests allowed to wait for an upstream delivery slot.
pub const DEFAULT_MAX_QUEUED: u64 = 64;
/// Default maximum time a queued request waits for a delivery slot.
pub const DEFAULT_QUEUE_TIMEOUT: f64 = 2.0;
/// Supported shared-state backends.
pub const STATE_BACKENDS: [&str; 2] = ["memory", "redis"];
/// Default namespace used for shared gateway state keys.
pub const DEFAULT_STATE_NAMESPACE: &str = "wayfinder";

/// Stable delivery identity for a configured gateway model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProviderKind {
    /// An HTTP endpoint implementing the OpenAI chat-completions contract.
    #[default]
    OpenAiCompatible,
    /// Apple's native on-device system language model.
    AppleFoundationModels,
    /// A bounded Codex app-server authenticated through ChatGPT.
    CodexAppServer,
}

impl ProviderKind {
    /// Stable TOML spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "openai-compatible",
            Self::AppleFoundationModels => "apple-foundation-models",
            Self::CodexAppServer => "codex-app-server",
        }
    }
}

/// Explicit locality asserted by a provider-specific configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderTier {
    /// Delivery remains on the local device.
    Local,
}

impl ProviderTier {
    /// Stable TOML spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
        }
    }
}

/// A typed provider target.
///
/// `api_key_env` names an environment variable. `api_key_cmd` is the legacy
/// command reference used to populate that variable when absent. Both fields
/// are inert configuration references here; neither can contain a resolved key
/// produced by this parser.
#[derive(Clone, Debug, PartialEq)]
pub struct GatewayModel {
    /// Provider delivery kind. Omitted TOML defaults to OpenAI-compatible.
    pub provider: ProviderKind,
    /// OpenAI-compatible API base URL; absent for native providers.
    pub base_url: Option<String>,
    /// Provider model identifier sent upstream.
    pub model: String,
    /// Explicit native-provider locality, when required by that provider.
    pub tier: Option<ProviderTier>,
    /// Optional environment-variable name holding the provider key.
    pub api_key_env: Option<String>,
    /// Optional legacy command reference used by a separate secret resolver.
    pub api_key_cmd: Option<String>,
    /// Optional informational cost per one thousand tokens.
    pub cost_per_1k: Option<f64>,
    /// Same-tier endpoint names attempted after this endpoint fails.
    pub fallbacks: Vec<String>,
    /// Optional maximum context size in tokens.
    pub context_window: Option<u64>,
}

/// A spend cap over the gateway's realized-cost ledger.
#[derive(Clone, Debug, PartialEq)]
pub struct Budget {
    /// Positive spend ceiling.
    pub limit: f64,
    /// One of [`BUDGET_WINDOWS`].
    pub window: String,
    /// One of [`BUDGET_BREACH_ACTIONS`].
    pub on_breach: String,
}

/// Exact-match response cache settings.
#[derive(Clone, Debug, PartialEq)]
pub struct CacheConfig {
    /// Whether response retention is enabled.
    pub enabled: bool,
    /// Entry lifetime in seconds; zero means no expiry.
    pub ttl: f64,
    /// Positive LRU entry-count bound.
    pub max_entries: u64,
    /// Positive aggregate body-size bound.
    pub max_bytes: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl: DEFAULT_CACHE_TTL,
            max_entries: DEFAULT_CACHE_MAX_ENTRIES,
            max_bytes: DEFAULT_CACHE_MAX_BYTES,
        }
    }
}

/// Fixed-window request and token limits.
#[derive(Clone, Debug, PartialEq)]
pub struct RateLimit {
    /// Optional positive requests-per-minute limit.
    pub rpm: Option<u64>,
    /// Optional positive upstream-tokens-per-minute limit.
    pub tpm: Option<u64>,
    /// Positive window duration in seconds.
    pub window: f64,
}

/// Process-local upstream delivery admission bounds.
#[derive(Clone, Debug, PartialEq)]
pub struct ConcurrencyConfig {
    /// Maximum simultaneous upstream deliveries.
    pub max_in_flight: u64,
    /// Maximum requests waiting for a delivery slot.
    pub max_queued: u64,
    /// Maximum queue wait in seconds.
    pub queue_timeout: f64,
}

/// Shared state configuration for replicated gateway policies.
#[derive(Clone, Debug, PartialEq)]
pub struct StateConfig {
    /// State backend implementation: `memory` or `redis`.
    pub backend: String,
    /// Redis connection URL when the backend is `redis`.
    pub url: Option<String>,
    /// Prefix used for all shared keys.
    pub namespace: String,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            backend: "memory".to_owned(),
            url: None,
            namespace: DEFAULT_STATE_NAMESPACE.to_owned(),
        }
    }
}

/// A bounded policy namespace shared by one or more virtual keys.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Workspace {
    /// Configured-model allowlist; empty means unrestricted.
    pub models: Vec<String>,
    /// Optional workspace-wide request/token limit.
    pub rate_limit: Option<RateLimit>,
}

impl Default for ConcurrencyConfig {
    fn default() -> Self {
        Self {
            max_in_flight: DEFAULT_MAX_IN_FLIGHT,
            max_queued: DEFAULT_MAX_QUEUED,
            queue_timeout: DEFAULT_QUEUE_TIMEOUT,
        }
    }
}

/// A gateway-issued virtual credential and its optional restrictions.
#[derive(Clone, PartialEq)]
pub struct VirtualKey {
    /// Lowercase SHA-256 digest of the credential, never its plaintext value.
    pub hash: String,
    /// Attribution labels.
    pub tags: Vec<String>,
    /// Optional workspace policy inherited by this key.
    pub workspace: Option<String>,
    /// Optional per-key spend cap.
    pub budget: Option<Budget>,
    /// Optional per-key request/token limit.
    pub rate_limit: Option<RateLimit>,
    /// Configured-model allowlist; empty means unrestricted.
    pub models: Vec<String>,
}

impl fmt::Debug for VirtualKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VirtualKey")
            .field("hash", &"<redacted sha256>")
            .field("tags", &self.tags)
            .field("workspace", &self.workspace)
            .field("budget", &self.budget)
            .field("rate_limit", &self.rate_limit)
            .field("models", &self.models)
            .finish()
    }
}

/// Complete semantic contents of the optional `[gateway]` table.
#[derive(Clone, Debug, PartialEq)]
pub struct GatewayConfig {
    /// Upstream endpoints keyed by routing model name.
    pub models: IndexMap<String, GatewayModel>,
    /// Transcript scope selected for routing.
    pub route_on: String,
    /// Whether conversations latch to the highest tier reached.
    pub sticky: bool,
    /// Calm turns required before a sticky latch decays; zero means never.
    pub sticky_cooldown: u64,
    /// Whether leading in-message slash directives are enabled.
    pub slash_directives: bool,
    /// Whether delivery is constrained to the cheapest/local tier.
    pub offline: bool,
    /// Number of retries after an initial upstream attempt.
    pub retries: u64,
    /// Consecutive failures that open an endpoint circuit breaker.
    pub breaker_threshold: u64,
    /// Circuit-breaker cooldown in seconds.
    pub breaker_cooldown: f64,
    /// Cross-tier behavior after same-tier attempts are exhausted.
    pub failover: String,
    /// Optional gateway-wide spend cap.
    pub budget: Option<Budget>,
    /// Optional exact-match response cache.
    pub cache: Option<CacheConfig>,
    /// Optional gateway-wide request/token limit.
    pub rate_limit: Option<RateLimit>,
    /// Process-local upstream delivery admission bounds.
    pub concurrency: ConcurrencyConfig,
    /// Shared state backend for fleet-wide policy counters.
    pub state: StateConfig,
    /// Workspace policy namespaces keyed by operator-selected identifier.
    pub workspaces: IndexMap<String, Workspace>,
    /// Virtual credentials keyed by operator-selected identifier.
    pub keys: IndexMap<String, VirtualKey>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            models: IndexMap::new(),
            route_on: "turn".to_owned(),
            sticky: false,
            sticky_cooldown: 0,
            slash_directives: false,
            offline: false,
            retries: 2,
            breaker_threshold: 5,
            breaker_cooldown: 30.0,
            failover: "same-tier".to_owned(),
            budget: None,
            cache: None,
            rate_limit: None,
            concurrency: ConcurrencyConfig::default(),
            state: StateConfig::default(),
            workspaces: IndexMap::new(),
            keys: IndexMap::new(),
        }
    }
}

/// Parse gateway semantics from a complete `wayfinder-router.toml` document.
///
/// Unknown fields are intentionally ignored, matching the current Python
/// runtime. Syntactically valid non-finite TOML floats are intentionally
/// rejected even at otherwise unbounded numeric fields.
pub fn gateway_config_from_toml(text: &str, where_: &str) -> Result<GatewayConfig, ConfigError> {
    let root: Value = toml::from_str(text).map_err(|error| ConfigError::InvalidToml {
        where_: where_.to_owned(),
        message: error.to_string(),
    })?;
    let root = root
        .as_table()
        .ok_or_else(|| invalid(where_, "document root must be a table"))?;
    let Some(raw_gateway) = root.get("gateway") else {
        return Ok(GatewayConfig::default());
    };
    let gateway = raw_gateway
        .as_table()
        .ok_or_else(|| invalid(where_, "'[gateway]' must be a table"))?;

    let route_on = optional_allowed_string(
        gateway.get("route_on"),
        "turn",
        &ROUTE_ON_SCOPES,
        where_,
        "gateway.route_on",
    )?;
    let sticky = optional_bool(gateway.get("sticky"), false, where_, "gateway.sticky")?;
    let sticky_cooldown = optional_non_negative_integer(
        gateway.get("sticky_cooldown"),
        0,
        where_,
        "gateway.sticky_cooldown",
    )?;
    let slash_directives = optional_bool(
        gateway.get("slash_directives"),
        false,
        where_,
        "gateway.slash_directives",
    )?;
    let offline = optional_bool(gateway.get("offline"), false, where_, "gateway.offline")?;
    let retries =
        optional_non_negative_integer(gateway.get("retries"), 2, where_, "gateway.retries")?;
    let breaker_threshold = optional_positive_integer(
        gateway.get("breaker_threshold"),
        5,
        where_,
        "gateway.breaker_threshold",
    )?;
    let breaker_cooldown = optional_non_negative_number(
        gateway.get("breaker_cooldown"),
        30.0,
        where_,
        "gateway.breaker_cooldown",
    )?;
    let failover = optional_allowed_string(
        gateway.get("failover"),
        "same-tier",
        &FAILOVER_POLICIES,
        where_,
        "gateway.failover",
    )?;
    let budget = parse_budget(gateway.get("budget"), where_)?;
    let cache = parse_cache(gateway.get("cache"), where_)?;
    let rate_limit = parse_rate_limit(gateway.get("rate_limit"), where_)?;
    let concurrency = parse_concurrency(gateway.get("concurrency"), where_)?;
    let state = parse_state(gateway.get("state"), where_)?;
    let workspaces = parse_workspaces(gateway.get("workspaces"), where_)?;
    let keys = parse_keys(gateway.get("keys"), where_)?;
    let models = parse_models(gateway.get("models"), where_)?;

    validate_model_fallbacks(&models, where_)?;
    validate_workspace_models(&workspaces, &models, where_)?;
    validate_keys(&keys, &workspaces, &models, where_)?;

    Ok(GatewayConfig {
        models,
        route_on,
        sticky,
        sticky_cooldown,
        slash_directives,
        offline,
        retries,
        breaker_threshold,
        breaker_cooldown,
        failover,
        budget,
        cache,
        rate_limit,
        concurrency,
        state,
        workspaces,
        keys,
    })
}

/// Deterministically emit the semantic gateway configuration as TOML.
///
/// Scalar defaults are omitted in the same places as the Python emitter. Keys
/// and models retain their [`IndexMap`] insertion order. Provider credential
/// fields remain inert references: this function never resolves an environment
/// variable or executes a command.
///
/// The generated document is parsed through [`gateway_config_from_toml`] before
/// it is returned. This rejects invalid manually-constructed values and guards
/// the table-path and string escaping boundary.
pub fn dump_gateway_toml(gateway: &GatewayConfig) -> Result<String, ConfigError> {
    let rendered = render_gateway_toml(gateway);
    gateway_config_from_toml(&rendered, "generated gateway TOML")?;
    Ok(rendered)
}

fn render_gateway_toml(gateway: &GatewayConfig) -> String {
    let mut blocks = Vec::new();
    let nondefault_gateway = gateway.route_on != "turn"
        || gateway.sticky
        || gateway.sticky_cooldown != 0
        || gateway.slash_directives
        || gateway.offline
        || gateway.retries != 2
        || gateway.breaker_threshold != 5
        || gateway.breaker_cooldown != 30.0
        || gateway.failover != "same-tier";
    if nondefault_gateway {
        let mut lines = vec!["[gateway]".to_owned()];
        if gateway.route_on != "turn" {
            lines.push(format!(
                "route_on = {}",
                quote_toml_string(&gateway.route_on)
            ));
        }
        if gateway.sticky {
            lines.push("sticky = true".to_owned());
        }
        if gateway.sticky_cooldown != 0 {
            lines.push(format!("sticky_cooldown = {}", gateway.sticky_cooldown));
        }
        if gateway.slash_directives {
            lines.push("slash_directives = true".to_owned());
        }
        if gateway.offline {
            lines.push("offline = true".to_owned());
        }
        if gateway.retries != 2 {
            lines.push(format!("retries = {}", gateway.retries));
        }
        if gateway.breaker_threshold != 5 {
            lines.push(format!("breaker_threshold = {}", gateway.breaker_threshold));
        }
        if gateway.breaker_cooldown != 30.0 {
            lines.push(format!(
                "breaker_cooldown = {}",
                format_number(gateway.breaker_cooldown)
            ));
        }
        if gateway.failover != "same-tier" {
            lines.push(format!(
                "failover = {}",
                quote_toml_string(&gateway.failover)
            ));
        }
        blocks.push(lines.join("\n"));
    }

    if let Some(budget) = &gateway.budget {
        blocks.push(render_budget("[gateway.budget]", budget));
    }
    if let Some(cache) = &gateway.cache {
        let mut lines = vec![
            "[gateway.cache]".to_owned(),
            format!("enabled = {}", cache.enabled),
        ];
        if cache.ttl != DEFAULT_CACHE_TTL {
            lines.push(format!("ttl = {}", format_number(cache.ttl)));
        }
        if cache.max_entries != DEFAULT_CACHE_MAX_ENTRIES {
            lines.push(format!("max_entries = {}", cache.max_entries));
        }
        if cache.max_bytes != DEFAULT_CACHE_MAX_BYTES {
            lines.push(format!("max_bytes = {}", cache.max_bytes));
        }
        blocks.push(lines.join("\n"));
    }
    if let Some(rate_limit) = &gateway.rate_limit {
        blocks.push(render_rate_limit("[gateway.rate_limit]", rate_limit));
    }
    if gateway.concurrency != ConcurrencyConfig::default() {
        let mut lines = vec!["[gateway.concurrency]".to_owned()];
        if gateway.concurrency.max_in_flight != DEFAULT_MAX_IN_FLIGHT {
            lines.push(format!(
                "max_in_flight = {}",
                gateway.concurrency.max_in_flight
            ));
        }
        if gateway.concurrency.max_queued != DEFAULT_MAX_QUEUED {
            lines.push(format!("max_queued = {}", gateway.concurrency.max_queued));
        }
        if gateway.concurrency.queue_timeout != DEFAULT_QUEUE_TIMEOUT {
            lines.push(format!(
                "queue_timeout = {}",
                format_number(gateway.concurrency.queue_timeout)
            ));
        }
        blocks.push(lines.join("\n"));
    }
    if gateway.state != StateConfig::default() {
        let mut lines = vec!["[gateway.state]".to_owned()];
        if gateway.state.backend != "memory" {
            lines.push(format!(
                "backend = {}",
                quote_toml_string(&gateway.state.backend)
            ));
        }
        if let Some(url) = &gateway.state.url {
            lines.push(format!("url = {}", quote_toml_string(url)));
        }
        if gateway.state.namespace != DEFAULT_STATE_NAMESPACE {
            lines.push(format!(
                "namespace = {}",
                quote_toml_string(&gateway.state.namespace)
            ));
        }
        blocks.push(lines.join("\n"));
    }

    for (workspace_id, workspace) in &gateway.workspaces {
        let segment = quote_toml_key_segment(workspace_id);
        let mut lines = vec![format!("[gateway.workspaces.{segment}]")];
        if !workspace.models.is_empty() {
            lines.push(format!(
                "models = {}",
                render_string_list(&workspace.models)
            ));
        }
        blocks.push(lines.join("\n"));
        if let Some(rate_limit) = &workspace.rate_limit {
            blocks.push(render_rate_limit(
                &format!("[gateway.workspaces.{segment}.rate_limit]"),
                rate_limit,
            ));
        }
    }

    for (key_id, key) in &gateway.keys {
        let key_segment = quote_toml_key_segment(key_id);
        let mut lines = vec![
            format!("[gateway.keys.{key_segment}]"),
            format!("hash = {}", quote_toml_string(&key.hash)),
        ];
        if !key.tags.is_empty() {
            lines.push(format!("tags = {}", render_string_list(&key.tags)));
        }
        if let Some(workspace) = &key.workspace {
            lines.push(format!("workspace = {}", quote_toml_string(workspace)));
        }
        if !key.models.is_empty() {
            lines.push(format!("models = {}", render_string_list(&key.models)));
        }
        blocks.push(lines.join("\n"));
        if let Some(budget) = &key.budget {
            blocks.push(render_budget(
                &format!("[gateway.keys.{key_segment}.budget]"),
                budget,
            ));
        }
        if let Some(rate_limit) = &key.rate_limit {
            blocks.push(render_rate_limit(
                &format!("[gateway.keys.{key_segment}.rate_limit]"),
                rate_limit,
            ));
        }
    }

    for (name, model) in &gateway.models {
        let mut lines = vec![format!("[gateway.models.{}]", quote_toml_key_segment(name))];
        if model.provider != ProviderKind::OpenAiCompatible {
            lines.push(format!(
                "provider = {}",
                quote_toml_string(model.provider.as_str())
            ));
        }
        if let Some(base_url) = &model.base_url {
            lines.push(format!("base_url = {}", quote_toml_string(base_url)));
        }
        lines.push(format!("model = {}", quote_toml_string(&model.model)));
        if let Some(tier) = model.tier {
            lines.push(format!("tier = {}", quote_toml_string(tier.as_str())));
        }
        if let Some(api_key_env) = &model.api_key_env {
            lines.push(format!("api_key_env = {}", quote_toml_string(api_key_env)));
        }
        if let Some(api_key_cmd) = &model.api_key_cmd {
            lines.push(format!("api_key_cmd = {}", quote_toml_string(api_key_cmd)));
        }
        if let Some(cost_per_1k) = model.cost_per_1k {
            lines.push(format!("cost_per_1k = {}", format_number(cost_per_1k)));
        }
        if !model.fallbacks.is_empty() {
            lines.push(format!(
                "fallbacks = {}",
                render_string_list(&model.fallbacks)
            ));
        }
        if let Some(context_window) = model.context_window {
            lines.push(format!("context_window = {context_window}"));
        }
        blocks.push(lines.join("\n"));
    }
    blocks.join("\n\n")
}

fn render_budget(header: &str, budget: &Budget) -> String {
    let mut lines = vec![
        header.to_owned(),
        format!("limit = {}", format_number(budget.limit)),
    ];
    if budget.window != "day" {
        lines.push(format!("window = {}", quote_toml_string(&budget.window)));
    }
    if budget.on_breach != "degrade" {
        lines.push(format!(
            "on_breach = {}",
            quote_toml_string(&budget.on_breach)
        ));
    }
    lines.join("\n")
}

fn render_rate_limit(header: &str, rate_limit: &RateLimit) -> String {
    let mut lines = vec![header.to_owned()];
    if let Some(rpm) = rate_limit.rpm {
        lines.push(format!("rpm = {rpm}"));
    }
    if let Some(tpm) = rate_limit.tpm {
        lines.push(format!("tpm = {tpm}"));
    }
    if rate_limit.window != DEFAULT_RATE_LIMIT_WINDOW {
        lines.push(format!("window = {}", format_number(rate_limit.window)));
    }
    lines.join("\n")
}

fn quote_toml_string(value: &str) -> String {
    Value::String(value.to_owned()).to_string()
}

fn quote_toml_key_segment(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        value.to_owned()
    } else {
        quote_toml_string(value)
    }
}

fn render_string_list(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| quote_toml_string(value))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn parse_budget(value: Option<&Value>, where_: &str) -> Result<Option<Budget>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let table = value
        .as_table()
        .ok_or_else(|| invalid(where_, "'[gateway.budget]' must be a table"))?;
    let limit = table
        .get("limit")
        .and_then(finite_number)
        .filter(|limit| *limit > 0.0)
        .ok_or_else(|| invalid(where_, "'gateway.budget.limit' must be a positive number"))?;
    let window = optional_allowed_string(
        table.get("window"),
        "day",
        &BUDGET_WINDOWS,
        where_,
        "gateway.budget.window",
    )?;
    let on_breach = optional_allowed_string(
        table.get("on_breach"),
        "degrade",
        &BUDGET_BREACH_ACTIONS,
        where_,
        "gateway.budget.on_breach",
    )?;
    Ok(Some(Budget {
        limit,
        window,
        on_breach,
    }))
}

fn parse_cache(value: Option<&Value>, where_: &str) -> Result<Option<CacheConfig>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let table = value
        .as_table()
        .ok_or_else(|| invalid(where_, "'[gateway.cache]' must be a table"))?;
    Ok(Some(CacheConfig {
        enabled: optional_bool(table.get("enabled"), false, where_, "gateway.cache.enabled")?,
        ttl: optional_non_negative_number(
            table.get("ttl"),
            DEFAULT_CACHE_TTL,
            where_,
            "gateway.cache.ttl",
        )?,
        max_entries: optional_positive_integer(
            table.get("max_entries"),
            DEFAULT_CACHE_MAX_ENTRIES,
            where_,
            "gateway.cache.max_entries",
        )?,
        max_bytes: optional_positive_integer(
            table.get("max_bytes"),
            DEFAULT_CACHE_MAX_BYTES,
            where_,
            "gateway.cache.max_bytes",
        )?,
    }))
}

fn parse_rate_limit(value: Option<&Value>, where_: &str) -> Result<Option<RateLimit>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let table = value
        .as_table()
        .ok_or_else(|| invalid(where_, "'[gateway.rate_limit]' must be a table"))?;
    let rpm = positive_integer(table.get("rpm"), where_, "gateway.rate_limit.rpm")?;
    let tpm = positive_integer(table.get("tpm"), where_, "gateway.rate_limit.tpm")?;
    if rpm.is_none() && tpm.is_none() {
        return Err(invalid(
            where_,
            "'[gateway.rate_limit]' must set 'rpm' and/or 'tpm'",
        ));
    }
    let window = optional_positive_number(
        table.get("window"),
        DEFAULT_RATE_LIMIT_WINDOW,
        where_,
        "gateway.rate_limit.window",
    )?;
    Ok(Some(RateLimit { rpm, tpm, window }))
}

fn parse_concurrency(
    value: Option<&Value>,
    where_: &str,
) -> Result<ConcurrencyConfig, ConfigError> {
    let Some(value) = value else {
        return Ok(ConcurrencyConfig::default());
    };
    let table = value
        .as_table()
        .ok_or_else(|| invalid(where_, "'[gateway.concurrency]' must be a table"))?;
    Ok(ConcurrencyConfig {
        max_in_flight: optional_positive_integer(
            table.get("max_in_flight"),
            DEFAULT_MAX_IN_FLIGHT,
            where_,
            "gateway.concurrency.max_in_flight",
        )?,
        max_queued: optional_non_negative_integer(
            table.get("max_queued"),
            DEFAULT_MAX_QUEUED,
            where_,
            "gateway.concurrency.max_queued",
        )?,
        queue_timeout: optional_positive_number(
            table.get("queue_timeout"),
            DEFAULT_QUEUE_TIMEOUT,
            where_,
            "gateway.concurrency.queue_timeout",
        )?,
    })
}

fn parse_state(value: Option<&Value>, where_: &str) -> Result<StateConfig, ConfigError> {
    let Some(value) = value else {
        return Ok(StateConfig::default());
    };
    let table = value
        .as_table()
        .ok_or_else(|| invalid(where_, "'[gateway.state]' must be a table"))?;
    let backend = optional_allowed_string(
        table.get("backend"),
        "memory",
        &STATE_BACKENDS,
        where_,
        "gateway.state.backend",
    )?;
    let url = optional_non_empty_string(table.get("url"), where_, "gateway.state.url")?;
    let namespace =
        optional_non_empty_string(table.get("namespace"), where_, "gateway.state.namespace")?
            .unwrap_or_else(|| DEFAULT_STATE_NAMESPACE.to_owned());
    if !valid_policy_id(&namespace) {
        return Err(invalid(
            where_,
            "'gateway.state.namespace' must be 1-128 visible ASCII characters",
        ));
    }
    if backend == "redis" && url.is_none() {
        return Err(invalid(
            where_,
            "'gateway.state.url' is required when gateway.state.backend is 'redis'",
        ));
    }
    if backend == "memory" && url.is_some() {
        return Err(invalid(
            where_,
            "'gateway.state.url' is only valid when gateway.state.backend is 'redis'",
        ));
    }
    Ok(StateConfig {
        backend,
        url,
        namespace,
    })
}

fn parse_keys(
    value: Option<&Value>,
    where_: &str,
) -> Result<IndexMap<String, VirtualKey>, ConfigError> {
    let Some(value) = value else {
        return Ok(IndexMap::new());
    };
    let table = value
        .as_table()
        .ok_or_else(|| invalid(where_, "'[gateway.keys]' must be a table"))?;
    let mut keys = IndexMap::new();
    for (key_id, value) in table {
        let entry = value
            .as_table()
            .ok_or_else(|| invalid(where_, format!("'[gateway.keys.{key_id}]' must be a table")))?;
        let hash = entry
            .get("hash")
            .and_then(Value::as_str)
            .filter(|hash| is_sha256_hex(hash))
            .ok_or_else(|| {
                invalid(
                    where_,
                    format!(
                        "'gateway.keys.{key_id}.hash' must be a 64-char SHA-256 hex digest \
                         (mint a key with `wayfinder-router keys new`)"
                    ),
                )
            })?
            .to_ascii_lowercase();
        let tags = optional_non_empty_string_list(
            entry.get("tags"),
            where_,
            &format!("gateway.keys.{key_id}.tags"),
            "a list of strings",
        )?;
        let workspace = entry
            .get("workspace")
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        invalid(
                            where_,
                            format!("'gateway.keys.{key_id}.workspace' must be a non-empty string"),
                        )
                    })
            })
            .transpose()?;
        let models = optional_non_empty_string_list(
            entry.get("models"),
            where_,
            &format!("gateway.keys.{key_id}.models"),
            "a list of model names",
        )?;
        let nested_where = format!("{where_} [gateway.keys.{key_id}]");
        let budget = parse_budget(entry.get("budget"), &nested_where)?;
        let rate_limit = parse_rate_limit(entry.get("rate_limit"), &nested_where)?;
        keys.insert(
            key_id.clone(),
            VirtualKey {
                hash,
                tags,
                workspace,
                budget,
                rate_limit,
                models,
            },
        );
    }
    Ok(keys)
}

fn parse_workspaces(
    value: Option<&Value>,
    where_: &str,
) -> Result<IndexMap<String, Workspace>, ConfigError> {
    let Some(value) = value else {
        return Ok(IndexMap::new());
    };
    let table = value
        .as_table()
        .ok_or_else(|| invalid(where_, "'[gateway.workspaces]' must be a table"))?;
    let mut workspaces = IndexMap::new();
    for (workspace_id, value) in table {
        if !valid_policy_id(workspace_id) {
            return Err(invalid(
                where_,
                "gateway workspace id must be 1-128 visible ASCII characters",
            ));
        }
        let entry = value.as_table().ok_or_else(|| {
            invalid(
                where_,
                format!("'[gateway.workspaces.{workspace_id}]' must be a table"),
            )
        })?;
        let models = optional_non_empty_string_list(
            entry.get("models"),
            where_,
            &format!("gateway.workspaces.{workspace_id}.models"),
            "a list of model names",
        )?;
        let nested_where = format!("{where_} [gateway.workspaces.{workspace_id}]");
        let rate_limit = parse_rate_limit(entry.get("rate_limit"), &nested_where)?;
        workspaces.insert(workspace_id.clone(), Workspace { models, rate_limit });
    }
    Ok(workspaces)
}

fn valid_policy_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

fn parse_models(
    value: Option<&Value>,
    where_: &str,
) -> Result<IndexMap<String, GatewayModel>, ConfigError> {
    let Some(value) = value else {
        return Ok(IndexMap::new());
    };
    // Python uses `gateway.get("models") or {}`. Retain the resulting
    // compatibility behavior for false-y TOML values rather than introducing
    // an undocumented schema change during the rewrite.
    if is_python_falsey(value) {
        return Ok(IndexMap::new());
    }
    let table = value
        .as_table()
        .ok_or_else(|| invalid(where_, "'[gateway.models]' must be a table"))?;
    let mut models = IndexMap::new();
    for (name, value) in table {
        let entry = value
            .as_table()
            .ok_or_else(|| invalid(where_, format!("'[gateway.models.{name}]' must be a table")))?;
        let provider = match entry.get("provider") {
            None => ProviderKind::OpenAiCompatible,
            Some(Value::String(value)) if value == "openai-compatible" => {
                ProviderKind::OpenAiCompatible
            }
            Some(Value::String(value)) if value == "apple-foundation-models" => {
                ProviderKind::AppleFoundationModels
            }
            Some(Value::String(value)) if value == "codex-app-server" => {
                ProviderKind::CodexAppServer
            }
            Some(_) => {
                return Err(invalid(
                    where_,
                    format!(
                        "'gateway.models.{name}.provider' must be one of openai-compatible, \
                         apple-foundation-models, codex-app-server"
                    ),
                ));
            }
        };
        let tier = match entry.get("tier") {
            None => None,
            Some(Value::String(value)) if value == "local" => Some(ProviderTier::Local),
            Some(_) => {
                return Err(invalid(
                    where_,
                    format!("'gateway.models.{name}.tier' must be local"),
                ));
            }
        };
        let base_url = optional_non_empty_string(
            entry.get("base_url"),
            where_,
            &format!("gateway.models.{name}.base_url"),
        )?;
        let model = required_non_empty_string(
            entry.get("model"),
            where_,
            &format!("gateway.models.{name}.model"),
            "a string",
        )?;
        let api_key_env = optional_non_empty_string(
            entry.get("api_key_env"),
            where_,
            &format!("gateway.models.{name}.api_key_env"),
        )?;
        let api_key_cmd = optional_non_empty_string(
            entry.get("api_key_cmd"),
            where_,
            &format!("gateway.models.{name}.api_key_cmd"),
        )?;
        if api_key_cmd.is_some() && api_key_env.is_none() {
            return Err(invalid(
                where_,
                format!(
                    "'gateway.models.{name}.api_key_cmd' needs 'api_key_env' to name the \
                     variable it fills"
                ),
            ));
        }
        match provider {
            ProviderKind::OpenAiCompatible => {
                if base_url.is_none() {
                    return Err(invalid(
                        where_,
                        format!("'gateway.models.{name}.base_url' must be a string"),
                    ));
                }
                if tier.is_some() {
                    return Err(invalid(
                        where_,
                        format!("'gateway.models.{name}.tier' is only valid for native providers"),
                    ));
                }
            }
            ProviderKind::AppleFoundationModels => {
                if base_url.is_some() {
                    return Err(invalid(
                        where_,
                        format!(
                            "'gateway.models.{name}.base_url' is not valid for \
                             apple-foundation-models"
                        ),
                    ));
                }
                if api_key_env.is_some() || api_key_cmd.is_some() {
                    return Err(invalid(
                        where_,
                        format!(
                            "'gateway.models.{name}' cannot configure credentials for \
                             apple-foundation-models"
                        ),
                    ));
                }
                if model != "system-default" {
                    return Err(invalid(
                        where_,
                        format!(
                            "'gateway.models.{name}.model' must be system-default for \
                             apple-foundation-models"
                        ),
                    ));
                }
                if tier != Some(ProviderTier::Local) {
                    return Err(invalid(
                        where_,
                        format!(
                            "'gateway.models.{name}.tier' must be local for \
                             apple-foundation-models"
                        ),
                    ));
                }
            }
            ProviderKind::CodexAppServer => {
                if base_url.is_some() {
                    return Err(invalid(
                        where_,
                        format!(
                            "'gateway.models.{name}.base_url' is not valid for codex-app-server"
                        ),
                    ));
                }
                if api_key_env.is_some() || api_key_cmd.is_some() {
                    return Err(invalid(
                        where_,
                        format!(
                            "'gateway.models.{name}' cannot configure credentials for \
                             codex-app-server"
                        ),
                    ));
                }
                if tier.is_some() {
                    return Err(invalid(
                        where_,
                        format!("'gateway.models.{name}.tier' is not valid for codex-app-server"),
                    ));
                }
            }
        }
        let cost_per_1k = optional_non_negative_finite_number(
            entry.get("cost_per_1k"),
            where_,
            &format!("gateway.models.{name}.cost_per_1k"),
        )?;
        let fallbacks = optional_non_empty_string_list(
            entry.get("fallbacks"),
            where_,
            &format!("gateway.models.{name}.fallbacks"),
            "a list of model names",
        )?;
        let context_window = positive_integer(
            entry.get("context_window"),
            where_,
            &format!("gateway.models.{name}.context_window"),
        )?;
        models.insert(
            name.clone(),
            GatewayModel {
                provider,
                base_url,
                model,
                tier,
                api_key_env,
                api_key_cmd,
                cost_per_1k,
                fallbacks,
                context_window,
            },
        );
    }
    Ok(models)
}

fn validate_model_fallbacks(
    models: &IndexMap<String, GatewayModel>,
    where_: &str,
) -> Result<(), ConfigError> {
    for (name, model) in models {
        for fallback in &model.fallbacks {
            if !models.contains_key(fallback) {
                return Err(invalid(
                    where_,
                    format!("'gateway.models.{name}.fallbacks' names unknown model '{fallback}'"),
                ));
            }
            if fallback == name {
                return Err(invalid(
                    where_,
                    format!("'gateway.models.{name}.fallbacks' cannot include itself"),
                ));
            }
        }
    }
    Ok(())
}

fn validate_workspace_models(
    workspaces: &IndexMap<String, Workspace>,
    models: &IndexMap<String, GatewayModel>,
    where_: &str,
) -> Result<(), ConfigError> {
    for (workspace_id, workspace) in workspaces {
        for model in &workspace.models {
            if !models.contains_key(model) {
                return Err(invalid(
                    where_,
                    format!(
                        "'gateway.workspaces.{workspace_id}.models' names unknown model '{model}'"
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn validate_keys(
    keys: &IndexMap<String, VirtualKey>,
    workspaces: &IndexMap<String, Workspace>,
    models: &IndexMap<String, GatewayModel>,
    where_: &str,
) -> Result<(), ConfigError> {
    for (key_id, key) in keys {
        let workspace = match key.workspace.as_deref() {
            Some(workspace_id) => Some(workspaces.get(workspace_id).ok_or_else(|| {
                invalid(
                    where_,
                    format!(
                        "'gateway.keys.{key_id}.workspace' names unknown workspace '{workspace_id}'"
                    ),
                )
            })?),
            None => None,
        };
        for model in &key.models {
            if !models.contains_key(model) {
                return Err(invalid(
                    where_,
                    format!("'gateway.keys.{key_id}.models' names unknown model '{model}'"),
                ));
            }
            if let Some(workspace) = workspace {
                if !workspace.models.is_empty() && !workspace.models.contains(model) {
                    return Err(invalid(
                        where_,
                        format!(
                            "'gateway.keys.{key_id}.models' cannot broaden its workspace model policy with '{model}'"
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn optional_bool(
    value: Option<&Value>,
    default: bool,
    where_: &str,
    label: &str,
) -> Result<bool, ConfigError> {
    match value {
        None => Ok(default),
        Some(Value::Boolean(value)) => Ok(*value),
        Some(_) => Err(invalid(where_, format!("'{label}' must be a boolean"))),
    }
}

fn optional_allowed_string(
    value: Option<&Value>,
    default: &str,
    allowed: &[&str],
    where_: &str,
    label: &str,
) -> Result<String, ConfigError> {
    let Some(value) = value else {
        return Ok(default.to_owned());
    };
    let value = value
        .as_str()
        .filter(|value| allowed.contains(value))
        .ok_or_else(|| {
            invalid(
                where_,
                format!("'{label}' must be one of {}", allowed.join(", ")),
            )
        })?;
    Ok(value.to_owned())
}

fn optional_non_negative_integer(
    value: Option<&Value>,
    default: u64,
    where_: &str,
    label: &str,
) -> Result<u64, ConfigError> {
    let Some(value) = value else {
        return Ok(default);
    };
    value
        .as_integer()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| invalid(where_, format!("'{label}' must be a non-negative integer")))
}

fn optional_positive_integer(
    value: Option<&Value>,
    default: u64,
    where_: &str,
    label: &str,
) -> Result<u64, ConfigError> {
    match positive_integer(value, where_, label)? {
        Some(value) => Ok(value),
        None => Ok(default),
    }
}

fn positive_integer(
    value: Option<&Value>,
    where_: &str,
    label: &str,
) -> Result<Option<u64>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value
        .as_integer()
        .and_then(|value| u64::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid(where_, format!("'{label}' must be a positive integer")))?;
    Ok(Some(value))
}

fn optional_non_negative_number(
    value: Option<&Value>,
    default: f64,
    where_: &str,
    label: &str,
) -> Result<f64, ConfigError> {
    let Some(value) = value else {
        return Ok(default);
    };
    finite_number(value)
        .filter(|value| *value >= 0.0)
        .ok_or_else(|| invalid(where_, format!("'{label}' must be a non-negative number")))
}

fn optional_positive_number(
    value: Option<&Value>,
    default: f64,
    where_: &str,
    label: &str,
) -> Result<f64, ConfigError> {
    let Some(value) = value else {
        return Ok(default);
    };
    finite_number(value)
        .filter(|value| *value > 0.0)
        .ok_or_else(|| invalid(where_, format!("'{label}' must be a positive number")))
}

fn optional_non_negative_finite_number(
    value: Option<&Value>,
    where_: &str,
    label: &str,
) -> Result<Option<f64>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = finite_number(value)
        .filter(|value| *value >= 0.0)
        .ok_or_else(|| invalid(where_, format!("'{label}' must be a non-negative number")))?;
    Ok(Some(value))
}

fn finite_number(value: &Value) -> Option<f64> {
    let value = match value {
        Value::Integer(value) => *value as f64,
        Value::Float(value) => *value,
        _ => return None,
    };
    value.is_finite().then_some(value)
}

fn required_non_empty_string(
    value: Option<&Value>,
    where_: &str,
    label: &str,
    expected: &str,
) -> Result<String, ConfigError> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid(where_, format!("'{label}' must be {expected}")))
}

fn optional_non_empty_string(
    value: Option<&Value>,
    where_: &str,
    label: &str,
) -> Result<Option<String>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid(where_, format!("'{label}' must be a non-empty string")))?;
    Ok(Some(value.to_owned()))
}

fn optional_non_empty_string_list(
    value: Option<&Value>,
    where_: &str,
    label: &str,
    expected: &str,
) -> Result<Vec<String>, ConfigError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| invalid(where_, format!("'{label}' must be {expected}")))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| invalid(where_, format!("'{label}' must be {expected}")))
        })
        .collect()
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_python_falsey(value: &Value) -> bool {
    match value {
        Value::String(value) => value.is_empty(),
        Value::Integer(value) => *value == 0,
        Value::Float(value) => *value == 0.0,
        Value::Boolean(value) => !value,
        Value::Array(value) => value.is_empty(),
        Value::Table(value) => value.is_empty(),
        Value::Datetime(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<GatewayConfig, ConfigError> {
        gateway_config_from_toml(text, "fixture")
    }

    #[test]
    fn full_gateway_config_parses_and_ignores_unknown_fields() -> Result<(), ConfigError> {
        let hash = "ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789";
        let text = format!(
            r#"
[gateway]
route_on = "user"
sticky = true
sticky_cooldown = 3
slash_directives = true
offline = true
retries = 4
breaker_threshold = 7
breaker_cooldown = 12.5
failover = "escalate"
future_flag = "ignored"

[gateway.budget]
limit = 25
window = "month"
on_breach = "block"
future_budget_field = true

[gateway.cache]
enabled = true
ttl = 600
max_entries = 2048
max_bytes = 134217728

[gateway.rate_limit]
rpm = 60
tpm = 100000
window = 30

[gateway.models.local]
base_url = "http://localhost:11434/v1"
model = "llama3.2"
cost_per_1k = 0
fallbacks = ["local-backup"]
context_window = 8192
future_model_field = [1, 2]

[gateway.models.local-backup]
base_url = "http://localhost:11435/v1"
model = "llama3.2"
api_key_env = "LOCAL_API_KEY"
api_key_cmd = "credential-helper read local"
cost_per_1k = 0.25

[gateway.keys.team-a]
hash = "{hash}"
tags = ["prod", "team-a"]
models = ["local"]
future_key_field = "ignored"

[gateway.keys.team-a.budget]
limit = 2.5

[gateway.keys.team-a.rate_limit]
rpm = 5
"#
        );

        let config = parse(&text)?;
        assert_eq!(config.route_on, "user");
        assert!(config.sticky);
        assert_eq!(config.sticky_cooldown, 3);
        assert!(config.slash_directives);
        assert!(config.offline);
        assert_eq!(config.retries, 4);
        assert_eq!(config.breaker_threshold, 7);
        assert_eq!(config.breaker_cooldown, 12.5);
        assert_eq!(config.failover, "escalate");
        assert_eq!(
            config.budget,
            Some(Budget {
                limit: 25.0,
                window: "month".to_owned(),
                on_breach: "block".to_owned(),
            })
        );
        assert_eq!(
            config.cache,
            Some(CacheConfig {
                enabled: true,
                ttl: 600.0,
                max_entries: 2_048,
                max_bytes: 134_217_728,
            })
        );
        assert_eq!(
            config.rate_limit,
            Some(RateLimit {
                rpm: Some(60),
                tpm: Some(100_000),
                window: 30.0,
            })
        );
        let local = config
            .models
            .get("local")
            .ok_or_else(|| invalid("test", "missing local model"))?;
        assert_eq!(local.fallbacks, ["local-backup"]);
        assert_eq!(local.context_window, Some(8_192));
        let backup = config
            .models
            .get("local-backup")
            .ok_or_else(|| invalid("test", "missing backup model"))?;
        assert_eq!(backup.api_key_env.as_deref(), Some("LOCAL_API_KEY"));
        assert_eq!(
            backup.api_key_cmd.as_deref(),
            Some("credential-helper read local")
        );
        let key = config
            .keys
            .get("team-a")
            .ok_or_else(|| invalid("test", "missing virtual key"))?;
        assert_eq!(key.hash, hash.to_ascii_lowercase());
        assert_eq!(key.tags, ["prod", "team-a"]);
        assert_eq!(key.models, ["local"]);
        assert_eq!(
            key.budget.as_ref().map(|budget| budget.window.as_str()),
            Some("day")
        );
        assert_eq!(key.rate_limit.as_ref().and_then(|limit| limit.rpm), Some(5));
        Ok(())
    }

    #[test]
    fn absent_and_present_blocks_apply_python_defaults() -> Result<(), ConfigError> {
        assert_eq!(
            parse("[routing]\nthreshold = 0.2\n")?,
            GatewayConfig::default()
        );

        let config = parse(
            "[gateway.cache]\n\
             \n[gateway.rate_limit]\n\
             rpm = 1\n\
             \n[gateway.keys.x]\n\
             hash = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n",
        )?;
        assert_eq!(config.cache, Some(CacheConfig::default()));
        assert_eq!(
            config.rate_limit,
            Some(RateLimit {
                rpm: Some(1),
                tpm: None,
                window: DEFAULT_RATE_LIMIT_WINDOW,
            })
        );
        let key = config
            .keys
            .get("x")
            .ok_or_else(|| invalid("test", "missing virtual key"))?;
        assert!(key.tags.is_empty());
        assert!(key.models.is_empty());
        assert!(key.budget.is_none());
        assert!(key.rate_limit.is_none());
        assert_eq!(config.concurrency, ConcurrencyConfig::default());
        Ok(())
    }

    #[test]
    fn concurrency_bounds_parse_validate_and_round_trip() -> Result<(), ConfigError> {
        let text =
            "[gateway.concurrency]\nmax_in_flight = 20\nmax_queued = 40\nqueue_timeout = 1.5";
        let config = parse(text)?;
        assert_eq!(
            config.concurrency,
            ConcurrencyConfig {
                max_in_flight: 20,
                max_queued: 40,
                queue_timeout: 1.5,
            }
        );
        assert_eq!(dump_gateway_toml(&config)?, text);

        for invalid_text in [
            "[gateway.concurrency]\nmax_in_flight = 0",
            "[gateway.concurrency]\nmax_queued = -1",
            "[gateway.concurrency]\nqueue_timeout = 0",
        ] {
            assert!(parse(invalid_text).is_err(), "accepted: {invalid_text}");
        }
        Ok(())
    }

    #[test]
    fn shared_state_defaults_and_redis_config_round_trip() -> Result<(), ConfigError> {
        assert_eq!(
            parse("[gateway]\nroute_on = \"turn\"\n")?.state,
            StateConfig::default()
        );
        let text = "[gateway.state]\nbackend = \"redis\"\nurl = \"redis://127.0.0.1:6379\"\nnamespace = \"prod\"";
        let config = parse(text)?;
        assert_eq!(
            config.state,
            StateConfig {
                backend: "redis".to_owned(),
                url: Some("redis://127.0.0.1:6379".to_owned()),
                namespace: "prod".to_owned(),
            }
        );
        assert_eq!(dump_gateway_toml(&config)?, text);
        for invalid_text in [
            "[gateway.state]\nbackend = \"redis\"",
            "[gateway.state]\nbackend = \"memory\"\nurl = \"redis://127.0.0.1\"",
            "[gateway.state]\nbackend = \"redis\"\nurl = \"redis://127.0.0.1\"\nnamespace = \"\"",
        ] {
            assert!(parse(invalid_text).is_err(), "accepted: {invalid_text}");
        }
        Ok(())
    }

    #[test]
    fn dump_defaults_to_an_empty_document() -> Result<(), ConfigError> {
        assert_eq!(dump_gateway_toml(&GatewayConfig::default())?, "");
        Ok(())
    }

    #[test]
    fn dump_matches_python_for_supported_simple_values() -> Result<(), ConfigError> {
        let config = parse(
            r#"
[gateway]
route_on = "user"
sticky = true
sticky_cooldown = 3
slash_directives = true
offline = true
retries = 4
breaker_threshold = 7
breaker_cooldown = 12.5
failover = "escalate"

[gateway.budget]
limit = 25
window = "month"
on_breach = "block"

[gateway.cache]
enabled = true
ttl = 600
max_entries = 2048
max_bytes = 134217728

[gateway.rate_limit]
rpm = 60
tpm = 100000
window = 30

[gateway.models.local]
base_url = "http://localhost:11434/v1"
model = "llama3.2"
cost_per_1k = 0
"#,
        )?;
        let expected = r#"[gateway]
route_on = "user"
sticky = true
sticky_cooldown = 3
slash_directives = true
offline = true
retries = 4
breaker_threshold = 7
breaker_cooldown = 12.5
failover = "escalate"

[gateway.budget]
limit = 25.0
window = "month"
on_breach = "block"

[gateway.cache]
enabled = true
ttl = 600.0
max_entries = 2048
max_bytes = 134217728

[gateway.rate_limit]
rpm = 60
tpm = 100000
window = 30.0

[gateway.models.local]
base_url = "http://localhost:11434/v1"
model = "llama3.2"
cost_per_1k = 0.0"#;
        assert_eq!(dump_gateway_toml(&config)?, expected);
        Ok(())
    }

    #[test]
    fn dump_quotes_path_segments_and_strings_then_round_trips() -> Result<(), ConfigError> {
        let mut config = GatewayConfig::default();
        config.models.insert(
            "local.prod".to_owned(),
            GatewayModel {
                provider: ProviderKind::OpenAiCompatible,
                base_url: Some("http://localhost:11434/a path\n/v1".to_owned()),
                model: "model-\"quoted\"".to_owned(),
                tier: None,
                api_key_env: Some("WAYFINDER_\"KEY".to_owned()),
                api_key_cmd: Some("credential helper\nread".to_owned()),
                cost_per_1k: Some(0.0),
                fallbacks: Vec::new(),
                context_window: Some(8_192),
            },
        );
        config.keys.insert(
            "team a".to_owned(),
            VirtualKey {
                hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                tags: vec!["quoted \"tag\"".to_owned(), "line\nbreak".to_owned()],
                workspace: None,
                budget: None,
                rate_limit: None,
                models: vec!["local.prod".to_owned()],
            },
        );

        let dumped = dump_gateway_toml(&config)?;
        assert!(dumped.contains("[gateway.keys.\"team a\"]"));
        assert!(dumped.contains("[gateway.models.\"local.prod\"]"));
        assert!(dumped.contains("model = 'model-\"quoted\"'"));
        assert!(dumped.contains("api_key_cmd = \"\"\""));
        assert_eq!(gateway_config_from_toml(&dumped, "round-trip")?, config);
        Ok(())
    }

    #[test]
    fn workspaces_round_trip_and_keys_may_only_narrow_their_model_policy() -> Result<(), ConfigError>
    {
        let hash = "a".repeat(64);
        let text = format!(
            r#"
[gateway.models.fast]
base_url = "https://fast.example/v1"
model = "fast-upstream"

[gateway.models.deep]
base_url = "https://deep.example/v1"
model = "deep-upstream"

[gateway.workspaces.production]
models = ["fast", "deep"]

[gateway.workspaces.production.rate_limit]
rpm = 120
tpm = 200000

[gateway.keys.mobile]
hash = "{hash}"
workspace = "production"
models = ["fast"]
"#
        );
        let config = parse(&text)?;
        assert_eq!(
            config.keys["mobile"].workspace.as_deref(),
            Some("production")
        );
        assert_eq!(config.workspaces["production"].models, ["fast", "deep"]);
        let dumped = dump_gateway_toml(&config)?;
        assert!(dumped.contains("[gateway.workspaces.production]"));
        assert!(dumped.contains("workspace = \"production\""));
        assert_eq!(parse(&dumped)?, config);

        let broadened = text.replace("models = [\"fast\"]", "models = [\"missing\"]");
        let error = parse(&broadened)
            .err()
            .ok_or_else(|| invalid("test", "unknown key model was accepted"))?;
        assert!(error.to_string().contains("unknown model 'missing'"));

        let outside = format!(
            "{}\n[gateway.models.outside]\nbase_url = \"https://outside.example/v1\"\nmodel = \"outside-upstream\"\n",
            text.replace("models = [\"fast\"]", "models = [\"outside\"]")
        );
        let error = parse(&outside)
            .err()
            .ok_or_else(|| invalid("test", "broadened workspace model was accepted"))?;
        assert!(error.to_string().contains("cannot broaden"));

        let unknown_workspace =
            text.replace("workspace = \"production\"", "workspace = \"missing\"");
        let error = parse(&unknown_workspace)
            .err()
            .ok_or_else(|| invalid("test", "unknown workspace was accepted"))?;
        assert!(error.to_string().contains("unknown workspace 'missing'"));
        Ok(())
    }

    #[test]
    fn apple_foundation_models_is_typed_keyless_local_and_round_trips() -> Result<(), ConfigError> {
        let text = concat!(
            "[gateway.models.apple-local]\n",
            "provider = \"apple-foundation-models\"\n",
            "model = \"system-default\"\n",
            "tier = \"local\"\n",
            "context_window = 4096"
        );
        let config = gateway_config_from_toml(text, "test")?;
        let model = config
            .models
            .get("apple-local")
            .ok_or_else(|| invalid("test", "missing apple model"))?;
        assert_eq!(model.provider, ProviderKind::AppleFoundationModels);
        assert_eq!(model.tier, Some(ProviderTier::Local));
        assert_eq!(model.base_url, None);
        assert_eq!(model.api_key_env, None);
        assert_eq!(dump_gateway_toml(&config)?, text);
        Ok(())
    }

    #[test]
    fn codex_app_server_is_typed_keyless_hosted_and_round_trips() -> Result<(), ConfigError> {
        let text = concat!(
            "[gateway.models.chatgpt]\n",
            "provider = \"codex-app-server\"\n",
            "model = \"gpt-5.6-sol\"\n",
            "context_window = 1050000"
        );
        let config = gateway_config_from_toml(text, "test")?;
        let model = config
            .models
            .get("chatgpt")
            .ok_or_else(|| invalid("test", "missing Codex app-server model"))?;
        assert_eq!(model.provider, ProviderKind::CodexAppServer);
        assert_eq!(model.model, "gpt-5.6-sol");
        assert_eq!(model.base_url, None);
        assert_eq!(model.api_key_env, None);
        assert_eq!(model.api_key_cmd, None);
        assert_eq!(model.tier, None);
        assert_eq!(model.context_window, Some(1_050_000));
        assert_eq!(dump_gateway_toml(&config)?, text);
        assert_eq!(gateway_config_from_toml(text, "round-trip")?, config);
        Ok(())
    }

    #[test]
    fn codex_app_server_requires_a_non_empty_model() {
        for model_field in ["", "model = \"\"\n"] {
            let text =
                format!("[gateway.models.chatgpt]\nprovider = \"codex-app-server\"\n{model_field}");
            assert!(parse(&text).is_err(), "unexpectedly accepted: {text}");
        }
    }

    #[test]
    fn codex_app_server_rejects_endpoint_credentials_and_native_tier() {
        for incompatible_fields in [
            "base_url = \"https://api.openai.com/v1\"\n",
            "api_key_env = \"OPENAI_API_KEY\"\n",
            "api_key_cmd = \"credential-helper read openai\"\n",
            concat!(
                "api_key_env = \"OPENAI_API_KEY\"\n",
                "api_key_cmd = \"credential-helper read openai\"\n"
            ),
            "tier = \"local\"\n",
        ] {
            let text = format!(
                "[gateway.models.chatgpt]\nprovider = \"codex-app-server\"\n\
                 model = \"gpt-5.6-sol\"\n{incompatible_fields}"
            );
            assert!(parse(&text).is_err(), "unexpectedly accepted: {text}");
        }
    }

    #[test]
    fn dump_revalidates_manually_constructed_config() {
        let config = GatewayConfig {
            route_on: "future-scope".to_owned(),
            ..GatewayConfig::default()
        };
        assert!(dump_gateway_toml(&config).is_err());
    }

    #[test]
    fn invalid_shapes_and_cross_references_are_rejected() {
        let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let cases = [
            "[gateway]\nsticky = \"yes\"\n".to_owned(),
            "[gateway]\nmodels = 1\n".to_owned(),
            "[gateway.models.local]\nmodel = \"m\"\n".to_owned(),
            concat!(
                "[gateway.models.cloud]\nbase_url = \"http://x/v1\"\nmodel = \"m\"\n",
                "api_key_cmd = \"credential-helper read cloud\"\n"
            )
            .to_owned(),
            concat!(
                "[gateway.models.local]\nbase_url = \"http://x/v1\"\nmodel = \"m\"\n",
                "fallbacks = [\"missing\"]\n"
            )
            .to_owned(),
            concat!(
                "[gateway.models.local]\nbase_url = \"http://x/v1\"\nmodel = \"m\"\n",
                "fallbacks = [\"local\"]\n"
            )
            .to_owned(),
            concat!(
                "[gateway.keys.x]\nhash = \"short\"\n",
                "[gateway.models.local]\nbase_url = \"http://x/v1\"\nmodel = \"m\"\n"
            )
            .to_owned(),
            format!(
                "[gateway.keys.x]\nhash = \"{hash}\"\nmodels = [\"missing\"]\n\
                 [gateway.models.local]\nbase_url = \"http://x/v1\"\nmodel = \"m\"\n"
            ),
            "[gateway.rate_limit]\nwindow = 60\n".to_owned(),
            "[gateway.budget]\nlimit = 0\n".to_owned(),
        ];
        for case in cases {
            assert!(parse(&case).is_err(), "unexpectedly accepted: {case}");
        }
    }

    #[test]
    fn non_finite_numbers_are_rejected_as_intentional_hardening() {
        for text in [
            "[gateway]\nbreaker_cooldown = nan\n",
            "[gateway.budget]\nlimit = inf\n",
            "[gateway.cache]\nttl = nan\n",
            "[gateway.rate_limit]\nrpm = 1\nwindow = inf\n",
            concat!(
                "[gateway.models.local]\nbase_url = \"http://x/v1\"\nmodel = \"m\"\n",
                "cost_per_1k = inf\n"
            ),
        ] {
            assert!(parse(text).is_err(), "unexpectedly accepted: {text}");
        }
    }

    #[test]
    fn virtual_key_debug_redacts_the_stored_digest() -> Result<(), ConfigError> {
        let hash = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
        let config = parse(&format!("[gateway.keys.team]\nhash = \"{hash}\"\n"))?;
        let rendered = format!("{config:?}");
        assert!(!rendered.contains(hash));
        assert!(rendered.contains("<redacted sha256>"));
        Ok(())
    }

    #[test]
    fn falsey_models_values_retain_python_compatibility() -> Result<(), ConfigError> {
        for text in [
            "[gateway]\nmodels = false\n",
            "[gateway]\nmodels = 0\n",
            "[gateway]\nmodels = \"\"\n",
            "[gateway]\nmodels = []\n",
        ] {
            assert!(parse(text)?.models.is_empty());
        }
        Ok(())
    }

    #[test]
    fn model_and_key_source_order_is_preserved() -> Result<(), ConfigError> {
        let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let text = format!(
            r#"
[gateway.models.zeta]
base_url = "http://z/v1"
model = "z"

[gateway.models.alpha]
base_url = "http://a/v1"
model = "a"

[gateway.models.middle]
base_url = "http://m/v1"
model = "m"

[gateway.keys.second]
hash = "{hash}"

[gateway.keys.first]
hash = "{hash}"
"#
        );

        let config = parse(&text)?;
        assert_eq!(
            config.models.keys().map(String::as_str).collect::<Vec<_>>(),
            ["zeta", "alpha", "middle"]
        );
        assert_eq!(
            config.keys.keys().map(String::as_str).collect::<Vec<_>>(),
            ["second", "first"]
        );
        Ok(())
    }
}
