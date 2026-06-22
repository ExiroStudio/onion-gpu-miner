//! Output / storage layer.
//!
//! Persist a successful match (Tor-compatible key files + a simple log)

use crate::types::MiningResult;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// Persist a match as a Tor-style hidden-service key directory plus an append
/// to `results.log`.
///
/// Layout:
/// ```text
/// <out_dir>/<host>.onion/
///     hostname
///     hs_ed25519_public_key
///     hs_ed25519_secret_key
/// <out_dir>/results.log
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
        .open(out_dir.join("results.log"))?;
    writeln!(log, "[{}] prefix: {} | seed: {} | w/c: {}/{}", 
        result.onion, result.matched_prefix, result.seed_hex, result.worker_id, result.counter)?;

    Ok(())
}

/// Tor key files start with a 32-byte, null-padded ASCII header.
fn ed25519_header(tag: &str) -> Vec<u8> {
    let mut header = tag.as_bytes().to_vec();
    header.resize(32, 0);
    header
}
