//! Generator / worker layer.
//!
//! Produces batches of [`Candidate`]s. Seed generation is **deterministic**: a
//! candidate is fully determined by `(master_seed, worker_id, counter)`. That
//! property is what makes checkpoint/resume trivial — we only need to persist
//! the per-worker counters.
//!
//! The generator holds the only mutable state (the counters) and is owned by a
//! single thread, so there is no shared mutable state and no locking.

use crate::types::Candidate;

/// Deterministically derive a 32-byte seed from `(master, worker_id, counter)`.
///
/// We overwrite the low bytes of the master seed with the worker id + counter.
/// This is cheap (no hashing in the generator hot path) and collision-free
/// across the `(worker_id, counter)` space, while the remaining master bytes
/// keep different runs independent.
///
/// NOTE: this lives in the generator, *not* the compute kernel — keeping it
/// here means the GPU port never has to reproduce the seeding scheme.
#[inline]
pub fn derive_seed(master: &[u8; 32], worker_id: u32, counter: u64) -> [u8; 32] {
    let mut seed = *master;
    seed[0..4].copy_from_slice(&worker_id.to_le_bytes());
    seed[4..12].copy_from_slice(&counter.to_le_bytes());
    seed
}

/// Stateful, single-threaded batch generator.
pub struct Generator {
    master: [u8; 32],
    workers: u32,
    /// Next counter value for each logical worker. This is the checkpoint state.
    counters: Vec<u64>,
}

impl Generator {
    pub fn new(master: [u8; 32], workers: u32, counters: Vec<u64>) -> Self {
        debug_assert_eq!(counters.len(), workers as usize);
        Self {
            master,
            workers,
            counters,
        }
    }

    /// Fill `buf` with `batch_size` fresh candidates, advancing the counters.
    ///
    /// Work is round-robined across logical workers so each worker's counter
    /// space stays contiguous and resumable.
    pub fn fill_batch(&mut self, batch_size: usize, buf: &mut Vec<Candidate>) {
        buf.clear();
        buf.reserve(batch_size);
        for i in 0..batch_size {
            let w = (i as u32) % self.workers;
            let counter = self.counters[w as usize];
            self.counters[w as usize] += 1;
            buf.push(Candidate {
                seed: derive_seed(&self.master, w, counter),
                worker_id: w,
                counter,
            });
        }
    }

    /// Snapshot of the per-worker counters, for checkpointing.
    pub fn counters(&self) -> &[u64] {
        &self.counters
    }

    pub fn master(&self) -> [u8; 32] {
        self.master
    }
}
