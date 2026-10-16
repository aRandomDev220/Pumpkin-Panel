//! In-process token-bucket rate limiter for auth endpoints.
//!
//! Deliberately dependency-free (spec 12.1/12.4: "reasonable throttling",
//! "per-user and per-IP rate limits"). A process-local limiter is enough for
//! the single-node manager; the spec forbids running two replicas against one
//! database, so there is exactly one authoritative limiter.
//!
//! The limiter is not a security boundary on its own — it blunts brute-force
//! and setup spam. Callers key it by client *and* by submitted username so a
//! single username cannot be hammered from many addresses.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Hard ceiling on tracked keys; prevents unbounded memory/CPU under
/// many-distinct-key floods (XFF-spoofed client keys, random usernames).
pub const DEFAULT_MAX_KEYS: usize = 10_000;
/// Run idle-pruning at most once per this many `check` calls (bounded cost).
pub const DEFAULT_PRUNE_INTERVAL: u64 = 1024;
/// Auth attempts allowed per client/user per [`AUTH_WINDOW`].
pub const AUTH_ATTEMPTS: u32 = 5;
/// Auth rate-limit window (single source of truth for the auth endpoints).
pub const AUTH_WINDOW: Duration = Duration::from_secs(60);

/// Token bucket: `capacity` attempts, refilling steadily over `window`.
#[derive(Debug)]
pub struct RateLimiter {
    buckets: HashMap<String, Bucket>,
    capacity: f64,
    refill_per_sec: f64,
    max_keys: usize,
    /// Buckets untouched for this long are dropped during periodic pruning.
    idle: Duration,
    /// Prune at most once per this many `check_at` calls (bounded cost).
    prune_interval: u64,
    calls: u64,
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    /// `capacity` attempts per `window` (5 per 60s for auth).
    pub fn new(capacity: u32, window: Duration) -> Self {
        Self::with_limits(capacity, window, DEFAULT_MAX_KEYS)
    }

    /// Like [`new`], with an explicit bucket-map cap (used by tests).
    pub fn with_limits(capacity: u32, window: Duration, max_keys: usize) -> Self {
        let capacity = capacity.max(1) as f64;
        let window_secs = window.as_secs_f64().max(1.0);
        Self {
            buckets: HashMap::new(),
            capacity,
            refill_per_sec: capacity / window_secs,
            max_keys: max_keys.max(1),
            idle: window.saturating_mul(10),
            prune_interval: DEFAULT_PRUNE_INTERVAL,
            calls: 0,
        }
    }

    /// Number of tracked keys (test/diagnostic seam).
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    /// Whether no keys are tracked.
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    /// Consume one token for `key`. `Ok(())` when allowed, otherwise
    /// `Err(retry_after_secs)` — the whole seconds until a token is available.
    pub fn check(&mut self, key: &str) -> Result<(), u64> {
        self.check_at(key, Instant::now())
    }

    fn check_at(&mut self, key: &str, now: Instant) -> Result<(), u64> {
        // Periodic, bounded pruning — never a full-map scan per request.
        self.calls = self.calls.wrapping_add(1);
        if self.prune_interval != 0 && self.calls.is_multiple_of(self.prune_interval) {
            self.prune(self.idle, now);
        }

        if !self.buckets.contains_key(key) {
            // Hard cap: evict one arbitrary bucket before inserting a new key,
            // so a many-distinct-key flood cannot grow memory without bound.
            if self.buckets.len() >= self.max_keys {
                if let Some(victim) = self.buckets.keys().next().cloned() {
                    self.buckets.remove(&victim);
                }
            }
            self.buckets.insert(
                key.to_owned(),
                Bucket {
                    tokens: self.capacity,
                    last: now,
                },
            );
        }

        let capacity = self.capacity;
        let refill_per_sec = self.refill_per_sec;
        let bucket = self.buckets.get_mut(key).expect("bucket just ensured");
        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * refill_per_sec).min(capacity);
        bucket.last = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let deficit = 1.0 - bucket.tokens;
            let secs = (deficit / refill_per_sec).ceil().max(1.0) as u64;
            Err(secs)
        }
    }

    /// Forget a key (used to clear the username bucket after a successful
    /// login, so a legitimate user is not punished for earlier typos).
    pub fn reset(&mut self, key: &str) {
        self.buckets.remove(key);
    }

    /// Drop buckets untouched for `idle` so a long-running process does not
    /// grow without bound under many distinct keys.
    pub fn prune(&mut self, idle: Duration, now: Instant) {
        self.buckets
            .retain(|_, b| now.saturating_duration_since(b.last) < idle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_capacity_then_limits() {
        let mut rl = RateLimiter::new(5, Duration::from_secs(60));
        for i in 0..5 {
            assert!(rl.check("k").is_ok(), "attempt {i} should pass");
        }
        assert!(rl.check("k").is_err(), "6th attempt must be limited");
        // Other keys are independent.
        assert!(rl.check("other").is_ok());
    }

    #[test]
    fn retry_after_is_bounded_by_window() {
        let mut rl = RateLimiter::new(5, Duration::from_secs(60));
        for _ in 0..5 {
            let _ = rl.check("k");
        }
        let secs = rl.check("k").unwrap_err();
        assert!((1..=60).contains(&secs), "retry-after {secs} out of range");
    }

    #[test]
    fn reset_restores_capacity() {
        let mut rl = RateLimiter::new(1, Duration::from_secs(60));
        assert!(rl.check("k").is_ok());
        assert!(rl.check("k").is_err());
        rl.reset("k");
        assert!(rl.check("k").is_ok());
    }

    #[test]
    fn prune_drops_idle_buckets() {
        let mut rl = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(rl.check_at("k", now).is_ok());
        rl.prune(Duration::from_secs(1), now + Duration::from_secs(120));
        // Fresh bucket after prune.
        assert!(rl.check_at("k", now + Duration::from_secs(120)).is_ok());
    }

    #[test]
    fn map_stays_bounded_under_many_distinct_keys() {
        // Cap of 100: a flood of distinct keys must not grow the map past it.
        let mut rl = RateLimiter::with_limits(5, Duration::from_secs(60), 100);
        for i in 0..10_000 {
            let _ = rl.check(&format!("key-{i}"));
        }
        assert!(rl.len() <= 100, "bucket map grew to {} (cap 100)", rl.len());
    }
}
