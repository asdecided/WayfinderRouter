//! Restart-safe storage for secret-free control-plane policy versions.
//!
//! Policy documents and activation heads are immutable. A monotonic generation
//! gives the head log compare-and-set semantics. The data plane never reads
//! this repository while handling a request.

#![forbid(unsafe_code)]

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::control_plane::{
    AdministrativeIdentity, DraftPolicyVersion, PolicyDocument, PolicyError, PolicyLifecycle,
    PolicyReceipt, ValidatedPolicyVersion,
};

/// Stable on-disk repository schema.
pub const POLICY_REPOSITORY_SCHEMA_VERSION: &str = "wf-policy-repository-v1";

const MAX_REPOSITORY_FILE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_REPOSITORY_HISTORY: usize = 256;

/// Durable active pointer and bounded rollback lineage.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRepositoryHead {
    /// Stable repository schema.
    pub schema_version: String,
    /// Monotonic compare-and-set generation.
    pub generation: u64,
    /// Policy and activation snapshot currently selected by the control plane.
    pub active: PolicyReceipt,
    /// Older activations, from oldest to newest.
    pub history: Vec<PolicyReceipt>,
}

/// Which durable head supplied a recovered active policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyRecoverySource {
    /// The current atomically replaced head.
    Primary,
    /// The preceding head retained before the last replacement.
    LastGood,
}

/// Validated active policy recovered without consulting any provider secret.
#[derive(Debug)]
pub struct RecoveredPolicy {
    /// Durable active pointer and rollback lineage.
    pub head: PolicyRepositoryHead,
    /// Revalidated content-addressed active policy.
    pub policy: ValidatedPolicyVersion,
    /// Primary or last-good recovery source.
    pub source: PolicyRecoverySource,
}

/// Storage contract consumed by a future authenticated administrative API.
pub trait PolicyRepository: Send + Sync {
    /// Persist an immutable draft before validation or activation.
    fn store_draft(&self, draft: &DraftPolicyVersion) -> Result<(), PolicyRepositoryError>;

    /// Persist an immutable validated version.
    fn store_validated(&self, policy: &ValidatedPolicyVersion)
    -> Result<(), PolicyRepositoryError>;

    /// Load and verify an immutable draft by content identity.
    fn load_draft(
        &self,
        version_id: &str,
    ) -> Result<Option<DraftPolicyVersion>, PolicyRepositoryError>;

    /// Load and semantically revalidate an immutable version.
    fn load_validated(
        &self,
        version_id: &str,
    ) -> Result<Option<ValidatedPolicyVersion>, PolicyRepositoryError>;

    /// Persist a validated version and atomically select its activation.
    fn commit_activation(
        &self,
        expected_generation: Option<u64>,
        policy: &ValidatedPolicyVersion,
        receipt: PolicyReceipt,
    ) -> Result<PolicyRepositoryHead, PolicyRepositoryError>;

    /// Atomically select the newest rollback target as a new activation.
    fn commit_rollback(
        &self,
        expected_generation: u64,
        receipt: PolicyReceipt,
    ) -> Result<PolicyRepositoryHead, PolicyRepositoryError>;

    /// Recover and revalidate the selected policy, falling back one head only.
    fn recover_active(&self) -> Result<Option<RecoveredPolicy>, PolicyRepositoryError>;
}

/// Single-host atomic-file implementation of [`PolicyRepository`].
///
/// The generation contract remains backend-independent. A later shared
/// repository can implement the same compare-and-set semantics.
pub struct FilePolicyRepository {
    root: PathBuf,
    writes: Mutex<()>,
}

impl FilePolicyRepository {
    /// Bind a repository to one private control-plane directory.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            writes: Mutex::new(()),
        }
    }

    /// Repository root for diagnostics and explicit operator backup.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn versions_dir(&self) -> PathBuf {
        self.root.join("versions")
    }

    fn version_path(&self, version_id: &str) -> PathBuf {
        self.versions_dir().join(format!("{version_id}.json"))
    }

    fn heads_dir(&self) -> PathBuf {
        self.root.join("heads")
    }

    fn head_path(&self, generation: u64) -> PathBuf {
        self.heads_dir().join(format!("{generation:020}.json"))
    }

    fn head_candidates(&self) -> Result<Vec<(u64, PathBuf)>, PolicyRepositoryError> {
        let directory = self.heads_dir();
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let entries = fs::read_dir(&directory).map_err(|source| PolicyRepositoryError::Io {
            path: directory,
            source,
        })?;
        let mut heads = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| PolicyRepositoryError::Io {
                path: self.heads_dir(),
                source,
            })?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(generation) = name
                .strip_suffix(".json")
                .filter(|value| value.len() == 20)
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|generation| *generation > 0)
            else {
                continue;
            };
            heads.push((generation, entry.path()));
        }
        heads.sort_unstable_by(|left, right| right.0.cmp(&left.0));
        Ok(heads)
    }

    fn store_version_unlocked(
        &self,
        stored: &StoredPolicyVersion,
    ) -> Result<(), PolicyRepositoryError> {
        stored.validate()?;
        let path = self.version_path(&stored.version_id);
        if path.exists() {
            let existing = load_stored_version(&path, &stored.version_id)?;
            if existing != *stored {
                return Err(PolicyRepositoryError::ImmutableVersionMismatch(
                    stored.version_id.clone(),
                ));
            }
            return Ok(());
        }
        let encoded = serde_json::to_vec(stored)?;
        match atomic_create(&path, &encoded) {
            Ok(()) => Ok(()),
            Err(error) if error.is_already_exists() => {
                let existing = load_stored_version(&path, &stored.version_id)?;
                if existing == *stored {
                    Ok(())
                } else {
                    Err(PolicyRepositoryError::ImmutableVersionMismatch(
                        stored.version_id.clone(),
                    ))
                }
            }
            Err(error) => Err(error),
        }
    }

    fn load_head(
        &self,
        path: &Path,
    ) -> Result<Option<PolicyRepositoryHead>, PolicyRepositoryError> {
        if !path.exists() {
            return Ok(None);
        }
        let bytes = bounded_read(path)?;
        let head: PolicyRepositoryHead = serde_json::from_slice(&bytes)?;
        validate_head(&head)?;
        Ok(Some(head))
    }

    fn load_recovery_candidate(
        &self,
        path: &Path,
        expected_generation: u64,
        source: PolicyRecoverySource,
    ) -> Result<Option<RecoveredPolicy>, PolicyRepositoryError> {
        let Some(head) = self.load_head(path)? else {
            return Ok(None);
        };
        if head.generation != expected_generation {
            return Err(PolicyRepositoryError::InvalidHead);
        }
        let policy = self
            .load_validated(&head.active.policy_version)?
            .ok_or_else(|| {
                PolicyRepositoryError::MissingVersion(head.active.policy_version.clone())
            })?;
        Ok(Some(RecoveredPolicy {
            head,
            policy,
            source,
        }))
    }

    fn primary_head_for_write(
        &self,
    ) -> Result<Option<PolicyRepositoryHead>, PolicyRepositoryError> {
        let Some((generation, path)) = self.head_candidates()?.into_iter().next() else {
            return Ok(None);
        };
        let head = self
            .load_head(&path)?
            .ok_or(PolicyRepositoryError::InvalidHead)?;
        if head.generation != generation {
            return Err(PolicyRepositoryError::InvalidHead);
        }
        Ok(Some(head))
    }

    fn create_head(&self, next: &PolicyRepositoryHead) -> Result<(), PolicyRepositoryError> {
        atomic_create(&self.head_path(next.generation), &serde_json::to_vec(next)?)
    }
}

impl PolicyRepository for FilePolicyRepository {
    fn store_draft(&self, draft: &DraftPolicyVersion) -> Result<(), PolicyRepositoryError> {
        let _write = self
            .writes
            .lock()
            .map_err(|_| PolicyRepositoryError::LockPoisoned)?;
        self.store_version_unlocked(&StoredPolicyVersion::from_draft(draft))
    }

    fn store_validated(
        &self,
        policy: &ValidatedPolicyVersion,
    ) -> Result<(), PolicyRepositoryError> {
        let _write = self
            .writes
            .lock()
            .map_err(|_| PolicyRepositoryError::LockPoisoned)?;
        self.store_version_unlocked(&StoredPolicyVersion::from_validated(policy))
    }

    fn load_draft(
        &self,
        version_id: &str,
    ) -> Result<Option<DraftPolicyVersion>, PolicyRepositoryError> {
        validate_version_id(version_id)?;
        let path = self.version_path(version_id);
        if !path.exists() {
            return Ok(None);
        }
        let stored = load_stored_version(&path, version_id)?;
        let draft = PolicyLifecycle::draft(stored.document, stored.drafted_by)?;
        if draft.version_id() != version_id {
            return Err(PolicyRepositoryError::VersionHashMismatch(
                version_id.to_owned(),
            ));
        }
        Ok(Some(draft))
    }

    fn load_validated(
        &self,
        version_id: &str,
    ) -> Result<Option<ValidatedPolicyVersion>, PolicyRepositoryError> {
        self.load_draft(version_id)?
            .map(DraftPolicyVersion::validate)
            .transpose()
            .map_err(PolicyRepositoryError::from)
    }

    fn commit_activation(
        &self,
        expected_generation: Option<u64>,
        policy: &ValidatedPolicyVersion,
        receipt: PolicyReceipt,
    ) -> Result<PolicyRepositoryHead, PolicyRepositoryError> {
        validate_receipt(&receipt)?;
        if receipt.policy_version != policy.version_id() {
            return Err(PolicyRepositoryError::ReceiptVersionMismatch);
        }
        let _write = self
            .writes
            .lock()
            .map_err(|_| PolicyRepositoryError::LockPoisoned)?;
        let current = self.primary_head_for_write()?;
        let actual_generation = current.as_ref().map(|head| head.generation);
        if expected_generation != actual_generation {
            return Err(PolicyRepositoryError::StaleGeneration {
                expected: expected_generation,
                actual: actual_generation,
            });
        }
        self.store_version_unlocked(&StoredPolicyVersion::from_validated(policy))?;
        let mut history = current
            .as_ref()
            .map_or_else(Vec::new, |head| head.history.clone());
        if let Some(head) = &current {
            history.push(head.active.clone());
        }
        if history.len() > MAX_REPOSITORY_HISTORY {
            history.remove(0);
        }
        let next = PolicyRepositoryHead {
            schema_version: POLICY_REPOSITORY_SCHEMA_VERSION.to_owned(),
            generation: actual_generation
                .unwrap_or(0)
                .checked_add(1)
                .ok_or(PolicyRepositoryError::GenerationExhausted)?,
            active: receipt,
            history,
        };
        validate_head(&next)?;
        if let Err(error) = self.create_head(&next) {
            if error.is_already_exists() {
                return Err(PolicyRepositoryError::StaleGeneration {
                    expected: expected_generation,
                    actual: self
                        .primary_head_for_write()?
                        .as_ref()
                        .map(|head| head.generation),
                });
            }
            return Err(error);
        }
        Ok(next)
    }

    fn commit_rollback(
        &self,
        expected_generation: u64,
        receipt: PolicyReceipt,
    ) -> Result<PolicyRepositoryHead, PolicyRepositoryError> {
        validate_receipt(&receipt)?;
        let _write = self
            .writes
            .lock()
            .map_err(|_| PolicyRepositoryError::LockPoisoned)?;
        let current = self
            .primary_head_for_write()?
            .ok_or(PolicyRepositoryError::NoActivePolicy)?;
        if current.generation != expected_generation {
            return Err(PolicyRepositoryError::StaleGeneration {
                expected: Some(expected_generation),
                actual: Some(current.generation),
            });
        }
        let mut history = current.history.clone();
        let target = history
            .pop()
            .ok_or(PolicyRepositoryError::NoRollbackTarget)?;
        if receipt.policy_version != target.policy_version {
            return Err(PolicyRepositoryError::ReceiptVersionMismatch);
        }
        if self.load_validated(&receipt.policy_version)?.is_none() {
            return Err(PolicyRepositoryError::MissingVersion(
                receipt.policy_version.clone(),
            ));
        }
        let next = PolicyRepositoryHead {
            schema_version: POLICY_REPOSITORY_SCHEMA_VERSION.to_owned(),
            generation: current
                .generation
                .checked_add(1)
                .ok_or(PolicyRepositoryError::GenerationExhausted)?,
            active: receipt,
            history,
        };
        validate_head(&next)?;
        if let Err(error) = self.create_head(&next) {
            if error.is_already_exists() {
                return Err(PolicyRepositoryError::StaleGeneration {
                    expected: Some(expected_generation),
                    actual: self
                        .primary_head_for_write()?
                        .as_ref()
                        .map(|head| head.generation),
                });
            }
            return Err(error);
        }
        Ok(next)
    }

    fn recover_active(&self) -> Result<Option<RecoveredPolicy>, PolicyRepositoryError> {
        let heads = self.head_candidates()?;
        let Some((primary_generation, primary_path)) = heads.first() else {
            return Ok(None);
        };
        match self.load_recovery_candidate(
            primary_path,
            *primary_generation,
            PolicyRecoverySource::Primary,
        ) {
            Ok(Some(recovered)) => Ok(Some(recovered)),
            Ok(None) => Err(PolicyRepositoryError::InvalidHead),
            Err(primary) => match heads.get(1) {
                Some((last_good_generation, last_good_path)) => match self.load_recovery_candidate(
                    last_good_path,
                    *last_good_generation,
                    PolicyRecoverySource::LastGood,
                ) {
                    Ok(Some(recovered)) => Ok(Some(recovered)),
                    Ok(None) => Err(primary),
                    Err(last_good) => Err(PolicyRepositoryError::RecoveryUnavailable {
                        primary: primary.to_string(),
                        last_good: last_good.to_string(),
                    }),
                },
                None => Err(primary),
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredPolicyVersion {
    schema_version: String,
    version_id: String,
    drafted_by: AdministrativeIdentity,
    document: PolicyDocument,
}

impl StoredPolicyVersion {
    fn from_draft(draft: &DraftPolicyVersion) -> Self {
        Self {
            schema_version: POLICY_REPOSITORY_SCHEMA_VERSION.to_owned(),
            version_id: draft.version_id().to_owned(),
            drafted_by: draft.drafted_by().clone(),
            document: draft.document().clone(),
        }
    }

    fn from_validated(policy: &ValidatedPolicyVersion) -> Self {
        Self {
            schema_version: POLICY_REPOSITORY_SCHEMA_VERSION.to_owned(),
            version_id: policy.version_id().to_owned(),
            drafted_by: policy.drafted_by().clone(),
            document: policy.document().clone(),
        }
    }

    fn validate(&self) -> Result<(), PolicyRepositoryError> {
        if self.schema_version != POLICY_REPOSITORY_SCHEMA_VERSION {
            return Err(PolicyRepositoryError::UnsupportedSchema(
                self.schema_version.clone(),
            ));
        }
        validate_version_id(&self.version_id)?;
        let draft = PolicyLifecycle::draft(self.document.clone(), self.drafted_by.clone())?;
        if draft.version_id() != self.version_id {
            return Err(PolicyRepositoryError::VersionHashMismatch(
                self.version_id.clone(),
            ));
        }
        Ok(())
    }
}

/// Durable repository, validation, or optimistic-concurrency failure.
#[derive(Debug, Error)]
pub enum PolicyRepositoryError {
    /// Repository synchronization was poisoned.
    #[error("policy repository lock is unavailable")]
    LockPoisoned,
    /// A repository file exceeded the bounded storage contract.
    #[error("policy repository file is too large: {0}")]
    FileTooLarge(PathBuf),
    /// A filesystem operation failed.
    #[error("policy repository I/O failed at {path}: {source}")]
    Io {
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },
    /// Repository JSON was malformed or outside the schema.
    #[error("policy repository JSON is invalid: {0}")]
    Serialization(#[from] serde_json::Error),
    /// A persisted policy failed the policy contract.
    #[error("stored policy is invalid: {0}")]
    Policy(#[from] PolicyError),
    /// Repository schema is not supported.
    #[error("unsupported policy repository schema '{0}'")]
    UnsupportedSchema(String),
    /// Active-head structure was outside the bounded repository contract.
    #[error("invalid policy repository head")]
    InvalidHead,
    /// An administrative generation could no longer advance safely.
    #[error("policy repository generation is exhausted")]
    GenerationExhausted,
    /// A repository path was not a regular file.
    #[error("policy repository path is not a regular file: {0}")]
    InvalidFileType(PathBuf),
    /// Version identity was not a bounded content address.
    #[error("invalid policy version identity")]
    InvalidVersionId,
    /// Snapshot identity was not a generated activation identity.
    #[error("invalid policy snapshot identity")]
    InvalidSnapshotId,
    /// Stored document bytes did not reproduce their version identity.
    #[error("stored policy '{0}' does not match its content identity")]
    VersionHashMismatch(String),
    /// Existing immutable bytes differed for the same version identity.
    #[error("immutable policy version '{0}' already has different content")]
    ImmutableVersionMismatch(String),
    /// Caller raced a newer administrative mutation.
    #[error("stale policy repository generation: expected {expected:?}, actual {actual:?}")]
    StaleGeneration {
        /// Generation read by the caller, or none for initial creation.
        expected: Option<u64>,
        /// Current durable generation, or none before first activation.
        actual: Option<u64>,
    },
    /// Receipt and policy content identities disagreed.
    #[error("policy receipt does not match the selected version")]
    ReceiptVersionMismatch,
    /// Active head referenced no immutable version.
    #[error("policy repository is missing version '{0}'")]
    MissingVersion(String),
    /// Rollback was requested before any activation.
    #[error("policy repository has no active policy")]
    NoActivePolicy,
    /// Active head has no preceding activation.
    #[error("policy repository has no rollback target")]
    NoRollbackTarget,
    /// Both primary and preceding heads were unusable.
    #[error("policy repository recovery failed: primary={primary}; last-good={last_good}")]
    RecoveryUnavailable {
        /// Sanitized primary failure.
        primary: String,
        /// Sanitized last-good failure.
        last_good: String,
    },
}

impl PolicyRepositoryError {
    fn is_already_exists(&self) -> bool {
        matches!(
            self,
            Self::Io { source, .. } if source.kind() == io::ErrorKind::AlreadyExists
        )
    }
}

fn load_stored_version(
    path: &Path,
    expected_version_id: &str,
) -> Result<StoredPolicyVersion, PolicyRepositoryError> {
    let bytes = bounded_read(path)?;
    let stored: StoredPolicyVersion = serde_json::from_slice(&bytes)?;
    stored.validate()?;
    if stored.version_id != expected_version_id {
        return Err(PolicyRepositoryError::VersionHashMismatch(
            expected_version_id.to_owned(),
        ));
    }
    Ok(stored)
}

fn validate_head(head: &PolicyRepositoryHead) -> Result<(), PolicyRepositoryError> {
    if head.schema_version != POLICY_REPOSITORY_SCHEMA_VERSION {
        return Err(PolicyRepositoryError::UnsupportedSchema(
            head.schema_version.clone(),
        ));
    }
    if head.generation == 0 || head.history.len() > MAX_REPOSITORY_HISTORY {
        return Err(PolicyRepositoryError::InvalidHead);
    }
    validate_receipt(&head.active)?;
    for receipt in &head.history {
        validate_receipt(receipt)?;
    }
    Ok(())
}

fn validate_receipt(receipt: &PolicyReceipt) -> Result<(), PolicyRepositoryError> {
    validate_version_id(&receipt.policy_version)?;
    let snapshot = receipt
        .snapshot_id
        .strip_prefix("wps1-")
        .ok_or(PolicyRepositoryError::InvalidSnapshotId)?;
    if snapshot.len() != 32 || !snapshot.bytes().all(is_lower_hex) {
        return Err(PolicyRepositoryError::InvalidSnapshotId);
    }
    Ok(())
}

fn validate_version_id(version_id: &str) -> Result<(), PolicyRepositoryError> {
    let digest = version_id
        .strip_prefix("wpv1-")
        .ok_or(PolicyRepositoryError::InvalidVersionId)?;
    if digest.len() != 64 || !digest.bytes().all(is_lower_hex) {
        return Err(PolicyRepositoryError::InvalidVersionId);
    }
    Ok(())
}

fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

fn bounded_read(path: &Path) -> Result<Vec<u8>, PolicyRepositoryError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| PolicyRepositoryError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(PolicyRepositoryError::InvalidFileType(path.to_path_buf()));
    }
    if metadata.len() > MAX_REPOSITORY_FILE_BYTES {
        return Err(PolicyRepositoryError::FileTooLarge(path.to_path_buf()));
    }
    fs::read(path).map_err(|source| PolicyRepositoryError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn atomic_create(path: &Path, bytes: &[u8]) -> Result<(), PolicyRepositoryError> {
    let parent = path.parent().ok_or_else(|| PolicyRepositoryError::Io {
        path: path.to_path_buf(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "repository path has no parent"),
    })?;
    fs::create_dir_all(parent).map_err(|source| PolicyRepositoryError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("policy");
    let stage = parent.join(format!(".{filename}.stage-{}", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&stage)
            .map_err(|source| PolicyRepositoryError::Io {
                path: stage.clone(),
                source,
            })?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| PolicyRepositoryError::Io {
                path: stage.clone(),
                source,
            })?;
        fs::hard_link(&stage, path).map_err(|source| PolicyRepositoryError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let _ = fs::remove_file(&stage);
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if stage.exists() {
        let _ = fs::remove_file(stage);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use wayfinder_routing_core::RoutingConfig;

    fn actor() -> Result<AdministrativeIdentity, PolicyError> {
        AdministrativeIdentity::new("test", "operator")
    }

    fn policy(threshold: f64) -> Result<ValidatedPolicyVersion, PolicyError> {
        PolicyLifecycle::draft(
            PolicyDocument::new(
                vec![crate::control_plane::PolicyProfile::new(
                    "default",
                    &RoutingConfig::binary(threshold),
                )?],
                Vec::new(),
            )?,
            actor()?,
        )?
        .validate()
    }

    fn receipt(policy: &ValidatedPolicyVersion, suffix: &str) -> PolicyReceipt {
        PolicyReceipt {
            policy_version: policy.version_id().to_owned(),
            snapshot_id: format!("wps1-{suffix:0>32}"),
        }
    }

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "wayfinder-policy-repository-{name}-{}",
            Uuid::new_v4()
        ))
    }

    #[test]
    fn immutable_versions_round_trip_and_reject_path_shaped_ids()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = test_root("version");
        let repository = FilePolicyRepository::new(&root);
        let policy = policy(0.5)?;
        repository.store_validated(&policy)?;
        let loaded = repository
            .load_validated(policy.version_id())?
            .ok_or("stored version is missing")?;
        assert_eq!(loaded.version_id(), policy.version_id());
        assert!(matches!(
            repository.load_draft("../../head"),
            Err(PolicyRepositoryError::InvalidVersionId)
        ));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn activation_is_generation_guarded_and_rollback_is_durable()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = test_root("generation");
        let repository = FilePolicyRepository::new(&root);
        let first = policy(0.9)?;
        let second = policy(0.2)?;
        let first_head = repository.commit_activation(None, &first, receipt(&first, "1"))?;
        assert_eq!(first_head.generation, 1);
        let second_head = repository.commit_activation(
            Some(first_head.generation),
            &second,
            receipt(&second, "2"),
        )?;
        assert_eq!(second_head.generation, 2);
        assert_eq!(second_head.history.len(), 1);
        assert!(matches!(
            repository.commit_activation(Some(1), &first, receipt(&first, "3")),
            Err(PolicyRepositoryError::StaleGeneration {
                expected: Some(1),
                actual: Some(2)
            })
        ));

        let rolled_back = repository.commit_rollback(2, receipt(&first, "4"))?;
        assert_eq!(rolled_back.generation, 3);
        assert_eq!(rolled_back.active.policy_version, first.version_id());
        assert!(rolled_back.history.is_empty());
        drop(repository);

        let reopened = FilePolicyRepository::new(&root);
        let recovered = reopened
            .recover_active()?
            .ok_or("active policy is missing")?;
        assert_eq!(recovered.source, PolicyRecoverySource::Primary);
        assert_eq!(recovered.head.generation, 3);
        assert_eq!(recovered.policy.version_id(), first.version_id());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn corrupt_primary_recovers_only_the_preceding_valid_activation()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = test_root("recovery");
        let repository = FilePolicyRepository::new(&root);
        let first = policy(0.9)?;
        let second = policy(0.2)?;
        let first_head = repository.commit_activation(None, &first, receipt(&first, "1"))?;
        repository.commit_activation(
            Some(first_head.generation),
            &second,
            receipt(&second, "2"),
        )?;
        let corrupt_path = repository.head_path(2);
        fs::remove_file(&corrupt_path)?;
        fs::write(corrupt_path, b"not-json")?;
        drop(repository);

        let reopened = FilePolicyRepository::new(&root);
        let recovered = reopened
            .recover_active()?
            .ok_or("last-good policy is missing")?;
        assert_eq!(recovered.source, PolicyRecoverySource::LastGood);
        assert_eq!(recovered.head.generation, 1);
        assert_eq!(recovered.policy.version_id(), first.version_id());
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
