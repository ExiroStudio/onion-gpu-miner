//! Future GPU compute backend — **not implemented** (intentionally).
//!
//! This file exists to nail down the extension point. It implements the exact
//! same [`ComputeBackend`] trait as the CPU backend, so swapping it in is a
//! one-line change in the controller:
//!
//! ```ignore
//! let backend: Arc<dyn ComputeBackend> = match cfg.backend {
//!     Backend::Cpu => Arc::new(CpuComputeBackend::new(cfg.workers)),
//!     Backend::Gpu => Arc::new(FutureGpuComputeBackend::new(/* device cfg */)),
//! };
//! ```
//!
//! Nothing else in the system needs to know which backend is live.

use super::ComputeBackend;
use crate::types::{Candidate, DerivedAddress};

/// Placeholder for a CUDA / Vulkan / wgpu-backed implementation.
///
/// Sketch of the eventual member layout:
/// ```ignore
/// pub struct FutureGpuComputeBackend {
///     device: CudaDevice,
///     stream: CudaStream,
///     kernel: CudaFunction,        // compiled ed25519_derive
///     d_seeds: DeviceBuffer<u8>,   // reused across batches
///     d_pubkeys: DeviceBuffer<u8>,
/// }
/// ```
#[allow(dead_code)]
pub struct FutureGpuComputeBackend {
    _private: (),
}

#[allow(dead_code)]
impl FutureGpuComputeBackend {
    pub fn new() -> Self {
        unimplemented!(
            "GPU compute backend is a future milestone; the CPU backend is the current target"
        )
    }
}

impl ComputeBackend for FutureGpuComputeBackend {
    fn name(&self) -> &str {
        "gpu-future"
    }

    fn process_batch(&self, _batch: &[Candidate]) -> Vec<DerivedAddress> {
        // FUTURE WORK (the whole point of the trait abstraction):
        //
        // 1. Stage seeds: copy `_batch[i].seed` into a contiguous device buffer.
        // 2. Launch the derivation kernel — one GPU thread per candidate:
        //      - SHA-512(seed)            (kernel or on-device libsodium-style)
        //      - clamp + scalar mult B    (field arithmetic in the kernel)
        //      - compress point -> pubkey
        // 3. (Optional) do SHA3-256 checksum + base32 on-device, or just copy
        //    pubkeys back and encode on the host (encode is cheap).
        // 4. Reassemble `DerivedAddress` preserving input order.
        //
        // Crucially: batch size, channels, matcher, metrics, and storage are
        // all unchanged. Only this method's body differs.
        unimplemented!("GPU compute backend not implemented")
    }
}
