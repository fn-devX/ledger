use std::{path::PathBuf, sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use ledger_api::http::{ApiDoc, router};
use ledger_application::LedgerService;
use ledger_infrastructure::{InMemoryLedger, UuidV7Generator};
use tower::ServiceExt;
use utoipa::OpenApi;

fn app() -> axum::Router {
    let svc = LedgerService::new(InMemoryLedger::new(), UuidV7Generator);
    router(Arc::new(svc), Duration::from_secs(5))
}

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("openapi.json")
}

#[test]
fn openapi_snapshot_is_up_to_date() {
    let actual = ApiDoc::openapi().to_pretty_json().unwrap() + "\n";
    let path = snapshot_path();
    if std::env::var_os("UPDATE_OPENAPI").is_some() {
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        expected == actual,
        "crates/api/openapi.json is out of date. Regenerate it with:\n    \
         UPDATE_OPENAPI=1 cargo test -p ledger-api --test openapi"
    );
}

#[tokio::test]
async fn every_documented_operation_is_routed() {
    let app = app();
    let spec = ApiDoc::openapi();
    let placeholder = uuid::Uuid::now_v7().to_string();
    let mut checked = 0;

    for (path, item) in &spec.paths.paths {
        let uri = path.replace("{id}", &placeholder);
        let methods = [
            (Method::GET, item.get.is_some()),
            (Method::POST, item.post.is_some()),
            (Method::PUT, item.put.is_some()),
            (Method::PATCH, item.patch.is_some()),
            (Method::DELETE, item.delete.is_some()),
        ];
        for method in methods
            .into_iter()
            .filter_map(|(m, documented)| documented.then_some(m))
        {
            let req = Request::builder()
                .method(method.clone())
                .uri(&uri)
                .header("content-type", "application/json")
                .header("idempotency-key", "openapi-route-check")
                .body(Body::from("{}"))
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            let status = resp.status();
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            assert_ne!(
                status,
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} {path} is documented but not routed"
            );
            assert!(
                status != StatusCode::NOT_FOUND || !body.is_empty(),
                "{method} {path} is documented but no route matched"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 8, "unexpected number of documented operations");
}

#[tokio::test]
async fn serves_spec_and_swagger_ui() {
    let app = app();

    let resp = app
        .clone()
        .oneshot(
            Request::get("/api-docs/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let served: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(served["info"]["title"], "Ledger API");
    assert_eq!(served, serde_json::to_value(ApiDoc::openapi()).unwrap());

    let resp = app
        .oneshot(Request::get("/swagger-ui/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&html).contains("swagger-ui"));
}
