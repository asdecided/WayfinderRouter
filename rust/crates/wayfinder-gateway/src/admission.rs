//! Bounded concurrent-delivery admission for one gateway process.

use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Default simultaneous upstream deliveries accepted by one process.
pub const DEFAULT_MAX_IN_FLIGHT: usize = 32;
/// Default requests allowed to wait for an upstream delivery slot.
pub const DEFAULT_MAX_QUEUED: usize = 64;
/// Default maximum time a queued request waits for a delivery slot.
pub const DEFAULT_QUEUE_TIMEOUT: Duration = Duration::from_secs(2);

/// Invalid admission configuration or a bounded overload result.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum AdmissionError {
    /// At least one in-flight delivery slot is required.
    #[error("gateway concurrency max_in_flight must be positive")]
    InvalidInFlightLimit,
    /// The configured waiter count cannot be represented on this target.
    #[error("gateway concurrency max_queued is outside the supported range")]
    InvalidQueueLimit,
    /// A positive finite queue timeout is required when queuing is enabled.
    #[error("gateway concurrency queue_timeout must be positive when max_queued is nonzero")]
    InvalidQueueTimeout,
    /// Every bounded queue slot is already occupied.
    #[error("gateway delivery queue is full")]
    QueueFull,
    /// The request did not obtain a delivery slot before its deadline.
    #[error("gateway delivery queue wait timed out")]
    QueueTimedOut,
    /// The internal admission primitive was unexpectedly closed.
    #[error("gateway delivery admission is unavailable")]
    Unavailable,
    /// The fleet-wide delivery lease limit is exhausted.
    #[error("gateway fleet delivery limit is exhausted")]
    SharedLimit,
}

/// One admitted upstream delivery. Dropping it releases capacity.
#[derive(Debug)]
pub struct AdmissionPermit {
    _permit: OwnedSemaphorePermit,
    waited: Duration,
}

impl AdmissionPermit {
    /// Time spent waiting for an in-flight slot.
    #[must_use]
    pub const fn waited(&self) -> Duration {
        self.waited
    }
}

/// Process-local in-flight and bounded-waiting limits.
#[derive(Clone, Debug)]
pub struct AdmissionPolicy {
    in_flight: Arc<Semaphore>,
    queue: Arc<Semaphore>,
    max_in_flight: usize,
    max_queued: usize,
    queue_timeout: Duration,
}

impl AdmissionPolicy {
    /// Construct validated admission state.
    pub fn new(
        max_in_flight: usize,
        max_queued: usize,
        queue_timeout: Duration,
    ) -> Result<Self, AdmissionError> {
        if max_in_flight == 0 || max_in_flight > Semaphore::MAX_PERMITS {
            return Err(AdmissionError::InvalidInFlightLimit);
        }
        if max_queued > Semaphore::MAX_PERMITS {
            return Err(AdmissionError::InvalidQueueLimit);
        }
        if max_queued > 0 && queue_timeout.is_zero() {
            return Err(AdmissionError::InvalidQueueTimeout);
        }
        Ok(Self {
            in_flight: Arc::new(Semaphore::new(max_in_flight)),
            queue: Arc::new(Semaphore::new(max_queued)),
            max_in_flight,
            max_queued,
            queue_timeout,
        })
    }

    /// Admit immediately, wait in the bounded queue, or return a stable overload reason.
    pub async fn acquire(&self) -> Result<AdmissionPermit, AdmissionError> {
        if let Ok(permit) = Arc::clone(&self.in_flight).try_acquire_owned() {
            return Ok(AdmissionPermit {
                _permit: permit,
                waited: Duration::ZERO,
            });
        }
        let queue_permit = Arc::clone(&self.queue)
            .try_acquire_owned()
            .map_err(|_| AdmissionError::QueueFull)?;
        let started = Instant::now();
        let permit = tokio::time::timeout(
            self.queue_timeout,
            Arc::clone(&self.in_flight).acquire_owned(),
        )
        .await
        .map_err(|_| AdmissionError::QueueTimedOut)?
        .map_err(|_| AdmissionError::Unavailable)?;
        drop(queue_permit);
        Ok(AdmissionPermit {
            _permit: permit,
            waited: started.elapsed(),
        })
    }

    /// Configured simultaneous delivery limit.
    #[must_use]
    pub const fn max_in_flight(&self) -> usize {
        self.max_in_flight
    }

    /// Configured bounded waiter count.
    #[must_use]
    pub const fn max_queued(&self) -> usize {
        self.max_queued
    }

    /// Maximum bounded queue wait.
    #[must_use]
    pub const fn queue_timeout(&self) -> Duration {
        self.queue_timeout
    }

    /// Current delivery count, sampled without blocking.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.max_in_flight
            .saturating_sub(self.in_flight.available_permits())
    }

    /// Current bounded waiter count, sampled without blocking.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.max_queued
            .saturating_sub(self.queue.available_permits())
    }
}

impl Default for AdmissionPolicy {
    fn default() -> Self {
        Self {
            in_flight: Arc::new(Semaphore::new(DEFAULT_MAX_IN_FLIGHT)),
            queue: Arc::new(Semaphore::new(DEFAULT_MAX_QUEUED)),
            max_in_flight: DEFAULT_MAX_IN_FLIGHT,
            max_queued: DEFAULT_MAX_QUEUED,
            queue_timeout: DEFAULT_QUEUE_TIMEOUT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn queue_is_bounded_and_capacity_returns_on_drop() -> Result<(), AdmissionError> {
        let policy = AdmissionPolicy::new(1, 1, Duration::from_secs(1))?;
        let active = policy.acquire().await?;
        let waiting_policy = policy.clone();
        let waiting = tokio::spawn(async move { waiting_policy.acquire().await });
        while policy.queued() != 1 {
            tokio::task::yield_now().await;
        }
        assert!(matches!(
            policy.acquire().await,
            Err(AdmissionError::QueueFull)
        ));
        drop(active);
        let admitted = waiting.await.map_err(|_| AdmissionError::Unavailable)??;
        assert!(admitted.waited() > Duration::ZERO);
        drop(admitted);
        assert_eq!(policy.in_flight(), 0);
        assert_eq!(policy.queued(), 0);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn queued_request_times_out_deterministically() -> Result<(), AdmissionError> {
        let policy = AdmissionPolicy::new(1, 1, Duration::from_secs(2))?;
        let _active = policy.acquire().await?;
        let waiting_policy = policy.clone();
        let waiting = tokio::spawn(async move { waiting_policy.acquire().await });
        while policy.queued() != 1 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(matches!(
            waiting.await.map_err(|_| AdmissionError::Unavailable)?,
            Err(AdmissionError::QueueTimedOut)
        ));
        Ok(())
    }

    #[test]
    fn invalid_bounds_fail_closed() {
        assert!(matches!(
            AdmissionPolicy::new(0, 0, Duration::ZERO),
            Err(AdmissionError::InvalidInFlightLimit)
        ));
        assert!(matches!(
            AdmissionPolicy::new(1, 1, Duration::ZERO),
            Err(AdmissionError::InvalidQueueTimeout)
        ));
        assert!(matches!(
            AdmissionPolicy::new(usize::MAX, 0, Duration::from_secs(1)),
            Err(AdmissionError::InvalidInFlightLimit)
        ));
        assert!(matches!(
            AdmissionPolicy::new(1, usize::MAX, Duration::from_secs(1)),
            Err(AdmissionError::InvalidQueueLimit)
        ));
    }
}
