# 分布式 Softmax 服务

[English](README.md) · **中文** · [日本語](README.ja.md)

一个生产级的分布式 softmax 计算服务。
向量被切分、分发到 GPU worker，并使用数值正确的两遍（two-pass）算法进行聚合——其结果等价于在完整向量上运行 softmax。

> **纯 Rust 运行时。** 当前使用的 gateway 和 worker 是 `gateway-rs/` 与 `worker-rs/`（通过 `cargo build --release` 构建）。`server/` 中遗留的 C++ gateway 仅作为参考保留在磁盘上，已不再属于受支持的部署路径。gateway 使用的 GPU 聚合内核位于 `gateway-rs/kernels/`，通过 NVRTC 进行 JIT 编译。

---

## 架构

```
Client (gRPC)
     ↓
Gateway Server (Rust / tonic)
     ↓  TRANSPORT=zmq      →  ZMQ PUSH  →  Worker (Rust)  [round-robin]
     ↓  TRANSPORT=zmq_nccl →  ZMQ PUSH per rank  →  Worker (Rust)  [rank-pinned, NCCL AllReduce]
     ↑                                                       ↓ Redis pub/sub
     └──────────── GetResult ←────── Redis (result store) ←──┘
```

gateway 和 worker 都是纯 Rust 二进制程序。

### 传输模式

| 模式 | 端点 | 适用场景 |
|------|----------|----------|
| `TRANSPORT=zmq` | `ipc:///tmp/softmax_tasks` 或 `tcp://` | 通用场景——同机使用 IPC，跨机使用 TCP |
| `TRANSPORT=zmq_nccl` | `ipc:///tmp/softmax_nccl_N` 或 `tcp://` | 多 GPU NCCL AllReduce——每个 rank 一个 socket |

### 多机支持

本代码库通过 `zmq_nccl` 传输模式完整支持多节点多 GPU 部署：

- **Gateway**（任意节点）：为每个 rank 绑定一个 TCP ZMQ PUSH socket（`tcp://0.0.0.0:{ZMQ_BASE_PORT+rank}`）
- **Workers**（任意节点）：设置 `ZMQ_GATEWAY_ADDR=<gateway-ip>`，每个 rank 会自动推导出自己的端点
- **Redis**：在所有节点间共享；用于 NCCL UID 会合（rendezvous）和结果存储
- **NCCL**：通信器初始化完成后，AllReduce 完全在 GPU 到 GPU 的链路上运行（NVLink 或 InfiniBand 上的 RDMA）

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

跨节点的 worker 通过 Redis 会合机制互相发现（rank 0 生成 NCCL unique ID；其他 rank 轮询直到它出现），随后初始化 NCCL 通信器并一起进入 ZMQ 拉取循环。

**网络要求**：节点之间必须具备直接的 TCP/IP 连通性。NCCL 的数据平面会建立自己的点对点连接（通常通过 InfiniBand 或可用的最快 NIC），不经过 Redis 或 ZMQ。

---

## 快速开始（单机）

### 1. 系统依赖

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

### 2. 构建 gateway 和 worker

```bash
cargo build --release --manifest-path gateway-rs/Cargo.toml
cargo build --release --manifest-path  worker-rs/Cargo.toml
# Binaries: gateway-rs/target/release/gateway
#           worker-rs/target/release/worker
```

`build.rs` 会自动运行 `tonic-build`，从 `proto/vector_service.proto` 生成 gRPC stub——无需手动执行 `protoc`。

要在 gateway 中启用 GPU 加速聚合，请使用 `--features cuda` 进行构建（参见下文 [GPU 加速聚合](#gpu-accelerated-aggregation-phase-2-optional)）。

### 3. 启动服务

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

## Docker（完全分离的容器）

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

`docker-compose.full.yml` 运行三个容器：
- `redis` — `redis:7-alpine`
- `gateway` — Rust gateway（`Dockerfile.gateway-rs`）；运行时仅需 `libzmq5`
- Workers 在 GPU 节点上单独运行（`Dockerfile.worker-rs`）；运行时仅需 `libzmq5`

---

## 环境变量

### Gateway

| 变量 | 默认值 | 描述 |
|----------|---------|-------------|
| `TRANSPORT` | `zmq` | `zmq` 或 `zmq_nccl` |
| `GATEWAY_ADDR` | `0.0.0.0:50051` | gRPC 监听地址 |
| `ZMQ_PUSH_ENDPOINT` | `ipc:///tmp/softmax_tasks` | ZMQ 绑定端点（当 `TRANSPORT=zmq` 时使用） |
| `ZMQ_BASE_ENDPOINT` | `ipc:///tmp/softmax` | NCCL 多 socket 模式的基础端点 |
| `NCCL_WORLD_SIZE` | `4` | GPU rank 数量（当 `TRANSPORT=zmq_nccl` 时使用） |
| `REDIS_HOST` | `localhost` | Redis 主机名 |
| `REDIS_PORT` | `6379` | Redis 端口 |
| `REDIS_PASSWORD` | _(none)_ | Redis AUTH 密码 |
| `NUM_SLICES` | `4` | 每个输入向量切分成多少个切片 |
| `SLICE_TIMEOUT_MS` | `30000` | 等待 worker 切片的最长时间（毫秒） |
| `RUST_LOG` | `info` | 日志详细程度（`info`、`debug`、`warn`、`error`） |

### Worker (worker-rs)

| 变量 | 默认值 | 描述 |
|----------|---------|-------------|
| `TRANSPORT` | `zmq` | `zmq` 或 `zmq_nccl`——必须与 gateway 匹配 |
| `ZMQ_PULL_ENDPOINT` | `ipc:///tmp/softmax_tasks` | ZMQ PULL 连接端点 |
| `ZMQ_GATEWAY_ADDR` | _(none)_ | 多机 TCP 模式下的 gateway IP |
| `ZMQ_BASE_PORT` | `5560` | NCCL 每 rank TCP 端点的基础端口 |
| `NCCL_RANK` | `-1` | GPU rank（`zmq_nccl` 模式下必填） |
| `NCCL_WORLD_SIZE` | `4` | GPU rank 总数 |
| `REDIS_HOST` | `localhost` | Redis 主机名 |
| `REDIS_PORT` | `6379` | Redis 端口 |
| `NUM_WORKERS` | `4` | 最大并发处理中的切片数 |
| `CUDA_DEVICE` | 同 `NCCL_RANK` | GPU 设备索引 |
| `RUST_LOG` | `info` | 日志详细程度 |

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

### 快速冒烟测试（`grpcurl`）

```bash
grpcurl -plaintext -import-path proto -proto vector_service.proto \
  -d '{"job_id": "job-1", "task": "softmax", "vector": [1.0, 2.0, 3.0, 4.0, 5.0]}' \
  localhost:50051 vector.VectorService/SubmitTask
# → returns probabilities summing to 1.0 directly in the response
```

gRPC schema 位于 [`proto/vector_service.proto`](proto/vector_service.proto)；可使用 `protoc` 生成你所选语言的 stub（Rust 用户通过 `gateway-rs/build.rs` 自动获得）。

---

## 设计决策

### 数值正确的分布式 softmax

Worker 返回部分统计量 `{exp_values, local_max, partial_sum}`，而非归一化后的 softmax。
gateway 使用两遍（two-pass）算法进行聚合：

```
global_max  = max(local_max_i  for all slices)
adjust_i    = exp(local_max_i - global_max)
global_sum  = sum(partial_sum_i * adjust_i)
result[i,j] = exp_values[i][j] * adjust_i / global_sum
```

这在数学上等价于对完整拼接向量执行的 softmax（已验证：相对 FP64 参考实现的最大误差 < 1e-8）。

### Gateway 并发模型

gateway 构建于 **tokio 异步任务** 之上——每个 `SubmitTask` 调用在等待切片时会挂起（通过 `tokio::sync::watch`），而不占用操作系统线程。这为客户端提供了同步式 API，同时能在很小的线程预算下支持高并发。

ZMQ socket 在 Rust 中是 `!Send` 的，因此每个 socket 运行在专用的 `std::thread` 上，并通过有界 `mpsc` channel 转发负载——在不使用 unsafe 代码的前提下保留了背压（backpressure）。

### GPU 加速聚合（Phase 2，可选） {#gpu-accelerated-aggregation-phase-2-optional}

在 Linux + CUDA 机器上使用 `--features cuda` 构建，即可通过 [cudarc](https://github.com/chelsea0x3b/cudarc) 启用 GPU 聚合：

```bash
cargo build --release --features cuda
```

CPU 与 GPU 之间的工作分配：

| 步骤 | 位置 | 复杂度 |
|------|-------|-----------|
| `global_max = max(local_max_i)` | CPU | O(num_slices) ≈ O(4) |
| `adjust_i = exp(local_max_i − global_max)` | CPU | O(num_slices) |
| `global_sum = Σ partial_sum_i × adjust_i` | CPU | O(num_slices) |
| `out[k] = exp_vals[k] × adjust[slice_of[k]] / global_sum` | **GPU** | O(total_elements) |

只有第 4 步值得放到 GPU 上——它随向量大小（数百万个元素）扩展，而第 1–3 步为 O(4)，在 CPU 上极快即可完成。该内核（`gateway-rs/kernels/aggregate_softmax.cu`）在运行时通过 NVRTC 进行 JIT 编译——构建时无需 `nvcc`。

如果不使用 `--features cuda`（或启动时未检测到 CUDA 设备），gateway 会透明地回退到 CPU 路径。

### ZMQ 与 ZMQ NCCL

- **ZMQ**（`TRANSPORT=zmq`）：gateway 绑定一个 PUSH socket；worker 连接 PULL socket。ZMQ 以 round-robin 方式将任务分发到所有已连接的 worker。同机使用 IPC（`ipc://`），跨机使用 TCP（`tcp://`）。无需 broker。
- **ZMQ NCCL**（`TRANSPORT=zmq_nccl`）：每个 GPU rank 一个专用 PUSH socket——切片 `i` 始终被投递到 GPU `i`，从而保证 NCCL AllReduce 所要求的顺序。在多机部署中，在每个 worker 上设置 `ZMQ_GATEWAY_ADDR` 即可自动推导 TCP 端点。

### Worker 并发模型

- ZMQ socket 运行在专用的 `std::thread` 上（ZMQ socket 是 `!Send` 的）。接收到的帧会被转发到一个无界的 `tokio::mpsc` channel 中。
- 每条消息在一个 `tokio::spawn` 任务中处理，并由 `tokio::sync::Semaphore(NUM_WORKERS)` 进行限流——每个 worker 进程最多有 `NUM_WORKERS` 个切片同时在处理中。
- 当 `--features cuda` 关闭时，部分 softmax（`softmax_partial`）使用纯 Rust 的 `f64` 运算；启用该特性后，`worker-rs/kernels/partial_softmax.cu` 中的每切片 CUDA 内核会通过 NVRTC 进行 JIT 编译，并经由 `cudarc` 进行调度。

### Redis pub/sub 结果通知

Worker 在写入每个结果后向 `slice_done:{job_id}` 发布消息。
gateway 的 `SliceNotifier` 在专用异步连接上订阅 `slice_done:*`，并通过 `tokio::sync::watch` channel 唤醒等待中的聚合任务——无轮询，也不阻塞任何操作系统线程。

---

## 项目结构

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

## 多机部署示例

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

另外，`ZMQ_GATEWAY_ADDR` + `ZMQ_BASE_PORT` 会为每个 worker 自动推导出 `tcp://{addr}:{base+rank}`，因此 Node B 的 worker 只需设置这两个变量，而无需为每个 rank 单独指定 `ZMQ_PULL_ENDPOINT`。

---

## 测试环境

| 环境 | 结果 |
|-------------|--------|
| Ubuntu 24.04, CUDA 12.4, 4× RTX 4090, `zmq_nccl` world_size=4 | ✅ NCCL AllReduce，所有和 = 1.0 |
| Ubuntu 24.04, CUDA 13.1, 2× A100 SXM (NVLink12), `zmq_nccl` world_size=2 | ✅ N=1k–262k，所有和 = 1.000000 |
| 相对 FP64 参考实现的数学正确性 | ✅ 最大误差 < 1e-8 |
| Worker 重试（杀死 worker 后重启） | ✅ 自动恢复 |
| 10 个并发任务，ZMQ 模式 | ✅ 所有和 = 1.0 |
| 多节点（2 个节点，直接 IP 路由） | ✅ 设计上支持——需要节点间 TCP 可达 |
