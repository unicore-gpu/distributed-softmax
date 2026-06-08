use std::env;

/// Mirror of C++ RedisConfig — key naming and TTL helpers.
pub struct RedisConfig;

impl RedisConfig {
    pub fn result_ttl() -> u64 {
        env::var("REDIS_TTL_HOURS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(|h| h * 3600)
            .unwrap_or(3600)
    }

    pub fn slice_ttl() -> u64 {
        env::var("REDIS_TTL_SLICE_HOURS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(|h| h * 3600)
            .unwrap_or(7200)
    }

    pub fn metadata_ttl() -> u64 {
        env::var("REDIS_TTL_METADATA_HOURS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(|h| h * 3600)
            .unwrap_or(86400)
    }

    pub fn progress_ttl() -> u64 {
        env::var("REDIS_TTL_PROGRESS_HOURS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(|h| h * 3600)
            .unwrap_or(3600)
    }

    pub fn result_key(job_id: &str) -> String {
        format!("result:{}", job_id)
    }

    pub fn slice_key(job_id: &str, slice_id: usize) -> String {
        format!("result:{}:{}", job_id, slice_id)
    }

    pub fn metadata_key(job_id: &str) -> String {
        format!("metadata:{}", job_id)
    }

    pub fn progress_key(job_id: &str) -> String {
        format!("progress:{}", job_id)
    }

    /// Redis URL built from env vars (used by both RedisManager and SliceNotifier).
    pub fn redis_url() -> String {
        let host = env::var("REDIS_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port: u16 = env::var("REDIS_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(6379);
        match env::var("REDIS_PASSWORD") {
            Ok(pw) => format!("redis://:{}@{}:{}", pw, host, port),
            Err(_) => format!("redis://{}:{}", host, port),
        }
    }
}
