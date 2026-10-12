#![allow(dead_code)]

use std::{
    future::{Future, ready},
    sync::Arc,
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use http_body_util::BodyExt;
use ledger_api::http::{AppState, HttpSettings, JwtKeys, router};
use ledger_application::{
    AuthService, LedgerService,
    ports::{BoxError, PasswordHasher},
    ratelimit::RateLimit,
};
use ledger_domain::Password;
use ledger_infrastructure::{InMemoryLedger, InMemoryRateLimiter, UuidV7Generator};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

pub const ADMIN_EMAIL: &str = "admin@ledger.test";
pub const ADMIN_PASSWORD: &str = "admin-password";

#[derive(Debug, Clone)]
pub struct PlainHasher;

impl PasswordHasher for PlainHasher {
    fn hash(&self, password: &Password) -> impl Future<Output = Result<String, BoxError>> + Send {
        ready(Ok(format!("plain:{}", password.expose())))
    }

    fn verify(
        &self,
        password: &str,
        hash: &str,
    ) -> impl Future<Output = Result<bool, BoxError>> + Send {
        ready(Ok(hash == format!("plain:{password}")))
    }

    fn decoy_hash(&self) -> &'static str {
        "plain:\u{0}decoy"
    }
}

pub struct TestApp {
    pub router: Router,
    pub jwt: Arc<JwtKeys>,
}

pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
}

pub struct Limits {
    pub api: u32,
    pub auth: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            api: 10_000,
            auth: 10_000,
        }
    }
}

impl TestApp {
    pub async fn new() -> Self {
        Self::build(PlainHasher, Limits::default()).await
    }

    pub async fn with_limits(limits: Limits) -> Self {
        Self::build(PlainHasher, limits).await
    }

    pub async fn build<H: PasswordHasher>(hasher: H, limits: Limits) -> Self {
        let repo = InMemoryLedger::new();
        let auth = AuthService::new(repo.clone(), hasher, UuidV7Generator);
        auth.ensure_admin(ADMIN_EMAIL, "Cashier", ADMIN_PASSWORD)
            .await
            .unwrap();
        let jwt = Arc::new(JwtKeys::random(Duration::from_secs(600)));
        let state = Arc::new(AppState {
            ledger: LedgerService::new(repo, UuidV7Generator),
            auth,
            jwt: jwt.clone(),
        });
        let settings = HttpSettings {
            request_timeout: Duration::from_secs(5),
            limiter: Arc::new(InMemoryRateLimiter::new()),
            api_limit: RateLimit::per_minute(limits.api),
            auth_limit: RateLimit::per_minute(limits.auth),
            trust_proxy_headers: true,
        };
        Self {
            router: router(state, &settings),
            jwt,
        }
    }

    pub async fn send(&self, req: Request<Body>) -> Response {
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        Response {
            status,
            headers,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        }
    }

    pub async fn call(
        &self,
        method: &str,
        uri: &str,
        token: Option<&str>,
        key: Option<&str>,
        body: Option<Value>,
    ) -> Response {
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        if let Some(k) = key {
            req = req.header("idempotency-key", k);
        }
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        self.send(req).await
    }

    pub async fn login(&self, email: &str, password: &str) -> String {
        let r = self
            .call(
                "POST",
                "/v1/auth/login",
                None,
                None,
                Some(json!({"email": email, "password": password})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "login failed: {}", r.body);
        r.body["access_token"].as_str().unwrap().to_owned()
    }

    pub async fn admin(&self) -> String {
        self.login(ADMIN_EMAIL, ADMIN_PASSWORD).await
    }

    pub async fn customer(&self, name: &str) -> String {
        let email = format!("{}-{}@ledger.test", name.to_lowercase(), Uuid::now_v7());
        let r = self
            .call(
                "POST",
                "/v1/auth/register",
                None,
                None,
                Some(json!({"email": email, "name": name, "password": "password123"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "register failed: {}", r.body);
        self.login(&email, "password123").await
    }

    pub async fn open(&self, token: &str) -> String {
        let r = self
            .call(
                "POST",
                "/v1/accounts",
                Some(token),
                None,
                Some(json!({"currency": "TJS"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "open failed: {}", r.body);
        r.body["id"].as_str().unwrap().to_owned()
    }

    pub async fn deposit(&self, admin: &str, account: &str, amount: i64) {
        let r = self
            .call(
                "POST",
                &format!("/v1/accounts/{account}/deposits"),
                Some(admin),
                Some(&Uuid::now_v7().to_string()),
                Some(json!({"amount_minor": amount, "currency": "TJS"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "deposit failed: {}", r.body);
    }
}
