# A100 SXM 4× Benchmark Report — NCCL AllReduce Mode

**Machine**: 4× NVIDIA A100-SXM4-40 GB, 12-link NVLink (NV12 full mesh, ~600 GB/s aggregate per GPU)
**Host**: Vast.ai, Ubuntu 24.04, 128 vCPU / 503 GB RAM
**Software**: Driver 595.71.05, CUDA 13.2, NCCL 2.29.7, Redis 7.0
**Stack**: pure-Rust — `gateway-rs` (gateway) + `worker-rs` (4 workers), built with `cargo build --release`
**Transport**: `TRANSPORT=zmq_nccl` (dedicated per-rank ZMQ PUSH/PULL + NCCL AllReduce over NVLink)
**Load tool**: [`ghz`](https://ghz.sh) 0.120, single gRPC connection, unique `job_id` per request
**Date**: 2026-06-08

> These numbers were measured on the current pure-Rust runtime (`gateway-rs` + `worker-rs`).
> The worker GPU path (`worker-rs`, `cudarc` + NVRTC + NCCL) was built with the default
> `--features cuda`. The gateway was built with `--no-default-features`: in `zmq_nccl` mode
> the gateway performs **no GPU math** — workers run the NCCL AllReduce and return fully
> normalized slices, and the gateway only concatenates them — so its GPU aggregation path is
> not exercised here.

---

## 1. System Configuration

| Component | Details |
|---|---|
| GPUs | 4× A100-SXM4-40 GB, 12-link NVLink (NV12 full mesh) |
| Gateway | Rust (`gateway-rs`), tonic/gRPC, `ZmqMultiPublisher` (one PUSH socket per rank) |
| Workers | 4× Rust (`worker-rs`), one per GPU (`CUDA_DEVICE=$RANK`) |
| Compute | `cudarc` — per-slice CUDA kernels JIT-compiled via NVRTC, NCCL AllReduce over NVLink |
| Storage | Redis pub/sub + result store (binary encoding) |
| Aggregation | Gateway concatenates pre-normalized slices — **no reduction math on the gateway** |

### NCCL Transport Architecture

```
Client ──gRPC──▶ Gateway (Rust / tonic)
                    │
          ┌─────────┼─────────┐─────────┐
         ZMQ       ZMQ       ZMQ       ZMQ
        PUSH0     PUSH1     PUSH2     PUSH3
          │         │         │         │
       Worker0   Worker1   Worker2   Worker3
       GPU 0     GPU 1     GPU 2     GPU 3
          │         │         │         │
          └──── NCCL AllReduce (NVLink) ─┘
                    │
              Redis (result store)
                    │
               Gateway concatenates
                    │
              gRPC response ──▶ Client
```

**Key property**: each rank processes slices in the same order — guaranteed by the dedicated
per-rank ZMQ queues — which is required for NCCL collective semantics. The distributed softmax
is numerically stable: workers exchange a global max and a global denominator via AllReduce
(the two-pass algorithm, run across ranks instead of on a single device).

---

## 2. Deployment

```bash
# Build (on the A100 machine)
cargo build --release --no-default-features --manifest-path gateway-rs/Cargo.toml   # CPU gateway
cargo build --release                        --manifest-path worker-rs/Cargo.toml   # GPU workers

# Redis
redis-server --daemonize yes --save '' --maxmemory 16gb --maxmemory-policy noeviction

# Gateway
TRANSPORT=zmq_nccl NCCL_WORLD_SIZE=4 REDIS_HOST=localhost \
  ./gateway-rs/target/release/gateway &

# 4 workers, one per GPU
for RANK in 0 1 2 3; do
  TRANSPORT=zmq_nccl NCCL_RANK=$RANK NCCL_WORLD_SIZE=4 CUDA_DEVICE=$RANK \
    ZMQ_PULL_ENDPOINT=ipc:///tmp/softmax_${RANK} REDIS_HOST=localhost \
    ./worker-rs/target/release/worker &
done
```

> **NCCL note**: `cudarc` 0.19's dynamic loader does not search for `libnccl.so.2` directly.
> On a host that only ships the runtime package, symlink it once:
> `ln -sf /lib/x86_64-linux-gnu/libnccl.so.2 /lib/x86_64-linux-gnu/libnccl.so && ldconfig`.

---

## 3. Correctness Verification

Each vector is sliced across the 4 ranks; workers AllReduce and return normalized
probabilities; the gateway concatenates. Output sums verified against a unit-sum reference:

| Vector size | Output sum | Status |
|---|---|---|
| 32      | 0.99999996 | ✅ PASS |
| 1 024   | 1.00000005 | ✅ PASS |
| 4 096   | 1.00000005 | ✅ PASS |
| 65 536  | 1.00000001 | ✅ PASS |

Deviation from 1.0 is within `float32` rounding (< 1e-6).

> **Slice-count constraint**: `zmq_nccl` requires every job to split into exactly
> `NCCL_WORLD_SIZE` (= 4) slices, since all ranks must participate in each AllReduce.
> Vector lengths that do not divide into 4 non-empty slices (e.g. very short vectors) leave a
> rank without data and stall the collective. All sizes below are multiples of 4.

---

## 4. Benchmark Results (`zmq_nccl` mode)

Single gRPC connection; `conc` = in-flight requests; GPUs warmed before measurement; latency in ms.

| Config | p50 | p90 | p99 | min | max | RPS | Errors |
|---|---|---|---|---|---|---|---|
| 1K elements, conc=1   |  2.26 |  2.77 |  4.33 | 1.30 |   6.00 |   181.6 | 0 |
| 1K elements, conc=8   |  5.09 |  9.12 | 11.58 | 1.54 |  19.17 |   695.9 | 0 |
| 1K elements, conc=16  |  5.60 |  7.24 |  8.94 | 1.68 |  26.60 | 1 072.4 | 0 |
| 4K elements, conc=1   |  2.89 |  3.84 |  4.92 | 1.94 |   6.80 |    69.3 | 0 |
| 4K elements, conc=8   |  5.75 |  9.71 | 12.03 | 2.05 |  17.62 |   312.0 | 0 |
| 64K elements, conc=1  | 18.35 | 20.51 | 22.41 | 9.84 | 117.02 |     5.3 | 0 |
| 64K elements, conc=4  | 27.24 | 89.81 | 98.66 |13.66 | 115.55 |    16.2 | 0 |

**0 errors across ~54 000 requests.** (A separate duration-based pass also ran clean apart from
in-flight requests cut off at the timer boundary, which `ghz` reports as connection closures —
a harness artifact, not a service error.)

### 4.1 Analysis

- **Small-vector latency is dominated by round-trip overhead**, not GPU compute: a 1K-element
  softmax is microseconds of math, so p50 (~2.3 ms at conc=1) reflects gRPC + ZMQ + NCCL
  handshake, not FLOPs. Throughput scales with concurrency to ~1 070 RPS (1K, conc=16) on a
  single gateway connection.
- **Ranks process jobs sequentially** to preserve NCCL AllReduce ordering. This bounds
  small-vector throughput but keeps the collective correct and the error rate at zero.
- **64K vectors are latency-bound by the collective**: p50 ~18 ms at conc=1. At conc=4 the
  sequential rank processing serializes the four in-flight jobs, inflating the tail
  (p90 ~90 ms) while raising aggregate RPS — the expected queuing trade-off.
- For production workloads with large embeddings (LLM hidden states, recommendation features),
  NVLink AllReduce is the right architecture: GPU-to-GPU bandwidth (~600 GB/s) far exceeds any
  Redis/CPU aggregation path.

### 4.2 Methodology Notes

- Load generated with `ghz` over a single gRPC connection; each request carries a unique
  `job_id` (`{{.RequestNumber}}`) to avoid Redis key collisions.
- GPUs were warmed (a 5 000-request burst) before measurement. Clock-locking was unavailable
  (the Vast.ai container denies `nvidia-smi -lgc`); without warmup, idle-time DVFS downclocking
  adds a multi-ms tail to low-concurrency runs.
- Numbers are from the current Rust stack and are **not** directly comparable to any earlier
  C++/Python measurements — different runtime, different client, different CUDA/NCCL versions.

---

## 5. Multi-Machine

Multi-node, multi-GPU deployment is supported by the same `zmq_nccl` transport (gateway on one
host, workers on any number of hosts; ZMQ switches IPC→TCP automatically; cross-machine NCCL
uses `NCCL_SOCKET_IFNAME` over InfiniBand/Ethernet). See the **Multi-machine** sections of
[`README.md`](README.md) for the deployment commands and network requirements.

This run measured a single 4×A100 node only; multi-machine numbers were not re-collected here.

---

## 6. Next Steps

1. **Re-collect multi-machine numbers** on a direct-RDMA (InfiniBand/RoCE) fabric.
2. **Pipeline parallelism** — process the next job while the current AllReduce is in flight.
3. **Persistent CUDA streams** — reuse streams across jobs instead of allocating per job.
4. **Repair the gateway GPU aggregation path** (`gpu_aggregator.rs`) against `cudarc` 0.19.4
   so plain-`zmq` mode can also offload normalization to the GPU (see note below).

> **Build caveat**: `gateway-rs/src/gpu_aggregator.rs` (the optional GPU aggregation used only in
> plain `zmq` mode) does not currently compile against the pinned `cudarc` 0.19.4 — the source
> targets an older `cudarc` API (`LaunchArgs::arg`/`PushKernelArg`, `memcpy_dtoh`,
> `Arc<CudaContext>`). It does not affect `zmq_nccl` mode or the worker GPU path, both of which
> build and run cleanly.
