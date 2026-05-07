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
