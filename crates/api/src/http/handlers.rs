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
    extract::{ApiJson, ApiPath, ApiQuery, IdempotencyKey},
};

type Svc<R, G> = State<Arc<LedgerService<R, G>>>;

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

pub async fn get_account<R: LedgerRepository, G: IdGenerator>(
    State(svc): Svc<R, G>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<axum::Json<AccountResponse>, ApiError> {
    Ok(axum::Json((&svc.get_account(id).await?).into()))
}

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

pub async fn get_transfer<R: LedgerRepository, G: IdGenerator>(
    State(svc): Svc<R, G>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<axum::Json<TransferResponse>, ApiError> {
    Ok(axum::Json((&svc.get_transfer(id).await?).into()))
}

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
