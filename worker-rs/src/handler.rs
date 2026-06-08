use anyhow::{bail, Context, Result};
use redis::{aio::ConnectionManager, AsyncCommands};
use serde::Deserialize;
use tracing::info;

use crate::softmax::PartialStats;

// ── Wire format ───────────────────────────────────────────────────────────────
//
// 0x01 — partial stats (standard two-pass mode):
//   [0x01][f64 local_max LE][f64 partial_sum LE][u32 n LE][f32×n exp_values LE]
//
// 0x02 — pre-normalized (NCCL AllReduce mode):
//   [0x02][u32 n LE][f32×n probabilities LE]
//
// Little-endian, matching Python's struct.pack('<ddI{n}f', ...).

const MAGIC_PARTIAL: u8 = 0x01;

#[allow(dead_code)]
const MAGIC_NORMALIZED: u8 = 0x02;

#[derive(Deserialize)]
pub struct TaskMessage {
    pub job_id: String,
    pub slice_id: usize,
    pub task: String,
    pub data: Vec<f64>,
}

pub fn encode_partial(stats: &PartialStats) -> Vec<u8> {
    let n = stats.exp_values.len() as u32;
    let mut buf = Vec::with_capacity(1 + 8 + 8 + 4 + n as usize * 4);
    buf.push(MAGIC_PARTIAL);
    buf.extend_from_slice(&stats.local_max.to_le_bytes());
    buf.extend_from_slice(&stats.partial_sum.to_le_bytes());
    buf.extend_from_slice(&n.to_le_bytes());
    for &v in &stats.exp_values {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    buf
}

#[allow(dead_code)]
pub fn encode_normalized(probs: &[f32]) -> Vec<u8> {
    let n = probs.len() as u32;
    let mut buf = Vec::with_capacity(1 + 4 + n as usize * 4);
    buf.push(MAGIC_NORMALIZED);
    buf.extend_from_slice(&n.to_le_bytes());
    for &v in probs {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    buf
}

/// Process one raw JSON message from any transport.
///
/// GPU path (default): partial softmax via cudarc, kernels JIT-compiled at
/// startup. Runs inside `tokio::task::spawn_blocking` to avoid stalling the
/// async runtime during CUDA calls.
///
/// CPU fallback (--no-default-features): pure Rust f64 computation.
pub async fn handle_task(raw: &[u8], redis: &ConnectionManager) -> Result<()> {
    let msg: TaskMessage =
        serde_json::from_slice(raw).context("deserialize task message")?;

    if msg.task != "softmax" {
        bail!("unknown task '{}' — only 'softmax' is supported", msg.task);
    }
    if msg.data.is_empty() {
        bail!("empty data slice for job {} slice {}", msg.job_id, msg.slice_id);
    }

    let stats = compute_partial(msg.data).await?;
    let payload = encode_partial(&stats);
    write_result(redis, &msg.job_id, msg.slice_id, &payload).await?;

    info!(
        job_id  = %&msg.job_id[..msg.job_id.len().min(8)],
        slice   = msg.slice_id,
        n       = stats.exp_values.len(),
        backend = if cfg!(feature = "cuda") { "GPU" } else { "CPU" },
        "slice done"
    );
    Ok(())
}

// ── Compute dispatch ─────────────────────────────────────────────────────────

async fn compute_partial(data: Vec<f64>) -> Result<PartialStats> {
    #[cfg(feature = "cuda")]
    {
        // CUDA calls are synchronous — offload to blocking thread pool so we
        // don't stall the tokio async runtime.
        tokio::task::spawn_blocking(move || {
            let data_f32: Vec<f32> = data.iter().map(|&x| x as f32).collect();
            crate::gpu_worker::global().partial_softmax(&data_f32)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    #[cfg(not(feature = "cuda"))]
    {
        Ok(crate::softmax::softmax_partial(&data))
    }
}

// ── Redis write + pub/sub notification ──────────────────────────────────────

pub async fn write_result(
    redis: &ConnectionManager,
    job_id: &str,
    slice_id: usize,
    payload: &[u8],
) -> Result<()> {
    let mut conn = redis.clone();
    let result_key = format!("result:{}:{}", job_id, slice_id);
    let pub_channel = format!("slice_done:{}", job_id);

    conn.set::<_, _, ()>(&result_key, payload)
        .await
        .with_context(|| format!("Redis SET {}", result_key))?;

    conn.publish::<_, _, ()>(&pub_channel, slice_id.to_string())
        .await
        .with_context(|| format!("Redis PUBLISH {}", pub_channel))?;

    Ok(())
}
