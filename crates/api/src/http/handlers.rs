use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use ledger_application::{
    Deposit, LedgerService, ListEntries, OpenAccount, TransferFunds, TransferOutcome,
    ports::{IdGenerator, LedgerRepository},
};
use uuid::Uuid;

use super::{
    ApiError, cursor,
    dto::{
        AccountResponse, DepositRequest, EntriesQuery, EntryPageResponse, OpenAccountRequest,
        TransferRequest, TransferResponse,
    },
    error::Problem,
    extract::{ApiJson, ApiPath, ApiQuery, IdempotencyKey},
};

type Svc<R, G> = State<Arc<LedgerService<R, G>>>;

#[utoipa::path(
    post,
    path = "/v1/accounts",
    tag = "accounts",
    summary = "Open a customer account",
    request_body = OpenAccountRequest,
    responses(
        (status = 201, description = "Account opened", body = AccountResponse),
        (status = 400, description = "Malformed JSON", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Invalid owner or currency", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn open_account<R: LedgerRepository, G: IdGenerator>(
    State(svc): Svc<R, G>,
    ApiJson(req): ApiJson<OpenAccountRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let account = svc
        .open_account(OpenAccount {
            owner: req.owner,
            currency: req.currency,
        })
        .await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(AccountResponse::from(&account)),
    ))
}

#[utoipa::path(
    get,
    path = "/v1/accounts/{id}",
    tag = "accounts",
    summary = "Get an account and its balance",
    params(("id" = Uuid, Path, description = "Account id")),
    responses(
        (status = 200, description = "Account", body = AccountResponse),
        (status = 404, description = "Account not found", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_account<R: LedgerRepository, G: IdGenerator>(
    State(svc): Svc<R, G>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<axum::Json<AccountResponse>, ApiError> {
    Ok(axum::Json((&svc.get_account(id).await?).into()))
}

#[utoipa::path(
    post,
    path = "/v1/accounts/{id}/deposits",
    tag = "accounts",
    summary = "Deposit money to an account",
    description = "Moves money from the currency's system account. Safe to retry with the same `Idempotency-Key`.",
    params(
        ("id" = Uuid, Path, description = "Account id"),
        ("Idempotency-Key" = String, Header, description = "Unique key of this operation, 1..=128 visible ASCII characters"),
    ),
    request_body = DepositRequest,
    responses(
        (status = 201, description = "Deposit posted", body = TransferResponse),
        (status = 200, description = "Replay of an earlier request with the same key", body = TransferResponse,
            headers(("Idempotent-Replayed" = String, description = "Always `true`"))),
        (status = 400, description = "Missing Idempotency-Key or malformed JSON", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "Account not found", body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Idempotency key reused with different parameters", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Validation or currency mismatch", body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "Too much contention, retry later", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn deposit<R: LedgerRepository, G: IdGenerator>(
    State(svc): Svc<R, G>,
    ApiPath(account): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    ApiJson(req): ApiJson<DepositRequest>,
) -> Result<Response, ApiError> {
    let outcome = svc
        .deposit(Deposit {
            idempotency_key: key,
            account,
            amount_minor: req.amount_minor,
            currency: req.currency,
        })
        .await?;
    Ok(transfer_response(&outcome))
}

#[utoipa::path(
    post,
    path = "/v1/transfers",
    tag = "transfers",
    summary = "Transfer money between two accounts",
    description = "Atomic double-entry transfer. Safe to retry with the same `Idempotency-Key`: the money moves exactly once.",
    params(
        ("Idempotency-Key" = String, Header, description = "Unique key of this operation, 1..=128 visible ASCII characters"),
    ),
    request_body = TransferRequest,
    responses(
        (status = 201, description = "Transfer posted", body = TransferResponse),
        (status = 200, description = "Replay of an earlier request with the same key", body = TransferResponse,
            headers(("Idempotent-Replayed" = String, description = "Always `true`"))),
        (status = 400, description = "Missing Idempotency-Key or malformed JSON", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "Account not found", body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Idempotency key reused with different parameters", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Insufficient funds, currency mismatch or validation error", body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "Too much contention, retry later", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn transfer<R: LedgerRepository, G: IdGenerator>(
    State(svc): Svc<R, G>,
    IdempotencyKey(key): IdempotencyKey,
    ApiJson(req): ApiJson<TransferRequest>,
) -> Result<Response, ApiError> {
    let outcome = svc
        .transfer(TransferFunds {
            idempotency_key: key,
            from: req.from,
            to: req.to,
            amount_minor: req.amount_minor,
            currency: req.currency,
        })
        .await?;
    Ok(transfer_response(&outcome))
}

#[utoipa::path(
    get,
    path = "/v1/transfers/{id}",
    tag = "transfers",
    summary = "Get a transfer",
    params(("id" = Uuid, Path, description = "Transfer id")),
    responses(
        (status = 200, description = "Transfer", body = TransferResponse),
        (status = 404, description = "Transfer not found", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_transfer<R: LedgerRepository, G: IdGenerator>(
    State(svc): Svc<R, G>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<axum::Json<TransferResponse>, ApiError> {
    Ok(axum::Json((&svc.get_transfer(id).await?).into()))
}

#[utoipa::path(
    get,
    path = "/v1/accounts/{id}/entries",
    tag = "accounts",
    summary = "Account statement",
    description = "Ledger entries of the account, newest first, with the running balance after each entry. \
        Cursor-based pagination: pass `next_cursor` from the previous page as `cursor`.",
    params(("id" = Uuid, Path, description = "Account id"), EntriesQuery),
    responses(
        (status = 200, description = "One page of entries", body = EntryPageResponse),
        (status = 400, description = "Invalid cursor or page size", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "Account not found", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_entries<R: LedgerRepository, G: IdGenerator>(
    State(svc): Svc<R, G>,
    ApiPath(account): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<EntriesQuery>,
) -> Result<axum::Json<EntryPageResponse>, ApiError> {
    let before = query.cursor.as_deref().map(cursor::decode).transpose()?;
    let page = svc
        .list_entries(ListEntries {
            account,
            before,
            limit: query.limit,
        })
        .await?;
    let next_cursor = page.next_before.map(cursor::encode);
    Ok(axum::Json(EntryPageResponse::new(&page, next_cursor)))
}

#[utoipa::path(
    get,
    path = "/health/ready",
    tag = "health",
    summary = "Readiness probe",
    description = "200 when the storage is reachable, 503 otherwise.",
    responses((status = 200, description = "Ready"), (status = 503, description = "Storage is unavailable"))
)]
pub async fn ready<R: LedgerRepository, G: IdGenerator>(State(svc): Svc<R, G>) -> StatusCode {
    match svc.repository().ping().await {
        Ok(()) => StatusCode::OK,
        Err(e) => {
            tracing::warn!(error = %e, "readiness check failed");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

fn transfer_response(outcome: &TransferOutcome) -> Response {
    let body = axum::Json(TransferResponse::from(&outcome.transfer));
    if outcome.replayed {
        let header = (
            HeaderName::from_static("idempotent-replayed"),
            HeaderValue::from_static("true"),
        );
        (StatusCode::OK, [header], body).into_response()
    } else {
        (StatusCode::CREATED, body).into_response()
    }
}

#[utoipa::path(
    get,
    path = "/health/live",
    tag = "health",
    summary = "Liveness probe",
    responses((status = 200, description = "The process is alive", body = String, example = "ok"))
)]
pub async fn live() -> &'static str {
    "ok"
}
