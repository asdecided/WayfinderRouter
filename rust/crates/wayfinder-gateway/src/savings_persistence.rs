//! Coalesced background persistence for the realized-savings ledger.
//!
//! The request path only marks the ledger dirty. One process-lifetime worker
//! writes a dirty ledger through [`SavingsLedger::save`] on Tokio's blocking
//! pool at most once per [`SAVINGS_PERSIST_INTERVAL`], so serialisation, file
//! writes, and fsyncs never run inside a request handler and never serialise
//! completions behind each other. Budgets are evaluated against the in-memory
//! ledger, so enforcement inside a running process is unaffected by when the
//! file is written.
//!
//! The worker is started explicitly (the `serve` entry point does this) and is
//! never started by constructing [`crate::AppState`], so tests and embedders
//! without a runtime-owned background task keep working. Without a running
//! worker, recorded turns stay in memory until the owner calls
//! [`crate::AppState::persist_savings`] or
//! [`crate::AppState::shutdown_savings_persister`].

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use thiserror::Error;
use tokio::runtime::Handle;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use wayfinder_service::pricing::{LedgerError, SavingsLedger};

use crate::metrics::GatewayMetrics;

/// Minimum spacing between background savings-ledger writes.
///
/// Durability trade-off: a graceful shutdown flushes everything, but an
/// unclean crash (SIGKILL, power loss, panic abort) can lose up to one
/// interval of recorded spend from the persisted file. One second bounds that
/// window while turning a write per completed turn into at most one write per
/// second, whatever the completion rate.
pub const SAVINGS_PERSIST_INTERVAL: Duration = Duration::from_secs(1);

/// Failure to start, flush, or stop savings persistence.
#[derive(Debug, Error)]
pub enum SavingsPersistError {
    /// The ledger could not be serialised or written.
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    /// The persister's worker slot lock was poisoned.
    #[error("savings persister lock is unavailable")]
    LockPoisoned,
    /// A background persister was requested outside an async runtime.
    #[error("savings persister requires an async runtime")]
    RuntimeUnavailable,
    /// A blocking save task or the worker task did not complete.
    #[error("savings persister task failed")]
    TaskFailed,
}

/// The ledger, destination, and metrics one persister writes for.
#[derive(Clone, Debug)]
pub(crate) struct SavingsTarget {
    pub(crate) ledger: Arc<SavingsLedger>,
    pub(crate) path: Arc<PathBuf>,
    pub(crate) metrics: Arc<GatewayMetrics>,
}

#[derive(Debug, Default)]
struct WorkerSlot {
    stop: Option<Arc<Notify>>,
    join: Option<JoinHandle<Result<(), SavingsPersistError>>>,
    started: bool,
}

/// Process-lifetime dirty flag and optional background writer for one ledger.
///
/// Hot reload carries this value over with the ledger and path, so a reload
/// never spawns a second writer, drops pending dirty state, or leaves a writer
/// bound to a stale path.
#[derive(Debug, Default)]
pub struct SavingsPersistence {
    dirty: AtomicBool,
    saves: AtomicU64,
    worker: Mutex<WorkerSlot>,
}

impl SavingsPersistence {
    /// Record that the in-memory ledger changed since the last write.
    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    /// Whether in-memory changes are waiting to be written.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Acquire)
    }

    /// Number of successful ledger writes made through this persister.
    #[must_use]
    pub fn completed_saves(&self) -> u64 {
        self.saves.load(Ordering::Acquire)
    }

    /// Whether a background worker is currently running.
    pub fn is_running(&self) -> Result<bool, SavingsPersistError> {
        self.worker
            .lock()
            .map(|slot| slot.join.is_some())
            .map_err(|_| SavingsPersistError::LockPoisoned)
    }

    /// Start the single background worker. Returns `false` when one was
    /// already started (running or shut down), so repeated calls never create
    /// a second writer.
    pub(crate) fn start(
        self: &Arc<Self>,
        target: SavingsTarget,
    ) -> Result<bool, SavingsPersistError> {
        let handle = Handle::try_current().map_err(|_| SavingsPersistError::RuntimeUnavailable)?;
        let mut slot = self
            .worker
            .lock()
            .map_err(|_| SavingsPersistError::LockPoisoned)?;
        if slot.started {
            return Ok(false);
        }
        let stop = Arc::new(Notify::new());
        slot.join = Some(handle.spawn(run_worker(Arc::clone(self), target, Arc::clone(&stop))));
        slot.stop = Some(stop);
        slot.started = true;
        Ok(true)
    }

    /// Stop the worker after a final write of any dirty state. Without a
    /// running worker, dirty state is written directly on the blocking pool.
    pub(crate) async fn shutdown(
        self: &Arc<Self>,
        target: SavingsTarget,
    ) -> Result<(), SavingsPersistError> {
        let (stop, join) = {
            let mut slot = self
                .worker
                .lock()
                .map_err(|_| SavingsPersistError::LockPoisoned)?;
            slot.started = true;
            (slot.stop.take(), slot.join.take())
        };
        match (stop, join) {
            (Some(stop), Some(join)) => {
                stop.notify_one();
                join.await.map_err(|_| SavingsPersistError::TaskFailed)?
            }
            _ => save_on_blocking_pool(Arc::clone(self), target)
                .await
                .map(|_| ()),
        }
    }

    /// Write the ledger now. `force` writes even when nothing is dirty.
    ///
    /// The dirty flag is cleared before the snapshot is taken, so turns
    /// recorded during a write mark it dirty again; a failed write restores it
    /// so the next attempt retries.
    pub(crate) fn save_now(
        &self,
        target: &SavingsTarget,
        force: bool,
    ) -> Result<bool, LedgerError> {
        let was_dirty = self.dirty.swap(false, Ordering::AcqRel);
        if !was_dirty && !force {
            return Ok(false);
        }
        match target.ledger.save(&target.path) {
            Ok(()) => {
                self.saves.fetch_add(1, Ordering::AcqRel);
                Ok(true)
            }
            Err(error) => {
                self.mark_dirty();
                report_failure(&target.metrics, &target.path, &error);
                Err(error)
            }
        }
    }
}

async fn save_on_blocking_pool(
    persistence: Arc<SavingsPersistence>,
    target: SavingsTarget,
) -> Result<bool, SavingsPersistError> {
    let metrics = Arc::clone(&target.metrics);
    let guard = Arc::clone(&persistence);
    match tokio::task::spawn_blocking(move || persistence.save_now(&target, false)).await {
        Ok(result) => result.map_err(SavingsPersistError::from),
        Err(error) => {
            guard.mark_dirty();
            let _ = metrics.record_savings_persist_failure();
            tracing::error!(error = %error, "savings ledger save task failed");
            Err(SavingsPersistError::TaskFailed)
        }
    }
}

async fn run_worker(
    persistence: Arc<SavingsPersistence>,
    target: SavingsTarget,
    stop: Arc<Notify>,
) -> Result<(), SavingsPersistError> {
    let mut interval = tokio::time::interval_at(
        tokio::time::Instant::now() + SAVINGS_PERSIST_INTERVAL,
        SAVINGS_PERSIST_INTERVAL,
    );
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            () = stop.notified() => break,
            _ = interval.tick() => {
                if persistence.is_dirty() {
                    // Failures are logged, counted, and leave the ledger dirty
                    // so the next tick retries.
                    let _ = save_on_blocking_pool(Arc::clone(&persistence), target.clone()).await;
                }
            }
        }
    }
    save_on_blocking_pool(persistence, target).await.map(|_| ())
}

fn report_failure(metrics: &GatewayMetrics, path: &Path, error: &LedgerError) {
    let _ = metrics.record_savings_persist_failure();
    let error_kind = match error {
        LedgerError::Io { source, .. } => format!("io:{:?}", source.kind()),
        LedgerError::Json(_) => "json".to_owned(),
        LedgerError::LockPoisoned => "lock_poisoned".to_owned(),
        LedgerError::InvalidDate(_) => "invalid_date".to_owned(),
    };
    tracing::error!(
        path = %path.display(),
        error_kind = %error_kind,
        "savings ledger persist failed; will retry"
    );
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use bytes::Bytes;
    use serde_json::{Value, json};
    use tower::ServiceExt;
    use wayfinder_routing_core::RoutingConfig;
    use wayfinder_service::pricing::{SavingsLedger, turn_cost};

    use super::SAVINGS_PERSIST_INTERVAL;
    use crate::delivery::{BufferedDelivery, BufferedDeliveryResponse, DeliveryFuture};
    use crate::{AppState, ConfiguredModel, build_router, utc_today};

    type TestResult = Result<(), Box<dyn Error>>;

    struct UsageDelivery;

    impl BufferedDelivery for UsageDelivery {
        fn send<'a>(&'a self, _model: &'a ConfiguredModel, _body: Value) -> DeliveryFuture<'a> {
            Box::pin(async {
                Ok(BufferedDeliveryResponse::new(
                    StatusCode::OK,
                    "application/json",
                    Bytes::from_static(
                        br#"{"object":"chat.completion","usage":{"prompt_tokens":100,"completion_tokens":5},"choices":[{"message":{"content":"ok"}}]}"#,
                    ),
                ))
            })
        }
    }

    fn temp_dir(label: &str) -> Result<std::path::PathBuf, Box<dyn Error>> {
        let directory =
            std::env::temp_dir().join(format!("wayfinder-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory)?;
        Ok(directory)
    }

    fn persisted_state(path: std::path::PathBuf) -> (AppState, Arc<SavingsLedger>) {
        let ledger = Arc::new(SavingsLedger::new(400, true));
        let state = AppState::new(
            RoutingConfig::binary(0.5),
            vec![
                ConfiguredModel::new("local", "http://127.0.0.1:11434/v1", "up", None, true)
                    .with_cost_per_1k(Some(0.0)),
            ],
            false,
            "test",
        )
        .with_delivery(Arc::new(UsageDelivery))
        .with_savings_ledger(Arc::clone(&ledger))
        .with_savings_path(path);
        (state, ledger)
    }

    fn record_turn(state: &AppState) -> TestResult {
        let cost = turn_cost("local", 100, 5, &state.price_table().costs, false, None)?;
        state.savings_ledger().record(&cost, utc_today()?, None)?;
        state.note_savings_recorded();
        Ok(())
    }

    fn assert_persisted_matches(path: &std::path::Path, ledger: &SavingsLedger) -> TestResult {
        let persisted = SavingsLedger::load(path)?;
        assert_eq!(persisted.to_value()?, ledger.to_value()?);
        Ok(())
    }

    fn failure_count(state: &AppState) -> Result<u64, Box<dyn Error>> {
        let text = state.metrics().render()?;
        let value = text
            .lines()
            .find_map(|line| line.strip_prefix("wayfinder_router_savings_persist_failures_total "))
            .ok_or("missing savings persist failure metric")?;
        Ok(value.parse()?)
    }

    #[tokio::test(start_paused = true)]
    async fn completed_turns_within_one_interval_coalesce_into_one_write() -> TestResult {
        let directory = temp_dir("savings-coalesce")?;
        let path = directory.join("savings.json");
        let (state, ledger) = persisted_state(path.clone());
        assert!(state.start_savings_persister()?);
        assert!(!state.start_savings_persister()?);

        let turns = 50;
        for _ in 0..turns {
            let request = Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&json!({
                    "model": "auto",
                    "messages": [{"role": "user", "content": "hi"}]
                }))?))?;
            let response = build_router(state.clone()).oneshot(request).await?;
            assert_eq!(response.status(), StatusCode::OK);
        }
        // The request path only marked the ledger dirty; nothing was written.
        assert_eq!(ledger.period(None, utc_today()?)?.requests, turns);
        assert!(state.savings_persistence().is_dirty());
        assert_eq!(state.savings_persistence().completed_saves(), 0);
        assert!(!path.exists());

        tokio::time::sleep(SAVINGS_PERSIST_INTERVAL + SAVINGS_PERSIST_INTERVAL / 2).await;
        assert_eq!(state.savings_persistence().completed_saves(), 1);
        assert!(!state.savings_persistence().is_dirty());
        assert_persisted_matches(&path, &ledger)?;

        // A clean ledger is never rewritten.
        tokio::time::sleep(SAVINGS_PERSIST_INTERVAL * 5).await;
        assert_eq!(state.savings_persistence().completed_saves(), 1);

        record_turn(&state)?;
        state.shutdown_savings_persister().await?;
        assert_eq!(state.savings_persistence().completed_saves(), 2);
        assert!(!state.savings_persistence().is_running()?);
        assert_persisted_matches(&path, &ledger)?;
        assert_eq!(failure_count(&state)?, 0);
        let _ = std::fs::remove_dir_all(directory);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn failed_save_is_counted_and_retried_until_it_succeeds() -> TestResult {
        let directory = temp_dir("savings-fault")?;
        // A regular file where the ledger's parent directory must be makes
        // every save fail until the blocker is removed.
        let blocker = directory.join("blocked");
        std::fs::write(&blocker, b"not a directory")?;
        let path = blocker.join("savings.json");
        let (state, ledger) = persisted_state(path.clone());
        assert!(state.start_savings_persister()?);

        record_turn(&state)?;
        tokio::time::sleep(SAVINGS_PERSIST_INTERVAL + SAVINGS_PERSIST_INTERVAL / 2).await;
        assert_eq!(failure_count(&state)?, 1);
        assert!(state.savings_persistence().is_dirty());
        assert_eq!(state.savings_persistence().completed_saves(), 0);

        record_turn(&state)?;
        tokio::time::sleep(SAVINGS_PERSIST_INTERVAL).await;
        assert_eq!(failure_count(&state)?, 2);
        assert!(state.savings_persistence().is_dirty());

        std::fs::remove_file(&blocker)?;
        tokio::time::sleep(SAVINGS_PERSIST_INTERVAL).await;
        assert_eq!(failure_count(&state)?, 2);
        assert!(!state.savings_persistence().is_dirty());
        assert_eq!(state.savings_persistence().completed_saves(), 1);
        assert_eq!(ledger.period(None, utc_today()?)?.requests, 2);
        assert_persisted_matches(&path, &ledger)?;

        state.shutdown_savings_persister().await?;
        let _ = std::fs::remove_dir_all(directory);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn graceful_shutdown_flushes_pending_dirty_state() -> TestResult {
        let directory = temp_dir("savings-shutdown")?;
        let path = directory.join("savings.json");
        let (state, ledger) = persisted_state(path.clone());
        assert!(state.start_savings_persister()?);
        for _ in 0..3 {
            record_turn(&state)?;
        }
        assert!(!path.exists());

        state.shutdown_savings_persister().await?;
        assert!(!state.savings_persistence().is_dirty());
        assert_eq!(state.savings_persistence().completed_saves(), 1);
        assert_eq!(ledger.period(None, utc_today()?)?.requests, 3);
        assert_persisted_matches(&path, &ledger)?;
        // A stopped persister is never restarted as a second writer.
        assert!(!state.start_savings_persister()?);
        assert!(!state.savings_persistence().is_running()?);
        let _ = std::fs::remove_dir_all(directory);
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_without_a_worker_still_flushes_dirty_state() -> TestResult {
        let directory = temp_dir("savings-unstarted")?;
        let path = directory.join("savings.json");
        let (state, ledger) = persisted_state(path.clone());
        record_turn(&state)?;
        assert!(!path.exists());
        state.shutdown_savings_persister().await?;
        assert_persisted_matches(&path, &ledger)?;
        let _ = std::fs::remove_dir_all(directory);
        Ok(())
    }

    #[test]
    fn explicit_flush_works_without_a_runtime() -> TestResult {
        let directory = temp_dir("savings-sync")?;
        let path = directory.join("savings.json");
        let (state, ledger) = persisted_state(path.clone());
        assert!(matches!(
            state.start_savings_persister(),
            Err(super::SavingsPersistError::RuntimeUnavailable)
        ));
        record_turn(&state)?;
        state.persist_savings()?;
        assert!(!state.savings_persistence().is_dirty());
        assert_persisted_matches(&path, &ledger)?;
        let _ = std::fs::remove_dir_all(directory);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn config_reload_keeps_one_persister_and_loses_nothing() -> TestResult {
        let directory = temp_dir("savings-reload")?;
        let path = directory.join("savings.json");
        let replacement_path = directory.join("replacement.json");
        let (previous, ledger) = persisted_state(path.clone());
        assert!(previous.start_savings_persister()?);
        record_turn(&previous)?;

        // A freshly built snapshot owns a fresh ledger, path, and persister;
        // adopting runtime state must keep the single process-lifetime writer
        // and its pending dirty state.
        let (replacement, _) = persisted_state(replacement_path.clone());
        let current = replacement.with_runtime_state_from(&previous);
        assert!(std::ptr::eq(
            current.savings_persistence(),
            previous.savings_persistence()
        ));
        assert!(current.savings_persistence().is_dirty());
        assert!(!current.start_savings_persister()?);

        record_turn(&current)?;
        tokio::time::sleep(SAVINGS_PERSIST_INTERVAL + SAVINGS_PERSIST_INTERVAL / 2).await;
        assert_eq!(current.savings_persistence().completed_saves(), 1);
        record_turn(&current)?;
        current.shutdown_savings_persister().await?;
        assert!(!previous.savings_persistence().is_running()?);

        assert_eq!(ledger.period(None, utc_today()?)?.requests, 3);
        assert_persisted_matches(&path, &ledger)?;
        assert!(!replacement_path.exists());
        let _ = std::fs::remove_dir_all(directory);
        Ok(())
    }
}
