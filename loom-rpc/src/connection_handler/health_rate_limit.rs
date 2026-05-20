// Per-connection token-bucket rate limiter for `daemon.health`.
//
// Why this exists (issue #58 / Sec-5 of #56 council review):
// `daemon.health({deep:true})` fans out one CBOR probe per running shim
// per call. An authed-but-compromised client could spam this method and
// amplify load N× (where N = active shim count). The bucket caps per-
// connection sustained call rate (default 10 RPS) with a burst tolerance
// (default 30). Returns `LoomErrorCode::TooManyRequests` when empty.
//
// Scope:
// - **Per-connection.** Each `ConnectionHandler` owns its own bucket; no
//   cross-connection state. This means N authed connections can each
//   spend 10 RPS sustained — that's intentional. The threat model is a
//   single compromised client, not a fleet of cooperating ones (which is
//   what auth + the unix-socket boundary are for).
// - **Method-scoped.** Only `daemon.health` consults the bucket today.
//   Other RPCs go through the existing dispatcher gates (auth, schema,
//   per-request timeout) unmodified.
//
// Algorithm: leaky-bucket / token-bucket hybrid. Tokens accrue at
// `refill_per_sec`, capped at `capacity` (burst). Each call consumes 1
// token; an empty bucket rejects. Lazy refill on each `try_consume`
// — no background timer task, which keeps this allocation-free under
// load.

use std::sync::OnceLock;
use std::time::Instant;

/// Default sustained rate for `daemon.health`. Override via
/// `LOOM_DAEMON_HEALTH_RATE_RPS`.
pub const DEFAULT_HEALTH_RATE_RPS: f64 = 10.0;

/// Default burst tolerance for `daemon.health`. Override via
/// `LOOM_DAEMON_HEALTH_RATE_BURST`.
pub const DEFAULT_HEALTH_RATE_BURST: f64 = 30.0;

/// Lazily-resolved env-var-or-default config. Read once per process
/// (a fresh daemon respawn picks up changes; live re-tuning isn't
/// supported).
pub fn health_rate_rps() -> f64 {
    static CACHED: OnceLock<f64> = OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("LOOM_DAEMON_HEALTH_RATE_RPS")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(DEFAULT_HEALTH_RATE_RPS)
    })
}

/// Lazily-resolved env-var-or-default burst config.
pub fn health_rate_burst() -> f64 {
    static CACHED: OnceLock<f64> = OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("LOOM_DAEMON_HEALTH_RATE_BURST")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(DEFAULT_HEALTH_RATE_BURST)
    })
}

/// Token bucket. `&mut self` enforces single-writer per call; in the
/// per-connection use site the handler wraps this in a
/// `parking_lot::Mutex`, but tests can drive it directly.
#[derive(Debug)]
pub struct TokenBucket {
    tokens: f64,
    capacity: f64,
    refill_per_sec: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// New bucket, initially full (burst budget available).
    pub fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Self {
            tokens: capacity,
            capacity,
            refill_per_sec,
            last_refill: Instant::now(),
        }
    }

    /// New bucket using the env-var-derived `daemon.health` config.
    pub fn for_health() -> Self {
        Self::new(health_rate_burst(), health_rate_rps())
    }

    /// Attempt to consume 1 token. Returns `true` if granted, `false`
    /// if the bucket is empty (rate-limit response should be returned
    /// to the client).
    ///
    /// Refills lazily based on wall-clock elapsed since `last_refill`,
    /// capped at `capacity`. Idempotent under contention since each
    /// caller holds `&mut self` for the full read-modify-write cycle.
    pub fn try_consume(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        self.last_refill = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn defaults_are_sensible() {
        // Off-by-zero would silently disable; off-by-huge would defeat
        // the purpose. Pin the numbers loudly via constants.
        assert_eq!(DEFAULT_HEALTH_RATE_RPS, 10.0);
        assert_eq!(DEFAULT_HEALTH_RATE_BURST, 30.0);
    }

    #[test]
    fn fresh_bucket_starts_at_capacity() {
        let mut b = TokenBucket::new(5.0, 10.0);
        // 5 successive consumes succeed; 6th fails (no time elapsed,
        // refill is negligible).
        for i in 0..5 {
            assert!(b.try_consume(), "consume {i} expected ok");
        }
        assert!(!b.try_consume(), "6th consume must be rate-limited");
    }

    #[test]
    fn burst_then_steady_then_recover() {
        // Matches issue #58 acceptance criteria: a sharp burst beyond
        // capacity sees at least one rejection.
        let mut b = TokenBucket::new(30.0, 10.0);
        let mut allowed = 0;
        let mut limited = 0;
        for _ in 0..50 {
            if b.try_consume() {
                allowed += 1;
            } else {
                limited += 1;
            }
        }
        assert!(
            allowed <= 30,
            "burst should not exceed capacity (got {allowed} allowed)"
        );
        assert!(
            limited > 0,
            "50 rapid calls must see at least 1 rate-limited \
             (issue #58 acceptance); got {limited} limited"
        );
    }

    #[test]
    fn empty_bucket_refills_over_time() {
        let mut b = TokenBucket::new(5.0, 100.0); // fast refill for test speed
        for _ in 0..5 {
            let _ = b.try_consume();
        }
        assert!(!b.try_consume(), "should be empty");
        sleep(Duration::from_millis(50)); // 50ms × 100 rps = 5 tokens
        assert!(b.try_consume(), "should have refilled");
    }

    #[test]
    fn refill_caps_at_capacity() {
        let mut b = TokenBucket::new(5.0, 1000.0);
        sleep(Duration::from_millis(20)); // would refill 20 tokens, capped at 5
        for i in 0..5 {
            assert!(
                b.try_consume(),
                "consume {i} after long idle should succeed"
            );
        }
        assert!(
            !b.try_consume(),
            "6th consume should fail — refill must cap at capacity, not accumulate unbounded"
        );
    }
}
