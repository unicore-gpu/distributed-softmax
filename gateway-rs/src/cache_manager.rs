/// CacheManager — periodic Redis cache stats / cleanup task.
/// Mirrors C++ `CacheManager`, ported to a tokio background task.
use std::time::Duration;

use tracing::info;

use crate::redis_manager::RedisManager;

pub fn spawn(redis: RedisManager, interval_minutes: u64) {
    tokio::spawn(async move {
        info!(
            "Cache manager started (cleanup every {} minutes)",
            interval_minutes
        );
        let interval = Duration::from_secs(interval_minutes * 60);
        loop {
            tokio::time::sleep(interval).await;
            run_cleanup(&redis).await;
        }
    });
}

async fn run_cleanup(redis: &RedisManager) {
    info!("Running cache cleanup check...");

    let result_keys = redis.keys_by_pattern("result:*").await;
    let metadata_keys = redis.keys_by_pattern("metadata:*").await;
    let progress_keys = redis.keys_by_pattern("progress:*").await;

    let total = result_keys.len() + metadata_keys.len() + progress_keys.len();
    let mut expired = 0usize;

    for key in result_keys
        .iter()
        .chain(metadata_keys.iter())
        .chain(progress_keys.iter())
    {
        if redis.ttl(key).await == -2 {
            expired += 1;
        }
    }

    info!(
        "Cache stats: total_keys={} result={} metadata={} progress={} expired={}",
        total,
        result_keys.len(),
        metadata_keys.len(),
        progress_keys.len(),
        expired,
    );
}
