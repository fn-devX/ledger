use ledger_telemetry::{
    install_local_subscriber,
    metrics::{self, HTTP_REQUESTS},
    propagation::{self, HeaderExtractor, HeaderInjector},
};

#[test]
fn trace_context_survives_a_round_trip_through_a_carrier() {
    install_local_subscriber();
    let producer = tracing::info_span!("producer");
    let carrier = producer.in_scope(propagation::current);
    let traceparent = carrier.get("traceparent").expect("traceparent injected");
    let trace_id = propagation::trace_id(&producer).unwrap();
    assert!(traceparent.contains(&trace_id), "{traceparent}");

    let consumer = tracing::info_span!("consumer");
    propagation::set_parent(&consumer, &carrier);
    assert_eq!(propagation::trace_id(&consumer), Some(trace_id));
}

#[test]
fn http_headers_carry_the_context() {
    install_local_subscriber();
    let client = tracing::info_span!("client");
    let mut headers = http::HeaderMap::new();
    opentelemetry::global::get_text_map_propagator(|p| {
        p.inject_context(
            &tracing_opentelemetry::OpenTelemetrySpanExt::context(&client),
            &mut HeaderInjector(&mut headers),
        );
    });
    let server = tracing::info_span!("server");
    propagation::set_parent(&server, &HeaderExtractor(&headers));
    assert_eq!(
        propagation::trace_id(&server),
        propagation::trace_id(&client)
    );
}

#[test]
fn a_missing_parent_starts_a_new_trace() {
    install_local_subscriber();
    let span = tracing::info_span!("orphan");
    propagation::set_parent(&span, &propagation::Carrier::new());
    assert!(propagation::trace_id(&span).is_some());
}

#[test]
fn prometheus_renders_recorded_metrics() {
    let metrics = metrics::install();
    ::metrics::counter!(HTTP_REQUESTS, "method" => "GET", "route" => "/x", "status" => "200")
        .increment(2);
    let text = metrics.render();
    assert!(
        text.contains(r#"http_requests_total{method="GET",route="/x",status="200"} 2"#),
        "{text}"
    );
    assert!(text.contains("# HELP http_requests_total"), "{text}");
}
