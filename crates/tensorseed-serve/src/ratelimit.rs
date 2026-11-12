// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! A small shared byte rate limiter (virtual clock, no burst beyond one
//! chunk), used to cap HTTP upload bandwidth across all connections.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

/// Limits the combined rate of everything that calls [`RateLimiter::acquire`].
#[derive(Debug)]
pub struct RateLimiter {
    bps: AtomicU64,
    next_free: Mutex<Instant>,
}

impl RateLimiter {
    /// A limiter at `bytes_per_sec` (0 = unlimited).
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            bps: AtomicU64::new(bytes_per_sec),
            next_free: Mutex::new(Instant::now()),
        }
    }

    /// Current limit in bytes per second (0 = unlimited).
    pub fn limit(&self) -> u64 {
        self.bps.load(Ordering::Relaxed)
    }

    /// Change the limit (0 = unlimited).
    pub fn set_limit(&self, bytes_per_sec: u64) {
        self.bps.store(bytes_per_sec, Ordering::Relaxed);
    }

    /// Wait until `n` more bytes may be sent.
    pub async fn acquire(&self, n: usize) {
        let bps = self.limit();
        if bps == 0 || n == 0 {
            return;
        }
        let cost = Duration::from_secs_f64(n as f64 / bps as f64);
        let start = {
            let mut next = self.next_free.lock().await;
            let now = Instant::now();
            let start = (*next).max(now);
            *next = start + cost;
            start
        };
        tokio::time::sleep_until(start).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn paces_bytes() {
        let l = RateLimiter::new(1000);
        let t0 = Instant::now();
        for _ in 0..5 {
            l.acquire(1000).await;
        }
        // The first chunk goes at once, the next four wait a second each.
        let e = t0.elapsed();
        assert!(
            e >= Duration::from_secs(4) && e < Duration::from_millis(4100),
            "{e:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unlimited_is_free() {
        let l = RateLimiter::new(0);
        let t0 = Instant::now();
        l.acquire(1 << 30).await;
        assert_eq!(t0.elapsed(), Duration::ZERO);
        l.set_limit(10);
        assert_eq!(l.limit(), 10);
    }
}
