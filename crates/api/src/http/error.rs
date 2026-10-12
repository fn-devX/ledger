use axum::{
    Json,
    extract::rejection::{JsonRejection, PathRejection, QueryRejection},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use ledger_application::AppError;
use ledger_domain::DomainError;
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error(transparent)]
    App(#[from] AppError),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(&'static str),
    #[error("too many requests, retry in {retry_after}s")]
    RateLimited { retry_after: u64 },
    #[error("internal error: {0}")]
    Internal(String),
}

impl From<QueryRejection> for ApiError {
    fn from(e: QueryRejection) -> Self {
        Self::BadRequest(e.body_text())
    }
}
impl From<JsonRejection> for ApiError {
    fn from(e: JsonRejection) -> Self {
        Self::BadRequest(e.body_text())
    }
}
impl From<PathRejection> for ApiError {
    fn from(e: PathRejection) -> Self {
        Self::BadRequest(e.body_text())
    }
}

#[derive(Serialize, ToSchema)]
pub struct Problem {
    #[serde(rename = "type")]
    #[schema(example = "insufficient_funds")]
    kind: &'static str,
    #[schema(example = "insufficient funds: available 75000, requested 500000")]
    title: String,
    #[schema(example = 422)]
    status: u16,
}

impl ApiError {
    fn classify(&self) -> (StatusCode, &'static str) {
        use AppError as A;
        use DomainError as D;
        match self {
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::RateLimited { .. } => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
            Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
            Self::App(e) => match e {
                A::Domain(D::InsufficientFunds { .. }) => {
                    (StatusCode::UNPROCESSABLE_ENTITY, "insufficient_funds")
                }
                A::Domain(D::CurrencyMismatch { .. }) => {
                    (StatusCode::UNPROCESSABLE_ENTITY, "currency_mismatch")
                }
                A::Domain(D::BalanceOverflow) => {
                    (StatusCode::UNPROCESSABLE_ENTITY, "balance_overflow")
                }
                A::Domain(_) => (StatusCode::UNPROCESSABLE_ENTITY, "validation_error"),
                A::AccountNotFound(_) | A::TransferNotFound(_) => {
                    (StatusCode::NOT_FOUND, "not_found")
                }
                A::InvalidPageSize { .. } => (StatusCode::BAD_REQUEST, "invalid_page_size"),
                A::InvalidCredentials => (StatusCode::UNAUTHORIZED, "invalid_credentials"),
                A::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
                A::EmailTaken => (StatusCode::CONFLICT, "email_taken"),
                A::IdempotencyKeyReused => (StatusCode::CONFLICT, "idempotency_key_reused"),
                A::Contention => (StatusCode::SERVICE_UNAVAILABLE, "contention"),
                A::Repository(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, kind) = self.classify();
        let title = if status.is_server_error() && kind == "internal" {
            tracing::error!(error = ?self, "internal error");
            "internal server error".to_owned()
        } else {
            self.to_string()
        };
        let mut resp = (
            status,
            Json(Problem {
                kind,
                title,
                status: status.as_u16(),
            }),
        )
            .into_response();
        let headers = resp.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        match &self {
            Self::App(AppError::Contention) => {
                headers.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            }
            Self::RateLimited { retry_after } => {
                headers.insert(header::RETRY_AFTER, HeaderValue::from(*retry_after));
            }
            Self::Unauthorized(_) | Self::App(AppError::InvalidCredentials) => {
                headers.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            }
            _ => {}
        }
        resp
    }
}
