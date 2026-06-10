use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

/// Simple token-bucket rate limiter. No external dependencies.
pub struct RateLimiter {
    tokens: f64,
    max_tokens: f64,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

impl RateLimiter {
    pub fn new(rate_per_sec: f64, burst: f64) -> Self {
        Self {
            tokens: burst,
            max_tokens: burst,
            refill_rate: rate_per_sec,
            last_refill: Instant::now(),
        }
    }

    /// Returns true if the action is allowed, false if rate-limited.
    pub fn check(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
        self.last_refill = now;
    }
}

/// Single per-IP entry: a token bucket plus the connection refcount.
/// `last_active` tracks the last time the entry was touched so the
/// maintenance task can reclaim entries from disconnected IPs after a TTL.
struct IpEntry {
    limiter: Mutex<RateLimiter>,
    refcount: u32,
    last_active: Instant,
}

/// Thread-safe per-IP token-bucket registry.
///
/// Used as a second rate-limit layer in addition to the per-connection limiter:
/// without it, an attacker can bypass the per-client cap by opening N TCP
/// connections from the same IP and getting N× quota.
pub struct IpRateLimiter {
    rate_per_sec: f64,
    burst: f64,
    map: Mutex<HashMap<IpAddr, IpEntry>>,
}

impl IpRateLimiter {
    pub fn new(rate_per_sec: f64, burst: f64) -> Self {
        Self {
            rate_per_sec,
            burst,
            map: Mutex::new(HashMap::new()),
        }
    }

    /// Register a new connection from `ip`. Increments the refcount, creating
    /// a fresh bucket if this is the first connection from that IP.
    pub fn track(&self, ip: IpAddr) {
        let mut map = self.map.lock().expect("ip limiter poisoned");
        let entry = map.entry(ip).or_insert_with(|| IpEntry {
            limiter: Mutex::new(RateLimiter::new(self.rate_per_sec, self.burst)),
            refcount: 0,
            last_active: Instant::now(),
        });
        entry.refcount = entry.refcount.saturating_add(1);
        entry.last_active = Instant::now();
    }

    /// Mark a connection from `ip` as gone. Refcount drops; the entry itself
    /// is reclaimed later by [`Self::sweep_idle`] (we keep the bucket warm
    /// briefly so reconnect storms don't reset the budget).
    pub fn untrack(&self, ip: IpAddr) {
        let mut map = self.map.lock().expect("ip limiter poisoned");
        if let Some(entry) = map.get_mut(&ip) {
            entry.refcount = entry.refcount.saturating_sub(1);
            entry.last_active = Instant::now();
        }
    }

    /// Returns true if the request is allowed.
    /// On a missing entry (shouldn't happen if `track` was called) we allow,
    /// since being permissive here is safer than locking out a legit client.
    pub fn check(&self, ip: IpAddr) -> bool {
        let mut map = self.map.lock().expect("ip limiter poisoned");
        match map.get_mut(&ip) {
            Some(entry) => {
                entry.last_active = Instant::now();
                let mut limiter = entry.limiter.lock().expect("ip bucket poisoned");
                limiter.check()
            }
            None => true,
        }
    }

    /// Drop entries that have refcount 0 and have been idle for at least `ttl`.
    /// Returns the number of entries removed. Called from the maintenance task.
    pub fn sweep_idle(&self, ttl: std::time::Duration) -> usize {
        let now = Instant::now();
        let mut map = self.map.lock().expect("ip limiter poisoned");
        let before = map.len();
        map.retain(|_, e| e.refcount > 0 || now.duration_since(e.last_active) < ttl);
        before - map.len()
    }

    /// Test-only inspection hook; no `is_empty` needed.
    #[cfg(test)]
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.map.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_allows_initial_requests() {
        let mut rl = RateLimiter::new(1.0, 5.0);
        for _ in 0..5 {
            assert!(rl.check());
        }
        // Burst exhausted — next should fail (no time for refill)
        assert!(!rl.check());
    }

    #[test]
    fn zero_burst_blocks_immediately() {
        let mut rl = RateLimiter::new(100.0, 0.0);
        assert!(!rl.check());
    }

    #[test]
    fn refill_restores_tokens() {
        let mut rl = RateLimiter::new(1000.0, 1.0);
        assert!(rl.check());
        assert!(!rl.check());
        // Sleep enough for at least 1 token to refill (1ms @ 1000/sec = 1 token)
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(rl.check());
    }

    #[test]
    fn ip_limiter_tracks_and_releases() {
        let l = IpRateLimiter::new(100.0, 1.0);
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        l.track(ip);
        l.track(ip);
        assert_eq!(l.len(), 1);
        assert!(l.check(ip));
        l.untrack(ip);
        l.untrack(ip);
        // Refcount=0 but TTL not elapsed → keep
        assert_eq!(l.sweep_idle(std::time::Duration::from_secs(60)), 0);
        assert_eq!(l.len(), 1);
        // TTL=0 → reclaim
        assert_eq!(l.sweep_idle(std::time::Duration::from_secs(0)), 1);
        assert_eq!(l.len(), 0);
    }

    #[test]
    fn ip_limiter_check_unknown_ip_allows() {
        let l = IpRateLimiter::new(1.0, 1.0);
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        // Never tracked — fail-open (only the per-conn limiter applies)
        assert!(l.check(ip));
    }

    #[test]
    fn ip_limiter_enforces_burst_across_connections() {
        let l = IpRateLimiter::new(0.0, 3.0); // 3 burst, no refill
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        l.track(ip);
        l.track(ip); // simulate 2 conns from same IP
                     // Across both conns, only 3 cmds allowed before lockout.
        assert!(l.check(ip));
        assert!(l.check(ip));
        assert!(l.check(ip));
        assert!(!l.check(ip));
    }

    #[test]
    fn tokens_capped_at_max() {
        let mut rl = RateLimiter::new(10000.0, 3.0);
        // Even after sleeping, tokens shouldn't exceed burst (3)
        std::thread::sleep(std::time::Duration::from_millis(10));
        for _ in 0..3 {
            assert!(rl.check());
        }
        assert!(!rl.check());
    }
}
