use std::time::Duration;

use async_nats::jetstream::consumer::{DeliverPolicy, pull};
use futures::StreamExt;
use ledger_application::ports::{EventPublisher, OutboxMessage};
use ledger_infrastructure::JetStreamPublisher;
use uuid::Uuid;

#[tokio::test]
async fn jetstream_drops_duplicate_event_ids() {
    let Ok(url) = std::env::var("NATS_URL") else {
        eprintln!("NATS_URL not set, skipping");
        return;
    };
    let publisher = JetStreamPublisher::connect(&url).await.unwrap();
    let subject = format!("ledger.test.{}", Uuid::now_v7().simple());
    let message = |id: Uuid, n: u8| OutboxMessage {
        event_id: id,
        subject: subject.clone(),
        payload: format!("{{\"n\":{n}}}").into_bytes(),
    };

    let first = Uuid::now_v7();
    publisher.publish(&message(first, 1)).await.unwrap();
    publisher.publish(&message(first, 1)).await.unwrap();
    publisher
        .publish(&message(Uuid::now_v7(), 2))
        .await
        .unwrap();

    let stream = publisher
        .context()
        .get_stream(ledger_events::STREAM)
        .await
        .unwrap();
    let consumer = stream
        .create_consumer(pull::OrderedConfig {
            filter_subject: subject.clone(),
            deliver_policy: DeliverPolicy::All,
            ..Default::default()
        })
        .await
        .unwrap();
    let mut messages = consumer.messages().await.unwrap();
    let mut payloads = Vec::new();
    while let Ok(Some(Ok(msg))) =
        tokio::time::timeout(Duration::from_millis(500), messages.next()).await
    {
        payloads.push(String::from_utf8(msg.payload.to_vec()).unwrap());
    }
    assert_eq!(
        payloads,
        [r#"{"n":1}"#, r#"{"n":2}"#],
        "the retried publish was deduplicated"
    );
}
