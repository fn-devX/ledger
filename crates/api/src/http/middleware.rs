use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use ledger_application::{
    Actor,
    ratelimit::{RateDecision, RateLimit, RateLimiter},
};

use ledger_auth::JwtKeys;

use super::ApiError;

pub async fn authenticate(
    State(jwt): State<Arc<JwtKeys>>,
    mut req: Request,
    next: Next,
) -> Response {
    if let Some(value) = req.headers().get(header::AUTHORIZATION) {
        let token = value
            .to_str()
            .ok()
            .and_then(|v| {
                v.strip_prefix("Bearer ")
                    .or_else(|| v.strip_prefix("bearer "))
            })
            .map(str::trim);
        let Some(token) = token else {
            return ApiError::Unauthorized("expected `Authorization: Bearer <token>`")
                .into_response();
        };
        match jwt.verify(token) {
            Ok(subject) => {
                req.extensions_mut().insert(Actor {
                    user_id: subject.user_id,
                    role: subject.role,
                    name: subject.name,
                });
            }
            Err(e) => return ApiError::from(e).into_response(),
        }
    }
    next.run(req).await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Auth,
    Api,
}

pub struct RateLimiting {
    pub limiter: Arc<dyn RateLimiter>,
    pub limit: RateLimit,
    pub scope: Scope,
    pub trust_proxy_headers: bool,
}

impl std::fmt::Debug for RateLimiting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiting")
            .field("limit", &self.limit)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

fn client_ip(req: &Request, trust_proxy_headers: bool) -> String {
    if trust_proxy_headers
        && let Some(ip) = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    {
        return ip.to_owned();
    }
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or_else(
            || "unknown".to_owned(),
            |ConnectInfo(addr)| addr.ip().to_string(),
        )
}

fn rate_key(req: &Request, cfg: &RateLimiting) -> String {
    match (cfg.scope, req.extensions().get::<Actor>()) {
        (Scope::Api, Some(actor)) => format!("api:user:{}", actor.user_id),
        (Scope::Api, None) => format!("api:ip:{}", client_ip(req, cfg.trust_proxy_headers)),
        (Scope::Auth, _) => format!("auth:ip:{}", client_ip(req, cfg.trust_proxy_headers)),
    }
}

fn ceil_secs(d: std::time::Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

fn set_headers(headers: &mut HeaderMap, d: &RateDecision) {
    let pairs = [
        ("ratelimit-limit", u64::from(d.limit)),
        ("ratelimit-remaining", u64::from(d.remaining)),
        ("ratelimit-reset", ceil_secs(d.reset_after)),
    ];
    for (name, value) in pairs {
        headers.insert(HeaderName::from_static(name), HeaderValue::from(value));
    }
}

pub async fn rate_limit(
    State(cfg): State<Arc<RateLimiting>>,
    req: Request,
    next: Next,
) -> Response {
    let key = rate_key(&req, &cfg);
    match cfg.limiter.check(&key, cfg.limit).await {
        Ok(decision) if decision.allowed => {
            let mut resp = next.run(req).await;
            set_headers(resp.headers_mut(), &decision);
            resp
        }
        Ok(decision) => {
            let retry_after = ceil_secs(decision.retry_after).max(1);
            let mut resp = ApiError::RateLimited { retry_after }.into_response();
            set_headers(resp.headers_mut(), &decision);
            resp
        }
        Err(e) => {
            tracing::warn!(error = %e, "rate limiter unavailable, allowing request");
            next.run(req).await
        }
    }
}
