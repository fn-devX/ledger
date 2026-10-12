use std::{sync::Arc, time::Duration};

use ledger_application::ports::{EventPublisher, OutboxStore};
use tokio::sync::Notify;

#[derive(Debug, Clone, Copy)]
pub struct RelayConfig {
    pub batch: u32,
    pub poll_interval: Duration,
    pub max_backoff: Duration,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            batch: 100,
            poll_interval: Duration::from_secs(5),
            max_backoff: Duration::from_secs(30),
        }
    }
}

pub async fn run_relay<S: OutboxStore>(
    store: S,
    publisher: Arc<dyn EventPublisher>,
    signal: Arc<Notify>,
    config: RelayConfig,
) {
    let mut backoff = Duration::ZERO;
    loop {
        match store.relay_batch(publisher.as_ref(), config.batch).await {
            Ok(sent) => {
                backoff = Duration::ZERO;
                if sent > 0 {
                    tracing::debug!(sent, "outbox events published");
                }
                if sent >= config.batch as usize {
                    continue;
                }
            }
            Err(e) => {
                backoff = (backoff * 2).clamp(Duration::from_millis(500), config.max_backoff);
                tracing::warn!(error = %e, retry_in = ?backoff, "outbox relay failed");
                tokio::time::sleep(backoff).await;
                continue;
            }
        }
        tokio::select! {
            () = signal.notified() => {}
            () = tokio::time::sleep(config.poll_interval) => {}
        }
    }
}
