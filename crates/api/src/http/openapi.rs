use utoipa::{
    Modify, OpenApi,
    openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
};

use super::{dto, error::Problem, handlers};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Ledger API",
        description = "Double-entry money ledger: accounts, deposits, idempotent transfers and paginated statements. \
            Amounts are integers in minor units (1 TJS = 100 dirams). Errors follow RFC 9457 (`application/problem+json`).\n\n\
            **Auth:** register or log in via `/v1/auth/*`, then send `Authorization: Bearer <token>` \
            (the Authorize button above). Customers work with their own accounts; admins can deposit and see everything.\n\n\
            **Rate limits:** every `/v1` response carries `RateLimit-Limit`, `RateLimit-Remaining` and `RateLimit-Reset`. \
            Over the limit the API answers `429` with `Retry-After`. Auth endpoints are limited per IP, the rest per user."
    ),
    paths(
        handlers::register,
        handlers::login,
        handlers::me,
        handlers::open_account,
        handlers::list_accounts,
        handlers::get_account,
        handlers::deposit,
        handlers::list_entries,
        handlers::transfer,
        handlers::get_transfer,
        handlers::live,
        handlers::ready,
    ),
    components(schemas(
        dto::RegisterRequest,
        dto::LoginRequest,
        dto::TokenResponse,
        dto::UserResponse,
        dto::MeResponse,
        dto::OpenAccountRequest,
        dto::DepositRequest,
        dto::TransferRequest,
        dto::AccountResponse,
        dto::TransferResponse,
        dto::EntryResponse,
        dto::EntryPageResponse,
        Problem,
    )),
    modifiers(&BearerAuth),
    tags(
        (name = "auth", description = "Registration and access tokens"),
        (name = "accounts", description = "Accounts, deposits and statements"),
        (name = "transfers", description = "Money transfers between accounts"),
        (name = "health", description = "Kubernetes probes"),
    )
)]
pub struct ApiDoc;

struct BearerAuth;

impl Modify for BearerAuth {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .build(),
            ),
        );
    }
}
