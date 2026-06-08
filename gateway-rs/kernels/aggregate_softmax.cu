// aggregate_softmax.cu
//
// Gateway-side CUDA kernel for the final normalization step of distributed softmax.
//
// Split of work between CPU and GPU:
//
//   CPU (O(num_slices), trivially fast):
//     global_max   = max(local_max_i  for i in slices)
//     adjust_i     = exp(local_max_i - global_max)
//     global_sum   = sum(partial_sum_i * adjust_i)
//
//   GPU (O(total_elements), embarrassingly parallel):
//     out[k] = exp_vals[k] * adjust[slice_of[k]] * inv_global_sum
//
// Using inv_global_sum = 1.0f / global_sum replaces one division per thread
// with a multiplication (faster on every NVIDIA architecture).

extern "C" __global__ void aggregate_normalize(
    const float* __restrict__ exp_vals,     // concatenated exp values from all slices
    const float* __restrict__ adjusts,      // adjust[i] = exp(local_max_i - global_max), one per slice
    const int*   __restrict__ slice_ids,    // slice_ids[k] = which slice element k belongs to
    float*       __restrict__ out,          // output probabilities (length = n)
    float                     inv_global_sum, // precomputed 1.0 / global_sum
    int                       n              // total number of elements
) {
    const int k = blockIdx.x * blockDim.x + threadIdx.x;
    if (k < n) {
        out[k] = exp_vals[k] * adjusts[slice_ids[k]] * inv_global_sum;
    }
}
