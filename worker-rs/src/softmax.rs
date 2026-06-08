/// Partial softmax for one slice of the input vector.
///
/// Returns the three values the gateway needs to perform the global two-pass
/// reduction:
///   local_max   = max(data)
///   exp_values  = exp(data_i - local_max)   (f32, matching wire format)
///   partial_sum = sum(exp_values)
pub struct PartialStats {
    pub local_max: f64,
    pub partial_sum: f64,
    pub exp_values: Vec<f32>,
}

pub fn softmax_partial(data: &[f64]) -> PartialStats {
    if data.is_empty() {
        return PartialStats {
            local_max: 0.0,
            partial_sum: 0.0,
            exp_values: vec![],
        };
    }

    let local_max = data.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    let exp_values: Vec<f32> = data
        .iter()
        .map(|&x| (x - local_max).exp() as f32)
        .collect();

    let partial_sum: f64 = exp_values.iter().map(|&v| v as f64).sum();

    PartialStats {
        local_max,
        partial_sum,
        exp_values,
    }
}
