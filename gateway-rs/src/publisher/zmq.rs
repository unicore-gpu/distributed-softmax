/// ZMQ publishers — mirrors C++ `ZmqPublisher` and `ZmqMultiPublisher`.
///
/// `zmq::Socket` is `!Send`, so each socket is created and lives entirely
/// inside a dedicated `std::thread`.  The public structs hold only the channel
/// `SyncSender`, which IS `Send + Sync`.
///
/// A oneshot-style `mpsc` channel is used to propagate bind errors back to the
/// caller so `new()` fails fast if the endpoint is unavailable.
use std::env;
use std::sync::mpsc;

use anyhow::{anyhow, Result};
use tracing::{error, info};

use super::Publisher;

// ── Single-socket publisher (ZMQ PUSH, round-robin) ──────────────────────────

pub struct ZmqPublisher {
    sender: mpsc::SyncSender<Vec<u8>>,
}

impl ZmqPublisher {
    pub fn new() -> Result<Self> {
        let endpoint = env::var("ZMQ_PUSH_ENDPOINT")
            .unwrap_or_else(|_| "ipc:///tmp/softmax_tasks".to_string());

        // Bounded channel; capacity mirrors the ZMQ HWM for backpressure.
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(50_000);
        // Separate channel to report bind success / failure back to the caller.
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();

        std::thread::Builder::new()
            .name("zmq-push".to_string())
            .spawn(move || {
                let ctx = zmq::Context::new();
                let sock = match ctx.socket(zmq::PUSH) {
                    Ok(s) => s,
                    Err(e) => {
                        ready_tx.send(Err(anyhow!("zmq socket error: {}", e))).ok();
                        return;
                    }
                };
                let _ = sock.set_sndhwm(50_000);
                let _ = sock.set_linger(1_000);
                if let Err(e) = sock.bind(&endpoint) {
                    ready_tx
                        .send(Err(anyhow!("zmq bind '{}' failed: {}", endpoint, e)))
                        .ok();
                    return;
                }
                info!("ZMQ PUSH socket bound to {}", endpoint);
                ready_tx.send(Ok(())).ok();

                for payload in rx {
                    if let Err(e) = sock.send(payload, 0) {
                        error!("ZMQ send error: {}", e);
                    }
                }
            })?;

        ready_rx
            .recv()
            .map_err(|_| anyhow!("ZMQ publisher thread died unexpectedly"))??;

        Ok(Self { sender: tx })
    }
}

#[async_trait::async_trait]
impl Publisher for ZmqPublisher {
    async fn publish(&self, _subject: &str, payload: &str) -> Result<()> {
        self.sender
            .send(payload.as_bytes().to_vec())
            .map_err(|e| anyhow!("ZMQ channel send failed: {}", e))
    }

    fn is_connected(&self) -> bool {
        true
    }
}

// ── Multi-socket publisher (ZMQ NCCL mode, one socket per rank) ──────────────

/// Mirrors C++ `ZmqMultiPublisher`.
/// `publish(rank_str, payload)` — `subject` is the rank as a decimal string.
pub struct ZmqMultiPublisher {
    senders: Vec<mpsc::SyncSender<Vec<u8>>>,
}

impl ZmqMultiPublisher {
    pub fn new(world_size: usize) -> Result<Self> {
        let base = env::var("ZMQ_BASE_ENDPOINT")
            .unwrap_or_else(|_| "ipc:///tmp/softmax".to_string());
        let tcp_mode = base.to_lowercase().contains("tcp");
        let base_port: u16 = env::var("ZMQ_BASE_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(5560);

        let mut senders = Vec::with_capacity(world_size);

        for rank in 0..world_size {
            let endpoint = if tcp_mode {
                format!("tcp://0.0.0.0:{}", base_port + rank as u16)
            } else {
                format!("{}_{}", base, rank)
            };

            let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(100_000);
            let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
            let ep = endpoint.clone();

            std::thread::Builder::new()
                .name(format!("zmq-push-{}", rank))
                .spawn(move || {
                    let ctx = zmq::Context::new();
                    let sock = match ctx.socket(zmq::PUSH) {
                        Ok(s) => s,
                        Err(e) => {
                            ready_tx.send(Err(anyhow!("zmq socket: {}", e))).ok();
                            return;
                        }
                    };
                    let _ = sock.set_linger(0);
                    let _ = sock.set_sndhwm(100_000);
                    if let Err(e) = sock.bind(&ep) {
                        ready_tx
                            .send(Err(anyhow!("zmq bind '{}': {}", ep, e)))
                            .ok();
                        return;
                    }
                    info!("ZMQ PUSH (rank {}) bound to {}", rank, ep);
                    ready_tx.send(Ok(())).ok();

                    for payload in rx {
                        if let Err(e) = sock.send(payload, 0) {
                            error!("ZMQ rank {} send error: {}", rank, e);
                        }
                    }
                })?;

            ready_rx
                .recv()
                .map_err(|_| anyhow!("ZMQ publisher thread for rank {} died", rank))??;

            senders.push(tx);
        }

        if tcp_mode {
            info!(
                "ZMQ multi-publisher TCP: workers connect to <GATEWAY_HOST>:{}..{}",
                base_port,
                base_port + world_size as u16 - 1
            );
        }

        Ok(Self { senders })
    }
}

#[async_trait::async_trait]
impl Publisher for ZmqMultiPublisher {
    async fn publish(&self, subject: &str, payload: &str) -> Result<()> {
        let rank: usize = subject
            .parse()
            .map_err(|_| anyhow!("ZmqMultiPublisher: invalid rank '{}'", subject))?;
        let sender = self
            .senders
            .get(rank)
            .ok_or_else(|| anyhow!("ZmqMultiPublisher: rank {} out of range", rank))?;
        sender
            .send(payload.as_bytes().to_vec())
            .map_err(|e| anyhow!("ZMQ rank {} channel send failed: {}", rank, e))
    }

    fn is_connected(&self) -> bool {
        true
    }
}
