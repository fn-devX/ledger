mod cursor;
mod dto;
mod error;
mod extract;
mod handlers;
pub mod jwt;
mod middleware;
pub mod openapi;

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    http::{HeaderName, StatusCode},
    middleware::from_fn_with_state,
    routing::{get, post},
};
use ledger_application::{
    AuthService, LedgerService,
    ports::{IdGenerator, LedgerRepository, PasswordHasher, UserRepository},
    ratelimit::{RateLimit, RateLimiter},
};
use tower::ServiceBuilder;
use tower_http::{
    catch_panic::CatchPanicLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

pub use error::ApiError;
pub use jwt::JwtKeys;
pub use openapi::ApiDoc;

use middleware::{RateLimiting, Scope, authenticate, rate_limit};

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

pub trait Store: LedgerRepository + UserRepository {}
impl<T: LedgerRepository + UserRepository> Store for T {}

#[derive(Debug)]
pub struct AppState<R, G, H> {
    pub ledger: LedgerService<R, G>,
    pub auth: AuthService<R, H, G>,
    pub jwt: Arc<JwtKeys>,
}

pub struct HttpSettings {
    pub request_timeout: Duration,
    pub limiter: Arc<dyn RateLimiter>,
    pub api_limit: RateLimit,
    pub auth_limit: RateLimit,
    pub trust_proxy_headers: bool,
}

impl std::fmt::Debug for HttpSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSettings")
            .field("request_timeout", &self.request_timeout)
            .field("api_limit", &self.api_limit)
            .field("auth_limit", &self.auth_limit)
            .finish_non_exhaustive()
    }
}

pub fn router<R: Store, G: IdGenerator, H: PasswordHasher>(
    state: Arc<AppState<R, G, H>>,
    settings: &HttpSettings,
) -> Router {
    let limiting = |scope, limit| {
        Arc::new(RateLimiting {
            limiter: settings.limiter.clone(),
            limit,
            scope,
            trust_proxy_headers: settings.trust_proxy_headers,
        })
    };

    let auth_routes = Router::new()
        .route("/auth/register", post(handlers::register::<R, G, H>))
        .route("/auth/login", post(handlers::login::<R, G, H>))
        .layer(from_fn_with_state(
            limiting(Scope::Auth, settings.auth_limit),
            rate_limit,
        ));

    let api_routes = Router::new()
        .route("/auth/me", get(handlers::me))
        .route(
            "/accounts",
            post(handlers::open_account::<R, G, H>).get(handlers::list_accounts::<R, G, H>),
        )
        .route("/accounts/{id}", get(handlers::get_account::<R, G, H>))
        .route(
            "/accounts/{id}/deposits",
            post(handlers::deposit::<R, G, H>),
        )
        .route(
            "/accounts/{id}/entries",
            get(handlers::list_entries::<R, G, H>),
        )
        .route("/transfers", post(handlers::transfer::<R, G, H>))
        .route("/transfers/{id}", get(handlers::get_transfer::<R, G, H>))
        .layer(from_fn_with_state(
            limiting(Scope::Api, settings.api_limit),
            rate_limit,
        ))
        .layer(from_fn_with_state(state.jwt.clone(), authenticate));

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
            settings.request_timeout,
        ));

    Router::new()
        .nest("/v1", auth_routes.merge(api_routes))
        .route("/health/live", get(handlers::live))
        .route("/health/ready", get(handlers::ready::<R, G, H>))
        .with_state(state)
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .layer(middleware)
}
