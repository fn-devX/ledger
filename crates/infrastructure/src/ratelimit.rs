use std::{collections::HashMap, sync::Mutex, time::Instant};

use ledger_application::{
    ports::RepoError,
    ratelimit::{RateDecision, RateFuture, RateLimit, RateLimiter, gcra},
};
use redis::{Script, aio::ConnectionManager};

const PRUNE_THRESHOLD: usize = 10_000;

#[derive(Debug)]
pub struct InMemoryRateLimiter {
    epoch: Instant,
    buckets: Mutex<HashMap<String, u64>>,
}

impl Default for InMemoryRateLimiter {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            buckets: Mutex::default(),
        }
    }
}

impl InMemoryRateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_now(&self, key: &str, limit: RateLimit) -> RateDecision {
        let now = u64::try_from(self.epoch.elapsed().as_micros()).unwrap_or(u64::MAX);
        let mut buckets = self
            .buckets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if buckets.len() > PRUNE_THRESHOLD {
            buckets.retain(|_, tat| *tat > now);
        }
        let (decision, new_tat) = gcra(&limit, buckets.get(key).copied(), now);
        if let Some(tat) = new_tat {
            buckets.insert(key.to_owned(), tat);
        }
        decision
    }
}

impl RateLimiter for InMemoryRateLimiter {
    fn check<'a>(&'a self, key: &'a str, limit: RateLimit) -> RateFuture<'a> {
        let decision = self.check_now(key, limit);
        Box::pin(async move { Ok(decision) })
    }
}

const GCRA_LUA: &str = r"
local t = redis.call('TIME')
local now = tonumber(t[1]) * 1000000 + tonumber(t[2])
local interval = tonumber(ARGV[1])
local capacity = interval * tonumber(ARGV[2])
local tat = tonumber(redis.call('GET', KEYS[1]) or now)
if tat < now then tat = now end
local new_tat = tat + interval
local used = new_tat - now
if used > capacity then
  return {0, used - capacity, tat - now}
end
redis.call('SET', KEYS[1], string.format('%d', new_tat), 'PX', math.ceil(used / 1000))
return {1, 0, used}
";

#[derive(Clone)]
pub struct RedisRateLimiter {
    conn: ConnectionManager,
    script: Script,
    prefix: String,
}

impl std::fmt::Debug for RedisRateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisRateLimiter")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

fn storage(e: redis::RedisError) -> RepoError {
    RepoError::Storage(Box::new(e))
}

impl RedisRateLimiter {
    pub async fn connect(url: &str, prefix: &str) -> Result<Self, RepoError> {
        let client = redis::Client::open(url).map_err(storage)?;
        let conn = ConnectionManager::new(client).await.map_err(storage)?;
        Ok(Self {
            conn,
            script: Script::new(GCRA_LUA),
            prefix: prefix.to_owned(),
        })
    }
}

impl RateLimiter for RedisRateLimiter {
    fn check<'a>(&'a self, key: &'a str, limit: RateLimit) -> RateFuture<'a> {
        Box::pin(async move {
            let mut conn = self.conn.clone();
            let (allowed, retry_after, used): (i64, i64, i64) = self
                .script
                .key(format!("{}{key}", self.prefix))
                .arg(limit.emission_interval_us())
                .arg(limit.burst)
                .invoke_async(&mut conn)
                .await
                .map_err(storage)?;
            Ok(RateDecision::from_raw(
                &limit,
                allowed == 1,
                u64::try_from(retry_after).unwrap_or(0),
                u64::try_from(used).unwrap_or(0),
            ))
        })
    }
}
