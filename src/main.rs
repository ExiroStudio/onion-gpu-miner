//! onion-gpu-miner — CPU-first Onion v3 vanity generator.
//! Minimal benchmark-oriented architecture.

mod compute;
mod controller;
mod matcher;
mod storage;
mod types;
mod util;
mod worker;

use clap::Parser;
use controller::Config;

const VALID_PREFIX_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";

#[derive(Parser, Debug)]
#[command(
    name = "onion-gpu-miner",
    version,
    about = "CPU-first Onion v3 vanity generator (Minimal Benchmark Architecture)"
)]
struct Cli {
    /// Target prefix(es). Repeatable, or pass several after one flag.
    /// Only base32 chars [a-z2-7] are valid in an onion host.
    #[arg(long = "prefix", required = true, num_args = 1.., value_name = "PREFIX")]
    prefixes: Vec<String>,

    /// Number of logical workers / crypto threads.
    #[arg(long)]
    workers: Option<usize>,

    /// Candidates derived per batch (the compute dispatch unit).
    #[arg(long, default_value_t = 262144)]
    batch_size: usize,
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
    let workers = cli.workers.unwrap_or_else(num_cpus::get);

    if workers == 0 {
        return Err("--workers must be >= 1".into());
    }

    if cli.batch_size == 0 {
        return Err("--batch-size must be >= 1".into());
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

    Ok(Config {
        prefixes,
        workers,
        batch_size: cli.batch_size,
    })
}
