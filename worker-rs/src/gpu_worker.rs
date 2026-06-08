//! GPU-accelerated partial softmax and NCCL AllReduce.
//!
//! cudarc 0.19 API reference (verified against gateway-rs/src/gpu_aggregator.rs):
//!   - CudaContext::new(id) → Arc<CudaContext>   (no extra Arc::new needed)
//!   - stream.clone_htod(&[T])  → CudaSlice<T>   (memcpy_stod is deprecated)
//!   - stream.alloc_zeros::<T>(n) → CudaSlice<T>
//!   - stream.memcpy_dtoh(&src, &mut dst) → ()
//!   - ctx.load_module(ptx)  + module.load_function(name)
//!   - stream.launch_builder(&fn) → LaunchArgs (needs PushKernelArg in scope)
//!   - unsafe { launcher.launch(cfg, LaunchConfig) }?

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use cudarc::driver::{CudaContext, CudaSlice, LaunchConfig, PushKernelArg};
use cudarc::nccl::{Comm, Id, ReduceOp};
use cudarc::nvrtc::compile_ptx;
use tracing::info;

use crate::softmax::PartialStats;

// ── Kernel source embedded at compile time ────────────────────────────────────
// Path is relative to this file (src/ → ../kernels/).
const KERNEL_SRC: &str = include_str!("../kernels/partial_softmax.cu");

// Must match BLOCK_SIZE defined in partial_softmax.cu.
const BLOCK_SIZE: u32 = 256;

// ── Global singleton ──────────────────────────────────────────────────────────

static GPU_WORKER: OnceLock<Arc<GpuWorker>> = OnceLock::new();

pub fn init(device_id: usize) -> Result<()> {
    let worker = GpuWorker::new(device_id)?;
    GPU_WORKER
        .set(Arc::new(worker))
        .map_err(|_| anyhow::anyhow!("GPU worker already initialized"))
}

pub fn global() -> &'static GpuWorker {
    GPU_WORKER
        .get()
        .expect("GPU worker not initialized — call gpu_worker::init() at startup")
}

// ── GpuWorker ─────────────────────────────────────────────────────────────────

pub struct GpuWorker {
    ctx: Arc<CudaContext>,
    ptx: cudarc::nvrtc::Ptx,
}

unsafe impl Send for GpuWorker {}
unsafe impl Sync for GpuWorker {}

impl GpuWorker {
    pub fn new(device_id: usize) -> Result<Self> {
        // CudaContext::new() already returns Arc<CudaContext>
        let ctx = CudaContext::new(device_id)
            .with_context(|| format!("init CUDA device {}", device_id))?;
        let ptx = compile_ptx(KERNEL_SRC).context("NVRTC: compile partial_softmax.cu")?;
        info!(device = device_id, "CUDA device initialized, PTX compiled");
        Ok(GpuWorker { ctx, ptx })
    }

    pub fn partial_softmax(&self, data: &[f32]) -> Result<PartialStats> {
        let n = data.len();
        let nb = ((n as u32) + BLOCK_SIZE - 1) / BLOCK_SIZE; // num_blocks
        let stream = self.ctx.default_stream();

        let module = self.ctx.load_module(self.ptx.clone()).context("load PTX module")?;
        let f_max = module.load_function("block_max_f32").context("get block_max_f32")?;
        let f_exp =
            module.load_function("exp_and_block_sum_f32").context("get exp_and_block_sum_f32")?;

        let cfg = LaunchConfig {
            block_dim: (BLOCK_SIZE, 1, 1),
            grid_dim: (nb, 1, 1),
            shared_mem_bytes: 0,
        };

        // ── Pass 1: per-block max ─────────────────────────────────────────────
        let d_input: CudaSlice<f32> = stream.clone_htod(data).context("H→D input")?;
        let mut d_bmaxes: CudaSlice<f32> =
            stream.alloc_zeros(nb as usize).context("alloc block_maxes")?;

        {
            let n_i32 = n as i32;
            let mut l = stream.launch_builder(&f_max);
            l.arg(&d_input);
            l.arg(&mut d_bmaxes);
            l.arg(&n_i32);
            unsafe { l.launch(cfg) }.context("launch block_max_f32")?;
        }

        let mut h_bmaxes = vec![0.0f32; nb as usize];
        stream.memcpy_dtoh(&d_bmaxes, &mut h_bmaxes).context("D→H block_maxes")?;
        let global_max = h_bmaxes.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

        // ── Pass 2: exp(x − max) + per-block sum ─────────────────────────────
        let mut d_exp: CudaSlice<f32> = stream.alloc_zeros(n).context("alloc exp")?;
        let mut d_bsums: CudaSlice<f32> =
            stream.alloc_zeros(nb as usize).context("alloc block_sums")?;

        {
        let n_i32 = n as i32;
            let mut l = stream.launch_builder(&f_exp);
            l.arg(&d_input);
            l.arg(&mut d_exp);
            l.arg(&mut d_bsums);
            l.arg(&global_max);
            l.arg(&n_i32);
            unsafe { l.launch(cfg) }.context("launch exp_and_block_sum_f32")?;
        }

        let mut h_exp = vec![0.0f32; n];
        stream.memcpy_dtoh(&d_exp, &mut h_exp).context("D→H exp")?;

        let mut h_bsums = vec![0.0f32; nb as usize];
        stream.memcpy_dtoh(&d_bsums, &mut h_bsums).context("D→H block_sums")?;
        let partial_sum: f32 = h_bsums.iter().sum();

        Ok(PartialStats {
            local_max: global_max as f64,
            partial_sum: partial_sum as f64,
            exp_values: h_exp,
        })
    }
}

// ── NCCL rendezvous ───────────────────────────────────────────────────────────

const NCCL_UID_KEY: &str = "nccl_uid";
const NCCL_UID_TTL: u64 = 60;

pub async fn nccl_rendezvous(
    rank: usize,
    world_size: usize,
    redis: &redis::aio::ConnectionManager,
) -> Result<Id> {
    use redis::AsyncCommands;
    let mut conn = redis.clone();

    let id_bytes: Vec<u8> = if rank == 0 {
        let id = Id::new().map_err(|e| anyhow::anyhow!("NCCL Id::new: {:?}", e))?;
        // SAFETY: Id wraps ncclUniqueId = [i8; 128].  Safe to reinterpret as bytes.
        let bytes =
            unsafe { std::slice::from_raw_parts(&id as *const Id as *const u8, 128).to_vec() };
        conn.set_ex::<_, _, ()>(NCCL_UID_KEY, bytes.clone(), NCCL_UID_TTL)
            .await
            .context("Redis SET nccl_uid")?;
        info!(rank, "NCCL UID generated ({} bytes)", bytes.len());
        bytes
    } else {
        let mut bytes: Option<Vec<u8>> = None;
        for attempt in 0..200 {
            bytes = conn.get(NCCL_UID_KEY).await.context("Redis GET nccl_uid")?;
            if bytes.is_some() {
                break;
            }
            if attempt % 20 == 0 {
                info!(rank, attempt, "waiting for NCCL UID from rank 0");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        bytes.ok_or_else(|| anyhow::anyhow!("rank {}: timed out waiting for NCCL UID", rank))?
    };

    conn.set_ex::<_, _, ()>(format!("nccl_ready:{}", rank), "1", NCCL_UID_TTL)
        .await
        .context("Redis SET nccl_ready")?;

    for attempt in 0..200 {
        let mut ready = 0usize;
        for r in 0..world_size {
            let v: Option<String> =
                conn.get(format!("nccl_ready:{}", r)).await.context("Redis GET nccl_ready")?;
            if v.is_some() {
                ready += 1;
            }
        }
        if ready == world_size {
            break;
        }
        if attempt % 20 == 0 {
            info!(rank, ready, world_size, "waiting for all ranks");
        }
        if attempt == 199 {
            anyhow::bail!("rank {}: not all ranks ready in time", rank);
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    anyhow::ensure!(id_bytes.len() == 128, "NCCL UID must be 128 bytes");
    // SAFETY: same layout.
    let id: Id = unsafe {
        let mut raw = std::mem::MaybeUninit::<Id>::uninit();
        std::ptr::copy_nonoverlapping(id_bytes.as_ptr(), raw.as_mut_ptr() as *mut u8, 128);
        raw.assume_init()
    };

    info!(rank, world_size, "NCCL rendezvous complete");
    Ok(id)
}

// ── NcclWorker ────────────────────────────────────────────────────────────────

pub struct NcclWorker {
    ctx: Arc<CudaContext>,
    comm: Comm,
    ptx: cudarc::nvrtc::Ptx,
}

// SAFETY: NcclWorker is always used from a single dedicated blocking thread.
unsafe impl Send for NcclWorker {}

impl NcclWorker {
    pub fn new(device_id: usize, world_size: usize, id: Id) -> Result<Self> {
        let ctx = CudaContext::new(device_id)
            .with_context(|| format!("init CUDA device {}", device_id))?;
        let ptx = compile_ptx(KERNEL_SRC).context("NVRTC: compile partial_softmax.cu")?;
        // Comm::from_rank takes Arc<CudaStream>, not Arc<CudaContext>.
        let stream = ctx.default_stream();
        let comm = Comm::from_rank(stream, device_id, world_size, id)
            .map_err(|e| anyhow::anyhow!("NCCL Comm::from_rank: {:?}", e))?;
        info!(device = device_id, world_size, "NCCL communicator initialized");
        Ok(NcclWorker { ctx, comm, ptx })
    }

    pub fn all_reduce_softmax(&self, data: &[f32]) -> Result<Vec<f32>> {
        let n = data.len();
        let nb = ((n as u32) + BLOCK_SIZE - 1) / BLOCK_SIZE;
        let stream = self.ctx.default_stream();

        let module = self.ctx.load_module(self.ptx.clone()).context("load PTX")?;
        let f_max = module.load_function("block_max_f32")?;
        let f_exp = module.load_function("exp_and_block_sum_f32")?;
        let f_norm = module.load_function("normalize_f32")?;

        let cfg = LaunchConfig {
            block_dim: (BLOCK_SIZE, 1, 1),
            grid_dim: (nb, 1, 1),
            shared_mem_bytes: 0,
        };

        // ── Block max → AllReduce MAX ─────────────────────────────────────────
        let n_i32 = n as i32;

        let d_input: CudaSlice<f32> = stream.clone_htod(data)?;
        let mut d_bmaxes: CudaSlice<f32> = stream.alloc_zeros(nb as usize)?;
        {
            let mut l = stream.launch_builder(&f_max);
            l.arg(&d_input);
            l.arg(&mut d_bmaxes);
            l.arg(&n_i32);
            unsafe { l.launch(cfg) }?;
        }
        let mut h_bmaxes = vec![0.0f32; nb as usize];
        stream.memcpy_dtoh(&d_bmaxes, &mut h_bmaxes)?;
        let local_max = h_bmaxes.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

        let mut d_lmax: CudaSlice<f32> = stream.clone_htod(&[local_max])?;
        let mut d_gmax: CudaSlice<f32> = stream.alloc_zeros(1)?;
        self.comm
            .all_reduce(&d_lmax, &mut d_gmax, &ReduceOp::Max)
            .map_err(|e| anyhow::anyhow!("AllReduce MAX: {:?}", e))?;
        self.ctx.synchronize().map_err(|e| anyhow::anyhow!("sync: {:?}", e))?;
        let mut h_gmax = vec![0.0f32; 1];
        stream.memcpy_dtoh(&d_gmax, &mut h_gmax)?;
        let global_max = h_gmax[0];

        // ── Exp + block sum → AllReduce SUM ──────────────────────────────────
        let mut d_exp: CudaSlice<f32> = stream.alloc_zeros(n)?;
        let mut d_bsums: CudaSlice<f32> = stream.alloc_zeros(nb as usize)?;
        {
            let mut l = stream.launch_builder(&f_exp);
            l.arg(&d_input);
            l.arg(&mut d_exp);
            l.arg(&mut d_bsums);
            l.arg(&global_max);
            l.arg(&n_i32);
            unsafe { l.launch(cfg) }?;
        }
        let mut h_bsums = vec![0.0f32; nb as usize];
        stream.memcpy_dtoh(&d_bsums, &mut h_bsums)?;
        let local_sum: f32 = h_bsums.iter().sum();

        let mut d_lsum: CudaSlice<f32> = stream.clone_htod(&[local_sum])?;
        let mut d_gsum: CudaSlice<f32> = stream.alloc_zeros(1)?;
        self.comm
            .all_reduce(&d_lsum, &mut d_gsum, &ReduceOp::Sum)
            .map_err(|e| anyhow::anyhow!("AllReduce SUM: {:?}", e))?;
        self.ctx.synchronize().map_err(|e| anyhow::anyhow!("sync: {:?}", e))?;
        let mut h_gsum = vec![0.0f32; 1];
        stream.memcpy_dtoh(&d_gsum, &mut h_gsum)?;
        let global_sum = h_gsum[0];

        // ── Normalize on GPU ──────────────────────────────────────────────────
        let mut d_out: CudaSlice<f32> = stream.alloc_zeros(n)?;
        {
            let mut l = stream.launch_builder(&f_norm);
            l.arg(&d_exp);
            l.arg(&mut d_out);
            l.arg(&global_sum);
            l.arg(&n_i32);
            unsafe { l.launch(cfg) }?;
        }
        let mut h_out = vec![0.0f32; n];
        stream.memcpy_dtoh(&d_out, &mut h_out)?;
        Ok(h_out)
    }
}
