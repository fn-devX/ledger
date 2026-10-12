use std::{sync::Arc, time::Duration};

use async_nats::{
    HeaderMap,
    jetstream::{
        self, Message,
        consumer::{DeliverPolicy, pull},
    },
};
use futures::StreamExt;
use ledger_events::{Event, RawEnvelope, TransferPosted};
use ledger_telemetry::{
    metrics as m,
    propagation::{self, Carrier},
};
use time::OffsetDateTime;
use tracing::field::Empty;

use crate::inbox::Inbox;

const PROPAGATED: [&str; 2] = ["traceparent", "tracestate"];

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
    let mut stream = ledger_events::jetstream::ensure_stream(js).await?;
    let history = stream.info().await?.state.last_sequence;
    let consumer = stream
        .create_consumer(pull::OrderedConfig {
            filter_subject: TransferPosted::SUBJECT.to_owned(),
            deliver_policy: DeliverPolicy::All,
            ..Default::default()
        })
        .await?;
    let mut messages = consumer.messages().await?;
    tracing::info!(
        history,
        "projecting ledger events from the beginning of the stream"
    );
    while let Some(message) = messages.next().await {
        let message = message?;
        let replay = message
            .info()
            .is_ok_and(|info| info.stream_sequence <= history);
        if replay {
            let outcome = project(&message.payload, inbox, false);
            count(outcome.event_type, "replayed");
        } else {
            process(&message, inbox);
        }
    }
    Ok(())
}

fn process(message: &Message, inbox: &Inbox) {
    let span = tracing::info_span!(
        "notifier.process",
        otel.name = %format_args!("process {}", message.subject),
        otel.kind = "consumer",
        otel.status_code = Empty,
        messaging.system = "nats",
        messaging.operation.type = "process",
        messaging.destination.name = %message.subject,
        messaging.message.id = Empty,
        trace_id = Empty,
    );
    if let Some(headers) = &message.headers {
        propagation::set_parent(&span, &carrier(headers));
    }
    if let Some(id) = propagation::trace_id(&span) {
        span.record("trace_id", id);
    }
    let outcome = span.in_scope(|| project(&message.payload, inbox, true));
    if outcome.result == "malformed" {
        span.record("otel.status_code", "ERROR");
    }
    count(outcome.event_type, outcome.result);
}

fn carrier(headers: &HeaderMap) -> Carrier {
    PROPAGATED
        .iter()
        .filter_map(|&key| {
            headers
                .get(key)
                .map(|v| (key.to_owned(), v.as_str().to_owned()))
        })
        .collect()
}

fn count(event_type: String, outcome: &'static str) {
    metrics::counter!(m::NOTIFIER_EVENTS, "type" => event_type, "outcome" => outcome).increment(1);
}

struct Outcome {
    event_type: String,
    result: &'static str,
}

fn project(payload: &[u8], inbox: &Inbox, live: bool) -> Outcome {
    let raw = match RawEnvelope::parse(payload) {
        Ok(raw) => raw,
        Err(e) => {
            tracing::warn!(error = %e, "skipping message that is not an event envelope");
            return Outcome {
                event_type: "unknown".into(),
                result: "malformed",
            };
        }
    };
    let event_type = raw.event_type.clone();
    if !raw.is::<TransferPosted>() {
        tracing::debug!(r#type = %raw.event_type, version = raw.version, "skipping unknown event");
        return Outcome {
            event_type,
            result: "skipped",
        };
    }
    let event = match raw.decode::<TransferPosted>() {
        Ok(event) => event,
        Err(e) => {
            tracing::warn!(error = %e, "skipping malformed transfer.posted event");
            return Outcome {
                event_type,
                result: "malformed",
            };
        }
    };
    tracing::Span::current().record("messaging.message.id", event.id.to_string());
    let delivered = inbox.apply(&event);
    if live {
        let lag = OffsetDateTime::now_utc() - event.occurred_at;
        metrics::histogram!(m::NOTIFIER_LAG).record(lag.as_seconds_f64().max(0.0));
        tracing::debug!(event_id = %event.id, delivered, "event projected");
    }
    Outcome {
        event_type,
        result: if delivered == 0 {
            "duplicate"
        } else {
            "projected"
        },
    }
}
