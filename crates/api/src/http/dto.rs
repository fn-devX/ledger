use ledger_application::{EntryPage, ports::EntryRecord};
use ledger_domain::{Account, Transfer};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAccountRequest {
    pub owner: String,
    pub currency: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepositRequest {
    pub amount_minor: i64,
    pub currency: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferRequest {
    pub from: Uuid,
    pub to: Uuid,
    pub amount_minor: i64,
    pub currency: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccountResponse {
    pub id: Uuid,
    pub kind: String,
    pub owner: String,
    pub currency: String,
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

#[derive(Debug, Serialize, Deserialize)]
pub struct TransferResponse {
    pub id: Uuid,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntriesQuery {
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EntryResponse {
    pub id: i64,
    pub transfer_id: Uuid,
    pub kind: String,
    pub counterparty: Uuid,
    pub delta_minor: i64,
    pub balance_after_minor: i64,
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

#[derive(Debug, Serialize, Deserialize)]
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
