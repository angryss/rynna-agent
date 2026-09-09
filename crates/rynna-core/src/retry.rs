//! Shared, pure backoff policy for model-provider adapters.
//!
//! The core performs no network I/O, so this module only computes *when* a retry
//! should happen. Adapters own the sleeping and the request rebuilding.
//!
//! The total sleep budget is deliberately small. A provider call inside a workflow
//! step runs under the aggregate 300-second tool-loop deadline and under the step's
//! reserved `active_seconds` slice, so backoff competes with real work. When a
//! provider asks for a longer wait than the budget allows we stop retrying and let
//! the caller park the run as resumable instead of sleeping through its allowance.

use std::time::Duration;

use crate::ProviderError;

/// Total number of attempts, including the first. Two retries.
pub const MAX_ATTEMPTS: u32 = 3;
/// Delay before the first retry; doubles thereafter.
pub const BASE_DELAY: Duration = Duration::from_millis(500);
/// Ceiling for any single sleep.
pub const MAX_DELAY: Duration = Duration::from_secs(4);
/// Ceiling for the sum of all sleeps within one provider call.
pub const MAX_TOTAL_DELAY: Duration = Duration::from_secs(5);

/// Tracks retry state across attempts of a single logical request.
#[derive(Debug, Default)]
pub struct Backoff {
    attempt: u32,
    slept: Duration,
}

impl Backoff {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attempts already made.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Records that an attempt was made and returns how long to wait before the
    /// next one, or `None` when the caller should give up and surface `error`.
    ///
    /// Returns `None` for non-transient errors, once attempts are exhausted, and
    /// when honouring the delay would exceed the total budget.
    pub fn next_delay(&mut self, error: &ProviderError) -> Option<Duration> {
        self.attempt += 1;
        if !error.is_transient() || self.attempt >= MAX_ATTEMPTS {
            return None;
        }
        let exponential = BASE_DELAY
            .checked_mul(1 << (self.attempt - 1))
            .unwrap_or(MAX_DELAY)
            .min(MAX_DELAY);
        // A server-supplied Retry-After wins when it is longer, so we never retry
        // sooner than asked; if it does not fit the budget we stop instead.
        let delay = error.retry_after().unwrap_or(exponential).max(exponential);
        if delay > MAX_DELAY || self.slept + delay > MAX_TOTAL_DELAY {
            return None;
        }
        self.slept += delay;
        Some(delay)
    }
}

/// Parses a `Retry-After` header value. Supports the delay-seconds form only;
/// the HTTP-date form is treated as absent because it is not used by the
/// providers Rynna targets and would need a clock to interpret safely.
pub fn parse_retry_after(value: Option<&str>) -> Option<Duration> {
    let seconds: u64 = value?.trim().parse().ok()?;
    Some(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderErrorKind;

    fn transient() -> ProviderError {
        ProviderError::new("overloaded").with_kind(ProviderErrorKind::Transient)
    }

    #[test]
    fn permanent_errors_are_never_retried() {
        let mut backoff = Backoff::new();
        assert_eq!(backoff.next_delay(&ProviderError::new("bad request")), None);
    }

    #[test]
    fn auth_errors_are_never_retried() {
        let mut backoff = Backoff::new();
        let error = ProviderError::new("unauthorized").with_status(401);
        assert_eq!(error.kind(), ProviderErrorKind::Auth);
        assert_eq!(backoff.next_delay(&error), None);
    }

    #[test]
    fn transient_errors_back_off_exponentially_then_stop() {
        let mut backoff = Backoff::new();
        assert_eq!(backoff.next_delay(&transient()), Some(BASE_DELAY));
        assert_eq!(backoff.next_delay(&transient()), Some(BASE_DELAY * 2));
        // Third call is the final attempt, so no further delay is offered.
        assert_eq!(backoff.next_delay(&transient()), None);
        assert_eq!(backoff.attempt(), MAX_ATTEMPTS);
    }

    #[test]
    fn total_sleep_stays_within_budget() {
        let mut backoff = Backoff::new();
        let mut total = Duration::ZERO;
        while let Some(delay) = backoff.next_delay(&transient()) {
            total += delay;
        }
        assert!(total <= MAX_TOTAL_DELAY, "slept {total:?}");
    }

    #[test]
    fn retry_after_is_honoured_when_it_fits() {
        let mut backoff = Backoff::new();
        let error = transient().with_retry_after(Some(Duration::from_secs(2)));
        assert_eq!(backoff.next_delay(&error), Some(Duration::from_secs(2)));
    }

    #[test]
    fn retry_after_longer_than_the_cap_stops_retrying() {
        // Sleeping 60s would burn the step's whole allowance; parking is better.
        let mut backoff = Backoff::new();
        let error = transient().with_retry_after(Some(Duration::from_secs(60)));
        assert_eq!(backoff.next_delay(&error), None);
    }

    #[test]
    fn retry_after_never_shortens_the_exponential_delay() {
        let mut backoff = Backoff::new();
        backoff.next_delay(&transient());
        // Exponential is now 1s; a 0s Retry-After must not undercut it.
        let error = transient().with_retry_after(Some(Duration::ZERO));
        assert_eq!(backoff.next_delay(&error), Some(BASE_DELAY * 2));
    }

    #[test]
    fn parses_delay_seconds_and_ignores_http_dates() {
        assert_eq!(parse_retry_after(Some("20")), Some(Duration::from_secs(20)));
        assert_eq!(parse_retry_after(Some(" 3 ")), Some(Duration::from_secs(3)));
        assert_eq!(
            parse_retry_after(Some("Wed, 21 Oct 2026 07:28:00 GMT")),
            None
        );
        assert_eq!(parse_retry_after(None), None);
    }

    #[test]
    fn classifies_rate_limit_and_overload_as_transient() {
        for status in [408, 429, 500, 502, 503, 504, 529] {
            assert_eq!(
                crate::classify_status(status),
                ProviderErrorKind::Transient,
                "status {status}"
            );
        }
        for status in [400, 404, 422] {
            assert_eq!(
                crate::classify_status(status),
                ProviderErrorKind::Permanent,
                "status {status}"
            );
        }
    }
}
