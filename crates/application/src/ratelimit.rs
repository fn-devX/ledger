use std::{future::Future, pin::Pin, time::Duration};

use crate::ports::RepoError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    pub burst: u32,
    pub period: Duration,
}

impl RateLimit {
    pub fn per_minute(burst: u32) -> Self {
        Self {
            burst: burst.max(1),
            period: Duration::from_secs(60),
        }
    }

    pub fn emission_interval_us(&self) -> u64 {
        let period = u64::try_from(self.period.as_micros()).unwrap_or(u64::MAX);
        (period / u64::from(self.burst.max(1))).max(1)
    }

    pub fn capacity_us(&self) -> u64 {
        self.emission_interval_us()
            .saturating_mul(u64::from(self.burst.max(1)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateDecision {
    pub allowed: bool,
    pub limit: u32,
    pub remaining: u32,
    pub retry_after: Duration,
    pub reset_after: Duration,
}

impl RateDecision {
    pub fn from_raw(limit: &RateLimit, allowed: bool, retry_after_us: u64, used_us: u64) -> Self {
        let interval = limit.emission_interval_us();
        let free = limit.capacity_us().saturating_sub(used_us);
        Self {
            allowed,
            limit: limit.burst,
            remaining: if allowed {
                u32::try_from(free / interval).unwrap_or(u32::MAX)
            } else {
                0
            },
            retry_after: Duration::from_micros(retry_after_us),
            reset_after: Duration::from_micros(used_us),
        }
    }
}

pub fn gcra(limit: &RateLimit, tat_us: Option<u64>, now_us: u64) -> (RateDecision, Option<u64>) {
    let interval = limit.emission_interval_us();
    let capacity = limit.capacity_us();
    let tat = tat_us.unwrap_or(now_us).max(now_us);
    let new_tat = tat + interval;
    let used = new_tat - now_us;
    if used > capacity {
        (
            RateDecision::from_raw(limit, false, used - capacity, tat - now_us),
            None,
        )
    } else {
        (RateDecision::from_raw(limit, true, 0, used), Some(new_tat))
    }
}

pub type RateFuture<'a> =
    Pin<Box<dyn Future<Output = Result<RateDecision, RepoError>> + Send + 'a>>;

pub trait RateLimiter: Send + Sync + 'static {
    fn check<'a>(&'a self, key: &'a str, limit: RateLimit) -> RateFuture<'a>;
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: u64 = 1_000_000;

    #[test]
    fn allows_a_burst_then_throttles_then_refills() {
        let limit = RateLimit::per_minute(3);
        let mut tat = None;
        let mut now = 0;
        for expected_remaining in [2, 1, 0] {
            let (d, next) = gcra(&limit, tat, now);
            assert!(d.allowed);
            assert_eq!(d.remaining, expected_remaining);
            tat = next;
        }
        let (denied, next) = gcra(&limit, tat, now);
        assert!(!denied.allowed);
        assert_eq!(next, None);
        assert_eq!(denied.retry_after, Duration::from_secs(20));

        now += 20 * SEC;
        let (d, next) = gcra(&limit, tat, now);
        assert!(d.allowed, "one token refills after one emission interval");
        assert_eq!(d.remaining, 0);
        tat = next;

        now += 60 * SEC;
        let (d, _) = gcra(&limit, tat, now);
        assert!(d.allowed);
        assert_eq!(d.remaining, 2, "idle time refills the whole burst");
    }

    #[test]
    fn denied_requests_do_not_consume_capacity() {
        let limit = RateLimit::per_minute(1);
        let (_, tat) = gcra(&limit, None, 0);
        for _ in 0..10 {
            let (d, next) = gcra(&limit, tat, SEC);
            assert!(!d.allowed);
            assert_eq!(next, None);
            assert_eq!(d.retry_after, Duration::from_secs(59));
        }
        assert!(gcra(&limit, tat, 60 * SEC).0.allowed);
    }
}
