use std::time::Duration;

use async_nats::jetstream::{self, Context, message::PublishMessage};
use ledger_application::ports::{BoxError, BoxFuture, EventPublisher, OutboxMessage};

const PUBLISH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct JetStreamPublisher {
    js: Context,
}

impl std::fmt::Debug for JetStreamPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JetStreamPublisher").finish_non_exhaustive()
    }
}

impl JetStreamPublisher {
    pub async fn connect(url: &str) -> Result<Self, BoxError> {
        let client = async_nats::connect(url).await?;
        let js = jetstream::new(client);
        ledger_events::jetstream::ensure_stream(&js).await?;
        Ok(Self { js })
    }

    pub fn context(&self) -> &Context {
        &self.js
    }
}

impl EventPublisher for JetStreamPublisher {
    fn publish<'a>(&'a self, message: &'a OutboxMessage) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(async move {
            let mut publish = PublishMessage::build()
                .payload(message.payload.clone().into())
                .message_id(message.event_id.to_string());
            for (name, value) in &message.headers {
                publish = publish.header(name.as_str(), value.as_str());
            }
            let ack = async {
                self.js
                    .send_publish(message.subject.clone(), publish)
                    .await?
                    .await
            };
            tokio::time::timeout(PUBLISH_TIMEOUT, ack)
                .await
                .map_err(|_| "timed out waiting for the JetStream ack")??;
            Ok(())
        })
    }
}
