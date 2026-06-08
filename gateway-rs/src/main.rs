mod aggregator;
mod cache_manager;
mod config;
#[cfg(feature = "cuda")]
mod gpu_aggregator;
mod publisher;
mod redis_manager;
mod service;
mod slice_notifier;

use std::env;
use std::sync::Arc;

use tonic::transport::Server;
use tracing::info;

use config::RedisConfig;
use publisher::{
    zmq::{ZmqMultiPublisher, ZmqPublisher},
    Publisher,
};
use redis_manager::RedisManager;
use service::{
    proto::vector_service_server::VectorServiceServer, GatewayService,
};
use slice_notifier::SliceNotifier;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── Logging ───────────────────────────────────────────────────────────────
    // Controlled by RUST_LOG env var (e.g. RUST_LOG=info, RUST_LOG=debug).
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // ── Configuration ─────────────────────────────────────────────────────────
    let addr = env::var("GATEWAY_ADDR").unwrap_or_else(|_| "0.0.0.0:50051".to_string());
    let transport = env::var("TRANSPORT").unwrap_or_else(|_| "zmq".to_string());

    info!("Gateway service starting...");
    info!("Address:   {}", addr);
    info!("Transport: {}", transport);

    // ── Message bus ───────────────────────────────────────────────────────────
    let nccl_dispatch = transport == "zmq_nccl";
    let world_size: usize = env::var("NCCL_WORLD_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 0)
        .unwrap_or(4);
    let publisher: Arc<dyn Publisher> = match transport.as_str() {
        "zmq" => Arc::new(ZmqPublisher::new()?),
        "zmq_nccl" => Arc::new(ZmqMultiPublisher::new(world_size)?),
        other => anyhow::bail!("unknown TRANSPORT '{}' (valid: zmq | zmq_nccl)", other),
    };

    if !publisher.is_connected() {
        anyhow::bail!("Failed to connect transport '{}'", transport);
    }

    // ── Redis ─────────────────────────────────────────────────────────────────
    let redis = RedisManager::new().await?;

    // ── Slice notifier (background pub/sub task) ──────────────────────────────
    let notifier = SliceNotifier::new(&RedisConfig::redis_url()).await?;

    // ── Cache manager (background cleanup task) ───────────────────────────────
    cache_manager::spawn(redis.clone(), 15);

    // ── gRPC service ──────────────────────────────────────────────────────────
    let service = GatewayService::new(redis, notifier, publisher, nccl_dispatch, world_size);

    info!("Configuration:");
    info!("  Result TTL:   {}s", RedisConfig::result_ttl());
    info!("  Slice TTL:    {}s", RedisConfig::slice_ttl());
    info!("  Metadata TTL: {}s", RedisConfig::metadata_ttl());

    info!("Gateway listening on {}", addr);

    Server::builder()
        .add_service(VectorServiceServer::new(service))
        .serve(addr.parse()?)
        .await?;

    Ok(())
}
