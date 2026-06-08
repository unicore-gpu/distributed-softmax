# Distributed Softmax Service

[English](README.md) · [中文](README.zh.md) · **日本語**

プロダクション品質の分散ソフトマックス計算サービスです。  
ベクトルはスライスに分割され、GPU ワーカーへディスパッチされ、数値的に正確な 2 パスアルゴリズムで集約されます。これはベクトル全体に対してソフトマックスを実行するのと等価です。

> **Pure-Rust ランタイム。** 現行のゲートウェイとワーカーは `gateway-rs/` と `worker-rs/`（`cargo build --release` でビルド）です。`server/` にあるレガシーの C++ ゲートウェイは参照用としてディスク上に残されているのみで、サポート対象のデプロイ経路には含まれません。ゲートウェイが使用する GPU 集約カーネルは `gateway-rs/kernels/` にあり、NVRTC を介して JIT コンパイルされます。

---

## アーキテクチャ

```
Client (gRPC)
     ↓
Gateway Server (Rust / tonic)
     ↓  TRANSPORT=zmq      →  ZMQ PUSH  →  Worker (Rust)  [round-robin]
     ↓  TRANSPORT=zmq_nccl →  ZMQ PUSH per rank  →  Worker (Rust)  [rank-pinned, NCCL AllReduce]
     ↑                                                       ↓ Redis pub/sub
     └──────────── GetResult ←────── Redis (result store) ←──┘
```

ゲートウェイとワーカーはいずれも純粋な Rust バイナリです。

### トランスポートモード

| モード | エンドポイント | 適した用途 |
|------|----------|----------|
| `TRANSPORT=zmq` | `ipc:///tmp/softmax_tasks` または `tcp://` | 一般用途 — 同一マシンでは IPC、マシン間では TCP |
| `TRANSPORT=zmq_nccl` | `ipc:///tmp/softmax_nccl_N` または `tcp://` | マルチ GPU の NCCL AllReduce — ランクごとに 1 ソケット |

### マルチマシン対応

本コードベースは `zmq_nccl` トランスポートを介したマルチノード・マルチ GPU デプロイメントに完全対応しています。

- **ゲートウェイ**（任意のノード）: ランクごとに 1 つの TCP ZMQ PUSH ソケットをバインドします（`tcp://0.0.0.0:{ZMQ_BASE_PORT+rank}`）
- **ワーカー**（任意のノード）: `ZMQ_GATEWAY_ADDR=<gateway-ip>` を設定すると、各ランクが自身のエンドポイントを自動的に導出します
- **Redis**: 全ノードで共有され、NCCL UID のランデブーと結果ストレージの両方に使用されます
- **NCCL**: コミュニケータが初期化されると、AllReduce は完全に GPU 間リンク（NVLink または InfiniBand 上の RDMA）で実行されます

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

異なるノードにまたがるワーカーは Redis ランデブーを介して互いを発見し（ランク 0 が NCCL 固有 ID を生成し、他のランクはそれが現れるまでポーリングします）、その後 NCCL コミュニケータを初期化して、共に ZMQ プルループに入ります。

**ネットワーク要件**: ノード間は直接 TCP/IP で接続できる必要があります。NCCL のデータプレーンは独自のピアツーピア接続を確立し（通常は InfiniBand または利用可能な最速の NIC を使用）、Redis や ZMQ を経由しません。

---

## クイックスタート（単一マシン）

### 1. システム依存関係

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

### 2. ゲートウェイとワーカーのビルド

```bash
cargo build --release --manifest-path gateway-rs/Cargo.toml
cargo build --release --manifest-path  worker-rs/Cargo.toml
# Binaries: gateway-rs/target/release/gateway
#           worker-rs/target/release/worker
```

`build.rs` は `tonic-build` を自動的に実行し、`proto/vector_service.proto` から gRPC スタブを生成します。手動での `protoc` ステップは不要です。

ゲートウェイで GPU アクセラレーション付き集約を有効にするには、`--features cuda` を付けてビルドします（後述の [GPU アクセラレーション付き集約](#gpu-accelerated-aggregation-phase-2-optional) を参照）。

### 3. サービスの起動

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

## Docker（完全に分離されたコンテナ）

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

`docker-compose.full.yml` は 3 つのコンテナを起動します。
- `redis` — `redis:7-alpine`
- `gateway` — Rust ゲートウェイ（`Dockerfile.gateway-rs`）。ランタイムは `libzmq5` のみ
- ワーカーは GPU ノード上で別途実行されます（`Dockerfile.worker-rs`）。ランタイムは `libzmq5` のみ

---

## 環境変数

### ゲートウェイ

| 変数 | デフォルト | 説明 |
|----------|---------|-------------|
| `TRANSPORT` | `zmq` | `zmq` または `zmq_nccl` |
| `GATEWAY_ADDR` | `0.0.0.0:50051` | gRPC のリッスンアドレス |
| `ZMQ_PUSH_ENDPOINT` | `ipc:///tmp/softmax_tasks` | ZMQ バインドエンドポイント（`TRANSPORT=zmq` 時に使用） |
| `ZMQ_BASE_ENDPOINT` | `ipc:///tmp/softmax` | NCCL マルチソケットモードのベースエンドポイント |
| `NCCL_WORLD_SIZE` | `4` | GPU ランク数（`TRANSPORT=zmq_nccl` 時に使用） |
| `REDIS_HOST` | `localhost` | Redis ホスト名 |
| `REDIS_PORT` | `6379` | Redis ポート |
| `REDIS_PASSWORD` | _(none)_ | Redis AUTH パスワード |
| `NUM_SLICES` | `4` | 各入力ベクトルを分割するスライス数 |
| `SLICE_TIMEOUT_MS` | `30000` | ワーカースライスの最大待機時間（ms） |
| `RUST_LOG` | `info` | ログ詳細度（`info`、`debug`、`warn`、`error`） |

### ワーカー（worker-rs）

| 変数 | デフォルト | 説明 |
|----------|---------|-------------|
| `TRANSPORT` | `zmq` | `zmq` または `zmq_nccl` — ゲートウェイと一致させる必要があります |
| `ZMQ_PULL_ENDPOINT` | `ipc:///tmp/softmax_tasks` | ZMQ PULL の接続エンドポイント |
| `ZMQ_GATEWAY_ADDR` | _(none)_ | マルチマシン TCP モードでのゲートウェイ IP |
| `ZMQ_BASE_PORT` | `5560` | NCCL ランク別 TCP エンドポイントのベースポート |
| `NCCL_RANK` | `-1` | GPU ランク（`zmq_nccl` では必須） |
| `NCCL_WORLD_SIZE` | `4` | GPU ランクの総数 |
| `REDIS_HOST` | `localhost` | Redis ホスト名 |
| `REDIS_PORT` | `6379` | Redis ポート |
| `NUM_WORKERS` | `4` | 同時に処理中となるスライスの最大数 |
| `CUDA_DEVICE` | `NCCL_RANK` と同じ | GPU デバイスインデックス |
| `RUST_LOG` | `info` | ログ詳細度 |

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

### クイックスモークテスト（`grpcurl`）

```bash
grpcurl -plaintext -import-path proto -proto vector_service.proto \
  -d '{"job_id": "job-1", "task": "softmax", "vector": [1.0, 2.0, 3.0, 4.0, 5.0]}' \
  localhost:50051 vector.VectorService/SubmitTask
# → returns probabilities summing to 1.0 directly in the response
```

gRPC スキーマは [`proto/vector_service.proto`](proto/vector_service.proto) にあります。`protoc` を使えば任意の言語でスタブを生成できます（Rust ユーザーは `gateway-rs/build.rs` を介して自動的に取得します）。

---

## 設計上の判断

### 数値的に正確な分散ソフトマックス

ワーカーは正規化済みソフトマックスではなく、部分統計量 `{exp_values, local_max, partial_sum}` を返します。  
ゲートウェイは 2 パスアルゴリズムで集約します。

```
global_max  = max(local_max_i  for all slices)
adjust_i    = exp(local_max_i - global_max)
global_sum  = sum(partial_sum_i * adjust_i)
result[i,j] = exp_values[i][j] * adjust_i / global_sum
```

これは、連結したベクトル全体に対するソフトマックスと数学的に等価です（検証済み: FP64 参照実装に対して最大誤差 < 1e-8）。

### ゲートウェイの並行性モデル

ゲートウェイは **tokio の非同期タスク** 上に構築されています。各 `SubmitTask` 呼び出しは、スライスを待つ間（`tokio::sync::watch` を介して）OS スレッドを占有せずにサスペンドします。これにより、クライアントには同期 API を提供しつつ、小さなスレッド予算で高い並行性を実現します。

ZMQ ソケットは Rust では `!Send` であるため、各ソケットは専用の `std::thread` 上に存在し、バウンドな `mpsc` チャネルを介してペイロードを転送します。これにより、unsafe コードなしでバックプレッシャーが維持されます。

### GPU アクセラレーション付き集約（フェーズ 2、オプション） {#gpu-accelerated-aggregation-phase-2-optional}

Linux + CUDA マシン上で `--features cuda` を付けてビルドすると、[cudarc](https://github.com/chelsea0x3b/cudarc) を介した GPU 集約が有効になります。

```bash
cargo build --release --features cuda
```

CPU と GPU の処理分担:

| ステップ | 実行場所 | 計算量 |
|------|-------|-----------|
| `global_max = max(local_max_i)` | CPU | O(num_slices) ≈ O(4) |
| `adjust_i = exp(local_max_i − global_max)` | CPU | O(num_slices) |
| `global_sum = Σ partial_sum_i × adjust_i` | CPU | O(num_slices) |
| `out[k] = exp_vals[k] × adjust[slice_of[k]] / global_sum` | **GPU** | O(total_elements) |

GPU に載せる価値があるのはステップ 4 のみです。これはベクトルサイズ（数百万要素）に応じてスケールしますが、ステップ 1〜3 は O(4) で CPU 上でも自明に高速です。カーネル（`gateway-rs/kernels/aggregate_softmax.cu`）は NVRTC を介して実行時に JIT コンパイルされるため、ビルド時に `nvcc` は不要です。

`--features cuda` を付けない場合（または起動時に CUDA デバイスが検出されない場合）、ゲートウェイは透過的に CPU パスへフォールバックします。

### ZMQ と ZMQ NCCL の比較

- **ZMQ**（`TRANSPORT=zmq`）: ゲートウェイが PUSH ソケットをバインドし、ワーカーが PULL ソケットで接続します。ZMQ はタスクを接続中の全ワーカーへラウンドロビンで分配します。同一マシンでは IPC（`ipc://`）、マシン間では TCP（`tcp://`）を使用します。ブローカーは不要です。
- **ZMQ NCCL**（`TRANSPORT=zmq_nccl`）: GPU ランクごとに専用の PUSH ソケットを 1 つ用意します。スライス `i` は常に GPU `i` へ配信され、NCCL AllReduce が要求する順序を保証します。マルチマシンデプロイメントでは、各ワーカーに `ZMQ_GATEWAY_ADDR` を設定すると TCP エンドポイントが自動的に導出されます。

### ワーカーの並行性モデル

- ZMQ ソケットは専用の `std::thread` 上に存在します（ZMQ ソケットは `!Send`）。受信したフレームはアンバウンドな `tokio::mpsc` チャネルへ転送されます。
- 各メッセージは `tokio::spawn` タスクで処理され、`tokio::sync::Semaphore(NUM_WORKERS)` によって上限が設けられます。1 ワーカープロセスあたり最大 `NUM_WORKERS` 個のスライスが同時進行します。
- 部分ソフトマックス（`softmax_partial`）は、`--features cuda` がオフのときは素の Rust `f64` 演算です。この機能を有効にすると、`worker-rs/kernels/partial_softmax.cu` 内のスライス単位の CUDA カーネルが NVRTC を介して JIT コンパイルされ、`cudarc` を介してディスパッチされます。

### Redis pub/sub による結果通知

ワーカーは各結果を書き込んだ後、`slice_done:{job_id}` へパブリッシュします。  
ゲートウェイの `SliceNotifier` は専用の非同期接続で `slice_done:*` をサブスクライブし、待機中の集約タスクを `tokio::sync::watch` チャネルを介してウェイクアップします。ポーリングも OS スレッドのブロックもありません。

---

## プロジェクト構成

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

## マルチマシンデプロイメントの例

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

あるいは、`ZMQ_GATEWAY_ADDR` + `ZMQ_BASE_PORT` によって各ワーカーの `tcp://{addr}:{base+rank}` が自動導出されるため、Node B のワーカーはランクごとの `ZMQ_PULL_ENDPOINT` の代わりにこの 2 つの変数だけで済みます。

---

## 検証環境

| 環境 | 結果 |
|-------------|--------|
| Ubuntu 24.04, CUDA 12.4, 4× RTX 4090, `zmq_nccl` world_size=4 | ✅ NCCL AllReduce、すべての合計 = 1.0 |
| Ubuntu 24.04, CUDA 13.1, 2× A100 SXM (NVLink12), `zmq_nccl` world_size=2 | ✅ N=1k–262k、すべての合計 = 1.000000 |
| FP64 参照に対する数学的正確性 | ✅ 最大誤差 < 1e-8 |
| ワーカーのリトライ（ワーカーの kill + 再起動） | ✅ 自動復旧 |
| 10 件の同時ジョブ、ZMQ モード | ✅ すべての合計 = 1.0 |
| マルチノード（2 ノード、直接 IP ルーティング） | ✅ 設計上サポート — ノード間の TCP 到達性が必要 |
