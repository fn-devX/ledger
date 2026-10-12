use std::sync::OnceLock;

use metrics::{Unit, describe_counter, describe_gauge, describe_histogram};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};

pub const HTTP_REQUESTS: &str = "http_requests_total";
pub const HTTP_DURATION: &str = "http_request_duration_seconds";
pub const HTTP_IN_FLIGHT: &str = "http_requests_in_flight";
pub const RATE_LIMIT_DECISIONS: &str = "ratelimit_decisions_total";
pub const TRANSFERS: &str = "ledger_transfers_total";
pub const TRANSFER_VOLUME: &str = "ledger_transfer_volume_minor_total";
pub const OUTBOX_PUBLISHED: &str = "ledger_outbox_published_total";
pub const OUTBOX_FAILURES: &str = "ledger_outbox_publish_failures_total";
pub const OUTBOX_PENDING: &str = "ledger_outbox_pending";
pub const OUTBOX_LAG: &str = "ledger_outbox_lag_seconds";
pub const DB_POOL_SIZE: &str = "db_pool_connections";
pub const DB_POOL_IDLE: &str = "db_pool_idle_connections";
pub const NOTIFIER_EVENTS: &str = "notifier_events_total";
pub const NOTIFIER_LAG: &str = "notifier_event_lag_seconds";
pub const NOTIFIER_SUBSCRIBERS: &str = "notifier_subscribers";
pub const GRPC_REQUESTS: &str = "grpc_server_requests_total";
pub const GRPC_DURATION: &str = "grpc_server_handling_seconds";

const LATENCY_BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];
const LAG_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 15.0, 60.0, 300.0, 1800.0,
];

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct Metrics {
    handle: PrometheusHandle,
}

impl Metrics {
    pub fn render(&self) -> String {
        self.handle.run_upkeep();
        self.handle.render()
    }
}

pub fn install() -> Metrics {
    let handle = HANDLE.get_or_init(|| {
        let recorder = PrometheusBuilder::new()
            .set_buckets_for_metric(Matcher::Suffix("_seconds".into()), LATENCY_BUCKETS)
            .and_then(|b| b.set_buckets_for_metric(Matcher::Full(OUTBOX_LAG.into()), LAG_BUCKETS))
            .and_then(|b| b.set_buckets_for_metric(Matcher::Full(NOTIFIER_LAG.into()), LAG_BUCKETS))
            .map_or_else(
                |_| PrometheusBuilder::new().build_recorder(),
                PrometheusBuilder::build_recorder,
            );
        let handle = recorder.handle();
        if metrics::set_global_recorder(recorder).is_ok() {
            describe();
        }
        handle
    });
    Metrics {
        handle: handle.clone(),
    }
}

fn describe() {
    describe_counter!(HTTP_REQUESTS, "HTTP requests by method, route and status");
    describe_histogram!(HTTP_DURATION, Unit::Seconds, "HTTP request latency");
    describe_gauge!(HTTP_IN_FLIGHT, "HTTP requests being served right now");
    describe_counter!(
        RATE_LIMIT_DECISIONS,
        "Rate limiter decisions by scope and outcome (allowed, limited, error)"
    );
    describe_counter!(
        TRANSFERS,
        "Posted transfers and deposits by kind and currency"
    );
    describe_counter!(
        TRANSFER_VOLUME,
        "Money moved by kind and currency, in minor units"
    );
    describe_counter!(OUTBOX_PUBLISHED, "Outbox events published to the broker");
    describe_counter!(
        OUTBOX_FAILURES,
        "Failed attempts to publish an outbox event"
    );
    describe_gauge!(OUTBOX_PENDING, "Outbox events waiting to be published");
    describe_histogram!(
        OUTBOX_LAG,
        Unit::Seconds,
        "Time from commit to broker ack for an outbox event"
    );
    describe_gauge!(DB_POOL_SIZE, "Open database connections");
    describe_gauge!(DB_POOL_IDLE, "Idle database connections");
    describe_counter!(
        NOTIFIER_EVENTS,
        "Ledger events seen by the notifier by type and outcome"
    );
    describe_histogram!(
        NOTIFIER_LAG,
        Unit::Seconds,
        "Time from the ledger event to its projection in the notifier"
    );
    describe_gauge!(NOTIFIER_SUBSCRIBERS, "Open gRPC Subscribe streams");
    describe_counter!(GRPC_REQUESTS, "gRPC calls by method and status code");
    describe_histogram!(GRPC_DURATION, Unit::Seconds, "gRPC call latency");
}
