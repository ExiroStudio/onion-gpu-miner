//! Controller layer for the minimal benchmark architecture.
//! Single synchronous pipeline, driven by the main thread.

use crate::compute::{cpu::CpuComputeBackend, expanded_secret_key, ComputeBackend};
use crate::matcher::Matcher;
use crate::storage;
use crate::types::MiningResult;
use crate::util::{fmt_duration, fmt_int, to_hex};
use crate::worker::Generator;

use rand::RngCore;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub static TOTAL_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
pub static TOTAL_MATCHES: AtomicU64 = AtomicU64::new(0);
pub static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Fully-resolved run configuration.
pub struct Config {
    pub prefixes: Vec<String>,
    pub workers: usize,
    pub batch_size: usize,
}

pub fn run(cfg: Config) -> Result<(), Box<dyn std::error::Error>> {
    let master = random_seed();
    let counters = vec![0u64; cfg.workers];

    let backend = CpuComputeBackend::new(cfg.workers);
    let matcher = Matcher::new(cfg.prefixes.clone());
    let mut generator = Generator::new(master, cfg.workers as u32, counters);

    ctrlc::set_handler(move || {
        SHUTDOWN.store(true, Ordering::Relaxed);
    }).expect("Error setting Ctrl-C handler");

    println!("onion-gpu-miner");
    println!("backend: cpu");
    println!("workers: {}", cfg.workers);
    println!("batch: {}", cfg.batch_size);
    println!("prefix: {}", cfg.prefixes.join(", "));
    println!("\nMining...\n");

    let out_dir = std::path::PathBuf::from("out");
    std::fs::create_dir_all(&out_dir).unwrap_or_default();

    let start_time = Instant::now();
    let mut buf = Vec::with_capacity(cfg.batch_size);

    while !SHUTDOWN.load(Ordering::Relaxed) {
        generator.fill_batch(cfg.batch_size, &mut buf);
        
        let derived = backend.process_batch(&buf);
        let hits = matcher.scan(&derived);
        
        TOTAL_ATTEMPTS.fetch_add(cfg.batch_size as u64, Ordering::Relaxed);
        
        for (idx, prefix) in hits {
            TOTAL_MATCHES.fetch_add(1, Ordering::Relaxed);
            let addr = &derived[idx];
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
            let _ = storage::save_result(&out_dir, &result, &addr.public_key, &secret);
        }
    }

    let elapsed = start_time.elapsed();
    let attempts = TOTAL_ATTEMPTS.load(Ordering::Relaxed);
    let matches = TOTAL_MATCHES.load(Ordering::Relaxed);
    let rate = (attempts as f64) / elapsed.as_secs_f64().max(1e-9);
    let attempts_per_worker = if cfg.workers > 0 { attempts / (cfg.workers as u64) } else { 0 };

    println!("Finished.\n");
    println!("Elapsed:\n{}", fmt_duration(elapsed));
    println!("\nAttempts:\n{}", attempts);
    println!("\nAverage:\n{}/s", fmt_int(rate as u64));
    println!("\nMatches:\n{}", matches);
    println!("\nWorkers:\n{}", cfg.workers);
    println!("\nBatch:\n{}", cfg.batch_size);
    println!("\nAttempts/worker:\n{}", attempts_per_worker);

    Ok(())
}

fn random_seed() -> [u8; 32] {
    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    seed
}
