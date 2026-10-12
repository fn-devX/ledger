use std::{sync::Arc, time::Duration};

use ledger_application::ratelimit::{RateLimit, RateLimiter};
use ledger_infrastructure::{InMemoryRateLimiter, RedisRateLimiter};
use uuid::Uuid;

fn key() -> String {
    Uuid::now_v7().to_string()
}

async fn burst_then_throttle(limiter: Arc<dyn RateLimiter>) {
    let limit = RateLimit::per_minute(3);
    let k = key();
    for expected in [2, 1, 0] {
        let d = limiter.check(&k, limit).await.unwrap();
        assert!(d.allowed);
        assert_eq!(d.limit, 3);
        assert_eq!(d.remaining, expected);
    }
    let denied = limiter.check(&k, limit).await.unwrap();
    assert!(!denied.allowed);
    assert_eq!(denied.remaining, 0);
    assert!(
        denied.retry_after > Duration::from_secs(19)
            && denied.retry_after <= Duration::from_secs(20)
    );
    assert!(
        denied.reset_after > Duration::from_secs(59)
            && denied.reset_after <= Duration::from_secs(60)
    );

    let other = limiter.check(&key(), limit).await.unwrap();
    assert!(other.allowed, "keys are independent");
}

async fn atomic_under_concurrency(limiter: Arc<dyn RateLimiter>) {
    let limit = RateLimit::per_minute(10);
    let k = Arc::new(key());
    let tasks: Vec<_> = (0..50)
        .map(|_| {
            let (limiter, k) = (limiter.clone(), k.clone());
            tokio::spawn(async move { limiter.check(&k, limit).await.unwrap().allowed })
        })
        .collect();
    let mut allowed = 0;
    for t in tasks {
        allowed += usize::from(t.await.unwrap());
    }
    assert_eq!(allowed, 10, "exactly the burst must pass");
}

async fn refills_over_time(limiter: Arc<dyn RateLimiter>) {
    let limit = RateLimit {
        burst: 2,
        period: Duration::from_millis(400),
    };
    let k = key();
    assert!(limiter.check(&k, limit).await.unwrap().allowed);
    assert!(limiter.check(&k, limit).await.unwrap().allowed);
    assert!(!limiter.check(&k, limit).await.unwrap().allowed);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        limiter.check(&k, limit).await.unwrap().allowed,
        "one token after one interval"
    );
}

macro_rules! contract {
    ($($name:ident),* $(,)?) => {
        mod memory {
            use super::*;
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::$name(Arc::new(InMemoryRateLimiter::new())).await;
                }
            )*
        }

        mod redis {
            use super::*;
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    let Ok(url) = std::env::var("REDIS_URL") else {
                        eprintln!("REDIS_URL not set, skipping");
                        return;
                    };
                    let limiter = RedisRateLimiter::connect(&url, "test:ratelimit:").await.unwrap();
                    super::$name(Arc::new(limiter)).await;
                }
            )*
        }
    };
}

contract!(
    burst_then_throttle,
    atomic_under_concurrency,
    refills_over_time
);
