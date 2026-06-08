/// ResultAggregator — port of C++ `ResultAggregator` + `aggregateSoftmax`.
///
/// Slice binary formats (produced by Python workers):
///   0x01 — partial stats (standard two-pass):
///     [0x01][f64 local_max][f64 partial_sum][u32 n][f32×n exp_values]
///   0x02 — pre-normalized (NCCL AllReduce):
///     [0x02][u32 n][f32×n probabilities]   ← gateway just concatenates
///   Fallback — JSON {"local_max":…,"partial_sum":…,"exp_values":[…]}
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tracing::{error, info, warn};

use crate::config::RedisConfig;
use crate::redis_manager::RedisManager;
use crate::slice_notifier::SliceNotifier;

#[cfg(feature = "cuda")]
use crate::gpu_aggregator::GpuAggregator;

/// Partial softmax statistics returned by each worker slice.
/// Exposed `pub(crate)` so `gpu_aggregator` can consume the same type.
pub(crate) struct SliceStats {
    /// exp(x_i - local_max) for standard mode, or final probabilities for NCCL mode.
    pub(crate) exp_values: Vec<f64>,
    pub(crate) local_max: f64,
    pub(crate) partial_sum: f64,
    /// true → NCCL mode (values are already normalized probabilities).
    pub(crate) is_normalized: bool,
}

impl Default for SliceStats {
    fn default() -> Self {
        Self {
            exp_values: Vec::new(),
            local_max: 0.0,
            partial_sum: 1.0,
            is_normalized: false,
        }
    }
}

pub struct ResultAggregator {
    redis: RedisManager,
    notifier: Arc<SliceNotifier>,
    slice_timeout: Duration,
    /// Optional GPU aggregator — Some when `cuda` feature is enabled and a
    /// CUDA device is available, None otherwise (falls back to CPU path).
    #[cfg(feature = "cuda")]
    gpu: Option<Arc<GpuAggregator>>,
}

impl ResultAggregator {
    pub fn new(redis: RedisManager, notifier: Arc<SliceNotifier>) -> Self {
        let ms = std::env::var("SLICE_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30_000);
        Self {
            redis,
            notifier,
            slice_timeout: Duration::from_millis(ms),
            #[cfg(feature = "cuda")]
            gpu: GpuAggregator::try_new(0).map(Arc::new),
        }
    }

    /// Wait for all slices, aggregate (GPU if available, else CPU), store in Redis.
    pub async fn aggregate(&self, job_id: &str, total_slices: usize) -> Result<String> {
        let stats = self.wait_for_all_slices(job_id, total_slices).await?;

        // NCCL mode: workers already normalized — just concatenate, no GPU needed.
        let result_json = if stats.first().map(|s| s.is_normalized).unwrap_or(false) {
            let vals: Vec<f64> = stats.iter().flat_map(|s| s.exp_values.iter().copied()).collect();
            serde_json::to_string(&vals)?
        } else {
            #[cfg(feature = "cuda")]
            if let Some(gpu) = &self.gpu {
                // GPU path: O(total_elements) normalization on device
                let result_f32 = gpu.aggregate(&stats)?;
                let result_f64: Vec<f64> = result_f32.into_iter().map(|v| v as f64).collect();
                serde_json::to_string(&result_f64)?
            } else {
                serde_json::to_string(&aggregate_softmax_cpu(&stats))?
            }
            #[cfg(not(feature = "cuda"))]
            serde_json::to_string(&aggregate_softmax_cpu(&stats))?
        };

        self.redis.store_final_result(job_id, &result_json).await?;
        Ok(result_json)
    }

    // ── Slice collection ──────────────────────────────────────────────────────

    async fn wait_for_all_slices(
        &self,
        job_id: &str,
        total_slices: usize,
    ) -> Result<Vec<SliceStats>> {
        let mut stats: Vec<SliceStats> = (0..total_slices).map(|_| SliceStats::default()).collect();

        // Register before pre-scanning Redis to avoid missing pub/sub events.
        self.notifier.register_job(job_id, total_slices);

        // Pre-read slices that workers already stored before we subscribed.
        for i in 0..total_slices {
            if let Ok(Some(s)) = self.read_slice(job_id, i).await {
                stats[i] = s;
                self.notifier.notify_slice(job_id);
            }
        }

        let ok = self
            .notifier
            .wait_for_all_slices(job_id, total_slices, self.slice_timeout)
            .await;
        self.notifier.unregister_job(job_id);

        if !ok {
            warn!("Timeout waiting for slices for job {}", job_id);
            return Err(anyhow!("Timeout waiting for slices for job {}", job_id));
        }

        // Read any slices that arrived purely via pub/sub (not pre-read above).
        for i in 0..total_slices {
            if stats[i].exp_values.is_empty() {
                match self.read_slice(job_id, i).await {
                    Ok(Some(s)) => stats[i] = s,
                    _ => {
                        error!("Failed to read slice {} for job {}", i, job_id);
                        return Err(anyhow!("Failed to read slice {} for job {}", i, job_id));
                    }
                }
            }
        }

        info!("All {} slices ready for job {}", total_slices, job_id);
        Ok(stats)
    }

    async fn read_slice(&self, job_id: &str, idx: usize) -> Result<Option<SliceStats>> {
        let key = RedisConfig::slice_key(job_id, idx);
        let raw = match self.redis.get_bytes(&key).await? {
            None => return Ok(None),
            Some(b) if b.is_empty() => return Ok(None),
            Some(b) => b,
        };

        let magic = raw[0];

        // ── 0x01: partial stats (standard two-pass) ───────────────────────────
        if magic == 0x01 {
            // Header: [0x01][f64 local_max 8B][f64 partial_sum 8B][u32 n 4B]
            const HDR: usize = 1 + 8 + 8 + 4;
            if raw.len() < HDR {
                return Ok(None);
            }
            let local_max = f64::from_ne_bytes(raw[1..9].try_into()?);
            let partial_sum = f64::from_ne_bytes(raw[9..17].try_into()?);
            let n = u32::from_ne_bytes(raw[17..21].try_into()?) as usize;
            if raw.len() != HDR + n * 4 {
                return Ok(None);
            }
            let mut exp_values = Vec::with_capacity(n);
            for i in 0..n {
                let off = HDR + i * 4;
                let v = f32::from_ne_bytes(raw[off..off + 4].try_into()?) as f64;
                exp_values.push(v);
            }
            return Ok(Some(SliceStats {
                exp_values,
                local_max,
                partial_sum,
                is_normalized: false,
            }));
        }

        // ── 0x02: pre-normalized (NCCL AllReduce) ─────────────────────────────
        if magic == 0x02 {
            const HDR2: usize = 1 + 4;
            if raw.len() < HDR2 {
                return Ok(None);
            }
            let n = u32::from_ne_bytes(raw[1..5].try_into()?) as usize;
            if raw.len() != HDR2 + n * 4 {
                return Ok(None);
            }
            let mut exp_values = Vec::with_capacity(n);
            for i in 0..n {
                let off = HDR2 + i * 4;
                let v = f32::from_ne_bytes(raw[off..off + 4].try_into()?) as f64;
                exp_values.push(v);
            }
            return Ok(Some(SliceStats {
                exp_values,
                local_max: 0.0,
                partial_sum: 1.0,
                is_normalized: true,
            }));
        }

        // ── JSON fallback (legacy workers / debug) ─────────────────────────────
        let s = String::from_utf8(raw)
            .map_err(|e| anyhow!("slice {} not valid UTF-8: {}", idx, e))?;
        let j: serde_json::Value = serde_json::from_str(&s)
            .map_err(|e| anyhow!("slice {} JSON parse failed: {}", idx, e))?;
        let local_max = j["local_max"].as_f64().unwrap_or(0.0);
        let partial_sum = j["partial_sum"].as_f64().unwrap_or(1.0);
        let exp_values = j["exp_values"]
            .as_array()
            .ok_or_else(|| anyhow!("slice {} missing exp_values", idx))?
            .iter()
            .filter_map(|v| v.as_f64())
            .collect();
        Ok(Some(SliceStats {
            exp_values,
            local_max,
            partial_sum,
            is_normalized: false,
        }))
    }
}

// ── CPU two-pass softmax aggregation (fallback when no GPU) ──────────────────

/// Standard two-pass aggregation entirely on CPU.
/// Called when the `cuda` feature is disabled OR no CUDA device is present.
fn aggregate_softmax_cpu(stats: &[SliceStats]) -> Vec<f64> {
    if stats.is_empty() {
        return vec![];
    }

    // global_max  = max(local_max_i)
    // adjust_i    = exp(local_max_i - global_max)
    // global_sum  = Σ partial_sum_i × adjust_i
    // result[k]   = exp_values[k] × adjust[slice_of[k]] / global_sum
    let global_max = stats
        .iter()
        .map(|s| s.local_max)
        .fold(f64::NEG_INFINITY, f64::max);

    let adjust: Vec<f64> = stats
        .iter()
        .map(|s| (s.local_max - global_max).exp())
        .collect();

    let global_sum: f64 = stats
        .iter()
        .zip(adjust.iter())
        .map(|(s, &a)| s.partial_sum * a)
        .sum();

    stats
        .iter()
        .zip(adjust.iter())
        .flat_map(|(s, &a)| s.exp_values.iter().map(move |&ev| ev * a / global_sum))
        .collect()
}
