use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use ledger_api::http::router;
use ledger_application::LedgerService;
use ledger_infrastructure::{InMemoryLedger, UuidV7Generator};
use serde_json::{Value, json};
use tower::ServiceExt;

fn app() -> Router {
    let svc = LedgerService::new(InMemoryLedger::new(), UuidV7Generator);
    router(Arc::new(svc), Duration::from_secs(5))
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    key: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
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
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn open(app: &Router) -> String {
    let (s, b) = call(
        app,
        "POST",
        "/v1/accounts",
        None,
        Some(json!({"owner": "Ali", "currency": "TJS"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    b["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn full_happy_path() {
    let app = app();
    let (a, b) = (open(&app).await, open(&app).await);

    let (s, _) = call(
        &app,
        "POST",
        &format!("/v1/accounts/{a}/deposits"),
        Some("dep-1"),
        Some(json!({"amount_minor": 50_000, "currency": "TJS"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);

    let body = json!({"from": a, "to": b, "amount_minor": 12_000, "currency": "TJS"});
    let (s, t) = call(
        &app,
        "POST",
        "/v1/transfers",
        Some("tr-1"),
        Some(body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);

    let (s, t2) = call(&app, "POST", "/v1/transfers", Some("tr-1"), Some(body)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t["id"], t2["id"]);

    let (_, acc) = call(&app, "GET", &format!("/v1/accounts/{a}"), None, None).await;
    assert_eq!(acc["balance_minor"], 38_000);

    let (s, got) = call(
        &app,
        "GET",
        &format!("/v1/transfers/{}", t["id"].as_str().unwrap()),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(got["amount_minor"], 12_000);
}

#[tokio::test]
async fn errors_are_problem_json() {
    let app = app();
    let (a, b) = (open(&app).await, open(&app).await);

    let body = json!({"from": a, "to": b, "amount_minor": 1, "currency": "TJS"});
    let (s, p) = call(&app, "POST", "/v1/transfers", Some("x"), Some(body.clone())).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(p["type"], "insufficient_funds");

    let (s, _) = call(&app, "POST", "/v1/transfers", None, Some(body)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "Idempotency-Key is required");

    let (s, p) = call(&app, "GET", "/v1/accounts/not-a-uuid", None, None).await;
    assert_eq!(
        (s, p["type"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_request"))
    );

    let (s, _) = call(
        &app,
        "GET",
        &format!("/v1/accounts/{}", uuid::Uuid::now_v7()),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (s, p) = call(
        &app,
        "POST",
        "/v1/accounts",
        None,
        Some(json!({"owner": "x", "currency": "usd"})),
    )
    .await;
    assert_eq!(
        (s, p["type"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("validation_error"))
    );
}

#[tokio::test]
async fn health_endpoints() {
    let app = app();
    assert_eq!(
        call(&app, "GET", "/health/live", None, None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/health/ready", None, None).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn entries_are_paginated_with_cursor() {
    let app = app();
    let (a, b) = (open(&app).await, open(&app).await);
    call(
        &app,
        "POST",
        &format!("/v1/accounts/{a}/deposits"),
        Some("dep"),
        Some(json!({"amount_minor": 10_000, "currency": "TJS"})),
    )
    .await;
    for (i, amount) in [100, 200, 300].into_iter().enumerate() {
        let body = json!({"from": a, "to": b, "amount_minor": amount, "currency": "TJS"});
        let (s, _) = call(
            &app,
            "POST",
            "/v1/transfers",
            Some(&format!("t{i}")),
            Some(body),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);
    }

    let (s, page) = call(
        &app,
        "GET",
        &format!("/v1/accounts/{a}/entries?limit=3"),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["delta_minor"], -300);
    assert_eq!(items[0]["balance_after_minor"], 9_400);
    assert_eq!(items[0]["counterparty"], b.as_str());
    assert_eq!(items[0]["currency"], "TJS");
    assert!(items[0]["created_at"].as_str().unwrap().ends_with('Z'));

    let cursor = page["next_cursor"].as_str().unwrap();
    let (s, rest) = call(
        &app,
        "GET",
        &format!("/v1/accounts/{a}/entries?limit=3&cursor={cursor}"),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(rest["items"].as_array().unwrap().len(), 1);
    assert_eq!(rest["items"][0]["kind"], "deposit");
    assert_eq!(rest["next_cursor"], Value::Null);

    let (s, p) = call(
        &app,
        "GET",
        &format!("/v1/accounts/{a}/entries?cursor=nope"),
        None,
        None,
    )
    .await;
    assert_eq!(
        (s, p["type"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_request"))
    );

    let (s, p) = call(
        &app,
        "GET",
        &format!("/v1/accounts/{a}/entries?limit=0"),
        None,
        None,
    )
    .await;
    assert_eq!(
        (s, p["type"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_page_size"))
    );

    let (s, _) = call(
        &app,
        "GET",
        &format!("/v1/accounts/{a}/entries?limit=abc"),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, _) = call(
        &app,
        "GET",
        &format!("/v1/accounts/{}/entries", uuid::Uuid::now_v7()),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
