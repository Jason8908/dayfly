use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::StatusCode;
use reqwest::header::HeaderMap;

const RETRY_AFTER: &str = "retry-after";
const RATE_LIMIT_REMAINING: &str = "x-ratelimit-remaining";
const RATE_LIMIT_RESET: &str = "x-ratelimit-reset";
// GitHub marks a secondary rate limit only in the message text, and asks for at least a
// minute's wait when it names none: https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api
const SECONDARY_LIMIT_PHRASE: &str = "secondary rate";
const THROTTLE_FLOOR: Duration = Duration::from_secs(60);
const MAX_RETRIES: u32 = 3;
const MAX_WAIT: Duration = Duration::from_mins(5);
const TRANSIENT_BASE: Duration = Duration::from_secs(1);
const MAX_JITTER: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Policy {
    pub(super) max_retries: u32,
    pub(super) max_wait: Duration,
    pub(super) transient_base: Duration,
    pub(super) throttle_floor: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_retries: MAX_RETRIES,
            max_wait: MAX_WAIT,
            transient_base: TRANSIENT_BASE,
            throttle_floor: THROTTLE_FLOOR,
        }
    }
}

impl Policy {
    pub(super) fn wait(&self, cause: Cause, retries: u32, now: SystemTime) -> Option<Duration> {
        if retries >= self.max_retries {
            return None;
        }
        let backoff = 2_u32.saturating_pow(retries);
        let wait = match cause {
            Cause::Throttled(Throttle::RetryAfter(wait)) => wait,
            Cause::Throttled(Throttle::Until(reset)) => {
                reset.duration_since(now).unwrap_or_default()
            }
            Cause::Throttled(Throttle::Backoff) => self.throttle_floor.saturating_mul(backoff),
            Cause::Transient => self.transient_base.saturating_mul(backoff),
        };
        (wait <= self.max_wait).then_some(wait)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Cause {
    Throttled(Throttle),
    Transient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Throttle {
    RetryAfter(Duration),
    Until(SystemTime),
    Backoff,
}

pub(super) fn throttle(status: StatusCode, headers: &HeaderMap, message: &str) -> Option<Throttle> {
    if status != StatusCode::FORBIDDEN && status != StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    if let Some(seconds) = header_u64(headers, RETRY_AFTER) {
        return Some(Throttle::RetryAfter(Duration::from_secs(seconds)));
    }
    if header_u64(headers, RATE_LIMIT_REMAINING) == Some(0) {
        let reset = header_u64(headers, RATE_LIMIT_RESET)
            .and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds)));
        return Some(reset.map_or(Throttle::Backoff, Throttle::Until));
    }
    let secondary = status == StatusCode::TOO_MANY_REQUESTS
        || message
            .to_ascii_lowercase()
            .contains(SECONDARY_LIMIT_PHRASE);
    secondary.then_some(Throttle::Backoff)
}

pub(super) fn jitter(wait: Duration) -> Duration {
    wait + wait.mul_f64(fastrand::f64() * MAX_JITTER)
}

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use reqwest::header::HeaderValue;

    use super::*;

    #[expect(
        clippy::duration_suboptimal_units,
        reason = "the reset headers in these tests are Unix timestamps in seconds"
    )]
    const NOW: Duration = Duration::from_secs(1_800_000_000);

    fn now() -> SystemTime {
        UNIX_EPOCH + NOW
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        pairs
            .iter()
            .map(|&(name, value)| (name.parse().unwrap(), HeaderValue::from_str(value).unwrap()))
            .collect()
    }

    fn waits(policy: &Policy, cause: Cause) -> Vec<Option<Duration>> {
        (0..=policy.max_retries)
            .map(|retries| policy.wait(cause, retries, now()))
            .collect()
    }

    #[test]
    fn retry_after_is_preferred_over_the_rate_limit_reset() {
        let headers = headers(&[
            ("retry-after", "7"),
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1800000030"),
        ]);

        assert_eq!(
            throttle(StatusCode::FORBIDDEN, &headers, ""),
            Some(Throttle::RetryAfter(Duration::from_secs(7)))
        );
    }

    #[test]
    fn exhausted_primary_limit_waits_for_the_reset() {
        let headers = headers(&[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1800000030"),
        ]);

        assert_eq!(
            throttle(StatusCode::TOO_MANY_REQUESTS, &headers, ""),
            Some(Throttle::Until(now() + Duration::from_secs(30)))
        );
    }

    #[test]
    fn exhausted_primary_limit_without_a_usable_reset_backs_off() {
        for reset in [None, Some("soon"), Some("18446744073709551615")] {
            let mut pairs = vec![("x-ratelimit-remaining", "0")];
            pairs.extend(reset.map(|reset| ("x-ratelimit-reset", reset)));

            assert_eq!(
                throttle(StatusCode::FORBIDDEN, &headers(&pairs), ""),
                Some(Throttle::Backoff),
                "{reset:?}"
            );
        }
    }

    #[test]
    fn secondary_limit_without_headers_backs_off() {
        assert_eq!(
            throttle(
                StatusCode::FORBIDDEN,
                &HeaderMap::new(),
                "You have exceeded a Secondary Rate Limit. Please wait a few minutes."
            ),
            Some(Throttle::Backoff)
        );
        assert_eq!(
            throttle(StatusCode::TOO_MANY_REQUESTS, &HeaderMap::new(), ""),
            Some(Throttle::Backoff)
        );
    }

    #[test]
    fn permission_denied_is_not_a_rate_limit() {
        let headers = headers(&[("x-ratelimit-remaining", "4999")]);

        assert_eq!(
            throttle(
                StatusCode::FORBIDDEN,
                &headers,
                "Resource not accessible by personal access token"
            ),
            None
        );
    }

    #[test]
    fn rate_limit_headers_on_other_statuses_are_ignored() {
        let headers = headers(&[("retry-after", "5"), ("x-ratelimit-remaining", "0")]);

        for status in [StatusCode::NOT_FOUND, StatusCode::SERVICE_UNAVAILABLE] {
            assert_eq!(
                throttle(status, &headers, "secondary rate limit"),
                None,
                "{status}"
            );
        }
    }

    #[test]
    fn retry_after_is_honoured_on_every_retry() {
        let after = Duration::from_secs(7);

        assert_eq!(
            waits(
                &Policy::default(),
                Cause::Throttled(Throttle::RetryAfter(after))
            ),
            [Some(after), Some(after), Some(after), None]
        );
    }

    #[test]
    fn reset_is_waited_for_and_a_past_reset_retries_at_once() {
        let policy = Policy::default();
        let wait = |reset| policy.wait(Cause::Throttled(Throttle::Until(reset)), 0, now());

        assert_eq!(
            wait(now() + Duration::from_secs(30)),
            Some(Duration::from_secs(30))
        );
        assert_eq!(wait(now() - Duration::from_secs(30)), Some(Duration::ZERO));
    }

    #[test]
    fn throttle_backoff_starts_at_a_minute_and_doubles() {
        assert_eq!(
            waits(&Policy::default(), Cause::Throttled(Throttle::Backoff)),
            [
                Some(Duration::from_secs(60)),
                Some(Duration::from_secs(120)),
                Some(Duration::from_secs(240)),
                None,
            ]
        );
    }

    #[test]
    fn transient_backoff_doubles_from_one_second() {
        assert_eq!(
            waits(&Policy::default(), Cause::Transient),
            [
                Some(Duration::from_secs(1)),
                Some(Duration::from_secs(2)),
                Some(Duration::from_secs(4)),
                None,
            ]
        );
    }

    #[test]
    fn waits_beyond_the_cap_give_up_instead_of_retrying_early() {
        let policy = Policy::default();
        let wait = |cause| policy.wait(cause, 0, now());

        assert_eq!(
            wait(Cause::Throttled(Throttle::RetryAfter(MAX_WAIT))),
            Some(MAX_WAIT)
        );
        assert_eq!(
            wait(Cause::Throttled(Throttle::RetryAfter(
                MAX_WAIT + Duration::from_secs(1)
            ))),
            None
        );
        assert_eq!(
            wait(Cause::Throttled(Throttle::Until(
                now() + Duration::from_secs(3600)
            ))),
            None
        );
    }

    #[test]
    fn backoff_saturates_instead_of_overflowing() {
        let policy = Policy {
            max_retries: u32::MAX,
            max_wait: Duration::MAX,
            ..Policy::default()
        };

        assert_eq!(
            policy.wait(Cause::Transient, 64, now()),
            Some(TRANSIENT_BASE.saturating_mul(u32::MAX))
        );
    }

    #[test]
    fn jitter_only_lengthens_the_wait_by_up_to_half() {
        let wait = Duration::from_secs(60);

        for _ in 0..1000 {
            let jittered = jitter(wait);
            assert!(
                wait <= jittered && jittered <= wait.mul_f64(1.0 + MAX_JITTER),
                "{jittered:?}"
            );
        }
        assert_eq!(jitter(Duration::ZERO), Duration::ZERO);
    }
}
