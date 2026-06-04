# onion-gpu-miner

A high-throughput **Onion v3 vanity address generator**, CPU-first but
architected so the compute-heavy kernel can migrate to GPU **without changing
the surrounding system**.

> Status: **CPU milestone complete.** GPU is a future milestone — the extension
> points exist (see [`src/compute/gpu.rs`](src/compute/gpu.rs)) but no GPU code
> is implemented.

```
cargo run --release -- \
    --prefix exiro \
    --workers 8 \
    --batch-size 65536
```

```
onion-gpu-miner  [cpu-rayon]
targets: exiro
------------------------------------
Attempts:
    4,210,000/s
    63,150,000 total

Worker:
    8/8 active

Queue:
    98%  (63/64 batches)

Matches:
    0

Elapsed:
    00:00:15
```

> Throughput is hardware-dependent. The screen above is the *shape* of the
> output; real CPU rates are on the order of a few million attempts/sec per
> machine. The 95,000,000/s figure in the brief is the GPU target this
> architecture is built to grow into — see the migration path below.

When a match is found it is written under `out/<host>.onion/` in the standard
Tor hidden-service layout (`hostname`, `hs_ed25519_public_key`,
`hs_ed25519_secret_key`) plus an appended `out/results.jsonl`.

---

## 1. Architecture

Six layers, one per module, each with a single responsibility. Data flows one
way; the only abstraction boundary that matters for the GPU port is the
**compute trait**.

```
                 ┌──────────────────────────────────────────────┐
   --prefix      │                CONTROLLER                     │
   --workers ───▶│  lifecycle · stats aggregation · checkpoints  │
   --batch-size  │  renders dashboard · persists results         │
                 └───────┬───────────────────────────▲───────────┘
                         │ spawns                     │ results (bounded)
            ┌────────────▼─────────┐         ┌────────┴───────────┐
            │  GENERATOR (worker)  │         │   STORAGE / OUTPUT  │
            │ deterministic seeds  │         │ Tor key files +     │
            │ (master,id,counter)  │         │ JSONL · checkpoints │
            └────────────┬─────────┘         └────────▲───────────┘
                         │ Vec<Candidate>             │ MiningResult
                         │ (bounded queue)            │
                         ▼                            │
            ┌──────────────────────────────────────┐ │
            │              COMPUTE                  │ │
            │  trait ComputeBackend (STATELESS)     │ │
            │  ┌─────────────┐   ┌────────────────┐ │ │
            │  │ Cpu (rayon) │   │ FutureGpu (TODO)│ │ │
            │  └─────────────┘   └────────────────┘ │ │
            │  seed → SHA512 → ed25519 pk → onion   │ │
            └──────────────────┬───────────────────┘ │
                               │ Vec<DerivedAddress>  │
                               ▼                      │
            ┌──────────────────────────────────────┐ │
            │               MATCHER                 │─┘
            │  multi-prefix · starts_with           │
            └──────────────────────────────────────┘

            METRICS (Arc, atomics only) — sampled by the controller once/sec
```

### The driving loop

The system is **batch-first**, exactly as specified — never `process_one()`:

```rust
loop {
    let batch = generator.fill_batch(batch_size); // GENERATOR
    let derived = backend.process_batch(&batch);  // COMPUTE  (rayon today, GPU tomorrow)
    let hits = matcher.scan(&derived);            // MATCHER
    // hits -> bounded result queue -> STORAGE
}
```

In the running program this loop is split across two threads connected by a
bounded queue:

* a **generator thread** fills `Vec<Candidate>` batches and pushes them into a
  bounded `crossbeam` channel (backpressure when full);
* a single **compute driver thread** pulls a batch and calls
  `backend.process_batch()`, which fans the work across a rayon pool sized to
  `--workers`.

One driver + one sized pool deliberately mirrors the GPU model — *one host
thread dispatching one large data-parallel kernel per batch* — and avoids
thread oversubscription. Because ed25519 derivation is far slower than seed
generation, the generator outruns compute and the bounded queue sits near full;
that is what the **Queue %** metric reports (it is real backpressure, not a
gauge).

### Core types ([`src/types.rs`](src/types.rs))

| Type | Role |
|------|------|
| `Candidate` | `{ seed, worker_id, counter }` — the unit fed to compute. Only `seed` is cryptographically meaningful; the rest rides along for traceability/checkpointing. |
| `DerivedAddress` | `{ seed, public_key, onion, worker_id, counter }` — compute output. `onion` is the 56-char base32 host (no suffix), matched against prefixes. |
| `MiningResult` | A persisted hit: full host, matched prefix, hex public/secret/seed, worker id, counter. |

### Design principles (how each is honored)

1. **No global mutable state** — `Metrics`, `Matcher`, and the backend are
   passed explicitly via `Arc`. The only mutable state (per-worker counters)
   lives inside the single-threaded `Generator`.
2. **Compute is stateless** — `derive_address(seed)` is a pure function: no I/O,
   no locks, no shared state. That is precisely what makes it parallel- and
   kernel-friendly.
3. **GPU-ready** — everything depends on the `ComputeBackend` trait;
   `CpuComputeBackend` and `FutureGpuComputeBackend` are interchangeable.
4. **Batch-first** — the trait takes `&[Candidate]`; there is no single-item path.
5. **No filesystem writes on the hot path** — the compute driver never touches
   disk. Result and checkpoint writes happen on the controller/generator threads.
6. **Bounded channels** — both the work queue and the result queue are bounded
   `crossbeam` channels.
7. **No async runtime** — plain OS threads + rayon. No tokio, no futures.

### Correctness

`cargo test` checks the ed25519 kernel against **RFC 8032 test vector 1** and
verifies onion hosts are valid 56-char base32. Generated keys use the exact Tor
file formats (64-byte public key file, 96-byte expanded secret key file) and
load directly into a Tor hidden-service directory.

---

## 2. Expected bottlenecks

Ordered by impact on the CPU build:

1. **Ed25519 scalar multiplication (`process_batch`)** — dominant cost by far.
   Each attempt is a SHA-512 + a fixed-base scalar multiply. This is ~99% of CPU
   time and is the entire reason a GPU port exists.
2. **SHA-512 key expansion** — coupled to the above; a second hashing pass that a
   GPU can absorb cheaply.
3. **Per-batch allocation** — the generator allocates a fresh `Vec<Candidate>`
   per batch and compute allocates a `Vec<DerivedAddress>`. Fine at current
   throughput; would become a target once compute is offloaded (use slab/arena
   reuse or pre-allocated device-staging buffers).
4. **Onion encoding (SHA3-256 + base32) + `String` allocation** — currently done
   for *every* candidate. Cheap relative to scalar-mult today, but once compute
   is on GPU this host-side work can dominate. Mitigation: compare prefixes on
   the raw public-key bits and only encode on a near-match.
5. **Matcher** — `starts_with` per candidate. Negligible now; for very large
   prefix sets switch to a trie / Aho-Corasick.
6. **Queue / channel coordination** — minor. The single-driver design keeps
   contention low; the bound provides backpressure rather than a bottleneck.

The architecture is intentionally arranged so that **only bottleneck #1–2 move
to the GPU**, and the seam (`ComputeBackend::process_batch`) is the only code
that changes.

---

## 3. Migration path: CPU → SIMD → GPU → Distributed

The trait boundary makes each step additive — drop in a new `ComputeBackend`
(or a new controller fan-out) without touching the other layers.

**Stage 0 — CPU (today).**
`CpuComputeBackend` maps the batch across a rayon pool. One core ≈ one logical
worker.

**Stage 1 — SIMD.**
Replace the scalar field arithmetic with a batched/vectorized ed25519
implementation (e.g. AVX2/AVX-512 field ops, or curve25519-dalek's
`Scalar`-batch APIs). Process N keys per instruction. *Surface change: none —
still a `CpuComputeBackend`, just a faster inner loop.* Encode-on-near-match
(bottleneck #4) lands here too.

**Stage 2 — GPU.**
Implement `FutureGpuComputeBackend` (CUDA / Vulkan / `wgpu`). `process_batch`:
upload seeds → launch `ed25519_derive` (one thread per candidate: SHA-512 +
clamp + scalar mult + compress) → download public keys → encode/compare. Keep
the cheap onion encode and matching host-side, or push them on-device too.
*Surface change: one line in the controller selecting the backend.* This is
where the ~10⁸/s target becomes reachable.

**Stage 3 — Distributed.**
Promote the controller to a coordinator: shard the `(worker_id, counter)` space
across nodes (the seed scheme is already deterministic and partitioned, so
sharding is just assigning disjoint `worker_id` ranges). Each node runs the
existing pipeline with its local backend; results and checkpoints stream back.
*Surface change: the generator's range assignment + a network result sink — the
compute/matcher/storage layers are untouched.*

```
CPU (rayon)  ──▶  SIMD (vectorized field ops)  ──▶  GPU (kernel dispatch)  ──▶  Distributed (sharded controller)
   trait              trait, faster inner            new trait impl,            coordinator over N nodes,
   impl               loop — no API change           1-line backend swap        deterministic range sharding
```

---

## 4. Benchmark instructions

Always benchmark the **release** build (`opt-level=3`, thin LTO).

### Built-in timed run

`--duration` auto-stops after N seconds; `--max-matches 0` disables early exit
so you measure raw throughput. The final summary prints total attempts and the
average rate:

```
cargo build --release

./target/release/onion-gpu-miner \
    --prefix zzzzzz \          # long/rare prefix so it won't stop early
    --workers $(nproc) \
    --batch-size 65536 \
    --max-matches 0 \
    --duration 30
```

Read `avg rate: …/s` from the summary.

### Sweeps

* **Thread scaling:** fix `--batch-size`, vary `--workers` (1, 2, 4, …, nproc).
  Rate should scale near-linearly until you hit physical cores.
* **Batch size:** fix `--workers`, vary `--batch-size` (4096 → 262144). Larger
  batches amortize dispatch/allocation; gains flatten once the pool is saturated
  and this is the knob that will matter most for GPU occupancy later.
* **Queue depth:** vary `--queue` to confirm the generator keeps the compute
  driver fed (watch the **Queue %** in the dashboard stay high).

### Reproducible runs

Pass a fixed master seed so two runs explore identical candidates:

```
./target/release/onion-gpu-miner --prefix ab --seed $(python3 -c "print('00'*32)") --duration 10 --max-matches 0
```

### Externally timed (wall clock)

```
hyperfine --warmup 1 \
  './target/release/onion-gpu-miner --prefix zzzzzz --workers 8 --max-matches 0 --duration 10'
```

### Profiling the hot path

```
cargo build --release
perf record -g ./target/release/onion-gpu-miner --prefix zzzzzz --max-matches 0 --duration 20
perf report   # expect ~all time in scalar-mult / SHA-512 — confirming bottleneck #1
```

---

## CLI reference

| Flag | Default | Meaning |
|------|---------|---------|
| `--prefix <P>...` | *(required)* | One or more target prefixes (base32 `[a-z2-7]`). Repeatable. |
| `--workers <N>` | # logical CPUs | Crypto threads / logical workers. |
| `--batch-size <N>` | `65536` | Candidates per compute dispatch. |
| `--queue <N>` | `64` | Bounded work-queue depth, in batches. |
| `--max-matches <N>` | `1` | Stop after N matches (`0` = run until `--duration`/Ctrl-C). |
| `--out <DIR>` | `out` | Output directory for matched keys. |
| `--checkpoint <FILE>` | `checkpoint.json` | Checkpoint path. |
| `--resume` | off | Resume counters from an existing checkpoint. |
| `--seed <HEX64>` | random | Fixed 32-byte master seed (64 hex chars). |
| `--duration <SECS>` | none | Auto-stop after N seconds (benchmarking). |

## Layout

```
src/
  main.rs            CLI + config validation
  types.rs           Candidate, DerivedAddress, MiningResult
  controller/        lifecycle, pipeline, dashboard, persistence orchestration
  worker/            deterministic seed + batch generation
  compute/           ComputeBackend trait
    cpu.rs           CpuComputeBackend (rayon)
    gpu.rs           FutureGpuComputeBackend (extension point, unimplemented)
  matcher/           multi-prefix matching
  metrics/           atomic counters (attempts, workers, matches, elapsed)
  storage/           Tor key files + checkpoint save/load
```

## Security / intended use

This is a vanity *address* generator for Onion v3 services you control. It
produces standard Tor key material for hidden services you operate. It does not
attack or impersonate existing services — a vanity prefix is cosmetic and brute
forcing a *full* 56-char address is computationally infeasible.
