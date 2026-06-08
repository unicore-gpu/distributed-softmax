// Partial-softmax kernels for worker-rs distributed aggregation.
//
// Compiled at runtime via NVRTC (no nvcc at build time).
//
// Two-pass algorithm, matching Python worker's softmax_partial():
//   Pass 1: parallel reduction → per-block max → CPU finds global_max
//   Pass 2: exp(x − global_max) fused with per-block sum reduction
//
// The gateway aggregates across slices:
//   global_sum = Σ partial_sum_i × exp(local_max_i − global_max)
//   output[k]  = exp_vals[k] × exp(local_max_k − global_max) / global_sum

#define BLOCK_SIZE 256

// ── Pass 1: block-level max reduction ────────────────────────────────────────
extern "C" __global__ void block_max_f32(
    const float* __restrict__ data,
    float* __restrict__ block_maxes,
    int n
) {
    __shared__ float sdata[BLOCK_SIZE];

    int tid = threadIdx.x;
    int i   = blockIdx.x * BLOCK_SIZE + tid;

    sdata[tid] = (i < n) ? data[i] : -1e38f;
    __syncthreads();

    for (int s = BLOCK_SIZE >> 1; s > 0; s >>= 1) {
        if (tid < s)
            sdata[tid] = fmaxf(sdata[tid], sdata[tid + s]);
        __syncthreads();
    }

    if (tid == 0)
        block_maxes[blockIdx.x] = sdata[0];
}

// ── Pass 2: exp(x − global_max) + block-level sum reduction ─────────────────
extern "C" __global__ void exp_and_block_sum_f32(
    const float* __restrict__ data,
    float* __restrict__ exp_out,
    float* __restrict__ block_sums,
    float global_max,
    int n
) {
    __shared__ float sdata[BLOCK_SIZE];

    int tid = threadIdx.x;
    int i   = blockIdx.x * BLOCK_SIZE + tid;

    float val = 0.0f;
    if (i < n) {
        val       = expf(data[i] - global_max);
        exp_out[i] = val;
    }
    sdata[tid] = val;
    __syncthreads();

    for (int s = BLOCK_SIZE >> 1; s > 0; s >>= 1) {
        if (tid < s)
            sdata[tid] += sdata[tid + s];
        __syncthreads();
    }

    if (tid == 0)
        block_sums[blockIdx.x] = sdata[0];
}

// ── NCCL mode: normalize on GPU after AllReduce ───────────────────────────────
// Called after NCCL AllReduce gives us the global sum.
// Produces the final probability distribution for this worker's slice.
extern "C" __global__ void normalize_f32(
    const float* __restrict__ exp_vals,
    float* __restrict__ out,
    float global_sum,
    int n
) {
    int i = blockIdx.x * BLOCK_SIZE + threadIdx.x;
    if (i < n)
        out[i] = exp_vals[i] / global_sum;
}
