mod dto;
mod error;
mod extract;
mod handlers;

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    http::{HeaderName, StatusCode},
    routing::{get, post},
};
use ledger_application::{
    LedgerService,
    ports::{IdGenerator, LedgerRepository},
};
use tower::ServiceBuilder;
use tower_http::{
    catch_panic::CatchPanicLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

pub use error::ApiError;

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

pub fn router<R: LedgerRepository, G: IdGenerator>(
    service: Arc<LedgerService<R, G>>,
    request_timeout: Duration,
) -> Router {
    let v1 = Router::new()
        .route("/accounts", post(handlers::open_account::<R, G>))
        .route("/accounts/{id}", get(handlers::get_account::<R, G>))
        .route("/accounts/{id}/deposits", post(handlers::deposit::<R, G>))
        .route("/transfers", post(handlers::transfer::<R, G>))
        .route("/transfers/{id}", get(handlers::get_transfer::<R, G>));

    let middleware = ServiceBuilder::new()
        .layer(SetRequestIdLayer::new(REQUEST_ID, MakeRequestUuid))
        .layer(
            TraceLayer::new_for_http().make_span_with(|req: &axum::http::Request<_>| {
                let request_id = req
                    .headers()
                    .get(REQUEST_ID)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default();
                tracing::info_span!("http", method = %req.method(), uri = %req.uri(), request_id)
            }),
        )
        .layer(PropagateRequestIdLayer::new(REQUEST_ID))
        .layer(CatchPanicLayer::new())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            request_timeout,
        ));

    Router::new()
        .nest("/v1", v1)
        .route("/health/live", get(|| async { "ok" }))
        .route("/health/ready", get(handlers::ready::<R, G>))
        .with_state(service)
        .layer(middleware)
}
