//! onion-gpu-miner — CPU-first Onion v3 vanity generator.
//!
//! Architecture (one module per layer):
//!   * `controller` — lifecycle, stats aggregation, checkpoints
//!   * `worker`     — deterministic seed/batch generation
//!   * `compute`    — stateless ed25519 -> onion kernel behind a trait
//!   * `matcher`    — multi-prefix comparison
//!   * `metrics`    — attempts/sec, active workers, elapsed, queue utilization
//!   * `storage`    — Tor-style key output + checkpoint persistence
//!
//! See README.md for the architecture write-up and the CPU -> SIMD -> GPU ->
//! Distributed migration path.

mod compute;
mod controller;
mod matcher;
mod metrics;
mod storage;
mod types;
mod util;
mod worker;

use clap::Parser;
use controller::Config;
use std::path::PathBuf;
use std::time::Duration;

/// RFC 4648 base32 lowercase alphabet — the only characters a v3 onion can
/// contain, so the only characters a prefix may use.
const VALID_PREFIX_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";

#[derive(Parser, Debug)]
#[command(
    name = "onion-gpu-miner",
    version,
    about = "CPU-first Onion v3 vanity generator (GPU-ready, batch-first architecture)"
)]
struct Cli {
    /// Target prefix(es). Repeatable, or pass several after one flag.
    /// Only base32 chars [a-z2-7] are valid in an onion host.
    #[arg(long = "prefix", required = true, num_args = 1.., value_name = "PREFIX")]
    prefixes: Vec<String>,

    /// Number of logical workers / crypto threads.
    #[arg(long, default_value_t = default_workers())]
    workers: usize,

    /// Candidates derived per batch (the compute dispatch unit).
    #[arg(long = "batch-size", default_value_t = 65536)]
    batch_size: usize,

    /// Bounded work-queue depth, in batches.
    #[arg(long, default_value_t = 64)]
    queue: usize,

    /// Stop after this many matches (0 = run until duration / Ctrl-C).
    #[arg(long = "max-matches", default_value_t = 1)]
    max_matches: u64,

    /// Output directory for matched key material.
    #[arg(long, default_value = "out")]
    out: PathBuf,

    /// Checkpoint file path.
    #[arg(long, default_value = "checkpoint.json")]
    checkpoint: PathBuf,

    /// Resume counters from an existing checkpoint.
    #[arg(long)]
    resume: bool,

    /// Fixed 32-byte master seed (64 hex chars). Default: random.
    #[arg(long, value_name = "HEX64")]
    seed: Option<String>,

    /// Auto-stop after N seconds (useful for benchmarking).
    #[arg(long, value_name = "SECONDS")]
    duration: Option<u64>,
}

fn default_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8)
}

fn main() {
    let cli = Cli::parse();

    let cfg = match build_config(cli) {
        Ok(cfg) => cfg,
        Err(msg) => {
            eprintln!("error: {msg}");
            std::process::exit(2);
        }
    };

    if let Err(e) = controller::run(cfg) {
        eprintln!("fatal: {e}");
        std::process::exit(1);
    }
}

fn build_config(cli: Cli) -> Result<Config, String> {
    if cli.workers == 0 {
        return Err("--workers must be >= 1".into());
    }
    if cli.batch_size == 0 {
        return Err("--batch-size must be >= 1".into());
    }
    if cli.queue == 0 {
        return Err("--queue must be >= 1".into());
    }

    // Normalize + validate prefixes against the onion alphabet.
    let mut prefixes = Vec::with_capacity(cli.prefixes.len());
    for raw in cli.prefixes {
        let p = raw.to_lowercase();
        if p.is_empty() {
            return Err("prefixes must not be empty".into());
        }
        if p.len() > types::ONION_BASE32_LEN {
            return Err(format!(
                "prefix '{p}' is longer than a v3 onion ({} chars)",
                types::ONION_BASE32_LEN
            ));
        }
        if let Some(bad) = p.bytes().find(|b| !VALID_PREFIX_CHARS.contains(b)) {
            return Err(format!(
                "prefix '{p}' contains invalid character '{}'; onion hosts use base32 [a-z2-7]",
                bad as char
            ));
        }
        prefixes.push(p);
    }

    let master_seed = match cli.seed {
        Some(hex) => Some(
            util::from_hex::<32>(&hex)
                .ok_or_else(|| "--seed must be exactly 64 hex characters".to_string())?,
        ),
        None => None,
    };

    Ok(Config {
        prefixes,
        workers: cli.workers,
        batch_size: cli.batch_size,
        queue_cap: cli.queue,
        max_matches: cli.max_matches,
        out_dir: cli.out,
        checkpoint_path: cli.checkpoint,
        resume: cli.resume,
        master_seed,
        duration: cli.duration.map(Duration::from_secs),
    })
}
