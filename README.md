# Distributed Softmax Service

**English** · [中文](README.zh.md) · [日本語](README.ja.md)

A production-grade distributed softmax computation service.  
Vectors are sliced, dispatched to GPU workers, and aggregated using a numerically correct two-pass algorithm — equivalent to running softmax on the full vector.

> **Pure-Rust runtime.** The active gateway and worker are `gateway-rs/` and `worker-rs/` (built with `cargo build --release`). The legacy C++ gateway in `server/` is kept on disk for reference only and is no longer part of the supported deployment path. GPU aggregation kernels used by the gateway live in `gateway-rs/kernels/` and are JIT-compiled via NVRTC.

---

## Architecture

```
Client (gRPC)
     ↓
Gateway Server (Rust / tonic)
     ↓  TRANSPORT=zmq      →  ZMQ PUSH  →  Worker (Rust)  [round-robin]
     ↓  TRANSPORT=zmq_nccl →  ZMQ PUSH per rank  →  Worker (Rust)  [rank-pinned, NCCL AllReduce]
     ↑                                                       ↓ Redis pub/sub
     └──────────── GetResult ←────── Redis (result store) ←──┘
```

Both gateway and worker are pure Rust binaries.

### Transport modes

| Mode | Endpoint | Best for |
|------|----------|----------|
| `TRANSPORT=zmq` | `ipc:///tmp/softmax_tasks` or `tcp://` | General use — IPC for same-machine, TCP for cross-machine |
| `TRANSPORT=zmq_nccl` | `ipc:///tmp/softmax_nccl_N` or `tcp://` | Multi-GPU NCCL AllReduce — one socket per rank |

### Multi-machine support

The codebase fully supports multi-node multi-GPU deployments via the `zmq_nccl` transport:

- **Gateway** (any node): binds one TCP ZMQ PUSH socket per rank (`tcp://0.0.0.0:{ZMQ_BASE_PORT+rank}`)
- **Workers** (any node): set `ZMQ_GATEWAY_ADDR=<gateway-ip>` and each rank auto-derives its own endpoint
- **Redis**: shared across all nodes; used for both NCCL UID rendezvous and result storage
- **NCCL**: once communicators are initialized, AllReduce runs entirely on GPU-to-GPU links (NVLink or RDMA over InfiniBand)

```
Node A (Orchestrator)               Node B
┌────────────────────┐              ┌──────────────────┐
│  Gateway (gRPC)    │              │                  │
│  Redis             │◄─────────────│  Worker rank 2   │
│  Worker rank 0     │  NCCL        │  Worker rank 3   │
│  Worker rank 1     │◄────────────►│                  │
└────────────────────┘  (NVLink /   └──────────────────┘
                         InfiniBand)
```

Workers across nodes discover each other via the Redis rendezvous (rank 0 generates the NCCL unique ID; other ranks poll until it appears), then initialize the NCCL communicator and enter the ZMQ pull loop together.

**Network requirement**: nodes must have direct TCP/IP connectivity. NCCL's data plane establishes its own peer-to-peer connections (typically over InfiniBand or the fastest available NIC) and does not go through Redis or ZMQ.

---

## Quick Start (single machine)

### 1. System dependencies

```bash
# Rust toolchain (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# System packages
apt-get install -y \
  protobuf-compiler \
  libzmq3-dev libzmq5 \
  redis-server
```

### 2. Build gateway and worker

```bash
cargo build --release --manifest-path gateway-rs/Cargo.toml
cargo build --release --manifest-path  worker-rs/Cargo.toml
# Binaries: gateway-rs/target/release/gateway
#           worker-rs/target/release/worker
```

`build.rs` runs `tonic-build` automatically to generate gRPC stubs from `proto/vector_service.proto` — no manual `protoc` step needed.

To enable GPU-accelerated aggregation in the gateway, build with `--features cuda` (see [GPU-accelerated aggregation](#gpu-accelerated-aggregation-phase-2-optional) below).

### 3. Start services

```bash
# Infrastructure
redis-server --daemonize yes --save ''

# Rust gateway
TRANSPORT=zmq ZMQ_PUSH_ENDPOINT=ipc:///tmp/softmax_tasks \
  REDIS_HOST=localhost RUST_LOG=info \
  ./gateway-rs/target/release/gateway &

# Rust worker — must match gateway transport
TRANSPORT=zmq ZMQ_PULL_ENDPOINT=ipc:///tmp/softmax_tasks \
  REDIS_HOST=localhost NUM_WORKERS=4 RUST_LOG=info \
  ./worker-rs/target/release/worker &
```

---

## Docker (fully separated containers)

```bash
# Build images
docker build -f Dockerfile.gateway-rs -t gateway-rs .
docker build -f Dockerfile.worker-rs  -t worker-rs  .

# Coordinator node (no GPU required)
docker-compose -f docker-compose.full.yml up -d

# Worker node — run on each GPU machine
COORDINATOR_IP=<coordinator-ip>
docker run -d --gpus all \
  -e TRANSPORT=zmq \
  -e ZMQ_PULL_ENDPOINT=tcp://${COORDINATOR_IP}:5560 \
  -e REDIS_HOST=${COORDINATOR_IP} \
  worker-rs:latest
```

`docker-compose.full.yml` runs three containers:
- `redis` — `redis:7-alpine`
- `gateway` — Rust gateway (`Dockerfile.gateway-rs`); runtime: `libzmq5` only
- Workers run separately on GPU nodes (`Dockerfile.worker-rs`); runtime: `libzmq5` only

---

## Environment Variables

### Gateway

| Variable | Default | Description |
|----------|---------|-------------|
| `TRANSPORT` | `zmq` | `zmq` or `zmq_nccl` |
| `GATEWAY_ADDR` | `0.0.0.0:50051` | gRPC listen address |
| `ZMQ_PUSH_ENDPOINT` | `ipc:///tmp/softmax_tasks` | ZMQ bind endpoint (used when `TRANSPORT=zmq`) |
| `ZMQ_BASE_ENDPOINT` | `ipc:///tmp/softmax` | Base endpoint for NCCL multi-socket mode |
| `NCCL_WORLD_SIZE` | `4` | Number of GPU ranks (used when `TRANSPORT=zmq_nccl`) |
| `REDIS_HOST` | `localhost` | Redis hostname |
| `REDIS_PORT` | `6379` | Redis port |
| `REDIS_PASSWORD` | _(none)_ | Redis AUTH password |
| `NUM_SLICES` | `4` | How many slices to split each input vector into |
| `SLICE_TIMEOUT_MS` | `30000` | Max wait time for worker slices (ms) |
| `RUST_LOG` | `info` | Log verbosity (`info`, `debug`, `warn`, `error`) |

### Worker (worker-rs)

| Variable | Default | Description |
|----------|---------|-------------|
| `TRANSPORT` | `zmq` | `zmq` or `zmq_nccl` — must match gateway |
| `ZMQ_PULL_ENDPOINT` | `ipc:///tmp/softmax_tasks` | ZMQ PULL connect endpoint |
| `ZMQ_GATEWAY_ADDR` | _(none)_ | Gateway IP for multi-machine TCP mode |
| `ZMQ_BASE_PORT` | `5560` | Base port for NCCL per-rank TCP endpoints |
| `NCCL_RANK` | `-1` | GPU rank (required for `zmq_nccl`) |
| `NCCL_WORLD_SIZE` | `4` | Total number of GPU ranks |
| `REDIS_HOST` | `localhost` | Redis hostname |
| `REDIS_PORT` | `6379` | Redis port |
| `NUM_WORKERS` | `4` | Max concurrent in-flight slices |
| `CUDA_DEVICE` | same as `NCCL_RANK` | GPU device index |
| `RUST_LOG` | `info` | Log verbosity |

---

## API

### SubmitTask

```protobuf
rpc SubmitTask(TaskRequest) returns (TaskResponse)

message TaskRequest {
  string job_id    = 1;  // unique identifier
  string task      = 2;  // "softmax"
  repeated float vector = 3;
}
```

### GetResult

```protobuf
rpc GetResult(ResultRequest) returns (ResultResponse)

// status: "pending" | "running" | "ready" | "failed"
```

### Quick smoke test (`grpcurl`)

```bash
grpcurl -plaintext -import-path proto -proto vector_service.proto \
  -d '{"job_id": "job-1", "task": "softmax", "vector": [1.0, 2.0, 3.0, 4.0, 5.0]}' \
  localhost:50051 vector.VectorService/SubmitTask
# → returns probabilities summing to 1.0 directly in the response
```

The gRPC schema lives in [`proto/vector_service.proto`](proto/vector_service.proto); generate stubs in your language of choice with `protoc` (Rust users get them automatically via `gateway-rs/build.rs`).

---

## Design Decisions

### Numerically correct distributed softmax

Workers return partial statistics `{exp_values, local_max, partial_sum}` instead of normalized softmax.  
The gateway aggregates using the two-pass algorithm:

```
global_max  = max(local_max_i  for all slices)
adjust_i    = exp(local_max_i - global_max)
global_sum  = sum(partial_sum_i * adjust_i)
result[i,j] = exp_values[i][j] * adjust_i / global_sum
```

This is mathematically equivalent to softmax over the full concatenated vector (verified: max error < 1e-8 vs an FP64 reference implementation).

### Gateway concurrency model

The gateway is built on **tokio async tasks** — each `SubmitTask` call suspends while waiting for slices (via `tokio::sync::watch`) without holding an OS thread. This gives a synchronous API to clients while supporting high concurrency on a small thread budget.

ZMQ sockets are `!Send` in Rust, so each socket lives on a dedicated `std::thread` and forwards payloads via a bounded `mpsc` channel — backpressure is preserved without unsafe code.

### GPU-accelerated aggregation (Phase 2, optional) {#gpu-accelerated-aggregation-phase-2-optional}

Build with `--features cuda` on a Linux + CUDA machine to enable GPU aggregation via [cudarc](https://github.com/chelsea0x3b/cudarc):

```bash
cargo build --release --features cuda
```

Work split between CPU and GPU:

| Step | Where | Complexity |
|------|-------|-----------|
| `global_max = max(local_max_i)` | CPU | O(num_slices) ≈ O(4) |
| `adjust_i = exp(local_max_i − global_max)` | CPU | O(num_slices) |
| `global_sum = Σ partial_sum_i × adjust_i` | CPU | O(num_slices) |
| `out[k] = exp_vals[k] × adjust[slice_of[k]] / global_sum` | **GPU** | O(total_elements) |

Only step 4 is worth putting on GPU — it scales with vector size (millions of elements), while steps 1–3 are O(4) and trivially fast on CPU. The kernel (`gateway-rs/kernels/aggregate_softmax.cu`) is JIT-compiled at runtime via NVRTC — no `nvcc` needed at build time.

Without `--features cuda` (or if no CUDA device is detected at startup), the gateway falls back to the CPU path transparently.

### ZMQ vs ZMQ NCCL

- **ZMQ** (`TRANSPORT=zmq`): gateway binds a PUSH socket; workers connect PULL sockets. ZMQ distributes tasks round-robin across all connected workers. Use IPC (`ipc://`) for same-machine, TCP (`tcp://`) for cross-machine. No broker required.
- **ZMQ NCCL** (`TRANSPORT=zmq_nccl`): one dedicated PUSH socket per GPU rank — slice `i` is always delivered to GPU `i`, guaranteeing the ordering required by NCCL AllReduce. Set `ZMQ_GATEWAY_ADDR` on each worker for automatic TCP endpoint derivation in multi-machine deployments.

### Worker concurrency model

- The ZMQ socket lives on a dedicated `std::thread` (ZMQ sockets are `!Send`). Received frames are forwarded into an unbounded `tokio::mpsc` channel.
- Each message is processed in a `tokio::spawn` task, bounded by a `tokio::sync::Semaphore(NUM_WORKERS)` — at most `NUM_WORKERS` slices in-flight per worker process.
- Partial softmax (`softmax_partial`) is plain Rust `f64` math when `--features cuda` is off; with the feature, the per-slice CUDA kernels in `worker-rs/kernels/partial_softmax.cu` are JIT-compiled via NVRTC and dispatched through `cudarc`.

### Redis pub/sub result notification

Workers publish to `slice_done:{job_id}` after writing each result.  
The gateway's `SliceNotifier` subscribes to `slice_done:*` on a dedicated async connection and wakes the waiting aggregation task via a `tokio::sync::watch` channel — no polling, no OS thread blocked.

---

## Project Structure

```
distributed-softmax/
├── gateway-rs/                       # Rust gateway (replaces server/)
│   ├── Cargo.toml                    # Dependencies: tonic, redis, zmq, tokio, …
│   ├── build.rs                      # tonic-build: proto → Rust stubs (auto)
│   └── src/
│       ├── main.rs                   # Entry point: transport selection, service startup
│       ├── config.rs                 # Redis key naming + TTL helpers
│       ├── redis_manager.rs          # Async Redis client (ConnectionManager)
│       ├── slice_notifier.rs         # Redis pub/sub + tokio::sync::watch
│       ├── aggregator.rs             # Two-pass softmax aggregation
│       ├── service.rs                # tonic VectorService impl (SubmitTask, GetResult)
│       ├── cache_manager.rs          # Background TTL stats task
│       ├── gpu_aggregator.rs         # cudarc GPU aggregation (compiled with --features cuda)
│       └── publisher/
│           ├── mod.rs                # Publisher trait
│           └── zmq.rs                # ZMQ PUSH (single + multi-rank for NCCL)
├── gateway-rs/kernels/
│   └── aggregate_softmax.cu          # GPU normalization kernel (JIT via NVRTC)
├── worker-rs/                        # Rust worker
│   ├── Cargo.toml                    # Dependencies: redis, zmq, tokio, …
│   ├── kernels/
│   │   └── partial_softmax.cu        # Per-slice CUDA kernels (JIT via NVRTC)
│   └── src/
│       ├── main.rs                   # Entry point: transport selection, Redis, startup
│       ├── softmax.rs                # softmax_partial() — plain Rust f64 path
│       ├── handler.rs                # parse JSON, encode 0x01 binary, Redis write
│       └── transport/
│           ├── mod.rs
│           └── zmq.rs                # ZMQ PULL on dedicated std::thread + mpsc bridge
├── server/                           # Legacy C++ gateway (kept for reference, not built)
│   ├── gateway_server.cc
│   └── …                             # CMakeLists, slice_notifier.h, redis_manager.h, etc.
├── proto/
│   └── vector_service.proto          # gRPC service definition (consumed by clients)
├── Dockerfile.gateway-rs             # Rust gateway image (multi-stage, ~15 MB runtime)
├── Dockerfile.worker-rs              # Rust worker image (multi-stage, ~10 MB runtime)
├── Dockerfile.gateway                # Legacy C++ gateway image (kept for reference)
└── docker-compose.full.yml           # Multi-container Rust deployment
```

---

## Multi-machine deployment example

```bash
# Node A — start Redis (bound to 0.0.0.0), gateway, and local ranks
redis-server --bind 0.0.0.0 --protected-mode no --daemonize yes

TRANSPORT=zmq_nccl NCCL_RANK=0 NCCL_WORLD_SIZE=4 \
  ZMQ_PULL_ENDPOINT="tcp://NODE_A_IP:5560" \
  REDIS_HOST=localhost CUDA_DEVICE=0 \
  ./worker-rs/target/release/worker &

TRANSPORT=zmq_nccl NCCL_RANK=1 NCCL_WORLD_SIZE=4 \
  ZMQ_PULL_ENDPOINT="tcp://NODE_A_IP:5561" \
  REDIS_HOST=localhost CUDA_DEVICE=1 \
  ./worker-rs/target/release/worker &

# Node B — workers point to Node A for Redis + ZMQ
TRANSPORT=zmq_nccl NCCL_RANK=2 NCCL_WORLD_SIZE=4 \
  ZMQ_GATEWAY_ADDR=NODE_A_IP ZMQ_BASE_PORT=5560 \
  REDIS_HOST=NODE_A_IP CUDA_DEVICE=0 \
  ./worker-rs/target/release/worker &

TRANSPORT=zmq_nccl NCCL_RANK=3 NCCL_WORLD_SIZE=4 \
  ZMQ_GATEWAY_ADDR=NODE_A_IP ZMQ_BASE_PORT=5560 \
  REDIS_HOST=NODE_A_IP CUDA_DEVICE=1 \
  ./worker-rs/target/release/worker &
```

Alternatively, `ZMQ_GATEWAY_ADDR` + `ZMQ_BASE_PORT` auto-derives `tcp://{addr}:{base+rank}` for each worker, so Node B workers only need those two variables instead of a per-rank `ZMQ_PULL_ENDPOINT`.

---

## Tested on

| Environment | Result |
|-------------|--------|
| Ubuntu 24.04, CUDA 12.4, 4× RTX 4090, `zmq_nccl` world_size=4 | ✅ NCCL AllReduce, all sums = 1.0 |
| Ubuntu 24.04, CUDA 13.1, 2× A100 SXM (NVLink12), `zmq_nccl` world_size=2 | ✅ N=1k–262k, all sums = 1.000000 |
| Math correctness vs FP64 reference | ✅ max error < 1e-8 |
| Worker retry (worker kill + restart) | ✅ automatic recovery |
| 10 concurrent jobs, ZMQ mode | ✅ all sums = 1.0 |
| Multi-node (2 nodes, direct IP routing) | ✅ supported by design — requires TCP reachability between nodes |
