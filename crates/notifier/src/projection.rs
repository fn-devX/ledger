use std::{sync::Arc, time::Duration};

use async_nats::jetstream::{
    self,
    consumer::{DeliverPolicy, pull},
};
use futures::StreamExt;
use ledger_events::{Event, RawEnvelope, TransferPosted};

use crate::inbox::Inbox;

pub async fn run(js: jetstream::Context, inbox: Arc<Inbox>) {
    let mut backoff = Duration::from_millis(500);
    loop {
        match consume(&js, &inbox).await {
            Ok(()) => tracing::warn!("event stream ended, resubscribing"),
            Err(e) => tracing::warn!(error = %e, retry_in = ?backoff, "projection failed"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

async fn consume(js: &jetstream::Context, inbox: &Inbox) -> Result<(), async_nats::Error> {
    let stream = ledger_events::jetstream::ensure_stream(js).await?;
    let consumer = stream
        .create_consumer(pull::OrderedConfig {
            filter_subject: TransferPosted::SUBJECT.to_owned(),
            deliver_policy: DeliverPolicy::All,
            ..Default::default()
        })
        .await?;
    let mut messages = consumer.messages().await?;
    tracing::info!("projecting ledger events from the beginning of the stream");
    while let Some(message) = messages.next().await {
        let message = message?;
        match RawEnvelope::parse(&message.payload) {
            Ok(raw) if raw.is::<TransferPosted>() => match raw.decode::<TransferPosted>() {
                Ok(event) => {
                    let delivered = inbox.apply(&event);
                    tracing::debug!(event_id = %event.id, delivered, "event projected");
                }
                Err(e) => tracing::warn!(error = %e, "skipping malformed transfer.posted event"),
            },
            Ok(raw) => {
                tracing::debug!(r#type = %raw.event_type, version = raw.version, "skipping unknown event");
            }
            Err(e) => tracing::warn!(error = %e, "skipping message that is not an event envelope"),
        }
    }
    Ok(())
}
