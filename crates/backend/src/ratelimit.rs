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

/// Token bucket: `capacity` attempts, refilling steadily over `window`.
#[derive(Debug)]
pub struct RateLimiter {
    buckets: HashMap<String, Bucket>,
    capacity: f64,
    refill_per_sec: f64,
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    /// `capacity` attempts per `window` (5 per 60s for auth).
    pub fn new(capacity: u32, window: Duration) -> Self {
        let capacity = capacity.max(1) as f64;
        let window_secs = window.as_secs_f64().max(1.0);
        Self {
            buckets: HashMap::new(),
            capacity,
            refill_per_sec: capacity / window_secs,
        }
    }

    /// Consume one token for `key`. `Ok(())` when allowed, otherwise
    /// `Err(retry_after_secs)` — the whole seconds until a token is available.
    pub fn check(&mut self, key: &str) -> Result<(), u64> {
        self.check_at(key, Instant::now())
    }

    fn check_at(&mut self, key: &str, now: Instant) -> Result<(), u64> {
        let capacity = self.capacity;
        let refill_per_sec = self.refill_per_sec;
        let bucket = self.buckets.entry(key.to_owned()).or_insert(Bucket {
            tokens: capacity,
            last: now,
        });
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
}
