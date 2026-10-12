use utoipa::OpenApi;

use super::{dto, error::Problem, handlers};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Ledger API",
        description = "Double-entry money ledger: accounts, deposits, idempotent transfers and paginated statements. \
            Amounts are integers in minor units (1 TJS = 100 dirams). Errors follow RFC 9457 (`application/problem+json`)."
    ),
    paths(
        handlers::open_account,
        handlers::get_account,
        handlers::deposit,
        handlers::list_entries,
        handlers::transfer,
        handlers::get_transfer,
        handlers::live,
        handlers::ready,
    ),
    components(schemas(
        dto::OpenAccountRequest,
        dto::DepositRequest,
        dto::TransferRequest,
        dto::AccountResponse,
        dto::TransferResponse,
        dto::EntryResponse,
        dto::EntryPageResponse,
        Problem,
    )),
    tags(
        (name = "accounts", description = "Accounts, deposits and statements"),
        (name = "transfers", description = "Money transfers between accounts"),
        (name = "health", description = "Kubernetes probes"),
    )
)]
pub struct ApiDoc;
