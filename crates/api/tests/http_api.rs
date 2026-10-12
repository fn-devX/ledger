mod common;

use axum::{body::Body, http::Request, http::StatusCode};
use common::{ADMIN_EMAIL, ADMIN_PASSWORD, Limits, TestApp};
use ledger_domain::{Email, Role, User, UserId};
use ledger_infrastructure::Argon2Hasher;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

#[tokio::test]
async fn full_happy_path() {
    let app = TestApp::new().await;
    let (admin, firuz, nigina) = (
        app.admin().await,
        app.customer("Firuz").await,
        app.customer("Nigina").await,
    );
    let (a, b) = (app.open(&firuz).await, app.open(&nigina).await);
    app.deposit(&admin, &a, 50_000).await;

    let body = json!({"from": a, "to": b, "amount_minor": 12_000, "currency": "TJS"});
    let t = app
        .call(
            "POST",
            "/v1/transfers",
            Some(&firuz),
            Some("tr-1"),
            Some(body.clone()),
        )
        .await;
    assert_eq!(t.status, StatusCode::CREATED);

    let again = app
        .call(
            "POST",
            "/v1/transfers",
            Some(&firuz),
            Some("tr-1"),
            Some(body),
        )
        .await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(t.body["id"], again.body["id"]);

    let mine = app
        .call("GET", "/v1/accounts", Some(&firuz), None, None)
        .await;
    assert_eq!(mine.status, StatusCode::OK);
    assert_eq!(mine.body.as_array().unwrap().len(), 1);
    assert_eq!(mine.body[0]["id"], a.as_str());

    let acc = app
        .call(
            "GET",
            &format!("/v1/accounts/{a}"),
            Some(&firuz),
            None,
            None,
        )
        .await;
    assert_eq!(acc.body["balance_minor"], 38_000);
    assert_eq!(acc.body["owner"], "Firuz");

    let id = t.body["id"].as_str().unwrap();
    for token in [&firuz, &nigina, &admin] {
        let got = app
            .call(
                "GET",
                &format!("/v1/transfers/{id}"),
                Some(token),
                None,
                None,
            )
            .await;
        assert_eq!(got.status, StatusCode::OK);
        assert_eq!(got.body["amount_minor"], 12_000);
    }
}

#[tokio::test]
async fn authentication_flow() {
    let app = TestApp::new().await;
    let email = format!("malika-{}@ledger.test", Uuid::now_v7());
    let reg = |email: &str, password: &str| json!({"email": email, "name": "Malika", "password": password});

    let r = app
        .call(
            "POST",
            "/v1/auth/register",
            None,
            None,
            Some(reg(&email, "password123")),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.body["role"], "customer");
    assert!(r.body.get("password_hash").is_none());

    let dup = app
        .call(
            "POST",
            "/v1/auth/register",
            None,
            None,
            Some(reg(&email.to_uppercase(), "password123")),
        )
        .await;
    assert_eq!(
        (dup.status, dup.body["type"].as_str()),
        (StatusCode::CONFLICT, Some("email_taken"))
    );

    let weak = app
        .call(
            "POST",
            "/v1/auth/register",
            None,
            None,
            Some(reg("x@ledger.test", "short")),
        )
        .await;
    assert_eq!(weak.status, StatusCode::UNPROCESSABLE_ENTITY);

    let bad = app
        .call(
            "POST",
            "/v1/auth/login",
            None,
            None,
            Some(json!({"email": email, "password": "wrong-password"})),
        )
        .await;
    assert_eq!(
        (bad.status, bad.body["type"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("invalid_credentials"))
    );
    assert_eq!(bad.headers["www-authenticate"], "Bearer");
    let unknown = app
        .call(
            "POST",
            "/v1/auth/login",
            None,
            None,
            Some(json!({"email": "ghost@ledger.test", "password": "password123"})),
        )
        .await;
    assert_eq!(
        unknown.body, bad.body,
        "unknown email and wrong password look the same"
    );

    let ok = app
        .call(
            "POST",
            "/v1/auth/login",
            None,
            None,
            Some(json!({"email": email, "password": "password123"})),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.body["token_type"], "Bearer");
    assert_eq!(ok.body["expires_in"], 600);
    let token = ok.body["access_token"].as_str().unwrap();

    let me = app
        .call("GET", "/v1/auth/me", Some(token), None, None)
        .await;
    assert_eq!(
        (
            me.status,
            me.body["name"].as_str(),
            me.body["role"].as_str()
        ),
        (StatusCode::OK, Some("Malika"), Some("customer"))
    );
}

#[tokio::test]
async fn rejects_missing_invalid_and_expired_tokens() {
    let app = TestApp::new().await;

    let none = app.call("GET", "/v1/auth/me", None, None, None).await;
    assert_eq!(
        (none.status, none.body["type"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("unauthorized"))
    );
    assert_eq!(none.headers["www-authenticate"], "Bearer");

    let garbage = app
        .call("GET", "/v1/auth/me", Some("not-a-jwt"), None, None)
        .await;
    assert_eq!(garbage.status, StatusCode::UNAUTHORIZED);

    let req = Request::get("/v1/auth/me")
        .header("authorization", "Basic YWJjOmRlZg==")
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.send(req).await.status, StatusCode::UNAUTHORIZED);

    let user = User::restore(
        UserId::from_uuid(Uuid::now_v7()),
        Email::parse("old@ledger.test").unwrap(),
        "Old".into(),
        Role::Admin,
        String::new(),
    );
    let expired = app
        .jwt
        .issue_at(&user, OffsetDateTime::now_utc() - time::Duration::hours(1))
        .unwrap();
    let r = app
        .call("GET", "/v1/auth/me", Some(&expired.token), None, None)
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.body["title"].as_str().unwrap().contains("expired"));

    let forged = ledger_api::http::JwtKeys::random(std::time::Duration::from_secs(60))
        .issue(&user)
        .unwrap();
    let r = app
        .call("GET", "/v1/accounts", Some(&forged.token), None, None)
        .await;
    assert_eq!(
        r.status,
        StatusCode::UNAUTHORIZED,
        "tokens signed with another key are rejected"
    );
}

#[tokio::test]
async fn authorization_rules() {
    let app = TestApp::new().await;
    let (admin, firuz, nigina) = (
        app.admin().await,
        app.customer("Firuz").await,
        app.customer("Nigina").await,
    );
    let (a, b) = (app.open(&firuz).await, app.open(&nigina).await);
    app.deposit(&admin, &a, 10_000).await;

    let dep = app
        .call(
            "POST",
            &format!("/v1/accounts/{b}/deposits"),
            Some(&nigina),
            Some("free-money"),
            Some(json!({"amount_minor": 1_000_000, "currency": "TJS"})),
        )
        .await;
    assert_eq!(
        (dep.status, dep.body["type"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );

    let peek = app
        .call(
            "GET",
            &format!("/v1/accounts/{a}"),
            Some(&nigina),
            None,
            None,
        )
        .await;
    assert_eq!(peek.status, StatusCode::NOT_FOUND);
    let peek = app
        .call(
            "GET",
            &format!("/v1/accounts/{a}/entries"),
            Some(&nigina),
            None,
            None,
        )
        .await;
    assert_eq!(peek.status, StatusCode::NOT_FOUND);

    let steal = app
        .call(
            "POST",
            "/v1/transfers",
            Some(&nigina),
            Some("steal"),
            Some(json!({"from": a, "to": b, "amount_minor": 5_000, "currency": "TJS"})),
        )
        .await;
    assert_eq!(steal.status, StatusCode::NOT_FOUND);

    let admin_view = app
        .call(
            "GET",
            &format!("/v1/accounts/{a}"),
            Some(&admin),
            None,
            None,
        )
        .await;
    assert_eq!(
        admin_view.body["balance_minor"], 10_000,
        "nothing was stolen, admin sees it"
    );
}

#[tokio::test]
async fn errors_are_problem_json() {
    let app = TestApp::new().await;
    let firuz = app.customer("Firuz").await;
    let (a, b) = (app.open(&firuz).await, app.open(&firuz).await);

    let body = json!({"from": a, "to": b, "amount_minor": 1, "currency": "TJS"});
    let r = app
        .call(
            "POST",
            "/v1/transfers",
            Some(&firuz),
            Some("x"),
            Some(body.clone()),
        )
        .await;
    assert_eq!(
        (r.status, r.body["type"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("insufficient_funds"))
    );
    assert_eq!(r.headers["content-type"], "application/problem+json");

    let r = app
        .call("POST", "/v1/transfers", Some(&firuz), None, Some(body))
        .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "Idempotency-Key is required"
    );

    let r = app
        .call("GET", "/v1/accounts/not-a-uuid", Some(&firuz), None, None)
        .await;
    assert_eq!(
        (r.status, r.body["type"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_request"))
    );

    let r = app
        .call(
            "GET",
            &format!("/v1/accounts/{}", Uuid::now_v7()),
            Some(&firuz),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let r = app
        .call(
            "POST",
            "/v1/accounts",
            Some(&firuz),
            None,
            Some(json!({"currency": "usd"})),
        )
        .await;
    assert_eq!(
        (r.status, r.body["type"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("validation_error"))
    );

    let r = app
        .call(
            "POST",
            "/v1/accounts",
            Some(&firuz),
            None,
            Some(json!({"currency": "TJS", "owner": "x"})),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "unknown fields are rejected"
    );
}

#[tokio::test]
async fn entries_are_paginated_with_cursor() {
    let app = TestApp::new().await;
    let (admin, firuz, nigina) = (
        app.admin().await,
        app.customer("Firuz").await,
        app.customer("Nigina").await,
    );
    let (a, b) = (app.open(&firuz).await, app.open(&nigina).await);
    app.deposit(&admin, &a, 10_000).await;
    for (i, amount) in [100, 200, 300].into_iter().enumerate() {
        let body = json!({"from": a, "to": b, "amount_minor": amount, "currency": "TJS"});
        let r = app
            .call(
                "POST",
                "/v1/transfers",
                Some(&firuz),
                Some(&format!("t{i}")),
                Some(body),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED);
    }

    let page = app
        .call(
            "GET",
            &format!("/v1/accounts/{a}/entries?limit=3"),
            Some(&firuz),
            None,
            None,
        )
        .await;
    assert_eq!(page.status, StatusCode::OK);
    let items = page.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["delta_minor"], -300);
    assert_eq!(items[0]["balance_after_minor"], 9_400);
    assert_eq!(items[0]["counterparty"], b.as_str());
    assert!(items[0]["created_at"].as_str().unwrap().ends_with('Z'));

    let cursor = page.body["next_cursor"].as_str().unwrap();
    let rest = app
        .call(
            "GET",
            &format!("/v1/accounts/{a}/entries?limit=3&cursor={cursor}"),
            Some(&firuz),
            None,
            None,
        )
        .await;
    assert_eq!(rest.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(rest.body["items"][0]["kind"], "deposit");
    assert_eq!(rest.body["next_cursor"], Value::Null);

    let r = app
        .call(
            "GET",
            &format!("/v1/accounts/{a}/entries?cursor=nope"),
            Some(&firuz),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app
        .call(
            "GET",
            &format!("/v1/accounts/{a}/entries?limit=0"),
            Some(&firuz),
            None,
            None,
        )
        .await;
    assert_eq!(
        (r.status, r.body["type"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_page_size"))
    );
}

#[tokio::test]
async fn api_rate_limit_is_per_user() {
    let app = TestApp::with_limits(Limits {
        api: 3,
        auth: 10_000,
    })
    .await;
    let (firuz, nigina) = (app.customer("Firuz").await, app.customer("Nigina").await);

    for expected_remaining in ["2", "1", "0"] {
        let r = app
            .call("GET", "/v1/auth/me", Some(&firuz), None, None)
            .await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.headers["ratelimit-limit"], "3");
        assert_eq!(r.headers["ratelimit-remaining"], expected_remaining);
    }
    let r = app
        .call("GET", "/v1/auth/me", Some(&firuz), None, None)
        .await;
    assert_eq!(
        (r.status, r.body["type"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    assert_eq!(r.headers["retry-after"], "20");
    assert_eq!(r.headers["ratelimit-remaining"], "0");

    let other = app
        .call("GET", "/v1/auth/me", Some(&nigina), None, None)
        .await;
    assert_eq!(
        other.status,
        StatusCode::OK,
        "each user has their own budget"
    );

    let health = app.call("GET", "/health/live", None, None, None).await;
    assert_eq!(health.status, StatusCode::OK, "probes are never limited");
}

#[tokio::test]
async fn login_is_rate_limited_per_ip() {
    let app = TestApp::with_limits(Limits {
        api: 10_000,
        auth: 3,
    })
    .await;
    let attempt = |ip: &'static str| {
        Request::post("/v1/auth/login")
            .header("content-type", "application/json")
            .header("x-forwarded-for", ip)
            .body(Body::from(
                json!({"email": ADMIN_EMAIL, "password": "guess"}).to_string(),
            ))
            .unwrap()
    };
    for _ in 0..3 {
        assert_eq!(
            app.send(attempt("203.0.113.7")).await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    let blocked = app.send(attempt("203.0.113.7")).await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(blocked.headers.contains_key("retry-after"));

    let other_ip = app.send(attempt("198.51.100.1")).await;
    assert_eq!(
        other_ip.status,
        StatusCode::UNAUTHORIZED,
        "other clients are not affected"
    );
}

#[tokio::test]
async fn real_argon2_end_to_end() {
    let app = TestApp::build(Argon2Hasher::new().unwrap(), Limits::default()).await;
    let token = app.login(ADMIN_EMAIL, ADMIN_PASSWORD).await;
    let me = app
        .call("GET", "/v1/auth/me", Some(&token), None, None)
        .await;
    assert_eq!(me.body["role"], "admin");
    let wrong = app
        .call(
            "POST",
            "/v1/auth/login",
            None,
            None,
            Some(json!({"email": ADMIN_EMAIL, "password": "nope-nope"})),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn health_endpoints_are_public() {
    let app = TestApp::new().await;
    assert_eq!(
        app.call("GET", "/health/live", None, None, None)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        app.call("GET", "/health/ready", None, None, None)
            .await
            .status,
        StatusCode::OK
    );
}
