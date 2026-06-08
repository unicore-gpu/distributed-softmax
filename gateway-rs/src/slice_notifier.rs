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
/// Usage:
///   1. `let rx = register_job(job_id)` before dispatching slices, so no
///      notification is missed.
///   2. On each `rx.changed()`, re-read the slices from Redis; the count is
///      only a wakeup hint — completeness is decided by the data actually
///      present, so duplicate notifications are harmless.
///   3. `unregister_job(job_id)` when done.
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use futures::StreamExt;
use redis::Client;
use tokio::sync::watch;
use tracing::{error, info, warn};

struct JobState {
    /// Bumped once per slice notification. Used only as a wakeup signal — the
    /// aggregator decides completeness from the slice data in Redis, so an
    /// over-count (e.g. a slice seen both by pre-read and pub/sub) is harmless.
    count_tx: watch::Sender<usize>,
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

    /// Register a job before dispatching slices and return a receiver that
    /// fires on every slice notification. Registering before dispatch
    /// guarantees we never miss a notification that arrives before the
    /// aggregator starts waiting.
    pub fn register_job(&self, job_id: &str) -> watch::Receiver<usize> {
        let (count_tx, count_rx) = watch::channel(0usize);
        self.jobs
            .insert(job_id.to_string(), Arc::new(JobState { count_tx }));
        count_rx
    }

    pub fn unregister_job(&self, job_id: &str) {
        self.jobs.remove(job_id);
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
