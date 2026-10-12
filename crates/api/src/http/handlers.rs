use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use ledger_application::{
    Deposit, ListEntries, OpenAccount, Register, TransferFunds, TransferOutcome,
    ports::{IdGenerator, PasswordHasher},
};
use ledger_telemetry::metrics as m;
use uuid::Uuid;

use super::{
    ApiError, AppState, Store, cursor,
    dto::{
        AccountResponse, DepositRequest, EntriesQuery, EntryPageResponse, LoginRequest, MeResponse,
        OpenAccountRequest, RegisterRequest, TokenResponse, TransferRequest, TransferResponse,
        UserResponse,
    },
    error::Problem,
    extract::{ApiJson, ApiPath, ApiQuery, CurrentActor, IdempotencyKey},
};

type St<R, G, H> = State<Arc<AppState<R, G, H>>>;

#[utoipa::path(
    post,
    path = "/v1/auth/register",
    tag = "auth",
    summary = "Register a customer",
    request_body = RegisterRequest,
    responses(
        (status = 201, description = "User registered", body = UserResponse),
        (status = 409, description = "Email is already registered", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Invalid email, name or password", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "Too many attempts from this IP", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn register<R: Store, G: IdGenerator, H: PasswordHasher>(
    State(app): St<R, G, H>,
    ApiJson(req): ApiJson<RegisterRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = app
        .auth
        .register(Register {
            email: req.email,
            name: req.name,
            password: req.password,
        })
        .await?;
    Ok((StatusCode::CREATED, Json(UserResponse::from(&user))))
}

#[utoipa::path(
    post,
    path = "/v1/auth/login",
    tag = "auth",
    summary = "Exchange email and password for an access token",
    description = "Returns a JWT to send as `Authorization: Bearer <token>`. \
        The response is the same for an unknown email and a wrong password.",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Access token", body = TokenResponse),
        (status = 401, description = "Invalid email or password", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "Too many attempts from this IP", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn login<R: Store, G: IdGenerator, H: PasswordHasher>(
    State(app): St<R, G, H>,
    ApiJson(req): ApiJson<LoginRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    let user = app.auth.login(&req.email, &req.password).await?;
    let issued = app.jwt.issue(&user)?;
    Ok(Json(TokenResponse {
        access_token: issued.token,
        token_type: "Bearer".into(),
        expires_in: issued.expires_in.as_secs(),
    }))
}

#[utoipa::path(
    get,
    path = "/v1/auth/me",
    tag = "auth",
    summary = "Who am I",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The caller, as seen in the token", body = MeResponse),
        (status = 401, description = "Missing, invalid or expired token", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn me(CurrentActor(actor): CurrentActor) -> Json<MeResponse> {
    Json(MeResponse::from(&actor))
}

#[utoipa::path(
    post,
    path = "/v1/accounts",
    tag = "accounts",
    summary = "Open an account for the caller",
    security(("bearer" = [])),
    request_body = OpenAccountRequest,
    responses(
        (status = 201, description = "Account opened", body = AccountResponse),
        (status = 400, description = "Malformed JSON", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "Missing, invalid or expired token", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Invalid currency", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "Rate limit exceeded", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn open_account<R: Store, G: IdGenerator, H: PasswordHasher>(
    State(app): St<R, G, H>,
    CurrentActor(actor): CurrentActor,
    ApiJson(req): ApiJson<OpenAccountRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let account = app
        .ledger
        .open_account(
            &actor,
            OpenAccount {
                currency: req.currency,
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(AccountResponse::from(&account))))
}

#[utoipa::path(
    get,
    path = "/v1/accounts",
    tag = "accounts",
    summary = "List the caller's accounts",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "Accounts held by the caller, oldest first", body = Vec<AccountResponse>),
        (status = 401, description = "Missing, invalid or expired token", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "Rate limit exceeded", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_accounts<R: Store, G: IdGenerator, H: PasswordHasher>(
    State(app): St<R, G, H>,
    CurrentActor(actor): CurrentActor,
) -> Result<Json<Vec<AccountResponse>>, ApiError> {
    let accounts = app.ledger.list_accounts(&actor).await?;
    Ok(Json(accounts.iter().map(AccountResponse::from).collect()))
}

#[utoipa::path(
    get,
    path = "/v1/accounts/{id}",
    tag = "accounts",
    summary = "Get an account and its balance",
    description = "Customers see only their own accounts; admins see all.",
    security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Account id")),
    responses(
        (status = 200, description = "Account", body = AccountResponse),
        (status = 401, description = "Missing, invalid or expired token", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "Account not found or not yours", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "Rate limit exceeded", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_account<R: Store, G: IdGenerator, H: PasswordHasher>(
    State(app): St<R, G, H>,
    CurrentActor(actor): CurrentActor,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<AccountResponse>, ApiError> {
    Ok(Json((&app.ledger.get_account(&actor, id).await?).into()))
}

#[utoipa::path(
    post,
    path = "/v1/accounts/{id}/deposits",
    tag = "accounts",
    summary = "Deposit money to an account (admin only)",
    description = "Moves money from the currency's system account. Safe to retry with the same `Idempotency-Key`.",
    security(("bearer" = [])),
    params(
        ("id" = Uuid, Path, description = "Account id"),
        ("Idempotency-Key" = String, Header, description = "Unique key of this operation for the caller, 1..=128 visible ASCII characters"),
    ),
    request_body = DepositRequest,
    responses(
        (status = 201, description = "Deposit posted", body = TransferResponse),
        (status = 200, description = "Replay of an earlier request with the same key", body = TransferResponse,
            headers(("Idempotent-Replayed" = String, description = "Always `true`"))),
        (status = 400, description = "Missing Idempotency-Key or malformed JSON", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "Missing, invalid or expired token", body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "The caller is not an admin", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "Account not found", body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Idempotency key reused with different parameters", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Validation or currency mismatch", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "Rate limit exceeded", body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "Too much contention, retry later", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn deposit<R: Store, G: IdGenerator, H: PasswordHasher>(
    State(app): St<R, G, H>,
    CurrentActor(actor): CurrentActor,
    ApiPath(account): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    ApiJson(req): ApiJson<DepositRequest>,
) -> Result<Response, ApiError> {
    let outcome = app
        .ledger
        .deposit(
            &actor,
            Deposit {
                idempotency_key: key,
                account,
                amount_minor: req.amount_minor,
                currency: req.currency,
            },
        )
        .await?;
    Ok(transfer_response(&outcome))
}

#[utoipa::path(
    post,
    path = "/v1/transfers",
    tag = "transfers",
    summary = "Transfer money from your account",
    description = "Atomic double-entry transfer. `from` must belong to the caller. \
        Safe to retry with the same `Idempotency-Key`: the money moves exactly once.",
    security(("bearer" = [])),
    params(
        ("Idempotency-Key" = String, Header, description = "Unique key of this operation for the caller, 1..=128 visible ASCII characters"),
    ),
    request_body = TransferRequest,
    responses(
        (status = 201, description = "Transfer posted", body = TransferResponse),
        (status = 200, description = "Replay of an earlier request with the same key", body = TransferResponse,
            headers(("Idempotent-Replayed" = String, description = "Always `true`"))),
        (status = 400, description = "Missing Idempotency-Key or malformed JSON", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "Missing, invalid or expired token", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "Account not found or `from` is not yours", body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Idempotency key reused with different parameters", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Insufficient funds, currency mismatch or validation error", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "Rate limit exceeded", body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "Too much contention, retry later", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn transfer<R: Store, G: IdGenerator, H: PasswordHasher>(
    State(app): St<R, G, H>,
    CurrentActor(actor): CurrentActor,
    IdempotencyKey(key): IdempotencyKey,
    ApiJson(req): ApiJson<TransferRequest>,
) -> Result<Response, ApiError> {
    let outcome = app
        .ledger
        .transfer(
            &actor,
            TransferFunds {
                idempotency_key: key,
                from: req.from,
                to: req.to,
                amount_minor: req.amount_minor,
                currency: req.currency,
            },
        )
        .await?;
    Ok(transfer_response(&outcome))
}

#[utoipa::path(
    get,
    path = "/v1/transfers/{id}",
    tag = "transfers",
    summary = "Get a transfer",
    description = "Visible to the holders of both accounts and to admins.",
    security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Transfer id")),
    responses(
        (status = 200, description = "Transfer", body = TransferResponse),
        (status = 401, description = "Missing, invalid or expired token", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "Transfer not found or not visible to you", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "Rate limit exceeded", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_transfer<R: Store, G: IdGenerator, H: PasswordHasher>(
    State(app): St<R, G, H>,
    CurrentActor(actor): CurrentActor,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<TransferResponse>, ApiError> {
    Ok(Json((&app.ledger.get_transfer(&actor, id).await?).into()))
}

#[utoipa::path(
    get,
    path = "/v1/accounts/{id}/entries",
    tag = "accounts",
    summary = "Account statement",
    description = "Ledger entries of the account, newest first, with the running balance after each entry. \
        Cursor-based pagination: pass `next_cursor` from the previous page as `cursor`.",
    security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Account id"), EntriesQuery),
    responses(
        (status = 200, description = "One page of entries", body = EntryPageResponse),
        (status = 400, description = "Invalid cursor or page size", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "Missing, invalid or expired token", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "Account not found or not yours", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "Rate limit exceeded", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_entries<R: Store, G: IdGenerator, H: PasswordHasher>(
    State(app): St<R, G, H>,
    CurrentActor(actor): CurrentActor,
    ApiPath(account): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<EntriesQuery>,
) -> Result<Json<EntryPageResponse>, ApiError> {
    let before = query.cursor.as_deref().map(cursor::decode).transpose()?;
    let page = app
        .ledger
        .list_entries(
            &actor,
            ListEntries {
                account,
                before,
                limit: query.limit,
            },
        )
        .await?;
    let next_cursor = page.next_before.map(cursor::encode);
    Ok(Json(EntryPageResponse::new(&page, next_cursor)))
}

#[utoipa::path(
    get,
    path = "/health/ready",
    tag = "health",
    summary = "Readiness probe",
    description = "200 when the storage is reachable, 503 otherwise.",
    responses((status = 200, description = "Ready"), (status = 503, description = "Storage is unavailable"))
)]
pub async fn ready<R: Store, G: IdGenerator, H: PasswordHasher>(
    State(app): St<R, G, H>,
) -> StatusCode {
    match app.ledger.repository().ping().await {
        Ok(()) => StatusCode::OK,
        Err(e) => {
            tracing::warn!(error = %e, "readiness check failed");
            StatusCode::SERVICE_UNAVAILABLE
        }
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

fn transfer_response(outcome: &TransferOutcome) -> Response {
    let transfer = &outcome.transfer;
    if !outcome.replayed {
        let labels = [
            ("kind", transfer.kind().as_str().to_owned()),
            ("currency", transfer.amount().currency().to_string()),
        ];
        metrics::counter!(m::TRANSFERS, &labels).increment(1);
        metrics::counter!(m::TRANSFER_VOLUME, &labels)
            .increment(transfer.amount().minor().unsigned_abs());
    }
    let body = Json(TransferResponse::from(transfer));
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
