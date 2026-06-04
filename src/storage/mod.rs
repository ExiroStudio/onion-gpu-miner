//! Output / storage layer.
//!
//! Two responsibilities, both deliberately kept *off* the compute hot path:
//!   * persist a successful match (Tor-compatible key files + a JSONL log)
//!   * save/load checkpoints (per-worker counters + master seed)
//!
//! Writes happen on the controller thread when a result arrives or on a coarse
//! interval — never inside `process_batch`.

use crate::types::MiningResult;
use crate::util::{from_hex, to_hex};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// Resumable progress: which master seed and how far each worker has counted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub master_seed_hex: String,
    pub counters: Vec<u64>,
}

impl Checkpoint {
    pub fn new(master: &[u8; 32], counters: Vec<u64>) -> Self {
        Self {
            master_seed_hex: to_hex(master),
            counters,
        }
    }

    pub fn master_seed(&self) -> Option<[u8; 32]> {
        from_hex::<32>(&self.master_seed_hex)
    }
}

pub fn save_checkpoint(path: &Path, cp: &Checkpoint) -> io::Result<()> {
    let json = serde_json::to_string_pretty(cp)?;
    // Write to a temp file then rename, so a crash mid-write can't corrupt it.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load_checkpoint(path: &Path) -> Option<Checkpoint> {
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Persist a match as a Tor-style hidden-service key directory plus an append
/// to `results.jsonl`.
///
/// Layout:
/// ```text
/// <out_dir>/<host>.onion/
///     hostname
///     hs_ed25519_public_key
///     hs_ed25519_secret_key
/// <out_dir>/results.jsonl
/// ```
pub fn save_result(
    out_dir: &Path,
    result: &MiningResult,
    public_key: &[u8; 32],
    secret_key: &[u8; 64],
) -> io::Result<()> {
    fs::create_dir_all(out_dir)?;

    let dir = out_dir.join(&result.onion);
    fs::create_dir_all(&dir)?;

    fs::write(dir.join("hostname"), format!("{}\n", result.onion))?;

    let mut pub_file = ed25519_header("== ed25519v1-public: type0 ==");
    pub_file.extend_from_slice(public_key);
    fs::write(dir.join("hs_ed25519_public_key"), pub_file)?;

    let mut sec_file = ed25519_header("== ed25519v1-secret: type0 ==");
    sec_file.extend_from_slice(secret_key);
    fs::write(dir.join("hs_ed25519_secret_key"), sec_file)?;

    // Append a structured log line.
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(out_dir.join("results.jsonl"))?;
    writeln!(log, "{}", serde_json::to_string(result)?)?;

    Ok(())
}

/// Tor key files start with a 32-byte, null-padded ASCII header.
fn ed25519_header(tag: &str) -> Vec<u8> {
    let mut header = tag.as_bytes().to_vec();
    header.resize(32, 0);
    header
}
