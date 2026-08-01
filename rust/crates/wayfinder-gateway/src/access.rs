//! Opt-in virtual-key authentication and fixed-window admission policy.
//!
//! Only configured SHA-256 digests and bounded attribution ids are retained.
//! Presented credentials exist for one constant-time comparison pass and are
//! never formatted, logged, or stored in application state.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use thiserror::Error;
use wayfinder_config::gateway::{GatewayConfig, RateLimit as ConfigRateLimit};

use crate::auth;
use crate::rate_limit::{RateLimitError, RateLimiter, RateResult, RateSnapshot};
use crate::state::{StateBackend, StateBackendError, StateLimitConfig};

/// Bound compare work and dynamic key-attribution cardinality.
pub const MAX_VIRTUAL_KEYS: usize = 256;
/// Bound workspace policy cardinality and metric attribution.
pub const MAX_WORKSPACES: usize = 256;
/// Maximum retained bytes in one key id.
pub const MAX_KEY_ID_BYTES: usize = 128;

type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

struct VirtualKeyPolicy {
    id: String,
    digest: String,
    workspace: Option<String>,
    allowed_models: Arc<[String]>,
    workspace_limiter: Option<Arc<RateLimiter>>,
    limiter: Option<Arc<RateLimiter>>,
}

impl fmt::Debug for VirtualKeyPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VirtualKeyPolicy")
            .field("id", &self.id)
            .field("digest", &"<redacted sha256>")
            .field("workspace", &self.workspace)
            .field("allowed_models", &self.allowed_models)
            .field("rate_limit_configured", &self.limiter.is_some())
            .finish()
    }
}

/// Invalid access-policy construction or synchronized state.
#[derive(Debug, Error, PartialEq)]
pub enum AccessPolicyError {
    /// The configured key count exceeds the bounded compare pass.
    #[error("gateway access policy exceeds the virtual-key bound")]
    TooManyKeys,
    /// The configured workspace count exceeds the bounded policy set.
    #[error("gateway access policy exceeds the workspace bound")]
    TooManyWorkspaces,
    /// A key id cannot safely be retained as operational metadata.
    #[error("gateway virtual-key id is empty, too long, or contains control characters")]
    InvalidKeyId,
    /// A configured limiter could not be constructed or synchronized.
    #[error(transparent)]
    RateLimit(#[from] RateLimitError),
    /// A shared-state backend operation failed while constructing policy state.
    #[error(transparent)]
    StateBackend(#[from] StateBackendError),
}

/// Shared authentication and rate-limit state.
#[derive(Clone)]
pub struct AccessPolicy {
    global_limiter: Option<Arc<RateLimiter>>,
    keys: Arc<[VirtualKeyPolicy]>,
    clock: Clock,
    shared: Option<Arc<SharedAccessPolicy>>,
}

impl AccessPolicy {
    /// Build from validated gateway configuration and a process monotonic clock.
    pub fn from_gateway_config(config: &GatewayConfig) -> Result<Self, AccessPolicyError> {
        let started = Instant::now();
        Self::from_gateway_config_with_clock(config, move || started.elapsed().as_secs_f64())
    }

    /// Build with an injected monotonic-seconds clock for deterministic tests.
    pub fn from_gateway_config_with_clock(
        config: &GatewayConfig,
        clock: impl Fn() -> f64 + Send + Sync + 'static,
    ) -> Result<Self, AccessPolicyError> {
        if config.keys.len() > MAX_VIRTUAL_KEYS {
            return Err(AccessPolicyError::TooManyKeys);
        }
        if config.workspaces.len() > MAX_WORKSPACES {
            return Err(AccessPolicyError::TooManyWorkspaces);
        }
        let global_limiter = config
            .rate_limit
            .as_ref()
            .map(new_limiter)
            .transpose()?
            .map(Arc::new);
        let workspaces = config
            .workspaces
            .iter()
            .map(|(id, workspace)| {
                let limiter = workspace
                    .rate_limit
                    .as_ref()
                    .map(new_limiter)
                    .transpose()?
                    .map(Arc::new);
                Ok((id.clone(), (workspace.models.clone(), limiter)))
            })
            .collect::<Result<BTreeMap<_, _>, RateLimitError>>()?;
        let mut keys = Vec::with_capacity(config.keys.len());
        for (id, key) in &config.keys {
            if !valid_key_id(id) {
                return Err(AccessPolicyError::InvalidKeyId);
            }
            let limiter = key
                .rate_limit
                .as_ref()
                .map(new_limiter)
                .transpose()?
                .map(Arc::new);
            let workspace = key
                .workspace
                .as_ref()
                .and_then(|workspace| workspaces.get(workspace));
            let allowed_models = if key.models.is_empty() {
                workspace.map_or_else(Vec::new, |(models, _)| models.clone())
            } else {
                key.models.clone()
            };
            keys.push(VirtualKeyPolicy {
                id: id.clone(),
                digest: key.hash.clone(),
                workspace: key.workspace.clone(),
                allowed_models: Arc::from(allowed_models),
                workspace_limiter: workspace.and_then(|(_, limiter)| limiter.clone()),
                limiter,
            });
        }
        Ok(Self {
            global_limiter,
            keys: Arc::from(keys),
            clock: Arc::new(clock),
            shared: None,
        })
    }

    /// Build policy state with an optional fleet-wide backend.
    pub fn from_gateway_config_with_backend(
        config: &GatewayConfig,
        clock: impl Fn() -> f64 + Send + Sync + 'static,
        backend: Arc<dyn StateBackend>,
    ) -> Result<Self, AccessPolicyError> {
        let mut policy = Self::from_gateway_config_with_clock(config, clock)?;
        policy.shared = Some(Arc::new(SharedAccessPolicy::new(config, backend)?));
        Ok(policy)
    }

    /// Whether this policy changes the otherwise-open gateway behavior.
    #[must_use]
    pub fn active(&self) -> bool {
        self.global_limiter.is_some() || !self.keys.is_empty()
    }

    /// Whether callers must present one of the configured virtual keys.
    ///
    /// A gateway-wide rate limit changes admission behavior but does not
    /// authenticate a network client. Managed data-plane listeners use this
    /// distinction to fail closed before accepting traffic.
    #[must_use]
    pub fn requires_authentication(&self) -> bool {
        !self.keys.is_empty()
    }

    pub(crate) fn admit_global(&self) -> Result<Option<RateResult>, AccessPolicyError> {
        self.global_limiter
            .as_ref()
            .map(|limiter| limiter.admit_at((self.clock)()))
            .transpose()
            .map_err(Into::into)
    }

    pub(crate) fn authenticate(&self, authorization: Option<&str>) -> Option<AccessGrant> {
        if self.keys.is_empty() {
            return Some(AccessGrant { key_index: None });
        }
        let presented = auth::extract_bearer(authorization);
        let key_id = auth::match_key(
            presented.as_deref(),
            self.keys
                .iter()
                .map(|key| (key.id.as_str(), key.digest.as_str())),
        )?;
        self.keys
            .iter()
            .position(|key| key.id == key_id)
            .map(|key_index| AccessGrant {
                key_index: Some(key_index),
            })
    }

    pub(crate) fn admit_key(
        &self,
        grant: AccessGrant,
    ) -> Result<Option<RateResult>, AccessPolicyError> {
        let now = (self.clock)();
        let workspace_result = if let Some(limiter) = self
            .key(grant)
            .and_then(|key| key.workspace_limiter.as_ref())
        {
            let result = limiter.admit_at(now)?;
            if !result.allowed() {
                return Ok(Some(result));
            }
            Some(result)
        } else {
            None
        };
        let key_result = self
            .key(grant)
            .and_then(|key| key.limiter.as_ref())
            .map(|limiter| limiter.admit_at(now))
            .transpose()
            .map_err(AccessPolicyError::from)?;
        Ok(key_result.or(workspace_result))
    }

    #[must_use]
    pub(crate) fn key_id(&self, grant: AccessGrant) -> Option<&str> {
        self.key(grant).map(|key| key.id.as_str())
    }

    #[must_use]
    pub(crate) fn workspace_id(&self, grant: AccessGrant) -> Option<&str> {
        self.key(grant).and_then(|key| key.workspace.as_deref())
    }

    #[must_use]
    pub(crate) fn allowed_models(&self, grant: AccessGrant) -> &[String] {
        self.key(grant)
            .map_or(&[], |key| key.allowed_models.as_ref())
    }

    pub(crate) fn tightest_snapshot(
        &self,
        grant: AccessGrant,
    ) -> Result<Option<RateSnapshot>, AccessPolicyError> {
        let now = (self.clock)();
        let global = match &self.global_limiter {
            Some(limiter) => limiter.snapshot_at(now)?,
            None => None,
        };
        let key = match self.key(grant).and_then(|key| key.limiter.as_ref()) {
            Some(limiter) => limiter.snapshot_at(now)?,
            None => None,
        };
        let workspace = match self
            .key(grant)
            .and_then(|key| key.workspace_limiter.as_ref())
        {
            Some(limiter) => limiter.snapshot_at(now)?,
            None => None,
        };
        Ok([global, workspace, key]
            .into_iter()
            .flatten()
            .min_by_key(|snapshot| snapshot.remaining))
    }

    pub(crate) fn add_tokens(
        &self,
        grant: AccessGrant,
        tokens: u64,
    ) -> Result<(), AccessPolicyError> {
        let now = (self.clock)();
        if let Some(limiter) = &self.global_limiter {
            limiter.add_tokens_at(tokens, now)?;
        }
        if let Some(limiter) = self
            .key(grant)
            .and_then(|key| key.workspace_limiter.as_ref())
        {
            limiter.add_tokens_at(tokens, now)?;
        }
        if let Some(limiter) = self.key(grant).and_then(|key| key.limiter.as_ref()) {
            limiter.add_tokens_at(tokens, now)?;
        }
        Ok(())
    }

    /// Reserve shared global state, degrading to local counters on outage.
    pub(crate) async fn admit_shared_global(
        &self,
    ) -> Result<Option<RateResult>, AccessPolicyError> {
        let Some(shared) = &self.shared else {
            return self.admit_global();
        };
        match shared.admit_global((self.clock)()).await {
            Ok(result) => Ok(result),
            Err(_) => {
                shared.degraded.store(true, Ordering::Relaxed);
                self.admit_global()
            }
        }
    }

    /// Reserve shared workspace/key state, degrading to local counters on outage.
    pub(crate) async fn admit_shared_key(
        &self,
        grant: AccessGrant,
    ) -> Result<Option<RateResult>, AccessPolicyError> {
        let Some(shared) = &self.shared else {
            return self.admit_key(grant);
        };
        match shared.admit_key(grant, (self.clock)()).await {
            Ok(result) => Ok(result),
            Err(_) => {
                shared.degraded.store(true, Ordering::Relaxed);
                self.admit_key(grant)
            }
        }
    }

    /// Read shared rate headers, falling back to local counters on outage.
    pub(crate) async fn tightest_snapshot_shared(
        &self,
        grant: AccessGrant,
    ) -> Result<Option<RateSnapshot>, AccessPolicyError> {
        let Some(shared) = &self.shared else {
            return self.tightest_snapshot(grant);
        };
        match shared.snapshot(grant, (self.clock)()).await {
            Ok(snapshot) => Ok(snapshot),
            Err(_) => {
                shared.degraded.store(true, Ordering::Relaxed);
                self.tightest_snapshot(grant)
            }
        }
    }

    /// Record shared token usage asynchronously without changing local callers.
    pub(crate) async fn add_shared_tokens(
        &self,
        grant: AccessGrant,
        tokens: u64,
    ) -> Result<(), AccessPolicyError> {
        let Some(shared) = &self.shared else {
            return Ok(());
        };
        if shared
            .add_tokens(grant, tokens, (self.clock)())
            .await
            .is_err()
        {
            shared.degraded.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Whether a shared backend has recently failed and local fallback is active.
    #[must_use]
    pub fn state_degraded(&self) -> bool {
        self.shared
            .as_ref()
            .is_some_and(|shared| shared.degraded.load(Ordering::Relaxed))
    }

    /// Selected backend name for prompt-free diagnostics.
    #[must_use]
    pub fn state_backend_name(&self) -> &'static str {
        self.shared
            .as_ref()
            .map_or("memory", |shared| shared.backend.backend_name())
    }

    /// Whether this policy uses the shared-state contract.
    #[must_use]
    pub(crate) fn uses_shared_state(&self) -> bool {
        self.shared.is_some()
    }

    fn key(&self, grant: AccessGrant) -> Option<&VirtualKeyPolicy> {
        grant.key_index.and_then(|index| self.keys.get(index))
    }
}

impl fmt::Debug for AccessPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccessPolicy")
            .field(
                "global_rate_limit_configured",
                &self.global_limiter.is_some(),
            )
            .field("virtual_key_count", &self.keys.len())
            .field("state_backend", &self.state_backend_name())
            .field("state_degraded", &self.state_degraded())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AccessGrant {
    key_index: Option<usize>,
}

#[derive(Clone)]
struct SharedLimit {
    key: Arc<str>,
    config: StateLimitConfig,
}

struct SharedKeyPolicy {
    workspace: Option<SharedLimit>,
    limiter: Option<SharedLimit>,
}

struct SharedAccessPolicy {
    backend: Arc<dyn StateBackend>,
    global: Option<SharedLimit>,
    keys: Arc<[SharedKeyPolicy]>,
    degraded: AtomicBool,
}

impl SharedAccessPolicy {
    fn new(
        config: &GatewayConfig,
        backend: Arc<dyn StateBackend>,
    ) -> Result<Self, AccessPolicyError> {
        let namespace = Arc::<str>::from(config.state.namespace.as_str());
        let global = config.rate_limit.as_ref().map(|limit| SharedLimit {
            key: format!("{namespace}:global").into(),
            config: shared_limit_config(limit),
        });
        let workspaces = config
            .workspaces
            .iter()
            .filter_map(|(id, workspace)| {
                workspace.rate_limit.as_ref().map(|limit| {
                    (
                        id.clone(),
                        SharedLimit {
                            key: format!("{namespace}:workspace:{id}").into(),
                            config: shared_limit_config(limit),
                        },
                    )
                })
            })
            .collect::<BTreeMap<_, _>>();
        let keys = config
            .keys
            .iter()
            .map(|(id, key)| SharedKeyPolicy {
                workspace: key
                    .workspace
                    .as_ref()
                    .and_then(|workspace| workspaces.get(workspace).cloned()),
                limiter: key.rate_limit.as_ref().map(|limit| SharedLimit {
                    key: format!("{namespace}:key:{id}").into(),
                    config: shared_limit_config(limit),
                }),
            })
            .collect::<Vec<_>>();
        Ok(Self {
            backend,
            global,
            keys: Arc::from(keys),
            degraded: AtomicBool::new(false),
        })
    }

    async fn admit_global(
        &self,
        now_seconds: f64,
    ) -> Result<Option<RateResult>, StateBackendError> {
        let Some(global) = self.global.as_ref() else {
            return Ok(None);
        };
        let result = self
            .backend
            .admit(&global.key, global.config, now_seconds)
            .await?;
        Ok(Some(result))
    }

    async fn admit_key(
        &self,
        grant: AccessGrant,
        now_seconds: f64,
    ) -> Result<Option<RateResult>, StateBackendError> {
        let Some(key) = grant.key_index.and_then(|index| self.keys.get(index)) else {
            return Ok(None);
        };
        if let Some(workspace) = key.workspace.as_ref() {
            let result = self
                .backend
                .admit(&workspace.key, workspace.config, now_seconds)
                .await?;
            if !result.allowed() {
                return Ok(Some(result));
            }
        }
        if let Some(limiter) = key.limiter.as_ref() {
            let result = self
                .backend
                .admit(&limiter.key, limiter.config, now_seconds)
                .await?;
            if !result.allowed() {
                return Ok(Some(result));
            }
        }
        Ok(None)
    }

    async fn snapshot(
        &self,
        grant: AccessGrant,
        now_seconds: f64,
    ) -> Result<Option<RateSnapshot>, StateBackendError> {
        let mut snapshots = Vec::new();
        if let Some(global) = self.global.as_ref() {
            if let Some(snapshot) = self
                .backend
                .snapshot(&global.key, global.config, now_seconds)
                .await?
            {
                snapshots.push(snapshot);
            }
        }
        if let Some(key) = grant.key_index.and_then(|index| self.keys.get(index)) {
            if let Some(workspace) = key.workspace.as_ref() {
                if let Some(snapshot) = self
                    .backend
                    .snapshot(&workspace.key, workspace.config, now_seconds)
                    .await?
                {
                    snapshots.push(snapshot);
                }
            }
            if let Some(limiter) = key.limiter.as_ref() {
                if let Some(snapshot) = self
                    .backend
                    .snapshot(&limiter.key, limiter.config, now_seconds)
                    .await?
                {
                    snapshots.push(snapshot);
                }
            }
        }
        Ok(snapshots
            .into_iter()
            .min_by_key(|snapshot| snapshot.remaining))
    }

    async fn add_tokens(
        &self,
        grant: AccessGrant,
        tokens: u64,
        now_seconds: f64,
    ) -> Result<(), StateBackendError> {
        if let Some(global) = self.global.as_ref() {
            self.backend
                .add_tokens(&global.key, global.config, tokens, now_seconds)
                .await?;
        }
        if let Some(key) = grant.key_index.and_then(|index| self.keys.get(index)) {
            if let Some(workspace) = key.workspace.as_ref() {
                self.backend
                    .add_tokens(&workspace.key, workspace.config, tokens, now_seconds)
                    .await?;
            }
            if let Some(limiter) = key.limiter.as_ref() {
                self.backend
                    .add_tokens(&limiter.key, limiter.config, tokens, now_seconds)
                    .await?;
            }
        }
        Ok(())
    }
}

fn shared_limit_config(limit: &ConfigRateLimit) -> StateLimitConfig {
    StateLimitConfig {
        rpm: limit.rpm,
        tpm: limit.tpm,
        window_seconds: limit.window,
    }
}

fn new_limiter(config: &ConfigRateLimit) -> Result<RateLimiter, RateLimitError> {
    RateLimiter::new(config.rpm, config.tpm, config.window)
}

fn valid_key_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_KEY_ID_BYTES
        && value.chars().all(|character| !character.is_control())
}

#[cfg(test)]
mod tests {
    use wayfinder_config::gateway::{GatewayConfig, RateLimit, VirtualKey, Workspace};

    use super::*;

    fn virtual_key(secret: &str) -> VirtualKey {
        VirtualKey {
            hash: auth::hash_key(secret),
            tags: Vec::new(),
            workspace: None,
            budget: None,
            rate_limit: None,
            models: Vec::new(),
        }
    }

    #[test]
    fn authentication_is_open_without_keys_and_constant_digest_state_is_redacted()
    -> Result<(), AccessPolicyError> {
        let open = AccessPolicy::from_gateway_config(&GatewayConfig::default())?;
        assert!(open.authenticate(None).is_some());

        let mut config = GatewayConfig::default();
        config
            .keys
            .insert("team-a".to_owned(), virtual_key("wf-secret"));
        let digest = config.keys["team-a"].hash.clone();
        let policy = AccessPolicy::from_gateway_config(&config)?;
        assert!(policy.requires_authentication());
        assert!(policy.authenticate(Some("Bearer wf-secret")).is_some());
        assert!(policy.authenticate(Some("Bearer wrong")).is_none());
        let rendered = format!("{policy:?}");
        assert!(!rendered.contains(&digest));
        assert!(!rendered.contains("wf-secret"));
        Ok(())
    }

    #[test]
    fn workspace_policy_is_shared_across_keys_and_keys_can_only_narrow_models()
    -> Result<(), AccessPolicyError> {
        let mut config = GatewayConfig::default();
        config.workspaces.insert(
            "production".to_owned(),
            Workspace {
                models: vec!["fast".to_owned(), "deep".to_owned()],
                rate_limit: Some(RateLimit {
                    rpm: Some(2),
                    tpm: None,
                    window: 60.0,
                }),
            },
        );
        let mut first = virtual_key("first-secret");
        first.workspace = Some("production".to_owned());
        let mut second = virtual_key("second-secret");
        second.workspace = Some("production".to_owned());
        second.models = vec!["fast".to_owned()];
        config.keys.insert("first".to_owned(), first);
        config.keys.insert("second".to_owned(), second);

        let policy = AccessPolicy::from_gateway_config_with_clock(&config, || 1.0)?;
        let first = policy
            .authenticate(Some("Bearer first-secret"))
            .ok_or(AccessPolicyError::InvalidKeyId)?;
        let second = policy
            .authenticate(Some("Bearer second-secret"))
            .ok_or(AccessPolicyError::InvalidKeyId)?;
        assert_eq!(policy.workspace_id(first), Some("production"));
        assert_eq!(policy.allowed_models(first), ["fast", "deep"]);
        assert_eq!(policy.allowed_models(second), ["fast"]);
        assert!(
            policy
                .admit_key(first)?
                .is_some_and(|result| result.allowed())
        );
        assert!(
            policy
                .admit_key(second)?
                .is_some_and(|result| result.allowed())
        );
        assert!(
            policy
                .admit_key(first)?
                .is_some_and(|result| !result.allowed())
        );
        Ok(())
    }

    #[test]
    fn global_and_key_limits_share_injected_clock_and_tightest_snapshot()
    -> Result<(), AccessPolicyError> {
        let mut config = GatewayConfig {
            rate_limit: Some(RateLimit {
                rpm: Some(100),
                tpm: Some(1_000),
                window: 60.0,
            }),
            ..GatewayConfig::default()
        };
        let mut key = virtual_key("wf-secret");
        key.rate_limit = Some(RateLimit {
            rpm: Some(2),
            tpm: Some(10),
            window: 60.0,
        });
        key.models = vec!["local".to_owned()];
        config.keys.insert("team-a".to_owned(), key);
        let policy = AccessPolicy::from_gateway_config_with_clock(&config, || 1_000.0)?;
        assert!(policy.admit_global()?.is_some_and(RateResult::allowed));
        let grant = policy
            .authenticate(Some("wf-secret"))
            .ok_or(AccessPolicyError::InvalidKeyId)?;
        assert!(policy.admit_key(grant)?.is_some_and(RateResult::allowed));
        assert_eq!(policy.key_id(grant), Some("team-a"));
        assert_eq!(policy.allowed_models(grant), ["local"]);
        assert_eq!(policy.tightest_snapshot(grant)?.map(|s| s.limit), Some(2));
        policy.add_tokens(grant, 60)?;
        assert_eq!(
            policy
                .admit_key(grant)?
                .and_then(|result| result.limited_by),
            Some(crate::rate_limit::LimitKind::Tokens)
        );
        Ok(())
    }

    #[tokio::test]
    async fn shared_backend_enforces_one_window_across_policy_handles()
    -> Result<(), AccessPolicyError> {
        let mut config = GatewayConfig {
            rate_limit: Some(RateLimit {
                rpm: Some(1),
                tpm: None,
                window: 60.0,
            }),
            ..GatewayConfig::default()
        };
        config.keys.insert("team-a".to_owned(), virtual_key("wf-a"));
        let backend = Arc::new(crate::state::MemoryStateBackend::default());
        let first_policy = AccessPolicy::from_gateway_config_with_backend(
            &config,
            || 1.0,
            Arc::clone(&backend) as Arc<dyn StateBackend>,
        )?;
        let second_policy = AccessPolicy::from_gateway_config_with_backend(
            &config,
            || 1.0,
            Arc::clone(&backend) as Arc<dyn StateBackend>,
        )?;
        let first = first_policy
            .authenticate(Some("Bearer wf-a"))
            .ok_or(AccessPolicyError::InvalidKeyId)?;
        assert!(
            first_policy
                .admit_shared_global()
                .await?
                .is_some_and(RateResult::allowed)
        );
        assert!(first_policy.admit_shared_key(first).await?.is_none());
        assert_eq!(
            second_policy
                .admit_shared_global()
                .await?
                .and_then(|result| result.limited_by),
            Some(crate::rate_limit::LimitKind::Requests)
        );
        Ok(())
    }
}
