use std::sync::Arc;

use tokio::{
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(crate) struct RateLimiter {
    inner: Arc<Mutex<RateLimiterInner>>,
}

struct RateLimiterInner {
    max_tokens: f64,
    tokens: f64,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

impl RateLimiter {
    /// Create a new rate limiter that starts full.
    ///
    /// - `max_tokens`: bucket capacity (e.g. 15 for Kraken public)
    /// - `refill_rate`: tokens restored per second (e.g. 0.33 for Kraken)
    pub(crate) fn new(max_tokens: u32, refill_rate: f64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RateLimiterInner {
                max_tokens: f64::from(max_tokens),
                tokens: f64::from(max_tokens),
                refill_rate,
                last_refill: Instant::now(),
            })),
        }
    }

    /// Acquire one token. Waits asynchronously if the bucket is empty.
    pub(crate) async fn acquire(&self) -> anyhow::Result<()> {
        loop {
            let wait_duration = {
                let mut inner = self.inner.lock().await;
                inner.refill();

                if inner.tokens >= 1.0 {
                    inner.tokens -= 1.0;
                    return Ok(());
                }

                let deficit = 1.0 - inner.tokens;
                Duration::from_secs_f64(deficit / inner.refill_rate)
            };

            tokio::time::sleep(wait_duration).await;
        }
    }

    /// Returns the current token count (for testing/diagnostics).
    #[cfg(test)]
    pub(crate) async fn available_tokens(&self) -> f64 {
        let mut inner = self.inner.lock().await;
        inner.refill();
        inner.tokens
    }
}

impl RateLimiterInner {
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
        self.last_refill = now;
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Context;
    use tokio::time::timeout;

    use super::*;

    #[tokio::test]
    async fn test_rate_limiter_initial_tokens() -> anyhow::Result<()> {
        let limiter = RateLimiter::new(15, 0.33);
        let tokens = limiter.available_tokens().await;
        assert!(
            (tokens - 15.0).abs() < 0.1,
            "expected ~15 tokens, got {tokens}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_rate_limiter_acquire_decrements() -> anyhow::Result<()> {
        let limiter = RateLimiter::new(15, 0.33);
        limiter.acquire().await.context("first acquire")?;
        let tokens = limiter.available_tokens().await;
        assert!(tokens < 15.0, "expected tokens < 15, got {tokens}");
        assert!(tokens > 13.0, "expected tokens > 13, got {tokens}");
        Ok(())
    }

    #[tokio::test]
    async fn test_rate_limiter_waits_when_empty() -> anyhow::Result<()> {
        // Small bucket that empties quickly
        let limiter = RateLimiter::new(2, 10.0); // 10 tokens/sec refill
        limiter.acquire().await.context("acquire 1")?;
        limiter.acquire().await.context("acquire 2")?;

        // Next acquire should need to wait ~100ms (1 token / 10 tokens/sec)
        let start = Instant::now();
        let result = timeout(Duration::from_secs(2), limiter.acquire()).await;
        let elapsed = start.elapsed();

        result
            .context("timed out waiting for rate limiter")?
            .context("acquire failed")?;
        assert!(
            elapsed.as_millis() >= 50,
            "expected at least 50ms wait, got {}ms",
            elapsed.as_millis()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_rate_limiter_refills_over_time() -> anyhow::Result<()> {
        let limiter = RateLimiter::new(5, 10.0); // fast refill for test
        // Drain all tokens
        for _ in 0..5 {
            limiter.acquire().await.context("draining")?;
        }
        let before = limiter.available_tokens().await;
        assert!(before < 1.0, "expected < 1 token after drain, got {before}");

        // Wait for refill
        tokio::time::sleep(Duration::from_millis(200)).await;

        let after = limiter.available_tokens().await;
        assert!(
            after > 1.0,
            "expected > 1 token after 200ms at 10/sec, got {after}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_rate_limiter_clone_shares_state() -> anyhow::Result<()> {
        let limiter = RateLimiter::new(5, 0.33);
        let clone = limiter.clone();

        limiter.acquire().await.context("acquire from original")?;
        let tokens = clone.available_tokens().await;
        assert!(
            tokens < 5.0,
            "clone should see decremented tokens, got {tokens}"
        );
        Ok(())
    }
}
