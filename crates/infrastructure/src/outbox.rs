use std::{sync::Arc, time::Duration};

use ledger_application::ports::{BoxError, BoxFuture, EventPublisher, OutboxMessage, OutboxStore};
use ledger_telemetry::{metrics as m, propagation};
use time::OffsetDateTime;
use tokio::sync::Notify;
use tracing::Instrument;

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

pub struct TracedPublisher {
    inner: Arc<dyn EventPublisher>,
    system: &'static str,
}

impl TracedPublisher {
    pub fn new(inner: Arc<dyn EventPublisher>, system: &'static str) -> Self {
        Self { inner, system }
    }
}

impl std::fmt::Debug for TracedPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TracedPublisher")
            .field("system", &self.system)
            .finish_non_exhaustive()
    }
}

impl EventPublisher for TracedPublisher {
    fn publish<'a>(&'a self, message: &'a OutboxMessage) -> BoxFuture<'a, Result<(), BoxError>> {
        let span = tracing::info_span!(
            "outbox.publish",
            otel.name = %format_args!("publish {}", message.subject),
            otel.kind = "producer",
            otel.status_code = tracing::field::Empty,
            messaging.system = self.system,
            messaging.operation.type = "send",
            messaging.destination.name = %message.subject,
            messaging.message.id = %message.event_id,
        );
        propagation::set_parent(&span, &message.headers);
        let outgoing = OutboxMessage {
            headers: propagation::inject(&span),
            ..message.clone()
        };
        Box::pin(
            async move {
                let result = self.inner.publish(&outgoing).await;
                let subject = outgoing.subject.clone();
                match &result {
                    Ok(()) => {
                        metrics::counter!(m::OUTBOX_PUBLISHED, "subject" => subject).increment(1);
                        let lag = OffsetDateTime::now_utc() - outgoing.created_at;
                        metrics::histogram!(m::OUTBOX_LAG).record(lag.as_seconds_f64().max(0.0));
                    }
                    Err(e) => {
                        metrics::counter!(m::OUTBOX_FAILURES, "subject" => subject).increment(1);
                        tracing::Span::current().record("otel.status_code", "ERROR");
                        tracing::warn!(error = %e, "publish failed");
                    }
                }
                result
            }
            .instrument(span),
        )
    }
}

pub async fn run_relay<S: OutboxStore>(
    store: S,
    publisher: Arc<dyn EventPublisher>,
    signal: Arc<Notify>,
    config: RelayConfig,
) {
    let publisher = TracedPublisher::new(publisher, "nats");
    let mut backoff = Duration::ZERO;
    loop {
        let outcome = store.relay_batch(&publisher, config.batch).await;
        match store.pending().await {
            #[allow(clippy::cast_precision_loss)]
            Ok(pending) => metrics::gauge!(m::OUTBOX_PENDING).set(pending as f64),
            Err(e) => tracing::debug!(error = %e, "cannot count pending outbox events"),
        }
        match outcome {
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
