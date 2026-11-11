//! Last-good immutable snapshot hot-reload state machine.
//!
//! The observed source version advances before parsing. A failed version is
//! therefore attempted once, retains the previous snapshot, and does not
//! thrash every request. File discovery, metadata, parsing, and logging stay at
//! the caller boundary so this state machine is deterministic in tests.

use std::fmt;
use std::sync::{Arc, Mutex};

use thiserror::Error;

/// Reload synchronization failure.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ReloadError {
    /// Internal state could not be synchronized.
    #[error("config reload state lock is unavailable")]
    LockPoisoned,
}

/// Result of checking one observed source version.
#[derive(Debug)]
pub enum ReloadOutcome<T, E> {
    /// Version was unchanged; loader was not called.
    Unchanged(Arc<T>),
    /// A new version parsed and became current.
    Reloaded(Arc<T>),
    /// A new version failed and the last-good snapshot remains current.
    Retained { current: Arc<T>, error: E },
}

impl<T, E> ReloadOutcome<T, E> {
    /// Snapshot that request handling should use for every outcome.
    #[must_use]
    pub fn current(&self) -> &Arc<T> {
        match self {
            Self::Unchanged(current) | Self::Reloaded(current) | Self::Retained { current, .. } => {
                current
            }
        }
    }
}

struct State<T, V> {
    version: V,
    current: Arc<T>,
    failures: u64,
}

/// Thread-safe versioned last-good configuration holder.
pub struct LastGood<T, V> {
    state: Mutex<State<T, V>>,
}

impl<T: fmt::Debug, V: fmt::Debug> fmt::Debug for LastGood<T, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.state.lock() {
            Ok(state) => formatter
                .debug_struct("LastGood")
                .field("version", &state.version)
                .field("current", &state.current)
                .field("failures", &state.failures)
                .finish(),
            Err(_) => formatter
                .debug_struct("LastGood")
                .field("state", &"<unavailable>")
                .finish(),
        }
    }
}

impl<T, V> LastGood<T, V>
where
    V: Clone + Eq,
{
    /// Construct from an already parsed initial snapshot and observed version.
    #[must_use]
    pub fn new(initial: T, version: V) -> Self {
        Self {
            state: Mutex::new(State {
                version,
                current: Arc::new(initial),
                failures: 0,
            }),
        }
    }

    /// Return the current immutable snapshot.
    pub fn current(&self) -> Result<Arc<T>, ReloadError> {
        self.state
            .lock()
            .map(|state| Arc::clone(&state.current))
            .map_err(|_| ReloadError::LockPoisoned)
    }

    /// Return the most recently observed source version, including failed ones.
    pub fn version(&self) -> Result<V, ReloadError> {
        self.state
            .lock()
            .map(|state| state.version.clone())
            .map_err(|_| ReloadError::LockPoisoned)
    }

    /// Number of distinct changed versions that failed to parse.
    #[cfg(test)]
    pub(crate) fn failure_count(&self) -> Result<u64, ReloadError> {
        self.state
            .lock()
            .map(|state| state.failures)
            .map_err(|_| ReloadError::LockPoisoned)
    }

    /// Check a source version and parse it at most once.
    ///
    /// The loader runs while the state lock is held, guaranteeing that
    /// concurrent requests cannot both parse or install the same version.
    pub fn refresh<E>(
        &self,
        observed_version: V,
        load: impl FnOnce() -> Result<T, E>,
    ) -> Result<ReloadOutcome<T, E>, ReloadError> {
        let mut state = self.state.lock().map_err(|_| ReloadError::LockPoisoned)?;
        if observed_version == state.version {
            return Ok(ReloadOutcome::Unchanged(Arc::clone(&state.current)));
        }
        state.version = observed_version;
        match load() {
            Ok(next) => {
                state.current = Arc::new(next);
                Ok(ReloadOutcome::Reloaded(Arc::clone(&state.current)))
            }
            Err(error) => {
                state.failures = state.failures.saturating_add(1);
                Ok(ReloadOutcome::Retained {
                    current: Arc::clone(&state.current),
                    error,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use super::*;

    #[test]
    fn unchanged_version_does_not_call_loader() -> Result<(), ReloadError> {
        let holder = LastGood::new("one".to_owned(), Some(1_u64));
        let calls = AtomicUsize::new(0);
        let outcome = holder.refresh(Some(1), || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok::<_, ()>("unexpected".to_owned())
        })?;
        assert!(matches!(outcome, ReloadOutcome::Unchanged(_)));
        assert_eq!(outcome.current().as_str(), "one");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        Ok(())
    }

    #[test]
    fn successful_change_becomes_current() -> Result<(), ReloadError> {
        let holder = LastGood::new(1, 1_u64);
        let outcome = holder.refresh(2, || Ok::<_, ()>(2))?;
        assert!(matches!(outcome, ReloadOutcome::Reloaded(_)));
        assert_eq!(*holder.current()?, 2);
        assert_eq!(holder.version()?, 2);
        assert_eq!(holder.failure_count()?, 0);
        Ok(())
    }

    #[test]
    fn failed_version_is_retained_and_not_retried() -> Result<(), ReloadError> {
        let holder = LastGood::new("good".to_owned(), 1_u64);
        let failed = holder.refresh(2, || Err::<String, _>("invalid"))?;
        match failed {
            ReloadOutcome::Retained { current, error } => {
                assert_eq!(current.as_str(), "good");
                assert_eq!(error, "invalid");
            }
            ReloadOutcome::Unchanged(_) | ReloadOutcome::Reloaded(_) => {
                return Err(ReloadError::LockPoisoned);
            }
        }
        let calls = AtomicUsize::new(0);
        let same = holder.refresh(2, || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok::<_, &str>("would-be-good".to_owned())
        })?;
        assert!(matches!(same, ReloadOutcome::Unchanged(_)));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(holder.failure_count()?, 1);
        Ok(())
    }

    #[test]
    fn later_version_recovers_after_failure() -> Result<(), ReloadError> {
        let holder = LastGood::new(1, 1_u64);
        let _ = holder.refresh(2, || Err::<u64, _>("bad"))?;
        let recovered = holder.refresh(3, || Ok::<_, &str>(3))?;
        assert!(matches!(recovered, ReloadOutcome::Reloaded(_)));
        assert_eq!(*holder.current()?, 3);
        assert_eq!(holder.failure_count()?, 1);
        Ok(())
    }

    #[test]
    fn concurrent_same_version_loads_once() -> Result<(), Box<dyn std::error::Error>> {
        let holder = Arc::new(LastGood::new(1_u64, 1_u64));
        let calls = Arc::new(AtomicUsize::new(0));
        let threads = (0..8)
            .map(|_| {
                let holder = Arc::clone(&holder);
                let calls = Arc::clone(&calls);
                thread::spawn(move || {
                    holder.refresh(2, || {
                        calls.fetch_add(1, Ordering::Relaxed);
                        Ok::<_, ()>(2)
                    })
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            let result = thread.join().map_err(|_| "reload test thread failed")??;
            assert_eq!(**result.current(), 2);
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        Ok(())
    }
}
