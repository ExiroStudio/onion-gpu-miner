//! CPU compute backend — the current milestone.
//!
//! Parallelism comes from a *dedicated* rayon pool sized to `--workers`, so the
//! number of OS threads doing crypto is explicit and bounded (no reliance on
//! the global pool, no oversubscription with the controller's own threads).
//!
//! `process_batch` is the literal seam described in the spec:
//! ```text
//! loop {
//!     generate_batch()   // generator/worker layer
//!     process_batch()    // <-- here
//!     compare()          // matcher layer
//! }
//! ```

use super::{derive_address, ComputeBackend};
use crate::types::{Candidate, DerivedAddress};
use rayon::prelude::*;
use rayon::ThreadPool;

pub struct CpuComputeBackend {
    pool: ThreadPool,
}

impl CpuComputeBackend {
    pub fn new(workers: usize) -> Self {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .thread_name(|i| format!("compute-{i}"))
            .build()
            .expect("failed to build rayon compute pool");
        Self { pool }
    }
}

impl ComputeBackend for CpuComputeBackend {
    fn name(&self) -> &str {
        "cpu-rayon"
    }

    fn process_batch(&self, batch: &[Candidate]) -> Vec<DerivedAddress> {
        // ===================================================================
        // GPU MIGRATION SEAM
        // -------------------------------------------------------------------
        // Today: fan the batch across the CPU pool with a data-parallel map.
        //
        // On GPU this becomes roughly:
        //   1. copy `batch` seeds into a pinned host buffer
        //   2. cudaMemcpyAsync -> device
        //   3. launch `ed25519_derive<<<grid, block>>>(d_seeds, d_pubkeys)`
        //      (one GPU thread per candidate; SHA-512 + scalar-mult on device)
        //   4. copy pubkeys back (or run the cheap onion encode on-device too)
        //
        // The signature and the rest of the program do not change.
        // ===================================================================
        self.pool
            .install(|| batch.par_iter().map(derive_address).collect())
    }
}
