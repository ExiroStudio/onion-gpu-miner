//! Shared, layer-agnostic data types.
//!
//! These are intentionally small, `Copy`-friendly, and free of behavior so they
//! can move across thread boundaries (and, later, host<->device boundaries)
//! cheaply. The compute kernel only ever needs `Candidate` in and
//! `DerivedAddress` out — that keeps the GPU port narrow.

use serde::Serialize;

/// Onion v3 address version byte (always 0x03).
pub const ONION_VERSION: u8 = 0x03;

/// Length of an encoded v3 onion (base32 of pubkey+checksum+version, no suffix).
pub const ONION_BASE32_LEN: usize = 56;

/// A unit of work handed to the compute layer.
///
/// `seed` is the only thing the cryptographic kernel needs; `worker_id` and
/// `counter` ride along purely for traceability / checkpointing and are passed
/// straight through. On GPU these become a tightly packed input buffer.
#[derive(Clone, Copy, Debug)]
pub struct Candidate {
    pub seed: [u8; 32],
    pub worker_id: u32,
    pub counter: u64,
}

/// The output of the compute layer for one candidate.
///
/// `onion` is the 56-char base32 host *without* the `.onion` suffix — that is
/// what the matcher compares against.
#[derive(Clone, Debug)]
pub struct DerivedAddress {
    pub seed: [u8; 32],
    pub public_key: [u8; 32],
    pub onion: String,
    pub worker_id: u32,
    pub counter: u64,
}

/// A successful match, ready to be persisted by the output layer.
#[derive(Clone, Debug, Serialize)]
pub struct MiningResult {
    /// Full host including the `.onion` suffix.
    pub onion: String,
    /// Which configured prefix matched.
    pub matched_prefix: String,
    pub public_key_hex: String,
    /// Tor-style 64-byte expanded secret key, hex encoded.
    pub secret_key_hex: String,
    /// 32-byte seed the key was derived from, hex encoded.
    pub seed_hex: String,
    pub worker_id: u32,
    pub counter: u64,
}
