use std::{sync::Arc, time::Duration};

use ledger_events::{Envelope, Event, Party, TransferPosted};
use ledger_notifier::{inbox::Inbox, projection};
use opentelemetry::trace::{SpanKind, TracerProvider as _};
use opentelemetry_sdk::{
    propagation::TraceContextPropagator,
    trace::{InMemorySpanExporter, SdkTracerProvider},
};
use time::OffsetDateTime;
use tracing_subscriber::layer::SubscriberExt;
use uuid::Uuid;

fn event_for(user: Uuid) -> Envelope<TransferPosted> {
    let party = |owner: &str, holder| Party {
        account_id: Uuid::now_v7(),
        holder_id: Some(holder),
        owner: owner.into(),
        balance_after_minor: 100,
    };
    Envelope::new(
        Uuid::now_v7(),
        OffsetDateTime::now_utc(),
        TransferPosted {
            transfer_id: Uuid::now_v7(),
            kind: "transfer".into(),
            amount_minor: 100,
            currency: "TJS".into(),
            initiated_by: None,
            from: party("Firuz", Uuid::now_v7()),
            to: party("Nigina", user),
        },
    )
}

async fn publish(js: &async_nats::jetstream::Context, user: Uuid, traceparent: Option<&str>) {
    let event = event_for(user);
    let mut message = async_nats::jetstream::message::PublishMessage::build()
        .payload(event.to_json().unwrap().into())
        .message_id(event.id.to_string());
    if let Some(value) = traceparent {
        message = message.header("traceparent", value);
    }
    js.send_publish(TransferPosted::SUBJECT, message)
        .await
        .unwrap()
        .await
        .unwrap();
}

async fn projected(inbox: &Inbox, user: Uuid) -> bool {
    for _ in 0..300 {
        if !inbox.list(user, 10).is_empty() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn trace_continues_across_the_broker() {
    let Ok(url) = std::env::var("NATS_URL") else {
        eprintln!("NATS_URL not set, skipping");
        return;
    };
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test"))),
    )
    .unwrap();

    let inbox = Arc::new(Inbox::new());
    let js = async_nats::jetstream::new(async_nats::connect(&url).await.unwrap());
    ledger_events::jetstream::ensure_stream(&js).await.unwrap();
    tokio::spawn(projection::run(js.clone(), inbox.clone()));

    let warmup = Uuid::now_v7();
    publish(&js, warmup, None).await;
    assert!(projected(&inbox, warmup).await, "the projection is live");

    let trace_id = format!("{:032x}", Uuid::now_v7().as_u128());
    let nigina = Uuid::now_v7();
    publish(
        &js,
        nigina,
        Some(&format!("00-{trace_id}-00f067aa0ba902b7-01")),
    )
    .await;
    assert!(projected(&inbox, nigina).await);

    let spans = exporter.get_finished_spans().unwrap();
    let process = spans
        .iter()
        .find(|s| {
            s.name.starts_with("process ") && s.span_context.trace_id().to_string() == trace_id
        })
        .expect("the consumer span joined the producer's trace");
    assert_eq!(process.parent_span_id.to_string(), "00f067aa0ba902b7");
    assert_eq!(process.span_kind, SpanKind::Consumer);
    assert!(
        spans.iter().all(|s| !s.name.starts_with("process ")
            || s.span_context.trace_id().to_string() == trace_id
            || s.parent_span_id.to_string() == "0000000000000000"),
        "events without context start their own traces"
    );
}
