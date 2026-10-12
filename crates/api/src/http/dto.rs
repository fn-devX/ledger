use ledger_application::{EntryPage, ports::EntryRecord};
use ledger_domain::{Account, Transfer};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenAccountRequest {
    #[schema(example = "Firuz Rahimov", min_length = 1, max_length = 200)]
    pub owner: String,
    #[schema(example = "TJS", pattern = "^[A-Z]{3}$")]
    pub currency: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DepositRequest {
    #[schema(example = 100_000, minimum = 1)]
    pub amount_minor: i64,
    #[schema(example = "TJS")]
    pub currency: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TransferRequest {
    pub from: Uuid,
    pub to: Uuid,
    #[schema(example = 25_000, minimum = 1)]
    pub amount_minor: i64,
    #[schema(example = "TJS")]
    pub currency: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AccountResponse {
    pub id: Uuid,
    #[schema(example = "customer")]
    pub kind: String,
    #[schema(example = "Firuz Rahimov")]
    pub owner: String,
    #[schema(example = "TJS")]
    pub currency: String,
    #[schema(example = 75_000)]
    pub balance_minor: i64,
    pub version: i64,
}

impl From<&Account> for AccountResponse {
    fn from(a: &Account) -> Self {
        Self {
            id: a.id().as_uuid(),
            kind: a.kind().as_str().to_owned(),
            owner: a.owner().to_owned(),
            currency: a.currency().to_string(),
            balance_minor: a.balance(),
            version: a.version(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TransferResponse {
    pub id: Uuid,
    #[schema(example = "transfer")]
    pub kind: String,
    pub from: Uuid,
    pub to: Uuid,
    pub amount_minor: i64,
    pub currency: String,
}

impl From<&Transfer> for TransferResponse {
    fn from(t: &Transfer) -> Self {
        Self {
            id: t.id().as_uuid(),
            kind: t.kind().as_str().to_owned(),
            from: t.from().as_uuid(),
            to: t.to().as_uuid(),
            amount_minor: t.amount().minor(),
            currency: t.amount().currency().to_string(),
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct EntriesQuery {
    #[param(minimum = 1, maximum = 100, default = 20)]
    pub limit: Option<u32>,
    #[param(value_type = Option<String>)]
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EntryResponse {
    pub id: i64,
    pub transfer_id: Uuid,
    #[schema(example = "transfer")]
    pub kind: String,
    pub counterparty: Uuid,
    #[schema(example = -25_000)]
    pub delta_minor: i64,
    #[schema(example = 75_000)]
    pub balance_after_minor: i64,
    #[schema(example = "TJS")]
    pub currency: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl EntryResponse {
    fn new(e: &EntryRecord, currency: &str) -> Self {
        Self {
            id: e.id,
            transfer_id: e.transfer_id.as_uuid(),
            kind: e.kind.as_str().to_owned(),
            counterparty: e.counterparty.as_uuid(),
            delta_minor: e.delta,
            balance_after_minor: e.balance_after,
            currency: currency.to_owned(),
            created_at: e.created_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EntryPageResponse {
    pub items: Vec<EntryResponse>,
    pub next_cursor: Option<String>,
}

impl EntryPageResponse {
    pub fn new(page: &EntryPage, next_cursor: Option<String>) -> Self {
        let currency = page.account.currency().to_string();
        Self {
            items: page
                .entries
                .iter()
                .map(|e| EntryResponse::new(e, &currency))
                .collect(),
            next_cursor,
        }
    }
}
