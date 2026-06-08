mod handler;
mod softmax;
mod transport;

#[cfg(feature = "cuda")]
mod gpu_worker;

use anyhow::{bail, Result};
use redis::aio::ConnectionManager;
use tracing::info;
use tracing_subscriber::EnvFilter;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

struct Config {
    transport: String,

    redis_host: String,
    redis_port: u16,

    zmq_pull_endpoint: String,

    zmq_gateway_addr: String,
    zmq_base_port: u16,
    nccl_rank: i32,
    nccl_world_size: u32,

    num_workers: usize,

    /// Which GPU device index to use (default: same as NCCL_RANK, else 0).
    cuda_device: usize,
}

impl Config {
    fn from_env() -> Self {
        let nccl_rank = env_i32("NCCL_RANK", -1);
        let zmq_gateway_addr = std::env::var("ZMQ_GATEWAY_ADDR").unwrap_or_default();
        let zmq_base_port = env_u16("ZMQ_BASE_PORT", 5560);

        let zmq_pull_endpoint = if !zmq_gateway_addr.is_empty() && nccl_rank >= 0 {
            format!("tcp://{}:{}", zmq_gateway_addr, zmq_base_port + nccl_rank as u16)
        } else {
            std::env::var("ZMQ_PULL_ENDPOINT")
                .unwrap_or_else(|_| "ipc:///tmp/softmax_tasks".into())
        };

        // CUDA_DEVICE overrides; fall back to NCCL_RANK so each worker uses its own GPU.
        let cuda_device = std::env::var("CUDA_DEVICE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| if nccl_rank >= 0 { nccl_rank as usize } else { 0 });

        Config {
            transport: std::env::var("TRANSPORT").unwrap_or_else(|_| "zmq".into()),

            redis_host: std::env::var("REDIS_HOST").unwrap_or_else(|_| "localhost".into()),
            redis_port: env_u16("REDIS_PORT", 6379),

            zmq_pull_endpoint,
            zmq_gateway_addr,
            zmq_base_port,
            nccl_rank,
            nccl_world_size: env_u32("NCCL_WORLD_SIZE", 4),

            num_workers: env_usize("NUM_WORKERS", 4),
            cuda_device,
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cfg = Config::from_env();

    info!(
        transport = %cfg.transport,
        redis     = %format!("{}:{}", cfg.redis_host, cfg.redis_port),
        backend   = if cfg!(feature = "cuda") { "GPU/CUDA" } else { "CPU" },
        "worker-rs starting"
    );

    // ── Redis connection ──────────────────────────────────────────────────────
    let redis_url = format!("redis://{}:{}", cfg.redis_host, cfg.redis_port);
    let redis = ConnectionManager::new(redis::Client::open(redis_url.as_str())?)
        .await
        .expect("Redis connection failed");
    info!("Redis connected");

    // ── GPU initialization (standard path) ────────────────────────────────────
    // For zmq_nccl the NCCL worker owns its own device; for other transports
    // we initialize the global GpuWorker here so it is ready before we start
    // accepting tasks.
    #[cfg(feature = "cuda")]
    if cfg.transport != "zmq_nccl" {
        gpu_worker::init(cfg.cuda_device)?;
        info!(device = cfg.cuda_device, "GPU worker ready");
    }

    // ── Transport dispatch ────────────────────────────────────────────────────
    match cfg.transport.as_str() {
        "zmq" => {
            info!(endpoint = %cfg.zmq_pull_endpoint, "transport: ZMQ PULL");
            transport::zmq::run(
                transport::zmq::ZmqConfig {
                    endpoint: cfg.zmq_pull_endpoint,
                    max_concurrent: cfg.num_workers,
                },
                redis,
            )
            .await?;
        }

        "zmq_nccl" => {
            let rank = cfg.nccl_rank;
            if rank < 0 {
                bail!("NCCL_RANK must be >= 0 for zmq_nccl transport");
            }
            let rank = rank as usize;
            let world_size = cfg.nccl_world_size as usize;

            let endpoint = if !cfg.zmq_gateway_addr.is_empty() {
                format!(
                    "tcp://{}:{}",
                    cfg.zmq_gateway_addr,
                    cfg.zmq_base_port + rank as u16
                )
            } else {
                cfg.zmq_pull_endpoint.clone()
            };

            info!(
                rank       = rank,
                world_size = world_size,
                endpoint   = %endpoint,
                device     = cfg.cuda_device,
                "transport: ZMQ NCCL"
            );

            run_nccl(rank, world_size, endpoint, redis).await?;
        }

        other => bail!("unknown TRANSPORT '{}' (valid: zmq | zmq_nccl)", other),
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// NCCL AllReduce loop
// ---------------------------------------------------------------------------
//
// Sequential processing — all ranks must call AllReduce in the same job order.
// The per-rank ZMQ PUSH sockets on the gateway guarantee this ordering.

#[cfg(feature = "cuda")]
async fn run_nccl(
    rank: usize,
    world_size: usize,
    endpoint: String,
    redis: ConnectionManager,
) -> Result<()> {
    // Redis rendezvous — async, uses tokio.
    let nccl_id = gpu_worker::nccl_rendezvous(rank, world_size, &redis).await?;

    // Build NCCL communicator in a blocking thread (CUDA init is synchronous).
    let nccl = tokio::task::spawn_blocking(move || {
        gpu_worker::NcclWorker::new(rank, world_size, nccl_id)
    })
    .await?
    .map_err(|e| anyhow::anyhow!("NCCL init: {}", e))?;

    info!(rank, world_size, "NCCL communicator ready — starting ZMQ PULL loop");

    // Hand off to a blocking thread: ZMQ recv + NCCL AllReduce + Redis write.
    // `block_on` bridges back to async for Redis writes.
    let rt = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let ctx = zmq::Context::new();
        let sock = ctx.socket(zmq::PULL).map_err(|e| anyhow::anyhow!("ZMQ socket: {}", e))?;
        sock.connect(&endpoint)
            .map_err(|e| anyhow::anyhow!("ZMQ connect {}: {}", endpoint, e))?;

        info!(rank, endpoint = %endpoint, "ZMQ PULL connected (NCCL mode)");

        loop {
            let raw = sock.recv_bytes(0).map_err(|e| anyhow::anyhow!("ZMQ recv: {}", e))?;

            let msg: handler::TaskMessage = match serde_json::from_slice(&raw) {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!("JSON parse error (skipping): {}", e);
                    continue;
                }
            };

            if msg.task != "softmax" {
                tracing::warn!(task = %msg.task, "unknown task — skipping");
                continue;
            }

            let data_f32: Vec<f32> = msg.data.iter().map(|&x| x as f32).collect();

            // GPU AllReduce — blocks until all ranks complete.
            let probs = nccl.all_reduce_softmax(&data_f32)?;

            let payload = handler::encode_normalized(&probs);
            rt.block_on(handler::write_result(&redis, &msg.job_id, msg.slice_id, &payload))?;

            info!(
                rank,
                job_id  = %&msg.job_id[..msg.job_id.len().min(8)],
                slice   = msg.slice_id,
                n       = probs.len(),
                "NCCL slice done"
            );
        }
    })
    .await??;

    Ok(())
}

// Without CUDA, zmq_nccl falls back to the standard CPU path on the per-rank endpoint.
#[cfg(not(feature = "cuda"))]
async fn run_nccl(
    rank: usize,
    _world_size: usize,
    endpoint: String,
    redis: ConnectionManager,
) -> Result<()> {
    tracing::warn!(
        rank,
        "zmq_nccl selected but compiled without --features cuda; \
         falling back to CPU partial-softmax (gateway performs aggregation)"
    );
    transport::zmq::run(
        transport::zmq::ZmqConfig {
            endpoint,
            max_concurrent: 1,
        },
        redis,
    )
    .await
}

// ---------------------------------------------------------------------------
// env helpers
// ---------------------------------------------------------------------------

fn env_u16(k: &str, d: u16) -> u16 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}
fn env_i32(k: &str, d: i32) -> i32 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}
fn env_u32(k: &str, d: u32) -> u32 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}
fn env_usize(k: &str, d: usize) -> usize {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}
