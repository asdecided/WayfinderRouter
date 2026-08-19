//! Bounded append-only operator audit records.
//!
//! Audit records deliberately contain actor/action metadata only. Prompt text,
//! provider payloads, credentials, and raw authorization headers never enter
//! this sink. One bounded process-lifetime worker serializes local and shared
//! writes so callers can observe durable completion and shutdown can drain the
//! exact accepted prefix.

#![forbid(unsafe_code)]

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::state::{StateBackend, StateBackendError};

const MAX_ACTOR_BYTES: usize = 128;
const MAX_ACTION_BYTES: usize = 96;
const MAX_METADATA_BYTES: usize = 2_048;
/// Maximum accepted audit commands waiting behind the ordered worker.
pub const AUDIT_QUEUE_CAPACITY: usize = 256;
/// Maximum audit events retained in one Redis namespace.
pub const MAX_SHARED_AUDIT_EVENTS: usize = 10_000;

/// Synchronization, queue, or destination failure while recording an event.
#[derive(Debug, Error)]
pub enum AuditError {
    /// The audit worker or destination lock was poisoned.
    #[error("audit log lock is unavailable")]
    LockPoisoned,
    /// The bounded audit queue cannot accept another event.
    #[error("audit queue is full")]
    QueueFull,
    /// The worker has stopped or is shutting down.
    #[error("audit worker is unavailable")]
    WorkerUnavailable,
    /// The underlying local file could not be written.
    #[error("audit log write failed: {0}")]
    Io(#[from] io::Error),
    /// The shared audit destination could not acknowledge the write.
    #[error("shared audit write failed: {0}")]
    Shared(#[from] StateBackendError),
    /// Both configured destinations rejected the same event.
    #[error("local and shared audit writes failed: local={local}; shared={shared}")]
    MultipleDestinations {
        /// Sanitized local failure.
        local: String,
        /// Sanitized shared failure.
        shared: String,
    },
    /// Metadata exceeded the bounded audit contract.
    #[error("audit metadata is too large")]
    MetadataTooLarge,
    /// The event could not be encoded as JSON.
    #[error("audit event serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    /// A worker-backed audit sink was requested outside an async runtime.
    #[error("audit worker requires an async runtime")]
    RuntimeUnavailable,
}

/// One prompt-free operator event.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AuditEvent {
    /// Stable event schema version.
    pub schema_version: u8,
    /// Unique event identity used for cross-sink correlation.
    pub event_id: String,
    /// SHA-256 of every serialized event field except this digest.
    pub content_sha256: String,
    /// Unix timestamp in seconds.
    pub timestamp: u64,
    /// OIDC subject, virtual-key id, or bounded local/system actor label.
    pub actor: String,
    /// Stable action name.
    pub action: String,
    /// Immutable policy content identity active for this event, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_version: Option<String>,
    /// Immutable runtime snapshot identity active for this event, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    /// Sanitized state before an operator action, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<Value>,
    /// Sanitized state after an operator action, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Value>,
}

#[derive(Serialize)]
struct HashableAuditEvent<'a> {
    schema_version: u8,
    event_id: &'a str,
    timestamp: u64,
    actor: &'a str,
    action: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    policy_version: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    before: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after: Option<&'a Value>,
}

/// A cheap clone of an optional ordered audit pipeline.
#[derive(Clone)]
pub struct AuditLog {
    sender: Option<Arc<Mutex<AuditSender>>>,
    shared: Option<Arc<Mutex<Option<SharedAuditSink>>>>,
    path: Option<Arc<PathBuf>>,
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::disabled()
    }
}

impl fmt::Debug for AuditLog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let shared = self.shared.as_ref().is_some_and(|shared| {
            shared
                .lock()
                .map(|destination| destination.is_some())
                .unwrap_or(false)
        });
        formatter
            .debug_struct("AuditLog")
            .field("path", &self.path())
            .field("shared", &shared)
            .finish()
    }
}

impl AuditLog {
    /// Construct a disabled sink for in-process callers that do not opt in.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            sender: None,
            shared: None,
            path: None,
        }
    }

    /// Open an append-only JSONL file and start its bounded ordered worker.
    pub fn from_path(path: impl Into<PathBuf>) -> Result<Self, AuditError> {
        let path = path.into();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Self::spawn(
            Some(Box::new(FileAuditSink { file })),
            None,
            Some(Arc::new(path)),
        )
    }

    /// Start a shared-only audit pipeline.
    pub fn from_shared_backend(
        backend: Arc<dyn StateBackend>,
        namespace: impl Into<Arc<str>>,
    ) -> Result<Self, AuditError> {
        Self::spawn(
            None,
            Some(SharedAuditSink {
                backend,
                key: namespace.into(),
            }),
            None,
        )
    }

    /// Add an acknowledged shared destination without replacing the local log.
    pub fn with_shared_backend(
        self,
        backend: Arc<dyn StateBackend>,
        namespace: impl Into<Arc<str>>,
    ) -> Result<Self, AuditError> {
        let destination = SharedAuditSink {
            backend,
            key: namespace.into(),
        };
        if let Some(shared) = self.shared.clone() {
            let mut shared = shared.lock().map_err(|_| AuditError::LockPoisoned)?;
            *shared = Some(destination);
            drop(shared);
            return Ok(self);
        }
        Self::spawn(None, Some(destination), self.path.clone())
    }

    /// Queue one event and await completion at every configured destination.
    pub async fn record(
        &self,
        actor: impl AsRef<str>,
        action: impl AsRef<str>,
        before: Option<Value>,
        after: Option<Value>,
    ) -> Result<(), AuditError> {
        self.record_optional_policy(actor, action, before, after, None, None)
            .await
    }

    /// Queue one event with the active policy and snapshot identities.
    pub async fn record_with_policy(
        &self,
        actor: impl AsRef<str>,
        action: impl AsRef<str>,
        before: Option<Value>,
        after: Option<Value>,
        policy_version: impl AsRef<str>,
        snapshot_id: impl AsRef<str>,
    ) -> Result<(), AuditError> {
        self.record_optional_policy(
            actor,
            action,
            before,
            after,
            Some(policy_version.as_ref()),
            Some(snapshot_id.as_ref()),
        )
        .await
    }

    async fn record_optional_policy(
        &self,
        actor: impl AsRef<str>,
        action: impl AsRef<str>,
        before: Option<Value>,
        after: Option<Value>,
        policy_version: Option<&str>,
        snapshot_id: Option<&str>,
    ) -> Result<(), AuditError> {
        if self.sender.is_none() {
            return Ok(());
        }
        let event = AuditEvent::new(
            actor.as_ref(),
            action.as_ref(),
            before,
            after,
            policy_version,
            snapshot_id,
        )?;
        let encoded = serde_json::to_string(&event)?;
        let (acknowledge, acknowledged) = oneshot::channel();
        self.try_send(AuditCommand::Record {
            encoded,
            acknowledge,
        })?;
        await_acknowledgement(acknowledged).await
    }

    /// Acknowledge every accepted event and durably flush the local file.
    pub async fn flush(&self) -> Result<(), AuditError> {
        if self.sender.is_none() {
            return Ok(());
        }
        let (acknowledge, acknowledged) = oneshot::channel();
        self.try_send(AuditCommand::Flush { acknowledge })?;
        await_acknowledgement(acknowledged).await
    }

    /// Stop accepting events, drain the ordered prefix, and acknowledge flush.
    pub async fn shutdown(&self) -> Result<(), AuditError> {
        let Some(sender) = &self.sender else {
            return Ok(());
        };
        let sender = {
            let mut state = sender.lock().map_err(|_| AuditError::LockPoisoned)?;
            if state.closing {
                return Err(AuditError::WorkerUnavailable);
            }
            state.closing = true;
            state.sender.clone()
        };
        let (acknowledge, acknowledged) = oneshot::channel();
        sender
            .send(AuditCommand::Shutdown { acknowledge })
            .await
            .map_err(|_| AuditError::WorkerUnavailable)?;
        await_acknowledgement(acknowledged).await
    }

    /// Path of the local file, when enabled, for diagnostics and tests.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref().map(PathBuf::as_path)
    }

    fn spawn(
        local: Option<Box<dyn LocalAuditSink>>,
        shared: Option<SharedAuditSink>,
        path: Option<Arc<PathBuf>>,
    ) -> Result<Self, AuditError> {
        let handle = Handle::try_current().map_err(|_| AuditError::RuntimeUnavailable)?;
        let (sender, receiver) = mpsc::channel(AUDIT_QUEUE_CAPACITY);
        let shared = Arc::new(Mutex::new(shared));
        handle.spawn(run_audit_worker(receiver, local, Arc::clone(&shared)));
        Ok(Self {
            sender: Some(Arc::new(Mutex::new(AuditSender {
                sender,
                closing: false,
            }))),
            shared: Some(shared),
            path,
        })
    }

    fn try_send(&self, command: AuditCommand) -> Result<(), AuditError> {
        let Some(sender) = &self.sender else {
            return Ok(());
        };
        let state = sender.lock().map_err(|_| AuditError::LockPoisoned)?;
        if state.closing {
            return Err(AuditError::WorkerUnavailable);
        }
        state.sender.try_send(command).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => AuditError::QueueFull,
            mpsc::error::TrySendError::Closed(_) => AuditError::WorkerUnavailable,
        })
    }
}

impl AuditEvent {
    fn new(
        actor: &str,
        action: &str,
        before: Option<Value>,
        after: Option<Value>,
        policy_version: Option<&str>,
        snapshot_id: Option<&str>,
    ) -> Result<Self, AuditError> {
        let actor = bounded_label(actor, "unknown", MAX_ACTOR_BYTES);
        let action = bounded_label(action, "unknown", MAX_ACTION_BYTES);
        if before.as_ref().is_some_and(metadata_too_large)
            || after.as_ref().is_some_and(metadata_too_large)
        {
            return Err(AuditError::MetadataTooLarge);
        }
        let mut event = Self {
            schema_version: 2,
            event_id: Uuid::new_v4().to_string(),
            content_sha256: String::new(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs()),
            actor,
            action,
            policy_version: policy_version
                .map(|value| bounded_label(value, "unknown", MAX_ACTION_BYTES)),
            snapshot_id: snapshot_id.map(|value| bounded_label(value, "unknown", MAX_ACTION_BYTES)),
            before,
            after,
        };
        event.content_sha256 = event_content_sha256(&event)?;
        Ok(event)
    }
}

struct AuditSender {
    sender: mpsc::Sender<AuditCommand>,
    closing: bool,
}

enum AuditCommand {
    Record {
        encoded: String,
        acknowledge: oneshot::Sender<Result<(), AuditError>>,
    },
    Flush {
        acknowledge: oneshot::Sender<Result<(), AuditError>>,
    },
    Shutdown {
        acknowledge: oneshot::Sender<Result<(), AuditError>>,
    },
}

trait LocalAuditSink: Send {
    fn append(&mut self, encoded: &str) -> io::Result<()>;
    fn flush_durable(&mut self) -> io::Result<()>;
}

struct FileAuditSink {
    file: File,
}

impl LocalAuditSink for FileAuditSink {
    fn append(&mut self, encoded: &str) -> io::Result<()> {
        let mut line = encoded.as_bytes().to_vec();
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.flush()
    }

    fn flush_durable(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.file.sync_all()
    }
}

#[derive(Clone)]
struct SharedAuditSink {
    backend: Arc<dyn StateBackend>,
    key: Arc<str>,
}

async fn run_audit_worker(
    mut receiver: mpsc::Receiver<AuditCommand>,
    mut local: Option<Box<dyn LocalAuditSink>>,
    shared: Arc<Mutex<Option<SharedAuditSink>>>,
) {
    while let Some(command) = receiver.recv().await {
        match command {
            AuditCommand::Record {
                encoded,
                acknowledge,
            } => {
                let result = write_event(&mut local, &shared, &encoded).await;
                acknowledge_result(acknowledge, result, "audit record acknowledgement was lost");
            }
            AuditCommand::Flush { acknowledge } => {
                acknowledge_result(
                    acknowledge,
                    flush_local(&mut local).await,
                    "audit flush acknowledgement was lost",
                );
            }
            AuditCommand::Shutdown { acknowledge } => {
                acknowledge_result(
                    acknowledge,
                    flush_local(&mut local).await,
                    "audit shutdown acknowledgement was lost",
                );
                return;
            }
        }
    }
    if let Err(error) = flush_local(&mut local).await {
        tracing::error!(error = %error, "audit worker final flush failed");
    }
}

async fn write_event(
    local: &mut Option<Box<dyn LocalAuditSink>>,
    shared: &Arc<Mutex<Option<SharedAuditSink>>>,
    encoded: &str,
) -> Result<(), AuditError> {
    let local_result = append_local(local, encoded).await;
    let shared = shared.lock().map_err(|_| AuditError::LockPoisoned)?.clone();
    let shared_result = match shared {
        Some(shared) => shared
            .backend
            .append_audit(&shared.key, encoded, MAX_SHARED_AUDIT_EVENTS)
            .await
            .map_err(AuditError::Shared),
        None => Ok(()),
    };
    combine_destination_results(local_result, shared_result)
}

fn acknowledge_result(
    acknowledge: oneshot::Sender<Result<(), AuditError>>,
    result: Result<(), AuditError>,
    message: &'static str,
) {
    if let Err(Err(error)) = acknowledge.send(result) {
        tracing::error!(error = %error, "{message}");
    }
}

fn combine_destination_results(
    local: Result<(), AuditError>,
    shared: Result<(), AuditError>,
) -> Result<(), AuditError> {
    match (local, shared) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(local), Err(shared)) => Err(AuditError::MultipleDestinations {
            local: local.to_string(),
            shared: shared.to_string(),
        }),
    }
}

async fn append_local(
    local: &mut Option<Box<dyn LocalAuditSink>>,
    encoded: &str,
) -> Result<(), AuditError> {
    let Some(mut sink) = local.take() else {
        return Ok(());
    };
    let encoded = encoded.to_owned();
    match tokio::task::spawn_blocking(move || {
        let result = sink.append(&encoded).map_err(AuditError::Io);
        (sink, result)
    })
    .await
    {
        Ok((sink, result)) => {
            *local = Some(sink);
            result
        }
        Err(error) => {
            tracing::error!(error = %error, "audit local append task failed");
            Err(AuditError::WorkerUnavailable)
        }
    }
}

async fn flush_local(local: &mut Option<Box<dyn LocalAuditSink>>) -> Result<(), AuditError> {
    let Some(mut sink) = local.take() else {
        return Ok(());
    };
    match tokio::task::spawn_blocking(move || {
        let result = sink.flush_durable().map_err(AuditError::Io);
        (sink, result)
    })
    .await
    {
        Ok((sink, result)) => {
            *local = Some(sink);
            result
        }
        Err(error) => {
            tracing::error!(error = %error, "audit local flush task failed");
            Err(AuditError::WorkerUnavailable)
        }
    }
}

async fn await_acknowledgement(
    acknowledged: oneshot::Receiver<Result<(), AuditError>>,
) -> Result<(), AuditError> {
    acknowledged
        .await
        .map_err(|_| AuditError::WorkerUnavailable)?
}

fn event_content_sha256(event: &AuditEvent) -> Result<String, AuditError> {
    let hashable = HashableAuditEvent {
        schema_version: event.schema_version,
        event_id: &event.event_id,
        timestamp: event.timestamp,
        actor: &event.actor,
        action: &event.action,
        policy_version: event.policy_version.as_deref(),
        snapshot_id: event.snapshot_id.as_deref(),
        before: event.before.as_ref(),
        after: event.after.as_ref(),
    };
    let digest = Sha256::digest(serde_json::to_vec(&hashable)?);
    let mut encoded = String::with_capacity(digest.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(encoded)
}

fn bounded_label(value: &str, fallback: &str, max_bytes: usize) -> String {
    let value = value.trim();
    if value.is_empty()
        || value.len() > max_bytes
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return fallback.to_owned();
    }
    value.to_owned()
}

fn metadata_too_large(value: &Value) -> bool {
    serde_json::to_vec(value).is_ok_and(|encoded| encoded.len() > MAX_METADATA_BYTES)
}

/// Actor attached to a request by the operator-auth middleware.
#[derive(Clone, Debug)]
pub struct AuditActor(pub String);

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    struct FailingLocalSink;

    impl LocalAuditSink for FailingLocalSink {
        fn append(&mut self, _encoded: &str) -> io::Result<()> {
            Err(io::Error::other("injected audit write failure"))
        }

        fn flush_durable(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn audit_path() -> PathBuf {
        std::env::temp_dir().join(format!("wayfinder-audit-{}.jsonl", Uuid::new_v4()))
    }

    #[tokio::test]
    async fn records_are_ordered_and_flush_is_acknowledged() -> Result<(), AuditError> {
        let path = audit_path();
        let log = AuditLog::from_path(&path)?;
        for index in 0..20 {
            log.record("operator", format!("action.{index}"), None, None)
                .await?;
        }
        log.flush().await?;
        let text = fs::read_to_string(&path)?;
        let events = text
            .lines()
            .map(serde_json::from_str::<AuditEvent>)
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(events.len(), 20);
        for (index, event) in events.iter().enumerate() {
            assert_eq!(event.action, format!("action.{index}"));
        }
        log.shutdown().await?;
        let _ = fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn event_identity_and_content_digest_detect_tampering() -> Result<(), AuditError> {
        let event = AuditEvent::new(
            "operator",
            "config.reload",
            None,
            Some(serde_json::json!({"version": "2"})),
            Some("wpv1-example"),
            Some("wps1-example"),
        )?;
        assert!(Uuid::parse_str(&event.event_id).is_ok());
        assert_eq!(event.content_sha256.len(), 64);
        assert_eq!(event.content_sha256, event_content_sha256(&event)?);
        let mut changed = event.clone();
        changed.action = "config.reload_failed".to_owned();
        assert_ne!(event.content_sha256, event_content_sha256(&changed)?);
        Ok(())
    }

    #[tokio::test]
    async fn bounded_queue_rejects_overflow_before_worker_runs() -> Result<(), AuditError> {
        let log = AuditLog::spawn(None, None, None)?;
        let mut acknowledgements = Vec::new();
        for index in 0..AUDIT_QUEUE_CAPACITY {
            let event = AuditEvent::new(
                "operator",
                &format!("action.{index}"),
                None,
                None,
                None,
                None,
            )?;
            let (acknowledge, acknowledged) = oneshot::channel();
            log.try_send(AuditCommand::Record {
                encoded: serde_json::to_string(&event)?,
                acknowledge,
            })?;
            acknowledgements.push(acknowledged);
        }
        let event = AuditEvent::new("operator", "overflow", None, None, None, None)?;
        let (acknowledge, _acknowledged) = oneshot::channel();
        assert!(matches!(
            log.try_send(AuditCommand::Record {
                encoded: serde_json::to_string(&event)?,
                acknowledge,
            }),
            Err(AuditError::QueueFull)
        ));
        for acknowledged in acknowledgements {
            await_acknowledgement(acknowledged).await?;
        }
        log.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn destination_failure_is_returned_to_the_caller() -> Result<(), AuditError> {
        let log = AuditLog::spawn(Some(Box::new(FailingLocalSink)), None, None)?;
        assert!(matches!(
            log.record("operator", "config.reload", None, None).await,
            Err(AuditError::Io(_))
        ));
        log.shutdown().await?;

        let shared = AuditLog::from_shared_backend(
            Arc::new(crate::state::MemoryStateBackend::default()),
            "test",
        )?;
        assert!(matches!(
            shared.record("operator", "config.reload", None, None).await,
            Err(AuditError::Shared(StateBackendError::Unavailable))
        ));
        shared.shutdown().await?;

        let path = audit_path();
        let local_fallback = AuditLog::from_path(&path)?.with_shared_backend(
            Arc::new(crate::state::MemoryStateBackend::default()),
            "test",
        )?;
        assert!(matches!(
            local_fallback
                .record("operator", "locally.persisted", None, None)
                .await,
            Err(AuditError::Shared(StateBackendError::Unavailable))
        ));
        local_fallback.shutdown().await?;
        assert!(fs::read_to_string(&path)?.contains("locally.persisted"));
        let _ = fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_flushes_and_rejects_late_events() -> Result<(), AuditError> {
        let path = audit_path();
        let log = AuditLog::from_path(&path)?;
        log.record("operator", "config.reload", None, None).await?;
        log.shutdown().await?;
        assert!(matches!(
            log.record("operator", "late", None, None).await,
            Err(AuditError::WorkerUnavailable)
        ));
        assert!(fs::read_to_string(&path)?.contains("config.reload"));
        let _ = fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn prompt_like_metadata_is_bounded_and_labels_are_sanitized() -> Result<(), AuditError> {
        let path = audit_path();
        let log = AuditLog::from_path(&path)?;
        assert!(matches!(
            log.record(
                "bad\nactor",
                "action",
                Some(Value::String("x".repeat(3_000))),
                None
            )
            .await,
            Err(AuditError::MetadataTooLarge)
        ));
        log.shutdown().await?;
        let _ = fs::remove_file(path);
        Ok(())
    }
}
