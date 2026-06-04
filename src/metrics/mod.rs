//! Metrics layer.
//!
//! A single `Arc<Metrics>` is shared explicitly (passed in, never global) and
//! uses only atomics — no locks on the hot path. The controller samples it once
//! per second to render the dashboard.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

pub struct Metrics {
    attempts: AtomicU64,
    matches: AtomicU64,
    active_workers: AtomicUsize,
    start: Instant,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            attempts: AtomicU64::new(0),
            matches: AtomicU64::new(0),
            active_workers: AtomicUsize::new(0),
            start: Instant::now(),
        }
    }

    #[inline]
    pub fn add_attempts(&self, n: u64) {
        self.attempts.fetch_add(n, Ordering::Relaxed);
    }

    pub fn attempts(&self) -> u64 {
        self.attempts.load(Ordering::Relaxed)
    }

    pub fn record_match(&self) -> u64 {
        self.matches.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn matches(&self) -> u64 {
        self.matches.load(Ordering::Relaxed)
    }

    pub fn set_active_workers(&self, n: usize) {
        self.active_workers.store(n, Ordering::Relaxed);
    }

    pub fn active_workers(&self) -> usize {
        self.active_workers.load(Ordering::Relaxed)
    }

    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}
