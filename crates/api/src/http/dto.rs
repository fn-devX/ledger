use ledger_domain::{Account, Transfer};
use serde::{Deserialize, Serialize};
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
