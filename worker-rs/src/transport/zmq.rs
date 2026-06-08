use std::sync::Arc;

use anyhow::{Context, Result};
use redis::aio::ConnectionManager;
use tokio::sync::{mpsc, Semaphore};
use tracing::{error, info};

use crate::handler::handle_task;

pub struct ZmqConfig {
    pub endpoint: String,
    pub max_concurrent: usize,
}

/// Run a ZMQ PULL worker.
///
/// `zmq::Socket` is `!Send`, so it lives on a dedicated `std::thread`.
/// Received byte frames are forwarded to an async mpsc channel so tokio
/// tasks can process them concurrently without touching the socket.
pub async fn run(cfg: ZmqConfig, redis: ConnectionManager) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let endpoint = cfg.endpoint.clone();

    // Spawn the blocking ZMQ receive loop on a dedicated OS thread.
    // Errors during socket setup are forwarded through a one-shot channel.
    let (setup_tx, setup_rx) = std::sync::mpsc::channel::<Result<()>>();

    std::thread::Builder::new()
        .name("zmq-pull".into())
        .spawn(move || {
            let ctx = zmq::Context::new();
            let sock = match ctx.socket(zmq::PULL) {
                Ok(s) => s,
                Err(e) => {
                    let _ = setup_tx.send(Err(anyhow::anyhow!("zmq socket: {}", e)));
                    return;
                }
            };
            if let Err(e) = sock.connect(&endpoint) {
                let _ = setup_tx.send(Err(anyhow::anyhow!("zmq connect {}: {}", endpoint, e)));
                return;
            }
            let _ = setup_tx.send(Ok(()));

            loop {
                match sock.recv_bytes(0) {
                    Ok(data) => {
                        if tx.send(data).is_err() {
                            // Receiver dropped — shut down cleanly.
                            break;
                        }
                    }
                    Err(e) => {
                        error!(err = %e, "ZMQ recv error");
                        break;
                    }
                }
            }
        })
        .context("spawn zmq thread")?;

    // Wait for socket setup result.
    setup_rx
        .recv()
        .context("zmq setup channel closed")??;

    info!(endpoint = %cfg.endpoint, "ZMQ PULL connected");

    let sem = Arc::new(Semaphore::new(cfg.max_concurrent));

    while let Some(data) = rx.recv().await {
        let redis_clone = redis.clone();
        let permit = Arc::clone(&sem).acquire_owned().await?;

        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = handle_task(&data, &redis_clone).await {
                error!(err = %e, "task processing failed");
            }
        });
    }

    Ok(())
}
