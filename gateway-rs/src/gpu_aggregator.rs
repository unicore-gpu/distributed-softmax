/// GPU-accelerated aggregation using cudarc.
///
/// Work split:
///   CPU  O(num_slices)    — global_max, adjust factors, global_sum
///   GPU  O(total_elems)   — parallel normalization:
///                           out[k] = exp_vals[k] * adjust[slice_ids[k]] * inv_sum
///
/// The CUDA kernel is JIT-compiled from source at first use via NVRTC, so
/// no nvcc is required at build time. The compiled PTX is cached in the struct.
///
/// Falls back to the CPU aggregator automatically if CUDA init fails.
use std::sync::Arc;

use anyhow::{Context, Result};
use cudarc::driver::{CudaContext, LaunchConfig};
use cudarc::nvrtc::compile_ptx;
use tracing::{info, warn};

/// Kernel source embedded at compile time from the kernels/ directory.
const KERNEL_SRC: &str = include_str!("../kernels/aggregate_softmax.cu");
const KERNEL_NAME: &str = "aggregate_normalize";

pub struct GpuAggregator {
    ctx: Arc<CudaContext>,
    /// PTX compiled once and reused across requests.
    ptx: cudarc::nvrtc::Ptx,
}

impl GpuAggregator {
    /// Try to initialize on GPU `device_id` (usually 0).
    /// Returns `None` if no CUDA-capable device is found.
    pub fn try_new(device_id: usize) -> Option<Self> {
        match Self::init(device_id) {
            Ok(agg) => {
                info!("GPU aggregator ready on device {}", device_id);
                Some(agg)
            }
            Err(e) => {
                warn!("GPU aggregator unavailable ({}), falling back to CPU", e);
                None
            }
        }
    }

    fn init(device_id: usize) -> Result<Self> {
        let ctx = CudaContext::new(device_id).context("CudaContext::new")?;
        let ptx = compile_ptx(KERNEL_SRC).context("NVRTC compile_ptx")?;
        Ok(Self {
            ctx: Arc::new(ctx),
            ptx,
        })
    }

    /// Aggregate partial slice statistics into final softmax probabilities on GPU.
    ///
    /// `stats` is the same `SliceStats` slice produced by the aggregator's
    /// `wait_for_all_slices`.  Returns `f32` probabilities (GPU native precision).
    pub fn aggregate(&self, stats: &[crate::aggregator::SliceStats]) -> Result<Vec<f32>> {
        if stats.is_empty() {
            return Ok(vec![]);
        }

        let stream = self.ctx.default_stream();

        // ── CPU: O(num_slices) global reduce ─────────────────────────────────
        let global_max = stats
            .iter()
            .map(|s| s.local_max)
            .fold(f64::NEG_INFINITY, f64::max);

        let adjusts: Vec<f32> = stats
            .iter()
            .map(|s| (s.local_max - global_max).exp() as f32)
            .collect();

        let global_sum: f32 = stats
            .iter()
            .zip(adjusts.iter())
            .map(|(s, &a)| s.partial_sum as f32 * a)
            .sum();

        let inv_global_sum = 1.0_f32 / global_sum;

        // ── Build flat host arrays for GPU upload ─────────────────────────────
        let total: usize = stats.iter().map(|s| s.exp_values.len()).sum();

        let mut exp_vals_host: Vec<f32> = Vec::with_capacity(total);
        let mut slice_ids_host: Vec<i32> = Vec::with_capacity(total);

        for (i, s) in stats.iter().enumerate() {
            for &v in &s.exp_values {
                exp_vals_host.push(v as f32);
                slice_ids_host.push(i as i32);
            }
        }

        // ── Upload to GPU ─────────────────────────────────────────────────────
        let exp_vals_dev = stream.memcpy_stod(&exp_vals_host)?;
        let adjusts_dev = stream.memcpy_stod(&adjusts)?;
        let slice_ids_dev = stream.memcpy_stod(&slice_ids_host)?;
        let mut out_dev = stream.alloc_zeros::<f32>(total)?;

        // ── Launch kernel ─────────────────────────────────────────────────────
        let module = self.ctx.load_module(self.ptx.clone())?;
        let kernel = module.load_function(KERNEL_NAME)?;

        let mut launcher = stream.launch_builder(&kernel);
        launcher.arg(&exp_vals_dev);
        launcher.arg(&adjusts_dev);
        launcher.arg(&slice_ids_dev);
        launcher.arg(&mut out_dev);
        launcher.arg(&inv_global_sum);
        launcher.arg(&(total as i32));

        // SAFETY: kernel reads only its declared inputs and writes only `out`.
        unsafe { launcher.launch(LaunchConfig::for_num_elems(total as u32)) }?;

        // ── Download result ───────────────────────────────────────────────────
        let result = stream.memcpy_dtoh(&out_dev)?;
        Ok(result)
    }
}
