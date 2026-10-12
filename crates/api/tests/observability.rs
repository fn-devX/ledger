mod common;

use axum::{body::Body, http::Request};
use common::TestApp;
use ledger_telemetry::install_local_subscriber;

const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";

#[tokio::test]
async fn metrics_use_route_templates_not_raw_paths() {
    install_local_subscriber();
    let app = TestApp::new().await;
    let alice = app.customer("Alice").await;
    let account = app.open(&alice).await;
    app.call(
        "GET",
        &format!("/v1/accounts/{account}"),
        Some(&alice),
        None,
        None,
    )
    .await;

    let resp = app
        .router
        .clone()
        .oneshot_text(Request::get("/metrics").body(Body::empty()).unwrap())
        .await;
    assert!(
        resp.contains(r#"route="/v1/accounts/{id}""#),
        "templated route label: {resp}"
    );
    assert!(!resp.contains(&account), "no raw ids in labels");
    assert!(
        resp.contains("http_request_duration_seconds_bucket"),
        "{resp}"
    );
    assert!(resp.contains(r#"ratelimit_decisions_total{scope="api",decision="allowed"}"#));
}

#[tokio::test]
async fn transfers_are_counted_once_per_posting() {
    install_local_subscriber();
    let app = TestApp::new().await;
    let admin = app.admin().await;
    let alice = app.customer("Alice").await;
    let account = app.open(&alice).await;
    app.deposit(&admin, &account, 1_000).await;

    let text = app
        .router
        .clone()
        .oneshot_text(Request::get("/metrics").body(Body::empty()).unwrap())
        .await;
    assert!(
        text.contains(r#"ledger_transfers_total{kind="deposit",currency="TJS"}"#),
        "{text}"
    );
}

#[tokio::test]
async fn incoming_trace_context_is_continued() {
    install_local_subscriber();
    let app = TestApp::new().await;
    let resp = app
        .send(
            Request::get("/v1/auth/me")
                .header("traceparent", format!("00-{TRACE}-00f067aa0ba902b7-01"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(resp.headers["x-trace-id"], TRACE);
}

#[tokio::test]
async fn every_request_gets_a_trace_id() {
    install_local_subscriber();
    let app = TestApp::new().await;
    let resp = app.call("GET", "/v1/auth/me", None, None, None).await;
    let id = resp.headers["x-trace-id"].to_str().unwrap();
    assert_eq!(id.len(), 32);
    assert_ne!(id, TRACE);
}

trait OneshotText {
    async fn oneshot_text(self, req: Request<Body>) -> String;
}

impl OneshotText for axum::Router {
    async fn oneshot_text(self, req: Request<Body>) -> String {
        use http_body_util::BodyExt;
        use tower::ServiceExt;
        let resp = self.oneshot(req).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }
}
