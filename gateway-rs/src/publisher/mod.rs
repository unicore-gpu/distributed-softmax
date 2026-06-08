pub mod zmq;

use anyhow::Result;

/// Common interface for all message transport backends.
/// Equivalent to C++ `IPublisher`.
#[async_trait::async_trait]
pub trait Publisher: Send + Sync {
    /// Publish `payload` to `subject`.
    ///
    /// - NATS: `subject` is the queue subject (e.g. `"task_queue"`).
    /// - ZMQ single: `subject` is ignored (round-robin push).
    /// - ZMQ multi (NCCL): `subject` is the rank as a decimal string (`"0"`, `"1"`, …).
    async fn publish(&self, subject: &str, payload: &str) -> Result<()>;

    fn is_connected(&self) -> bool;
}
