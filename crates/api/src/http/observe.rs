use std::time::{Duration, Instant};

use axum::{
    extract::{MatchedPath, Request, State},
    http::{HeaderName, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use ledger_telemetry::{
    metrics::{self as m, Metrics},
    propagation::{self, HeaderExtractor},
};
use tracing::{Span, field::Empty};

use super::REQUEST_ID;

const TRACE_ID: HeaderName = HeaderName::from_static("x-trace-id");
const UNTRACED: [&str; 3] = ["/metrics", "/health/live", "/health/ready"];

pub fn make_span<B>(req: &axum::http::Request<B>) -> Span {
    let path = req.uri().path();
    if UNTRACED.contains(&path) {
        return Span::none();
    }
    let request_id = req
        .headers()
        .get(REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or(path, MatchedPath::as_str);
    let span = tracing::info_span!(
        "http",
        otel.name = %format_args!("{} {route}", req.method()),
        otel.kind = "server",
        otel.status_code = Empty,
        http.request.method = %req.method(),
        http.route = route,
        http.response.status_code = Empty,
        url.path = %path,
        request_id,
        trace_id = Empty,
    );
    propagation::set_parent(&span, &HeaderExtractor(req.headers()));
    span
}

pub fn on_response<B>(res: &axum::http::Response<B>, latency: Duration, span: &Span) {
    if span.is_none() {
        return;
    }
    let status = res.status();
    span.record("http.response.status_code", status.as_u16());
    if status.is_server_error() {
        span.record("otel.status_code", "ERROR");
    }
    tracing::debug!(
        status = status.as_u16(),
        latency_ms = latency.as_millis(),
        "finished"
    );
}

struct InFlight;

impl InFlight {
    fn start() -> Self {
        metrics::gauge!(m::HTTP_IN_FLIGHT).increment(1);
        Self
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        metrics::gauge!(m::HTTP_IN_FLIGHT).decrement(1);
    }
}

pub async fn observe(req: Request, next: Next) -> Response {
    let _in_flight = InFlight::start();
    let started = Instant::now();
    let method = req.method().to_string();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned());

    let span = Span::current();
    let trace_id = propagation::trace_id(&span);
    if let Some(id) = &trace_id {
        span.record("trace_id", id.as_str());
    }

    let mut res = next.run(req).await;

    let status = res.status().as_u16().to_string();
    let labels = [("method", method), ("route", route), ("status", status)];
    metrics::counter!(m::HTTP_REQUESTS, &labels).increment(1);
    metrics::histogram!(m::HTTP_DURATION, &labels).record(started.elapsed().as_secs_f64());
    if let Some(id) = trace_id.and_then(|id| HeaderValue::from_str(&id).ok()) {
        res.headers_mut().insert(TRACE_ID, id);
    }
    res
}

pub async fn metrics(State(metrics): State<Metrics>) -> impl IntoResponse {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        metrics.render(),
    )
}
