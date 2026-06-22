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

use super::{derive_pubkey, derive_match, finalize_match, ComputeBackend, EdBackend};
use crate::types::{Candidate, DerivedAddress};
use rayon::prelude::*;
use rayon::ThreadPool;

const BACKEND: EdBackend = EdBackend::Libsodium;

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
        let prefixes = crate::matcher::PREFIXES.get().expect("Prefixes not initialized");
        
        self.pool.install(|| {
            batch.par_iter().filter_map(|c| {
                let pk = derive_pubkey(&c.seed, BACKEND);
                if derive_match(&pk, prefixes) {
                    Some(finalize_match(c, &pk))
                } else {
                    None
                }
            }).collect()
        })
    }
}
