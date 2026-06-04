//! Controller layer.
//!
//! Owns the lifecycle: builds the pipeline, spawns the generator and the
//! compute driver, aggregates statistics, renders the dashboard, persists
//! results and checkpoints, and coordinates a clean shutdown.
//!
//! Pipeline shape (all bounded, no async runtime, no global state):
//!
//! ```text
//!   [generator thread] --(bounded batch queue)--> [compute driver thread]
//!                                                        | process_batch (rayon pool)
//!                                                        | matcher.scan
//!                                                        v
//!                                              (bounded result queue)
//!                                                        |
//!                                                        v
//!                                                 [controller / main]
//!                                                   - dashboard
//!                                                   - storage writes
//!                                                   - checkpoints
//! ```
//!
//! Why a single compute *driver* thread feeding a sized rayon pool (rather than
//! N consumer threads each running rayon)? It mirrors the GPU model exactly:
//! one host thread dispatches one large data-parallel kernel per batch. It also
//! avoids thread oversubscription. The generator outruns the (crypto-bound)
//! compute step, so the bounded queue stays near-full — which is what the
//! "Queue utilization" metric reflects.

use crate::compute::{cpu::CpuComputeBackend, expanded_secret_key, ComputeBackend};
use crate::matcher::Matcher;
use crate::metrics::Metrics;
use crate::storage::{self, Checkpoint};
use crate::types::{Candidate, MiningResult};
use crate::util::{fmt_duration, fmt_int, to_hex};
use crate::worker::Generator;

use crossbeam_channel::{bounded, RecvTimeoutError, SendTimeoutError};
use rand::RngCore;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Fully-resolved run configuration (built from CLI args in `main`).
pub struct Config {
    pub prefixes: Vec<String>,
    pub workers: usize,
    pub batch_size: usize,
    pub queue_cap: usize,
    pub max_matches: u64,
    pub out_dir: PathBuf,
    pub checkpoint_path: PathBuf,
    pub resume: bool,
    pub master_seed: Option<[u8; 32]>,
    pub duration: Option<Duration>,
}

/// What the compute driver hands back to the controller for persistence.
type ResultMsg = (MiningResult, [u8; 32], [u8; 64]);

pub fn run(cfg: Config) -> Result<(), Box<dyn std::error::Error>> {
    // ---- Resolve master seed + starting counters (checkpoint/resume) -------
    let mut master = cfg.master_seed.unwrap_or_else(random_seed);
    let mut counters = vec![0u64; cfg.workers];

    if cfg.resume {
        if let Some(cp) = storage::load_checkpoint(&cfg.checkpoint_path) {
            if let Some(seed) = cp.master_seed() {
                master = seed;
            }
            if cp.counters.len() == cfg.workers {
                counters = cp.counters;
                println!("Resumed from checkpoint: {}", cfg.checkpoint_path.display());
            } else {
                eprintln!(
                    "warning: checkpoint worker count ({}) != --workers ({}); ignoring counters",
                    cp.counters.len(),
                    cfg.workers
                );
            }
        }
    }

    // ---- Shared, explicitly-passed state (no globals) ----------------------
    let metrics = Arc::new(Metrics::new());
    let matcher = Arc::new(Matcher::new(cfg.prefixes.clone()));
    let backend: Arc<dyn ComputeBackend> = Arc::new(CpuComputeBackend::new(cfg.workers));
    let shutdown = Arc::new(AtomicBool::new(false));

    // ---- Bounded channels --------------------------------------------------
    let (batch_tx, batch_rx) = bounded::<Vec<Candidate>>(cfg.queue_cap);
    let (result_tx, result_rx) = bounded::<ResultMsg>(1024);
    // A read-only clone purely for sampling queue depth in the dashboard.
    let queue_probe = batch_rx.clone();

    print_banner(&cfg, backend.name(), &master);

    // ---- Generator thread --------------------------------------------------
    let gen_handle = {
        let shutdown = shutdown.clone();
        let checkpoint_path = cfg.checkpoint_path.clone();
        let workers = cfg.workers;
        let batch_size = cfg.batch_size;
        thread::Builder::new()
            .name("generator".into())
            .spawn(move || {
                let mut generator = Generator::new(master, workers as u32, counters);
                let mut batches_made: u64 = 0;

                'outer: while !shutdown.load(Ordering::Relaxed) {
                    let mut buf = Vec::with_capacity(batch_size);
                    generator.fill_batch(batch_size, &mut buf);

                    // Checkpoint occasionally — off the hot path. The generator
                    // exclusively owns the counters, so no locking is needed.
                    batches_made += 1;
                    if batches_made.is_multiple_of(256) {
                        let cp = Checkpoint::new(&generator.master(), generator.counters().to_vec());
                        let _ = storage::save_checkpoint(&checkpoint_path, &cp);
                    }

                    // Backpressure: block until the consumer drains, but wake
                    // periodically to observe the shutdown flag (so we never
                    // deadlock on a full queue after a stop is requested).
                    let mut pending = buf;
                    'send: loop {
                        match batch_tx.send_timeout(pending, Duration::from_millis(200)) {
                            Ok(()) => break 'send,
                            Err(SendTimeoutError::Timeout(b)) => {
                                if shutdown.load(Ordering::Relaxed) {
                                    break 'outer;
                                }
                                pending = b;
                            }
                            Err(SendTimeoutError::Disconnected(_)) => break 'outer,
                        }
                    }
                }

                // Final checkpoint on the way out — reached on every exit path
                // (clean shutdown, backpressure-during-shutdown, or disconnect).
                let cp = Checkpoint::new(&generator.master(), generator.counters().to_vec());
                let _ = storage::save_checkpoint(&checkpoint_path, &cp);
            })?
    };

    // ---- Compute driver thread ---------------------------------------------
    let compute_handle = {
        let shutdown = shutdown.clone();
        let metrics = metrics.clone();
        let matcher = matcher.clone();
        let backend = backend.clone();
        let workers = cfg.workers;
        let max_matches = cfg.max_matches;
        thread::Builder::new()
            .name("compute-driver".into())
            .spawn(move || {
                while !shutdown.load(Ordering::Relaxed) {
                    let batch = match batch_rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(b) => b,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    };

                    // === HOT PATH: generate -> process -> compare ===========
                    metrics.set_active_workers(workers);
                    let derived = backend.process_batch(&batch);
                    metrics.add_attempts(batch.len() as u64);
                    let hits = matcher.scan(&derived);
                    metrics.set_active_workers(0);

                    for (idx, prefix) in hits {
                        let addr = &derived[idx];
                        // Expanded secret recomputed only on a match (rare).
                        let secret = expanded_secret_key(&addr.seed);
                        let result = MiningResult {
                            onion: format!("{}.onion", addr.onion),
                            matched_prefix: prefix,
                            public_key_hex: to_hex(&addr.public_key),
                            secret_key_hex: to_hex(&secret),
                            seed_hex: to_hex(&addr.seed),
                            worker_id: addr.worker_id,
                            counter: addr.counter,
                        };
                        let total = metrics.record_match();
                        let _ = result_tx.send((result, addr.public_key, secret));
                        if max_matches > 0 && total >= max_matches {
                            shutdown.store(true, Ordering::Relaxed);
                        }
                    }
                }
            })?
    };

    // ---- Controller main loop: dashboard + persistence ---------------------
    let mut last_attempts = 0u64;
    let mut last_sample = Instant::now();
    let mut found_hosts: Vec<String> = Vec::new();

    while !shutdown.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_secs(1));

        // Drain results and persist them (off the compute hot path).
        while let Ok((result, pk, sk)) = result_rx.try_recv() {
            storage::save_result(&cfg.out_dir, &result, &pk, &sk)?;
            found_hosts.push(result.onion.clone());
        }

        // Duration-bounded benchmark runs.
        if let Some(limit) = cfg.duration {
            if metrics.elapsed() >= limit {
                shutdown.store(true, Ordering::Relaxed);
            }
        }

        let now = Instant::now();
        let attempts = metrics.attempts();
        let dt = (now - last_sample).as_secs_f64().max(1e-9);
        let rate = (attempts.saturating_sub(last_attempts)) as f64 / dt;
        last_attempts = attempts;
        last_sample = now;

        let queue_used = queue_probe.len();
        render_dashboard(
            backend.name(),
            &matcher,
            rate,
            attempts,
            metrics.active_workers(),
            cfg.workers,
            queue_used,
            cfg.queue_cap,
            metrics.matches(),
            metrics.elapsed(),
            &found_hosts,
        );
    }

    // ---- Shutdown ----------------------------------------------------------
    let _ = gen_handle.join();
    let _ = compute_handle.join();

    // Final drain of anything still in flight.
    while let Ok((result, pk, sk)) = result_rx.try_recv() {
        storage::save_result(&cfg.out_dir, &result, &pk, &sk)?;
        found_hosts.push(result.onion.clone());
    }

    print_summary(&metrics, &found_hosts, &cfg.out_dir);
    Ok(())
}

fn random_seed() -> [u8; 32] {
    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    seed
}

fn print_banner(cfg: &Config, backend: &str, master: &[u8; 32]) {
    println!("onion-gpu-miner — Onion v3 vanity generator");
    println!("  backend     : {backend} (CPU milestone)");
    println!("  prefixes    : {}", cfg.prefixes.join(", "));
    println!("  workers     : {}", cfg.workers);
    println!("  batch size  : {}", fmt_int(cfg.batch_size as u64));
    println!("  queue cap   : {} batches", cfg.queue_cap);
    println!("  master seed : {}", to_hex(master));
    println!("  output dir  : {}", cfg.out_dir.display());
    println!();
}

#[allow(clippy::too_many_arguments)]
fn render_dashboard(
    backend: &str,
    matcher: &Matcher,
    rate: f64,
    attempts: u64,
    active: usize,
    total_workers: usize,
    queue_used: usize,
    queue_cap: usize,
    matches: u64,
    elapsed: Duration,
    found: &[String],
) {
    let queue_pct = if queue_cap == 0 {
        0
    } else {
        (queue_used * 100) / queue_cap
    };

    // Clear screen + home cursor for a stable, refreshing dashboard.
    print!("\x1b[2J\x1b[H");
    println!("onion-gpu-miner  [{backend}]");
    println!("targets: {}", matcher.prefixes().join(", "));
    println!("------------------------------------");
    println!("Attempts:");
    println!("    {}/s", fmt_int(rate as u64));
    println!("    {} total", fmt_int(attempts));
    println!();
    println!("Worker:");
    println!("    {active}/{total_workers} active");
    println!();
    println!("Queue:");
    println!("    {queue_pct}%  ({queue_used}/{queue_cap} batches)");
    println!();
    println!("Matches:");
    println!("    {matches}");
    println!();
    println!("Elapsed:");
    println!("    {}", fmt_duration(elapsed));
    if !found.is_empty() {
        println!();
        println!("Found:");
        for host in found.iter().rev().take(5) {
            println!("    {host}");
        }
    }
    let _ = io::stdout().flush();
}

fn print_summary(metrics: &Metrics, found: &[String], out_dir: &std::path::Path) {
    print!("\x1b[2J\x1b[H");
    println!("=== run complete ===");
    println!("elapsed : {}", fmt_duration(metrics.elapsed()));
    println!("attempts: {}", fmt_int(metrics.attempts()));
    let secs = metrics.elapsed().as_secs_f64().max(1e-9);
    println!("avg rate: {}/s", fmt_int((metrics.attempts() as f64 / secs) as u64));
    println!("matches : {}", found.len());
    for host in found {
        println!("  {host}");
    }
    if !found.is_empty() {
        println!("keys written under: {}", out_dir.display());
    }
}
