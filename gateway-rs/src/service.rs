/// VectorService gRPC implementation — port of C++ `VectorServiceImpl`.
///
/// All state is held in `Arc`s so the service can be freely cloned by tonic's
/// thread pool.
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tonic::{Request, Response, Status};
use tracing::error;

use crate::aggregator::ResultAggregator;
use crate::config::RedisConfig;
use crate::publisher::Publisher;
use crate::redis_manager::{JobMetadata, RedisManager};
use crate::slice_notifier::SliceNotifier;

// Include the tonic-generated code for our proto package.
pub mod proto {
    tonic::include_proto!("vector");
}

use proto::vector_service_server::VectorService;
use proto::{ResultRequest, ResultResponse, TaskRequest, TaskResponse};

fn num_slices() -> usize {
    std::env::var("NUM_SLICES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(4)
}

#[derive(Clone)]
pub struct GatewayService {
    redis: RedisManager,
    notifier: Arc<SliceNotifier>,
    publisher: Arc<dyn Publisher>,
    nccl_dispatch: bool,
    /// Number of GPU ranks in NCCL mode; each job must produce exactly this
    /// many non-empty slices so every rank participates in the AllReduce.
    world_size: usize,
}

impl GatewayService {
    pub fn new(
        redis: RedisManager,
        notifier: Arc<SliceNotifier>,
        publisher: Arc<dyn Publisher>,
        nccl_dispatch: bool,
        world_size: usize,
    ) -> Self {
        Self {
            redis,
            notifier,
            publisher,
            nccl_dispatch,
            world_size,
        }
    }
}

/// Split `data` into exactly `n` contiguous, non-empty, as-even-as-possible
/// chunks. Requires `data.len() >= n`. The first `len % n` chunks are one
/// element larger than the rest.
fn even_chunks(data: &[f64], n: usize) -> Vec<Vec<f64>> {
    let len = data.len();
    let base = len / n;
    let rem = len % n;
    let mut out = Vec::with_capacity(n);
    let mut start = 0;
    for i in 0..n {
        let take = base + if i < rem { 1 } else { 0 };
        out.push(data[start..start + take].to_vec());
        start += take;
    }
    out
}

#[tonic::async_trait]
impl VectorService for GatewayService {
    // ── SubmitTask ────────────────────────────────────────────────────────────
    async fn submit_task(
        &self,
        request: Request<TaskRequest>,
    ) -> Result<Response<TaskResponse>, Status> {
        let req = request.into_inner();

        if req.task != "softmax" {
            return Err(Status::invalid_argument("only softmax task supported"));
        }

        let vector: Vec<f64> = req.vector.iter().map(|&v| v as f64).collect();
        let vector_size = vector.len();
        if vector_size == 0 {
            return Err(Status::invalid_argument("empty input vector"));
        }

        // Slice the vector. In NCCL mode every rank must receive exactly one
        // non-empty slice — all ranks participate in each AllReduce, so a job
        // that yields fewer slices than ranks would stall the collective and
        // desync the pipeline. Split into exactly `world_size` even chunks and
        // reject vectors too short to give every rank data.
        let slices: Vec<Vec<f64>> = if self.nccl_dispatch {
            if vector_size < self.world_size {
                return Err(Status::invalid_argument(format!(
                    "vector length {} < world_size {}: cannot give every GPU rank a \
                     non-empty slice in NCCL mode",
                    vector_size, self.world_size
                )));
            }
            even_chunks(&vector, self.world_size)
        } else {
            let num_slices = num_slices();
            let slice_size = (vector_size + num_slices - 1) / num_slices;
            vector.chunks(slice_size).map(|c| c.to_vec()).collect()
        };
        let total_slices = slices.len();

        // Store job metadata in Redis.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let metadata = JobMetadata {
            job_id: req.job_id.clone(),
            task_type: req.task.clone(),
            total_slices,
            created_at: now,
            status: "submitted".to_string(),
        };
        if let Err(e) = self.redis.set_job_metadata(&req.job_id, &metadata).await {
            error!("Failed to store metadata for {}: {}", req.job_id, e);
            return Err(Status::internal("Failed to store job metadata"));
        }

        // Publish each slice to the message bus.
        for (slice_id, data) in slices.iter().enumerate() {
            let msg = serde_json::json!({
                "job_id":   req.job_id,
                "slice_id": slice_id,
                "task":     req.task,
                "data":     data,
            });

            // NCCL mode: deliver slice i exclusively to GPU i (ordering required).
            // All other transports: subject is ignored (ZMQ round-robin / NATS).
            let subject = if self.nccl_dispatch {
                slice_id.to_string()
            } else {
                "task_queue".to_string()
            };

            if let Err(e) = self.publisher.publish(&subject, &msg.to_string()).await {
                error!(
                    "Failed to publish slice {} for job {}: {}",
                    slice_id, req.job_id, e
                );
                return Err(Status::internal(format!("Publish failed: {}", e)));
            }
        }

        // Synchronous aggregation: block this task until all slices are collected.
        // Concurrency comes from tokio's async task scheduler — no thread starvation.
        let aggregator = ResultAggregator::new(self.redis.clone(), self.notifier.clone());
        let result_str = match aggregator.aggregate(&req.job_id, total_slices).await {
            Ok(s) => s,
            Err(e) => {
                error!("Aggregation failed for {}: {}", req.job_id, e);
                return Err(Status::internal(format!("Aggregation failed: {}", e)));
            }
        };

        let result_floats: Vec<f32> = serde_json::from_str::<Vec<f64>>(&result_str)
            .map_err(|e| Status::internal(format!("Failed to parse result: {}", e)))?
            .into_iter()
            .map(|v| v as f32)
            .collect();

        Ok(Response::new(TaskResponse {
            message: "OK".to_string(),
            result: result_floats,
        }))
    }

    // ── GetResult ─────────────────────────────────────────────────────────────
    async fn get_result(
        &self,
        request: Request<ResultRequest>,
    ) -> Result<Response<ResultResponse>, Status> {
        let job_id = request.into_inner().job_id;

        let metadata = match self.redis.get_job_metadata(&job_id).await {
            Ok(Some(m)) => m,
            Ok(None) => {
                return Ok(Response::new(ResultResponse {
                    job_id,
                    status: "not_found".to_string(),
                    message: "Job not found or expired".to_string(),
                    ..Default::default()
                }))
            }
            Err(e) => {
                return Err(Status::internal(format!("Redis error: {}", e)));
            }
        };

        let total_slices = metadata.total_slices;

        // Check for the final aggregated result first.
        let final_key = RedisConfig::result_key(&job_id);
        if let Ok(Some(raw)) = self.redis.get_str(&final_key).await {
            let ttl = self.redis.ttl(&final_key).await;
            let message = if ttl > 0 {
                format!("Result ready (expires in {} seconds)", ttl)
            } else {
                "Result ready".to_string()
            };

            let result: Vec<f32> = match serde_json::from_str::<Vec<f64>>(&raw) {
                Ok(v) => v.into_iter().map(|x| x as f32).collect(),
                Err(e) => {
                    return Ok(Response::new(ResultResponse {
                        job_id,
                        status: "failed".to_string(),
                        message: format!("Failed to parse result: {}", e),
                        total_slices: total_slices as i32,
                        completed_slices: total_slices as i32,
                        ..Default::default()
                    }))
                }
            };

            return Ok(Response::new(ResultResponse {
                job_id,
                status: "ready".to_string(),
                message,
                result,
                completed_slices: total_slices as i32,
                total_slices: total_slices as i32,
            }));
        }

        // Report progress.
        let completed = self
            .redis
            .get_completed_slice_count(&job_id, total_slices)
            .await;

        let (status, message) = if completed == 0 {
            (
                "pending".to_string(),
                "Job submitted, waiting for processing to start".to_string(),
            )
        } else if completed < total_slices {
            (
                "running".to_string(),
                format!(
                    "Processing in progress: {}/{} slices completed",
                    completed, total_slices
                ),
            )
        } else {
            (
                "running".to_string(),
                "All slices completed, aggregating results...".to_string(),
            )
        };

        Ok(Response::new(ResultResponse {
            job_id,
            status,
            message,
            completed_slices: completed as i32,
            total_slices: total_slices as i32,
            ..Default::default()
        }))
    }
}
