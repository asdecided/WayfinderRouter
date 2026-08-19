//! Versioned, secret-free control-plane policy lifecycle.
//!
//! The data plane receives complete immutable [`AppState`] snapshots. It never
//! calls a control plane while handling a request. Administrative identity and
//! policy documents deliberately contain no provider credential fields.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use wayfinder_config::{TierOrderPolicy, dump_routing_toml, routing_config_from_toml};
use wayfinder_routing_core::RoutingConfig;

use crate::AppState;
use crate::audit::{AuditError, AuditLog};
use crate::reload::{LastGood, ReloadError, ReloadOutcome};

/// Wire schema for policy documents introduced by WF-ADR-0071.
pub const POLICY_SCHEMA_VERSION: &str = "wf-policy-v1";
/// Marker used by locally loaded configurations that are not yet managed.
pub const UNMANAGED_POLICY_ID: &str = "unmanaged";
/// Marker used by locally loaded snapshots that are not yet managed.
pub const UNMANAGED_SNAPSHOT_ID: &str = "unmanaged";

const MAX_ID_BYTES: usize = 128;
const MAX_ROUTING_TOML_BYTES: usize = 64 * 1024;
const MAX_PROFILES: usize = 256;
const MAX_BINDINGS: usize = 4_096;
const MAX_ACTIVATION_HISTORY: usize = 256;

/// Administrative subject authorizing a lifecycle mutation.
///
/// This contract is intentionally distinct from virtual keys and provider
/// credential references.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdministrativeIdentity {
    /// Stable identity-provider issuer or bounded local authority label.
    pub issuer: String,
    /// Stable administrative subject within the issuer.
    pub subject: String,
}

impl AdministrativeIdentity {
    /// Construct and validate a secret-free administrative identity.
    pub fn new(issuer: impl Into<String>, subject: impl Into<String>) -> Result<Self, PolicyError> {
        let identity = Self {
            issuer: issuer.into(),
            subject: subject.into(),
        };
        validate_id("administrative issuer", &identity.issuer)?;
        validate_id("administrative subject", &identity.subject)?;
        Ok(identity)
    }

    fn audit_actor(&self) -> String {
        format!("{}:{}", self.issuer, self.subject)
    }
}

/// One reusable routing configuration.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyProfile {
    /// Operator-selected profile identity.
    pub id: String,
    /// Deterministic routing-only TOML; it cannot contain provider credentials.
    pub routing_toml: String,
}

impl PolicyProfile {
    /// Construct a profile from an already validated routing configuration.
    pub fn new(id: impl Into<String>, routing: &RoutingConfig) -> Result<Self, PolicyError> {
        let profile = Self {
            id: id.into(),
            routing_toml: dump_routing_toml(routing),
        };
        profile.validate()?;
        Ok(profile)
    }

    fn validate(&self) -> Result<RoutingConfig, PolicyError> {
        validate_id("profile id", &self.id)?;
        if self.routing_toml.len() > MAX_ROUTING_TOML_BYTES {
            return Err(PolicyError::RoutingDocumentTooLarge);
        }
        let routing = routing_config_from_toml(
            &self.routing_toml,
            &format!("policy profile '{}'", self.id),
            None,
            TierOrderPolicy::StrictInput,
        )
        .map_err(|error| PolicyError::InvalidRouting {
            profile_id: self.id.clone(),
            message: error.to_string(),
        })?;
        if dump_routing_toml(&routing) != self.routing_toml {
            return Err(PolicyError::NonCanonicalRouting(self.id.clone()));
        }
        Ok(routing)
    }
}

/// Entity class that can be attached to a reusable profile.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BindingKind {
    /// A named client or integration.
    Client,
    /// A policy workspace.
    Workspace,
    /// A gateway-issued virtual-key identity, never its credential value.
    Key,
}

/// Attach one client, workspace, or virtual-key identity to a profile.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyBinding {
    /// Bound entity class.
    pub kind: BindingKind,
    /// Stable entity identity. For keys this is the key id, not the secret.
    pub subject: String,
    /// Referenced profile identity.
    pub profile_id: String,
}

/// Complete immutable content of one policy version.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDocument {
    /// Stable wire schema.
    pub schema_version: String,
    /// Reusable routing profiles.
    pub profiles: Vec<PolicyProfile>,
    /// Profile used when no more specific binding matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_profile_id: Option<String>,
    /// Entity-to-profile bindings.
    pub bindings: Vec<PolicyBinding>,
}

impl PolicyDocument {
    /// Construct and validate a versionable policy document.
    pub fn new(
        profiles: Vec<PolicyProfile>,
        bindings: Vec<PolicyBinding>,
    ) -> Result<Self, PolicyError> {
        let default_profile_id = if profiles.len() == 1 {
            profiles.first().map(|profile| profile.id.clone())
        } else {
            profiles
                .iter()
                .find(|profile| profile.id == "default")
                .map(|profile| profile.id.clone())
        };
        Self::new_with_default(profiles, bindings, default_profile_id)
    }

    /// Construct a document with an explicit default profile.
    pub fn new_with_default(
        profiles: Vec<PolicyProfile>,
        bindings: Vec<PolicyBinding>,
        default_profile_id: Option<String>,
    ) -> Result<Self, PolicyError> {
        let document = Self {
            schema_version: POLICY_SCHEMA_VERSION.to_owned(),
            profiles,
            default_profile_id,
            bindings,
        };
        document.validate_shape()?;
        Ok(document)
    }

    fn validate_shape(&self) -> Result<(), PolicyError> {
        if self.schema_version != POLICY_SCHEMA_VERSION {
            return Err(PolicyError::UnsupportedSchema(self.schema_version.clone()));
        }
        if self.profiles.is_empty() || self.profiles.len() > MAX_PROFILES {
            return Err(PolicyError::InvalidProfileCount);
        }
        if self.bindings.len() > MAX_BINDINGS {
            return Err(PolicyError::TooManyBindings);
        }
        let mut profile_ids = BTreeSet::new();
        for profile in &self.profiles {
            validate_id("profile id", &profile.id)?;
            if !profile_ids.insert(profile.id.clone()) {
                return Err(PolicyError::DuplicateProfile(profile.id.clone()));
            }
        }
        let default_profile_id = self.default_profile_id()?;
        if !profile_ids.contains(default_profile_id) {
            return Err(PolicyError::UnknownDefaultProfile(
                default_profile_id.to_owned(),
            ));
        }
        let mut targets = BTreeSet::new();
        for binding in &self.bindings {
            validate_id("binding subject", &binding.subject)?;
            validate_id("binding profile id", &binding.profile_id)?;
            if !profile_ids.contains(&binding.profile_id) {
                return Err(PolicyError::UnknownProfile(binding.profile_id.clone()));
            }
            let target = (binding.kind, binding.subject.clone());
            if !targets.insert(target) {
                return Err(PolicyError::DuplicateBinding(binding.subject.clone()));
            }
        }
        Ok(())
    }

    fn default_profile_id(&self) -> Result<&str, PolicyError> {
        match self.default_profile_id.as_deref() {
            Some(profile_id) => {
                validate_id("default profile id", profile_id)?;
                Ok(profile_id)
            }
            None if self.profiles.len() == 1 => Ok(&self.profiles[0].id),
            None => Err(PolicyError::MissingDefaultProfile),
        }
    }
}

/// Authenticated or host-supplied identities available for profile resolution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BindingContext<'a> {
    /// Stable client identity supplied by a trusted host adapter, when known.
    pub client_id: Option<&'a str>,
    /// Authenticated workspace identity, when known.
    pub workspace_id: Option<&'a str>,
    /// Authenticated virtual-key identity, never the credential value.
    pub key_id: Option<&'a str>,
}

/// Deterministic result of resolving one request to a policy profile.
#[derive(Clone, Copy, Debug)]
pub struct ResolvedPolicyProfile<'a> {
    /// Selected profile identity.
    pub profile_id: &'a str,
    /// Most-specific binding that selected the profile, or `None` for default.
    pub binding_kind: Option<BindingKind>,
    /// Parsed immutable routing configuration.
    pub routing: &'a RoutingConfig,
}

/// Immutable draft produced before semantic validation.
#[derive(Clone, Debug)]
pub struct DraftPolicyVersion {
    version_id: String,
    document: Arc<PolicyDocument>,
    drafted_by: AdministrativeIdentity,
}

impl DraftPolicyVersion {
    /// Content-addressed policy version identity.
    #[must_use]
    pub fn version_id(&self) -> &str {
        &self.version_id
    }

    /// Secret-free administrator that created this draft.
    #[must_use]
    pub const fn drafted_by(&self) -> &AdministrativeIdentity {
        &self.drafted_by
    }

    /// Immutable policy document.
    #[must_use]
    pub fn document(&self) -> &PolicyDocument {
        &self.document
    }

    /// Validate every referenced profile and binding.
    pub fn validate(self) -> Result<ValidatedPolicyVersion, PolicyError> {
        self.document.validate_shape()?;
        let mut routing = BTreeMap::new();
        for profile in &self.document.profiles {
            routing.insert(profile.id.clone(), profile.validate()?);
        }
        let bindings = self
            .document
            .bindings
            .iter()
            .map(|binding| {
                (
                    (binding.kind, binding.subject.clone()),
                    binding.profile_id.clone(),
                )
            })
            .collect();
        let default_profile_id = Arc::from(self.document.default_profile_id()?.to_owned());
        Ok(ValidatedPolicyVersion {
            version_id: self.version_id,
            document: self.document,
            drafted_by: self.drafted_by,
            routing: Arc::new(routing),
            bindings: Arc::new(bindings),
            default_profile_id,
        })
    }
}

/// Immutable, semantically validated policy version eligible for activation.
#[derive(Clone, Debug)]
pub struct ValidatedPolicyVersion {
    version_id: String,
    document: Arc<PolicyDocument>,
    drafted_by: AdministrativeIdentity,
    routing: Arc<BTreeMap<String, RoutingConfig>>,
    bindings: Arc<BTreeMap<(BindingKind, String), String>>,
    default_profile_id: Arc<str>,
}

impl ValidatedPolicyVersion {
    /// Content-addressed policy version identity.
    #[must_use]
    pub fn version_id(&self) -> &str {
        &self.version_id
    }

    /// Immutable policy document.
    #[must_use]
    pub fn document(&self) -> &PolicyDocument {
        &self.document
    }

    /// Secret-free administrator that created the draft.
    #[must_use]
    pub const fn drafted_by(&self) -> &AdministrativeIdentity {
        &self.drafted_by
    }

    /// Parsed routing configuration for one profile.
    #[must_use]
    pub fn routing(&self, profile_id: &str) -> Option<&RoutingConfig> {
        self.routing.get(profile_id)
    }

    /// Resolve key, workspace, client, then default profile in that order.
    #[must_use]
    pub fn resolve(&self, context: BindingContext<'_>) -> ResolvedPolicyProfile<'_> {
        for (kind, subject) in [
            (BindingKind::Key, context.key_id),
            (BindingKind::Workspace, context.workspace_id),
            (BindingKind::Client, context.client_id),
        ] {
            let Some(subject) = subject else {
                continue;
            };
            if let Some(profile_id) = self.bindings.get(&(kind, subject.to_owned())) {
                return ResolvedPolicyProfile {
                    profile_id,
                    binding_kind: Some(kind),
                    routing: &self.routing[profile_id],
                };
            }
        }
        ResolvedPolicyProfile {
            profile_id: &self.default_profile_id,
            binding_kind: None,
            routing: &self.routing[&*self.default_profile_id],
        }
    }
}

/// Policy version and activation snapshot stamped on runtime receipts.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyReceipt {
    /// Immutable policy content identity.
    pub policy_version: String,
    /// Unique immutable activation identity.
    pub snapshot_id: String,
}

/// Result of an activation attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivationOutcome {
    /// The validated policy became the active immutable snapshot.
    Activated(PolicyReceipt),
    /// Preparation failed; the data plane retained its last-known-good snapshot.
    Retained {
        /// Snapshot that remains active.
        active: PolicyReceipt,
        /// Sanitized preparation or control-plane failure.
        error: String,
    },
}

/// Versioned policy lifecycle and its independently held data-plane snapshot.
pub struct PolicyLifecycle {
    holder: Arc<LastGood<AppState, String>>,
    audit_log: AuditLog,
    active: PolicyReceipt,
    history: Vec<PolicyReceipt>,
    runtimes_by_snapshot: BTreeMap<String, AppState>,
}

impl PolicyLifecycle {
    /// Create an immutable draft with a content-addressed version id.
    pub fn draft(
        document: PolicyDocument,
        actor: AdministrativeIdentity,
    ) -> Result<DraftPolicyVersion, PolicyError> {
        document.validate_shape()?;
        let encoded = serde_json::to_vec(&document)?;
        Ok(DraftPolicyVersion {
            version_id: format!("wpv1-{}", sha256_hex(&encoded)),
            document: Arc::new(document),
            drafted_by: actor,
        })
    }

    /// Install the first validated policy and start the data-plane holder.
    pub async fn bootstrap<F>(
        policy: &ValidatedPolicyVersion,
        prepare: F,
        actor: &AdministrativeIdentity,
        audit_log: AuditLog,
    ) -> Result<Self, PolicyError>
    where
        F: FnOnce(&ValidatedPolicyVersion) -> Result<AppState, String>,
    {
        let receipt = new_receipt(policy.version_id());
        let runtime = prepare(policy).map_err(PolicyError::RuntimePreparation)?;
        audit_log
            .record_with_policy(
                actor.audit_actor(),
                "policy.activate",
                None,
                Some(json!({"source": "bootstrap"})),
                &receipt.policy_version,
                &receipt.snapshot_id,
            )
            .await?;
        let runtime = runtime
            .with_audit_log(audit_log.clone())
            .with_validated_policy(policy.clone())
            .with_policy_receipt(&receipt);
        let mut runtimes = BTreeMap::new();
        runtimes.insert(receipt.snapshot_id.clone(), runtime.clone());
        Ok(Self {
            holder: Arc::new(LastGood::new(runtime, receipt.snapshot_id.clone())),
            audit_log,
            active: receipt,
            history: Vec::new(),
            runtimes_by_snapshot: runtimes,
        })
    }

    /// Clone the holder used by a request path that never calls the controller.
    #[must_use]
    pub fn runtime_holder(&self) -> Arc<LastGood<AppState, String>> {
        Arc::clone(&self.holder)
    }

    /// Current immutable policy/snapshot identity.
    #[must_use]
    pub const fn active(&self) -> &PolicyReceipt {
        &self.active
    }

    /// Activate a validated version, or retain the current snapshot on failure.
    pub async fn activate<F>(
        &mut self,
        policy: &ValidatedPolicyVersion,
        prepare: F,
        actor: &AdministrativeIdentity,
    ) -> Result<ActivationOutcome, PolicyError>
    where
        F: FnOnce(&ValidatedPolicyVersion) -> Result<AppState, String>,
    {
        let receipt = new_receipt(policy.version_id());
        let runtime = match prepare(policy) {
            Ok(runtime) => {
                let current = self.holder.current()?;
                runtime
                    .with_runtime_state_from(&current)
                    .with_validated_policy(policy.clone())
                    .with_policy_receipt(&receipt)
            }
            Err(error) => {
                self.audit_log
                    .record_with_policy(
                        actor.audit_actor(),
                        "policy.activate_failed",
                        None,
                        Some(json!({
                            "candidate_policy_version": receipt.policy_version,
                            "candidate_snapshot_id": receipt.snapshot_id,
                            "reason": "runtime preparation failed",
                        })),
                        &self.active.policy_version,
                        &self.active.snapshot_id,
                    )
                    .await?;
                return Ok(ActivationOutcome::Retained {
                    active: self.active.clone(),
                    error,
                });
            }
        };
        self.audit_log
            .record_with_policy(
                actor.audit_actor(),
                "policy.activate",
                Some(json!({
                    "policy_version": self.active.policy_version,
                    "snapshot_id": self.active.snapshot_id,
                })),
                None,
                &receipt.policy_version,
                &receipt.snapshot_id,
            )
            .await?;
        let outcome = self.holder.refresh(receipt.snapshot_id.clone(), || {
            Ok::<_, String>(runtime.clone())
        })?;
        if !matches!(outcome, ReloadOutcome::Reloaded(_)) {
            return Err(PolicyError::SnapshotNotInstalled);
        }
        self.runtimes_by_snapshot
            .insert(receipt.snapshot_id.clone(), runtime);
        if self.history.len() == MAX_ACTIVATION_HISTORY {
            let expired = self.history.remove(0);
            self.runtimes_by_snapshot.remove(&expired.snapshot_id);
        }
        self.history.push(self.active.clone());
        self.active = receipt.clone();
        Ok(ActivationOutcome::Activated(receipt))
    }

    /// Restore the immediately preceding policy as a new immutable snapshot.
    pub async fn rollback(
        &mut self,
        actor: &AdministrativeIdentity,
    ) -> Result<PolicyReceipt, PolicyError> {
        let target = self
            .history
            .last()
            .cloned()
            .ok_or(PolicyError::NoRollbackTarget)?;
        let receipt = new_receipt(&target.policy_version);
        let current = self.holder.current()?;
        let runtime = self
            .runtimes_by_snapshot
            .get(&target.snapshot_id)
            .cloned()
            .ok_or(PolicyError::MissingRuntime)?
            .with_runtime_state_from(&current)
            .with_policy_receipt(&receipt);
        self.audit_log
            .record_with_policy(
                actor.audit_actor(),
                "policy.rollback",
                Some(json!({
                    "policy_version": self.active.policy_version,
                    "snapshot_id": self.active.snapshot_id,
                })),
                Some(json!({"rolled_back_to": target.policy_version})),
                &receipt.policy_version,
                &receipt.snapshot_id,
            )
            .await?;
        let outcome = self
            .holder
            .refresh(receipt.snapshot_id.clone(), || Ok::<_, String>(runtime))?;
        if !matches!(outcome, ReloadOutcome::Reloaded(_)) {
            return Err(PolicyError::SnapshotNotInstalled);
        }
        self.runtimes_by_snapshot.insert(
            receipt.snapshot_id.clone(),
            self.holder.current()?.as_ref().clone(),
        );
        let _ = self.history.pop();
        self.runtimes_by_snapshot.remove(&self.active.snapshot_id);
        self.active = receipt.clone();
        Ok(receipt)
    }
}

/// Policy contract, validation, lifecycle, or audit failure.
#[derive(Debug, Error)]
pub enum PolicyError {
    /// One bounded identifier was empty, too long, or contained control bytes.
    #[error("invalid {0}")]
    InvalidIdentifier(&'static str),
    /// Policy schema is not supported.
    #[error("unsupported policy schema '{0}'")]
    UnsupportedSchema(String),
    /// Documents must contain between one and 256 profiles.
    #[error("policy must contain between 1 and {MAX_PROFILES} profiles")]
    InvalidProfileCount,
    /// Documents may contain at most 4096 bindings.
    #[error("policy may contain at most {MAX_BINDINGS} bindings")]
    TooManyBindings,
    /// Profile identities must be unique.
    #[error("duplicate profile '{0}'")]
    DuplicateProfile(String),
    /// Multi-profile policies require one explicit default.
    #[error("multi-profile policy requires a default profile")]
    MissingDefaultProfile,
    /// The default referenced no defined profile.
    #[error("default references unknown profile '{0}'")]
    UnknownDefaultProfile(String),
    /// A binding referenced no defined profile.
    #[error("binding references unknown profile '{0}'")]
    UnknownProfile(String),
    /// One entity can bind to only one profile in a version.
    #[error("duplicate binding for '{0}'")]
    DuplicateBinding(String),
    /// Routing source exceeded its bounded contract.
    #[error("routing document is too large")]
    RoutingDocumentTooLarge,
    /// One profile did not parse as routing-only TOML.
    #[error("profile '{profile_id}' has invalid routing: {message}")]
    InvalidRouting {
        /// Profile that failed validation.
        profile_id: String,
        /// Sanitized parser diagnostic.
        message: String,
    },
    /// Routing profiles accept only the canonical routing fragment.
    #[error("profile '{0}' is not canonical routing-only TOML")]
    NonCanonicalRouting(String),
    /// Policy JSON could not be encoded for content identity.
    #[error("policy serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    /// Audit persistence rejected a lifecycle mutation.
    #[error("policy audit failed: {0}")]
    Audit(#[from] AuditError),
    /// The initial policy could not produce any last-known-good snapshot.
    #[error("initial policy runtime preparation failed: {0}")]
    RuntimePreparation(String),
    /// Immutable snapshot synchronization failed.
    #[error("policy snapshot state failed: {0}")]
    Reload(#[from] ReloadError),
    /// No preceding activation exists.
    #[error("no previous policy activation is available")]
    NoRollbackTarget,
    /// A previously activated policy lost its runtime snapshot.
    #[error("runtime snapshot for rollback is unavailable")]
    MissingRuntime,
    /// The holder did not accept a distinct immutable snapshot.
    #[error("runtime snapshot was not installed")]
    SnapshotNotInstalled,
}

fn validate_id(kind: &'static str, value: &str) -> Result<(), PolicyError> {
    if value.trim().is_empty()
        || value.len() > MAX_ID_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(PolicyError::InvalidIdentifier(kind));
    }
    Ok(())
}

fn new_receipt(policy_version: &str) -> PolicyReceipt {
    PolicyReceipt {
        policy_version: policy_version.to_owned(),
        snapshot_id: format!("wps1-{}", Uuid::new_v4().simple()),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::build_reloadable_router;

    fn actor() -> Result<AdministrativeIdentity, PolicyError> {
        AdministrativeIdentity::new("https://id.example", "operator-1")
    }

    fn document(threshold: f64) -> Result<PolicyDocument, PolicyError> {
        PolicyDocument::new(
            vec![PolicyProfile::new(
                "default",
                &RoutingConfig::binary(threshold),
            )?],
            vec![PolicyBinding {
                kind: BindingKind::Workspace,
                subject: "engineering".to_owned(),
                profile_id: "default".to_owned(),
            }],
        )
    }

    #[test]
    fn draft_and_validate_are_distinct_immutable_steps() -> Result<(), PolicyError> {
        let draft = PolicyLifecycle::draft(document(0.5)?, actor()?)?;
        assert!(draft.version_id().starts_with("wpv1-"));
        assert_eq!(draft.drafted_by().subject, "operator-1");
        let version = draft.validate()?;
        assert_eq!(version.document().schema_version, POLICY_SCHEMA_VERSION);
        assert_eq!(version.drafted_by().issuer, "https://id.example");
        assert_eq!(
            version.routing("default"),
            Some(&RoutingConfig::binary(0.5))
        );
        Ok(())
    }

    #[test]
    fn bindings_are_bounded_and_reference_existing_profiles() -> Result<(), PolicyError> {
        let invalid = PolicyDocument::new(
            vec![PolicyProfile::new("default", &RoutingConfig::default())?],
            vec![PolicyBinding {
                kind: BindingKind::Client,
                subject: "codex".to_owned(),
                profile_id: "missing".to_owned(),
            }],
        );
        assert!(
            matches!(invalid, Err(PolicyError::UnknownProfile(profile)) if profile == "missing")
        );
        Ok(())
    }

    #[test]
    fn bindings_resolve_key_then_workspace_then_client_then_default() -> Result<(), PolicyError> {
        let profiles = [
            ("default", 0.9),
            ("client", 0.7),
            ("workspace", 0.5),
            ("key", 0.1),
        ]
        .into_iter()
        .map(|(id, threshold)| PolicyProfile::new(id, &RoutingConfig::binary(threshold)))
        .collect::<Result<Vec<_>, _>>()?;
        let bindings = vec![
            PolicyBinding {
                kind: BindingKind::Client,
                subject: "codex".to_owned(),
                profile_id: "client".to_owned(),
            },
            PolicyBinding {
                kind: BindingKind::Workspace,
                subject: "engineering".to_owned(),
                profile_id: "workspace".to_owned(),
            },
            PolicyBinding {
                kind: BindingKind::Key,
                subject: "agent-1".to_owned(),
                profile_id: "key".to_owned(),
            },
        ];
        let policy = PolicyLifecycle::draft(
            PolicyDocument::new_with_default(
                profiles,
                bindings,
                Some("default".to_owned()),
            )?,
            actor()?,
        )?
        .validate()?;

        let key = policy.resolve(BindingContext {
            client_id: Some("codex"),
            workspace_id: Some("engineering"),
            key_id: Some("agent-1"),
        });
        assert_eq!(key.profile_id, "key");
        assert_eq!(key.binding_kind, Some(BindingKind::Key));

        let workspace = policy.resolve(BindingContext {
            client_id: Some("codex"),
            workspace_id: Some("engineering"),
            key_id: None,
        });
        assert_eq!(workspace.profile_id, "workspace");
        assert_eq!(workspace.binding_kind, Some(BindingKind::Workspace));

        let client = policy.resolve(BindingContext {
            client_id: Some("codex"),
            workspace_id: None,
            key_id: None,
        });
        assert_eq!(client.profile_id, "client");
        assert_eq!(client.binding_kind, Some(BindingKind::Client));

        let fallback = policy.resolve(BindingContext::default());
        assert_eq!(fallback.profile_id, "default");
        assert_eq!(fallback.binding_kind, None);
        Ok(())
    }

    #[test]
    fn serialized_contract_has_no_credential_or_administrator_fields() -> Result<(), PolicyError> {
        let version = PolicyLifecycle::draft(document(0.5)?, actor()?)?;
        let encoded = serde_json::to_string(version.document())?;
        assert!(!encoded.contains("api_key"));
        assert!(!encoded.contains("credential"));
        assert!(!encoded.contains("operator-1"));
        Ok(())
    }

    #[test]
    fn profile_rejects_non_routing_configuration() -> Result<(), PolicyError> {
        let profile = PolicyProfile {
            id: "default".to_owned(),
            routing_toml:
                "[gateway.models.cloud]\nmodel = \"provider-model\"\napi_key_env = \"SECRET\""
                    .to_owned(),
        };
        assert!(matches!(
            profile.validate(),
            Err(PolicyError::NonCanonicalRouting(profile_id)) if profile_id == "default"
        ));
        Ok(())
    }

    async fn route_receipt(
        router: axum::Router,
    ) -> Result<(String, String), Box<dyn std::error::Error>> {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"model":"auto","messages":[{"role":"user","content":"hello"}]}"#,
            ))?;
        let response = router.oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let policy_version = response
            .headers()
            .get("x-wayfinder-policy-version")
            .and_then(|value| value.to_str().ok())
            .ok_or("missing policy version header")?
            .to_owned();
        let snapshot_id = response
            .headers()
            .get("x-wayfinder-policy-snapshot")
            .and_then(|value| value.to_str().ok())
            .ok_or("missing snapshot header")?
            .to_owned();
        Ok((policy_version, snapshot_id))
    }

    #[tokio::test]
    async fn activate_observe_retain_and_rollback_without_restarting_data_plane()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor = actor()?;
        let first = PolicyLifecycle::draft(document(0.9)?, actor.clone())?.validate()?;
        let mut lifecycle = PolicyLifecycle::bootstrap(
            &first,
            |policy| {
                let routing = policy
                    .routing("default")
                    .cloned()
                    .ok_or_else(|| "default profile is missing".to_owned())?;
                Ok(AppState::new(routing, Vec::new(), false, "test"))
            },
            &actor,
            AuditLog::disabled(),
        )
        .await?;
        let first_receipt = lifecycle.active().clone();
        let router = build_reloadable_router(lifecycle.runtime_holder());
        assert_eq!(
            route_receipt(router.clone()).await?,
            (
                first_receipt.policy_version.clone(),
                first_receipt.snapshot_id.clone()
            )
        );

        let second = PolicyLifecycle::draft(document(0.1)?, actor.clone())?.validate()?;
        let activated = lifecycle
            .activate(
                &second,
                |policy| {
                    let routing = policy
                        .routing("default")
                        .cloned()
                        .ok_or_else(|| "default profile is missing".to_owned())?;
                    Ok(AppState::new(routing, Vec::new(), false, "test"))
                },
                &actor,
            )
            .await?;
        let ActivationOutcome::Activated(second_receipt) = activated else {
            return Err("validated policy was not activated".into());
        };
        assert_eq!(
            route_receipt(router.clone()).await?,
            (
                second_receipt.policy_version.clone(),
                second_receipt.snapshot_id.clone()
            )
        );

        let unavailable = lifecycle
            .activate(
                &first,
                |_| Err("control plane unavailable".to_owned()),
                &actor,
            )
            .await?;
        assert!(matches!(
            unavailable,
            ActivationOutcome::Retained { active, .. } if active == second_receipt
        ));
        assert_eq!(
            route_receipt(router.clone()).await?,
            (
                second_receipt.policy_version.clone(),
                second_receipt.snapshot_id.clone()
            )
        );

        let rolled_back = lifecycle.rollback(&actor).await?;
        assert_eq!(rolled_back.policy_version, first_receipt.policy_version);
        assert_ne!(rolled_back.snapshot_id, first_receipt.snapshot_id);
        assert_eq!(
            route_receipt(router).await?,
            (rolled_back.policy_version, rolled_back.snapshot_id)
        );
        Ok(())
    }
}
