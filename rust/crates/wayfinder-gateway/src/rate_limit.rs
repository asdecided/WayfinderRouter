//! Deterministic fixed-window RPM/TPM limiting.
//!
//! Admission reserves one request and, when supplied, a conservative token
//! budget before provider delivery. Completed requests reconcile the budget
//! against observed usage in the same fixed window. The caller supplies
//! monotonic seconds so tests never sleep.

use std::sync::Mutex;

use thiserror::Error;

/// Default fixed window in seconds.
pub const DEFAULT_WINDOW_SECONDS: f64 = 60.0;

/// The dimension that rejected an admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    /// Requests per window.
    Requests,
    /// Upstream tokens per window.
    Tokens,
}

impl LimitKind {
    /// Header/metric compatibility label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requests => "rpm",
            Self::Tokens => "tpm",
        }
    }
}

/// Result of one request admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateResult {
    /// `None` when admitted, otherwise the tripped dimension.
    pub limited_by: Option<LimitKind>,
    /// Whole seconds until the current window rolls; zero when admitted.
    pub retry_after_seconds: u64,
}

/// Exact fixed-window reservation returned by a successful admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateReservation {
    pub(crate) window_id: u64,
    pub(crate) reserved_tokens: u64,
    pub(crate) reserved_request: bool,
}

impl RateReservation {
    /// Fixed-window identifier captured at admission.
    #[must_use]
    pub const fn window_id(self) -> u64 {
        self.window_id
    }

    /// Conservative token budget charged at admission.
    #[must_use]
    pub const fn reserved_tokens(self) -> u64 {
        self.reserved_tokens
    }

    /// Whether this reservation also owns one request slot.
    #[must_use]
    pub const fn reserved_request(self) -> bool {
        self.reserved_request
    }
}

/// Admission result plus the reservation needed for later reconciliation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReservationResult {
    /// Rate-limit decision.
    pub rate: RateResult,
    /// Present only when the request and token budget were reserved.
    pub reservation: Option<RateReservation>,
}

impl RateResult {
    /// Whether the request slot was reserved.
    #[must_use]
    pub const fn allowed(self) -> bool {
        self.limited_by.is_none()
    }
}

/// Request-rate response-header snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateSnapshot {
    /// Configured request limit.
    pub limit: u64,
    /// Remaining request slots after prior admissions.
    pub remaining: u64,
    /// Whole seconds until reset.
    pub reset_seconds: u64,
}

/// Current counters for diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateStats {
    /// Admitted requests in the current state window.
    pub requests: u64,
    /// Recorded upstream tokens in the current state window.
    pub tokens: u64,
}

/// Invalid construction or poisoned synchronization state.
#[derive(Debug, Error, PartialEq)]
pub enum RateLimitError {
    /// The window must be finite and positive.
    #[error("rate-limit window must be finite and positive")]
    InvalidWindow,
    /// The supplied monotonic time must be finite and non-negative.
    #[error("rate-limit time must be finite and non-negative")]
    InvalidTime,
    /// Internal state could not be synchronized.
    #[error("rate-limit state lock is unavailable")]
    LockPoisoned,
}

#[derive(Clone, Copy, Debug)]
struct Configuration {
    rpm: Option<u64>,
    tpm: Option<u64>,
    window_seconds: f64,
}

#[derive(Clone, Copy, Debug)]
struct State {
    configuration: Configuration,
    window_id: Option<u64>,
    requests: u64,
    tokens: u64,
}

/// Thread-safe fixed-window request/token limiter.
#[derive(Debug)]
pub struct RateLimiter {
    state: Mutex<State>,
}

impl RateLimiter {
    /// Construct a limiter. `None` disables the corresponding dimension.
    pub fn new(
        rpm: Option<u64>,
        tpm: Option<u64>,
        window_seconds: f64,
    ) -> Result<Self, RateLimitError> {
        let configuration = validated_configuration(rpm, tpm, window_seconds)?;
        Ok(Self {
            state: Mutex::new(State {
                configuration,
                window_id: None,
                requests: 0,
                tokens: 0,
            }),
        })
    }

    /// Whether either dimension is configured.
    pub fn active(&self) -> Result<bool, RateLimitError> {
        let state = self
            .state
            .lock()
            .map_err(|_| RateLimitError::LockPoisoned)?;
        Ok(state.configuration.rpm.is_some() || state.configuration.tpm.is_some())
    }

    /// Whether this limiter enforces a token ceiling.
    pub fn tokens_active(&self) -> Result<bool, RateLimitError> {
        let state = self
            .state
            .lock()
            .map_err(|_| RateLimitError::LockPoisoned)?;
        Ok(state.configuration.tpm.is_some())
    }

    /// Admit and reserve one request at the supplied monotonic time.
    pub fn admit_at(&self, now_seconds: f64) -> Result<RateResult, RateLimitError> {
        Ok(self.reserve_at(0, now_seconds)?.rate)
    }

    /// Atomically reserve one request and a conservative token budget.
    pub fn reserve_at(
        &self,
        tokens: u64,
        now_seconds: f64,
    ) -> Result<ReservationResult, RateLimitError> {
        self.reserve_inner(tokens, true, now_seconds)
    }

    /// Reserve only a token budget after RPM admission already succeeded.
    pub fn reserve_tokens_at(
        &self,
        tokens: u64,
        now_seconds: f64,
    ) -> Result<ReservationResult, RateLimitError> {
        self.reserve_inner(tokens, false, now_seconds)
    }

    fn reserve_inner(
        &self,
        tokens: u64,
        reserve_request: bool,
        now_seconds: f64,
    ) -> Result<ReservationResult, RateLimitError> {
        validate_time(now_seconds)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| RateLimitError::LockPoisoned)?;
        if (!reserve_request || state.configuration.rpm.is_none())
            && state.configuration.tpm.is_none()
        {
            return Ok(ReservationResult {
                rate: RateResult {
                    limited_by: None,
                    retry_after_seconds: 0,
                },
                reservation: None,
            });
        }
        roll(&mut state, now_seconds)?;
        if reserve_request
            && state
                .configuration
                .rpm
                .is_some_and(|limit| state.requests >= limit)
        {
            return Ok(ReservationResult {
                rate: rejected(LimitKind::Requests, &state, now_seconds),
                reservation: None,
            });
        }
        if state.configuration.tpm.is_some_and(|limit| {
            if tokens == 0 {
                state.tokens >= limit
            } else {
                state.tokens.saturating_add(tokens) > limit
            }
        }) {
            return Ok(ReservationResult {
                rate: rejected(LimitKind::Tokens, &state, now_seconds),
                reservation: None,
            });
        }
        if reserve_request {
            state.requests = state.requests.saturating_add(1);
        }
        state.tokens = state.tokens.saturating_add(tokens);
        let window_id = state.window_id.ok_or(RateLimitError::InvalidTime)?;
        Ok(ReservationResult {
            rate: RateResult {
                limited_by: None,
                retry_after_seconds: 0,
            },
            reservation: Some(RateReservation {
                window_id,
                reserved_tokens: tokens,
                reserved_request: reserve_request,
            }),
        })
    }

    /// Reconcile observed usage only when the original fixed window is active.
    ///
    /// Returns `false` after rollover so a late completion cannot charge a new
    /// window. Callers retain the original conservative reservation in that
    /// case.
    pub fn reconcile_at(
        &self,
        reservation: RateReservation,
        actual_tokens: u64,
        now_seconds: f64,
    ) -> Result<bool, RateLimitError> {
        validate_time(now_seconds)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| RateLimitError::LockPoisoned)?;
        if state.window_id != Some(reservation.window_id) {
            return Ok(false);
        }
        if actual_tokens >= reservation.reserved_tokens {
            state.tokens = state
                .tokens
                .saturating_add(actual_tokens - reservation.reserved_tokens);
        } else {
            state.tokens = state
                .tokens
                .saturating_sub(reservation.reserved_tokens - actual_tokens);
        }
        Ok(true)
    }

    /// Roll back a failed multi-scope admission in the original fixed window.
    pub fn rollback_at(
        &self,
        reservation: RateReservation,
        now_seconds: f64,
    ) -> Result<bool, RateLimitError> {
        validate_time(now_seconds)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| RateLimitError::LockPoisoned)?;
        if state.window_id != Some(reservation.window_id) {
            return Ok(false);
        }
        if reservation.reserved_request {
            state.requests = state.requests.saturating_sub(1);
        }
        state.tokens = state.tokens.saturating_sub(reservation.reserved_tokens);
        Ok(true)
    }

    /// Add non-negative upstream token usage to the active window.
    pub fn add_tokens_at(&self, tokens: u64, now_seconds: f64) -> Result<(), RateLimitError> {
        validate_time(now_seconds)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| RateLimitError::LockPoisoned)?;
        if state.configuration.tpm.is_none() {
            return Ok(());
        }
        roll(&mut state, now_seconds)?;
        state.tokens = state.tokens.saturating_add(tokens);
        Ok(())
    }

    /// Apply hot-reloaded limits while retaining current-window counters.
    pub fn reconfigure(
        &self,
        rpm: Option<u64>,
        tpm: Option<u64>,
        window_seconds: f64,
    ) -> Result<(), RateLimitError> {
        let configuration = validated_configuration(rpm, tpm, window_seconds)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| RateLimitError::LockPoisoned)?;
        state.configuration = configuration;
        Ok(())
    }

    /// Current RPM header fields, or `None` when RPM is disabled.
    pub fn snapshot_at(&self, now_seconds: f64) -> Result<Option<RateSnapshot>, RateLimitError> {
        validate_time(now_seconds)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| RateLimitError::LockPoisoned)?;
        let Some(limit) = state.configuration.rpm else {
            return Ok(None);
        };
        roll(&mut state, now_seconds)?;
        Ok(Some(RateSnapshot {
            limit,
            remaining: limit.saturating_sub(state.requests),
            reset_seconds: retry_after(&state, now_seconds),
        }))
    }

    /// Current counters without rolling the clock.
    pub fn stats(&self) -> Result<RateStats, RateLimitError> {
        let state = self
            .state
            .lock()
            .map_err(|_| RateLimitError::LockPoisoned)?;
        Ok(RateStats {
            requests: state.requests,
            tokens: state.tokens,
        })
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                configuration: Configuration {
                    rpm: None,
                    tpm: None,
                    window_seconds: DEFAULT_WINDOW_SECONDS,
                },
                window_id: None,
                requests: 0,
                tokens: 0,
            }),
        }
    }
}

fn validated_configuration(
    rpm: Option<u64>,
    tpm: Option<u64>,
    window_seconds: f64,
) -> Result<Configuration, RateLimitError> {
    if !window_seconds.is_finite() || window_seconds <= 0.0 {
        return Err(RateLimitError::InvalidWindow);
    }
    Ok(Configuration {
        rpm,
        tpm,
        window_seconds,
    })
}

fn validate_time(now_seconds: f64) -> Result<(), RateLimitError> {
    if !now_seconds.is_finite() || now_seconds < 0.0 {
        return Err(RateLimitError::InvalidTime);
    }
    Ok(())
}

fn roll(state: &mut State, now_seconds: f64) -> Result<(), RateLimitError> {
    let quotient = python_floor_div_positive(now_seconds, state.configuration.window_seconds);
    if !quotient.is_finite() || quotient >= u64::MAX as f64 {
        return Err(RateLimitError::InvalidTime);
    }
    let window_id = quotient as u64;
    if Some(window_id) != state.window_id {
        state.window_id = Some(window_id);
        state.requests = 0;
        state.tokens = 0;
    }
    Ok(())
}

// CPython's float `//` is not simply `(a / b).floor()`: it derives the
// quotient from `fmod` and snaps it to the nearest integral quotient. That is
// why `1.0 // 0.1 == 9.0`. Preserve that behavior for window compatibility.
fn python_floor_div_positive(numerator: f64, denominator: f64) -> f64 {
    let modulo = numerator % denominator;
    let division = (numerator - modulo) / denominator;
    if division == 0.0 {
        return 0.0;
    }
    let floored = division.floor();
    if division - floored > 0.5 {
        floored + 1.0
    } else {
        floored
    }
}

fn rejected(kind: LimitKind, state: &State, now_seconds: f64) -> RateResult {
    RateResult {
        limited_by: Some(kind),
        retry_after_seconds: retry_after(state, now_seconds),
    }
}

fn retry_after(state: &State, now_seconds: f64) -> u64 {
    let next_window = state.window_id.unwrap_or(0).saturating_add(1);
    let next = next_window as f64 * state.configuration.window_seconds;
    let remaining = (next - now_seconds).ceil().max(1.0);
    remaining as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inert_limiter_always_allows_without_counting() -> Result<(), RateLimitError> {
        let limiter = RateLimiter::default();
        assert!(!limiter.active()?);
        assert!(limiter.admit_at(0.0)?.allowed());
        assert_eq!(
            limiter.stats()?,
            RateStats {
                requests: 0,
                tokens: 0,
            }
        );
        Ok(())
    }

    #[test]
    fn rpm_reserves_only_allowed_requests() -> Result<(), RateLimitError> {
        let limiter = RateLimiter::new(Some(2), None, 60.0)?;
        assert!(limiter.admit_at(0.0)?.allowed());
        assert!(limiter.admit_at(1.0)?.allowed());
        let rejected = limiter.admit_at(2.0)?;
        assert_eq!(rejected.limited_by, Some(LimitKind::Requests));
        assert_eq!(rejected.retry_after_seconds, 58);
        assert_eq!(limiter.stats()?.requests, 2);
        Ok(())
    }

    #[test]
    fn tpm_can_overshoot_one_completed_request() -> Result<(), RateLimitError> {
        let limiter = RateLimiter::new(None, Some(100), 60.0)?;
        assert!(limiter.admit_at(0.0)?.allowed());
        limiter.add_tokens_at(150, 1.0)?;
        let rejected = limiter.admit_at(2.0)?;
        assert_eq!(rejected.limited_by, Some(LimitKind::Tokens));
        assert_eq!(limiter.stats()?.tokens, 150);
        Ok(())
    }

    #[test]
    fn token_reservation_rejects_before_mutation() -> Result<(), RateLimitError> {
        let limiter = RateLimiter::new(Some(2), Some(100), 60.0)?;
        let admitted = limiter.reserve_at(80, 1.0)?;
        assert!(admitted.rate.allowed());
        assert_eq!(limiter.stats()?.tokens, 80);

        let rejected = limiter.reserve_at(21, 2.0)?;
        assert_eq!(rejected.rate.limited_by, Some(LimitKind::Tokens));
        assert!(rejected.reservation.is_none());
        assert_eq!(
            limiter.stats()?,
            RateStats {
                requests: 1,
                tokens: 80,
            }
        );
        Ok(())
    }

    #[test]
    fn token_reservation_reconciles_and_rolls_back_in_original_window() -> Result<(), RateLimitError>
    {
        let limiter = RateLimiter::new(Some(2), Some(100), 60.0)?;
        let first = limiter
            .reserve_at(80, 1.0)?
            .reservation
            .ok_or(RateLimitError::InvalidTime)?;
        assert!(limiter.reconcile_at(first, 20, 2.0)?);
        assert_eq!(limiter.stats()?.tokens, 20);

        let second = limiter
            .reserve_at(50, 3.0)?
            .reservation
            .ok_or(RateLimitError::InvalidTime)?;
        assert!(limiter.rollback_at(second, 4.0)?);
        assert_eq!(
            limiter.stats()?,
            RateStats {
                requests: 1,
                tokens: 20,
            }
        );
        Ok(())
    }

    #[test]
    fn late_reconciliation_never_charges_a_new_window() -> Result<(), RateLimitError> {
        let limiter = RateLimiter::new(None, Some(100), 60.0)?;
        let reservation = limiter
            .reserve_at(80, 59.0)?
            .reservation
            .ok_or(RateLimitError::InvalidTime)?;
        assert!(limiter.reserve_at(10, 60.0)?.rate.allowed());
        assert!(!limiter.reconcile_at(reservation, 100, 61.0)?);
        assert_eq!(limiter.stats()?.tokens, 10);
        Ok(())
    }

    #[test]
    fn window_boundary_resets_both_dimensions() -> Result<(), RateLimitError> {
        let limiter = RateLimiter::new(Some(1), Some(1), 60.0)?;
        assert!(limiter.admit_at(59.999)?.allowed());
        limiter.add_tokens_at(1, 59.999)?;
        assert!(limiter.admit_at(60.0)?.allowed());
        assert_eq!(
            limiter.stats()?,
            RateStats {
                requests: 1,
                tokens: 0,
            }
        );
        Ok(())
    }

    #[test]
    fn fractional_windows_match_python_float_floor_division() -> Result<(), RateLimitError> {
        let limiter = RateLimiter::new(Some(1), None, 0.1)?;
        assert!(limiter.admit_at(0.99)?.allowed());
        assert_eq!(limiter.admit_at(1.0)?.limited_by, Some(LimitKind::Requests));
        assert!(limiter.admit_at(1.01)?.allowed());
        Ok(())
    }

    #[test]
    fn unrepresentable_window_index_returns_error_instead_of_overflow() -> Result<(), RateLimitError>
    {
        let limiter = RateLimiter::new(Some(1), None, f64::MIN_POSITIVE)?;
        assert_eq!(
            limiter.admit_at(1.0).err(),
            Some(RateLimitError::InvalidTime)
        );
        Ok(())
    }

    #[test]
    fn snapshot_matches_python_headers() -> Result<(), RateLimitError> {
        let limiter = RateLimiter::new(Some(3), None, 60.0)?;
        assert!(limiter.admit_at(1.0)?.allowed());
        assert_eq!(
            limiter.snapshot_at(1.0)?,
            Some(RateSnapshot {
                limit: 3,
                remaining: 2,
                reset_seconds: 59,
            })
        );
        Ok(())
    }

    #[test]
    fn reconfigure_retains_counts_and_can_tighten_immediately() -> Result<(), RateLimitError> {
        let limiter = RateLimiter::new(Some(5), None, 60.0)?;
        assert!(limiter.admit_at(1.0)?.allowed());
        limiter.reconfigure(Some(1), None, 60.0)?;
        assert_eq!(limiter.admit_at(2.0)?.limited_by, Some(LimitKind::Requests));
        Ok(())
    }

    #[test]
    fn malformed_time_and_window_are_rejected() {
        assert_eq!(
            RateLimiter::new(Some(1), None, 0.0).err(),
            Some(RateLimitError::InvalidWindow)
        );
        let limiter = RateLimiter::default();
        assert_eq!(
            limiter.admit_at(f64::NAN).err(),
            Some(RateLimitError::InvalidTime)
        );
    }
}
