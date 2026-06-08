/// SliceNotifier — async rewrite of the C++ singleton using tokio primitives.
///
/// Design:
///   - One background tokio task holds a Redis pub/sub connection and listens
///     for `slice_done:{job_id}` messages.
///   - Per-job state lives in a `DashMap<String, Arc<JobState>>`.
///   - Each job gets a `tokio::sync::watch` channel that tracks how many slices
///     have been notified.  `watch` guarantees no missed updates even under
///     concurrent senders.
///
/// Race-condition fix (identical to C++ version):
///   Callers must:
///     1. `register_job(job_id, total)`
///     2. Pre-scan Redis and call `notify_slice(job_id)` for each found slice.
///     3. `wait_for_all_slices(...)` — returns immediately if already complete.
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use futures::StreamExt;
use redis::Client;
use tokio::sync::watch;
use tokio::time::timeout;
use tracing::{error, info, warn};

struct JobState {
    /// Current count of notified slices.
    count_tx: watch::Sender<usize>,
    #[allow(dead_code)]
    total: usize,
}

pub struct SliceNotifier {
    jobs: Arc<DashMap<String, Arc<JobState>>>,
}

impl SliceNotifier {
    /// Create the notifier and spawn the Redis pub/sub subscriber task.
    pub async fn new(redis_url: &str) -> anyhow::Result<Arc<Self>> {
        let jobs: Arc<DashMap<String, Arc<JobState>>> = Arc::new(DashMap::new());
        let notifier = Arc::new(Self { jobs: jobs.clone() });

        let client = Client::open(redis_url)?;
        let jobs_bg = jobs.clone();
        tokio::spawn(async move {
            Self::subscriber_loop(client, jobs_bg).await;
        });

        info!("SliceNotifier subscriber task started");
        Ok(notifier)
    }

    /// Register a job before dispatching slices so we never miss a pub/sub
    /// notification that arrives before `wait_for_all_slices` is called.
    pub fn register_job(&self, job_id: &str, total: usize) {
        let (count_tx, _) = watch::channel(0usize);
        self.jobs.insert(
            job_id.to_string(),
            Arc::new(JobState { count_tx, total }),
        );
    }

    pub fn unregister_job(&self, job_id: &str) {
        self.jobs.remove(job_id);
    }

    /// Increment the received-slice counter for a job.
    /// Called by:
    ///   - The background subscriber (pub/sub message).
    ///   - The aggregator during its pre-scan of Redis (race-condition fix).
    pub fn notify_slice(&self, job_id: &str) {
        if let Some(state) = self.jobs.get(job_id) {
            state.count_tx.send_modify(|v| *v += 1);
        }
    }

    /// Block until all `total_slices` arrive, or until `timeout_duration` elapses.
    /// Returns `true` on success, `false` on timeout.
    pub async fn wait_for_all_slices(
        &self,
        job_id: &str,
        total_slices: usize,
        timeout_duration: Duration,
    ) -> bool {
        // Clone the Arc so we don't hold the DashMap shard lock across awaits.
        let state = match self.jobs.get(job_id) {
            Some(entry) => entry.value().clone(),
            None => return false,
        };

        let result = timeout(timeout_duration, async move {
            let mut rx = state.count_tx.subscribe();
            loop {
                // `borrow_and_update` marks current value as "seen".
                // `changed()` then blocks until the value increases again.
                if *rx.borrow_and_update() >= total_slices {
                    return;
                }
                if rx.changed().await.is_err() {
                    // Sender dropped (job unregistered concurrently) — treat as done
                    return;
                }
            }
        })
        .await;

        result.is_ok()
    }

    // ── Background subscriber ─────────────────────────────────────────────────

    async fn subscriber_loop(
        client: Client,
        jobs: Arc<DashMap<String, Arc<JobState>>>,
    ) {
        loop {
            match client.get_async_pubsub().await {
                Ok(mut pubsub) => {
                    if let Err(e) = pubsub.psubscribe("slice_done:*").await {
                        error!("SliceNotifier psubscribe error: {}", e);
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                    info!("SliceNotifier subscribed to slice_done:*");

                    let mut stream = pubsub.on_message();
                    while let Some(msg) = stream.next().await {
                        let channel = msg.get_channel_name().to_string();
                        if let Some(job_id) = channel.strip_prefix("slice_done:") {
                            if let Some(state) = jobs.get(job_id) {
                                state.count_tx.send_modify(|v| *v += 1);
                            }
                        }
                    }

                    warn!("SliceNotifier pub/sub stream ended, reconnecting...");
                }
                Err(e) => {
                    error!("SliceNotifier connect failed: {} — retrying in 1s", e);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }
}
