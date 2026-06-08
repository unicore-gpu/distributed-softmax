use anyhow::Result;
use redis::{aio::ConnectionManager, AsyncCommands, Client};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;

use crate::config::RedisConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobMetadata {
    pub job_id: String,
    pub task_type: String,
    pub total_slices: usize,
    pub created_at: u64,
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "submitted".to_string()
}

/// Async Redis client wrapping a `ConnectionManager` (auto-reconnect, multiplexed).
/// Cheap to clone — all clones share the same underlying connection.
#[derive(Clone)]
pub struct RedisManager {
    conn: ConnectionManager,
}

impl RedisManager {
    pub async fn new() -> Result<Self> {
        let url = RedisConfig::redis_url();
        info!("Connecting to Redis: {}", url);
        let client = Client::open(url)?;
        let conn = ConnectionManager::new(client).await?;
        info!("Redis connection ready");
        Ok(Self { conn })
    }

    // ── Low-level helpers ────────────────────────────────────────────────────

    pub async fn set_ex(&self, key: &str, value: &str, ttl: u64) -> Result<()> {
        let mut c = self.conn.clone();
        c.set_ex::<_, _, ()>(key, value, ttl).await?;
        Ok(())
    }

    /// Returns raw bytes (needed for the binary slice format).
    pub async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let mut c = self.conn.clone();
        Ok(c.get(key).await?)
    }

    pub async fn get_str(&self, key: &str) -> Result<Option<String>> {
        let mut c = self.conn.clone();
        Ok(c.get(key).await?)
    }

    pub async fn exists(&self, key: &str) -> bool {
        let mut c = self.conn.clone();
        c.exists::<_, bool>(key).await.unwrap_or(false)
    }

    pub async fn ttl(&self, key: &str) -> i64 {
        let mut c = self.conn.clone();
        c.ttl::<_, i64>(key).await.unwrap_or(-1)
    }

    pub async fn keys_by_pattern(&self, pattern: &str) -> Vec<String> {
        let mut c = self.conn.clone();
        c.keys::<_, Vec<String>>(pattern)
            .await
            .unwrap_or_default()
    }

    // ── Domain operations ────────────────────────────────────────────────────

    pub async fn set_job_metadata(&self, job_id: &str, meta: &JobMetadata) -> Result<()> {
        let key = RedisConfig::metadata_key(job_id);
        let value = serde_json::to_string(meta)?;
        self.set_ex(&key, &value, RedisConfig::metadata_ttl()).await
    }

    pub async fn get_job_metadata(&self, job_id: &str) -> Result<Option<JobMetadata>> {
        let key = RedisConfig::metadata_key(job_id);
        match self.get_str(&key).await? {
            None => Ok(None),
            Some(s) => Ok(Some(serde_json::from_str(&s)?)),
        }
    }

    pub async fn store_final_result(&self, job_id: &str, result_json: &str) -> Result<()> {
        let key = RedisConfig::result_key(job_id);
        self.set_ex(&key, result_json, RedisConfig::result_ttl())
            .await?;
        self.update_progress(job_id, "completed").await?;
        info!("Aggregated result stored for {}", job_id);
        Ok(())
    }

    pub async fn update_progress(&self, job_id: &str, status: &str) -> Result<()> {
        let key = RedisConfig::progress_key(job_id);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let value = serde_json::json!({
            "job_id": job_id,
            "status": status,
            "updated_at": now,
        })
        .to_string();
        self.set_ex(&key, &value, RedisConfig::progress_ttl()).await
    }

    /// Count how many slice keys already exist in Redis (for GetResult progress).
    pub async fn get_completed_slice_count(&self, job_id: &str, total_slices: usize) -> usize {
        let mut count = 0usize;
        for i in 0..total_slices {
            if self.exists(&RedisConfig::slice_key(job_id, i)).await {
                count += 1;
            }
        }
        count
    }
}
