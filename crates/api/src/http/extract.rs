use std::future::{Future, ready};

use axum::{
    extract::{FromRequest, FromRequestParts},
    http::request::Parts,
};

use super::ApiError;

#[derive(Debug, FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub struct ApiJson<T>(pub T);

#[derive(Debug, FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(ApiError))]
pub struct ApiPath<T>(pub T);

#[derive(Debug, FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(ApiError))]
pub struct ApiQuery<T>(pub T);

#[derive(Debug)]
pub struct IdempotencyKey(pub String);

impl<S: Send + Sync> FromRequestParts<S> for IdempotencyKey {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        _: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        ready(
            parts
                .headers
                .get("idempotency-key")
                .and_then(|v| v.to_str().ok())
                .map(|v| Self(v.to_owned()))
                .ok_or_else(|| ApiError::BadRequest("missing Idempotency-Key header".into())),
        )
    }
}
