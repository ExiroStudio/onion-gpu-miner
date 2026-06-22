//! Compute layer — the cryptographic kernel, isolated behind a trait.
//!
//! This is the *only* layer that should change when we move to GPU. Everything
//! above it (controller, generator, matcher, metrics, storage) talks to the
//! [`ComputeBackend`] trait and never sees how a batch is actually processed.
//!
//! Design rules honored here:
//!   * **Stateless** — `derive_address` is a pure function of `seed`. No shared
//!     mutable state, no I/O, no locks. This is what makes it trivially
//!     parallelizable (and later, kernel-izable).
//!   * **Batch-first** — the trait takes a `&[Candidate]` and returns a
//!     `Vec<DerivedAddress>`. There is no `process_one`.

pub mod cpu;
pub mod gpu;

use crate::types::{Candidate, DerivedAddress, ONION_VERSION};
use curve25519_dalek::{constants::ED25519_BASEPOINT_TABLE, scalar::Scalar};
use sha2::Digest as _;
use sha2::Sha512;
use sha3::Sha3_256;

/// The single seam between the rest of the system and the cryptographic kernel.
///
/// ```ignore
/// trait ComputeBackend {
///     fn process_batch(&self, batch: &[Candidate]) -> Vec<DerivedAddress>;
/// }
/// ```
///
/// Implementors today: [`cpu::CpuComputeBackend`].
/// Implementors tomorrow: [`gpu::FutureGpuComputeBackend`].
pub trait ComputeBackend: Send + Sync {
    /// Human-readable backend name (shown in the dashboard).
    fn name(&self) -> &str;

    /// Derive every candidate in the batch. Must be a pure, order-preserving
    /// map: `out[i]` corresponds to `batch[i]`.
    fn process_batch(&self, batch: &[Candidate]) -> Vec<DerivedAddress>;
}

/// RFC 4648 base32 alphabet, lowercase (the onion encoding).
const B32_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// The pure compute kernel for a single candidate.
///
/// This is the function that will be ported to a GPU kernel. It does:
///   1. SHA-512 expand the seed (ed25519 key clamping).
///   2. Scalar-multiply the basepoint to get the public key.
///   3. SHA3-256 checksum + base32 encode to form the onion host.
///
/// Steps 1–2 dominate the cost and are the prime GPU candidates; step 3 is
/// cheap and could stay host-side even after the GPU port.
#[inline]
pub fn derive_address(c: &Candidate) -> DerivedAddress {
    let public_key = public_key_from_seed(&c.seed);
    let onion = encode_onion(&public_key);
    DerivedAddress {
        seed: c.seed,
        public_key,
        onion,
        worker_id: c.worker_id,
        counter: c.counter,
    }
}

/// Ed25519 public key from a 32-byte seed.
///
/// Note: the clamped scalar is reduced mod the group order before
/// multiplication. Since the basepoint has order `L`, `(a mod L) * B == a * B`,
/// so the resulting public-key point is identical to a reference ed25519
/// implementation.
#[inline]
pub fn public_key_from_seed(seed: &[u8; 32]) -> [u8; 32] {
    let h = Sha512::digest(seed);
    let mut a = [0u8; 32];
    a.copy_from_slice(&h[..32]);
    // ed25519 clamping.
    a[0] &= 248;
    a[31] &= 127;
    a[31] |= 64;

    let scalar = Scalar::from_bytes_mod_order(a);
    let point = ED25519_BASEPOINT_TABLE * &scalar;
    point.compress().to_bytes()
}

/// Tor-style 64-byte *expanded* secret key (`hs_ed25519_secret_key` payload):
/// `clamp(SHA512(seed)[..32]) || SHA512(seed)[32..]`.
///
/// Only computed on a match (rare), so it stays out of the hot path.
pub fn expanded_secret_key(seed: &[u8; 32]) -> [u8; 64] {
    let h = Sha512::digest(seed);
    let mut out = [0u8; 64];
    out.copy_from_slice(&h);
    out[0] &= 248;
    out[31] &= 127;
    out[31] |= 64;
    out
}

/// Encode a public key as a 56-char v3 onion host (no `.onion` suffix).
#[inline]
pub fn encode_onion(pubkey: &[u8; 32]) -> String {
    // checksum = SHA3-256(".onion checksum" || pubkey || version)[..2]
    let mut hasher = Sha3_256::new();
    hasher.update(b".onion checksum");
    hasher.update(pubkey);
    hasher.update([ONION_VERSION]);
    let checksum = hasher.finalize();

    // address bytes = pubkey(32) || checksum(2) || version(1) = 35 bytes
    let mut data = [0u8; 35];
    data[..32].copy_from_slice(pubkey);
    data[32] = checksum[0];
    data[33] = checksum[1];
    data[34] = ONION_VERSION;

    base32_lower(&data)
}

/// RFC 4648 base32 (lowercase, no padding).
#[inline]
fn base32_lower(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for &b in data {
        buffer = (buffer << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = (buffer >> bits) & 0x1f;
            out.push(B32_ALPHABET[idx as usize] as char);
        }
    }
    if bits > 0 {
        let idx = (buffer << (5 - bits)) & 0x1f;
        out.push(B32_ALPHABET[idx as usize] as char);
    }
    out
}

#[inline]
pub fn derive_pubkey(seed: &[u8; 32]) -> [u8; 32] {
    public_key_from_seed(seed)
}

#[inline]
pub fn matches_prefix_fast(pubkey: &[u8; 32], prefix: &[u8]) -> bool {
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    let mut prefix_idx = 0;

    for &b in pubkey {
        buffer = (buffer << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = (buffer >> bits) & 0x1f;
            if prefix[prefix_idx] != B32_ALPHABET[idx as usize] {
                return false;
            }
            prefix_idx += 1;
            if prefix_idx == prefix.len() {
                return true;
            }
        }
    }
    
    let mut hasher = Sha3_256::new();
    hasher.update(b".onion checksum");
    hasher.update(pubkey);
    hasher.update([ONION_VERSION]);
    let checksum = hasher.finalize();
    
    let data = [checksum[0], checksum[1], ONION_VERSION];
    for &b in &data {
        buffer = (buffer << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = (buffer >> bits) & 0x1f;
            if prefix[prefix_idx] != B32_ALPHABET[idx as usize] {
                return false;
            }
            prefix_idx += 1;
            if prefix_idx == prefix.len() {
                return true;
            }
        }
    }
    
    if bits > 0 {
        let idx = (buffer << (5 - bits)) & 0x1f;
        if prefix[prefix_idx] != B32_ALPHABET[idx as usize] {
            return false;
        }
        prefix_idx += 1;
        if prefix_idx == prefix.len() {
            return true;
        }
    }
    
    false
}

#[inline]
pub fn derive_match(pubkey: &[u8; 32], prefixes: &[Vec<u8>]) -> bool {
    for p in prefixes {
        if matches_prefix_fast(pubkey, p) {
            return true;
        }
    }
    false
}

#[inline]
pub fn finalize_match(c: &Candidate, pubkey: &[u8; 32]) -> DerivedAddress {
    DerivedAddress {
        seed: c.seed,
        public_key: *pubkey,
        onion: encode_onion(pubkey),
        worker_id: c.worker_id,
        counter: c.counter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onion_is_56_chars_and_valid_base32() {
        let pk = public_key_from_seed(&[7u8; 32]);
        let onion = encode_onion(&pk);
        assert_eq!(onion.len(), crate::types::ONION_BASE32_LEN);
        assert!(onion.bytes().all(|b| B32_ALPHABET.contains(&b)));
    }

    #[test]
    fn matches_rfc8032_test_vector_1() {
        // RFC 8032, Section 7.1, Test 1.
        let seed = crate::util::from_hex::<32>(
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
        )
        .unwrap();
        let pk = public_key_from_seed(&seed);
        assert_eq!(
            crate::util::to_hex(&pk),
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
        );
    }

    #[test]
    fn derivation_is_deterministic() {
        let c = Candidate { seed: [1u8; 32], worker_id: 0, counter: 0 };
        assert_eq!(derive_address(&c).onion, derive_address(&c).onion);
    }
}
