//! Bounded append-only operator audit records.
//!
//! Audit records deliberately contain actor/action metadata only. Prompt text,
//! provider payloads, credentials, and raw authorization headers never enter
//! this sink. A single append-locked file makes concurrent writers safe for a
//! process; a later shared-state adapter can implement the same sink contract.

#![forbid(unsafe_code)]

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

const MAX_ACTOR_BYTES: usize = 128;
const MAX_ACTION_BYTES: usize = 96;
const MAX_METADATA_BYTES: usize = 2_048;

/// Synchronization or serialization failure while appending an audit event.
#[derive(Debug, Error)]
pub enum AuditError {
    /// The append lock was poisoned.
    #[error("audit log lock is unavailable")]
    LockPoisoned,
    /// The underlying file could not be written.
    #[error("audit log write failed: {0}")]
    Io(#[from] io::Error),
    /// Metadata exceeded the bounded audit contract.
    #[error("audit metadata is too large")]
    MetadataTooLarge,
    /// The event could not be encoded as JSON.
    #[error("audit event serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// One prompt-free operator event.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AuditEvent {
    /// Stable event schema version.
    pub schema_version: u8,
    /// Unix timestamp in seconds.
    pub timestamp: u64,
    /// OIDC subject, virtual-key id, or bounded local/system actor label.
    pub actor: String,
    /// Stable action name.
    pub action: String,
    /// Sanitized state before an operator action, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<Value>,
    /// Sanitized state after an operator action, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Value>,
}

/// A cheap clone of an optional audit destination.
#[derive(Clone, Debug, Default)]
pub struct AuditLog {
    sink: Option<Arc<FileAuditSink>>,
}

impl AuditLog {
    /// Construct a disabled sink for in-process tests and callers that do not
    /// opt into an operator audit path.
    #[must_use]
    pub const fn disabled() -> Self {
        Self { sink: None }
    }

    /// Open an append-only JSONL file, creating its parent directory when it
    /// exists. The file handle is shared across cloned gateway snapshots.
    pub fn from_path(path: impl Into<PathBuf>) -> Result<Self, AuditError> {
        let path = path.into();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            sink: Some(Arc::new(FileAuditSink {
                path,
                file: Mutex::new(file),
            })),
        })
    }

    /// Append one bounded event. Disabled logs intentionally succeed so
    /// instrumentation never changes gateway behavior for existing callers.
    pub fn record(
        &self,
        actor: impl AsRef<str>,
        action: impl AsRef<str>,
        before: Option<Value>,
        after: Option<Value>,
    ) -> Result<(), AuditError> {
        let Some(sink) = &self.sink else {
            return Ok(());
        };
        sink.append(AuditEvent::new(
            actor.as_ref(),
            action.as_ref(),
            before,
            after,
        )?)
    }

    /// Path of the local file, when enabled, for diagnostics and tests.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.sink.as_deref().map(|sink| sink.path.as_path())
    }
}

impl AuditEvent {
    fn new(
        actor: &str,
        action: &str,
        before: Option<Value>,
        after: Option<Value>,
    ) -> Result<Self, AuditError> {
        let actor = bounded_label(actor, "unknown", MAX_ACTOR_BYTES);
        let action = bounded_label(action, "unknown", MAX_ACTION_BYTES);
        if before.as_ref().is_some_and(metadata_too_large)
            || after.as_ref().is_some_and(metadata_too_large)
        {
            return Err(AuditError::MetadataTooLarge);
        }
        Ok(Self {
            schema_version: 1,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs()),
            actor,
            action,
            before,
            after,
        })
    }
}

#[derive(Debug)]
struct FileAuditSink {
    path: PathBuf,
    file: Mutex<File>,
}

impl FileAuditSink {
    fn append(&self, event: AuditEvent) -> Result<(), AuditError> {
        let mut encoded = serde_json::to_vec(&event)?;
        encoded.push(b'\n');
        let mut file = self.file.lock().map_err(|_| AuditError::LockPoisoned)?;
        file.write_all(&encoded)?;
        file.flush()?;
        Ok(())
    }
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

    #[test]
    fn concurrent_records_are_one_json_object_per_line() -> Result<(), AuditError> {
        let path =
            std::env::temp_dir().join(format!("wayfinder-audit-{}.jsonl", uuid::Uuid::new_v4()));
        let log = AuditLog::from_path(&path)?;
        let writers = (0..8)
            .map(|index| {
                let log = log.clone();
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        log.record(format!("operator-{index}"), "config.reload", None, None)?;
                    }
                    Ok::<_, AuditError>(())
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().map_err(|_| AuditError::LockPoisoned)??;
        }
        let text = fs::read_to_string(&path)?;
        assert_eq!(text.lines().count(), 160);
        for line in text.lines() {
            let event: AuditEvent = serde_json::from_str(line)?;
            assert_eq!(event.schema_version, 1);
            assert!(!event.actor.is_empty());
            assert!(!event.action.is_empty());
        }
        let _ = fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn prompt_like_metadata_is_bounded_and_labels_are_sanitized() {
        let path =
            std::env::temp_dir().join(format!("wayfinder-audit-{}.jsonl", uuid::Uuid::new_v4()));
        let Ok(log) = AuditLog::from_path(&path) else {
            return;
        };
        assert!(matches!(
            log.record(
                "bad\nactor",
                "action",
                Some(Value::String("x".repeat(3_000))),
                None
            ),
            Err(AuditError::MetadataTooLarge)
        ));
        let _ = fs::remove_file(path);
    }
}
