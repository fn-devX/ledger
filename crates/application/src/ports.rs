use std::future::Future;

use ledger_domain::{Account, AccountId, Currency, IdempotencyKey, Transfer, TransferId};
use uuid::Uuid;

pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("optimistic concurrency conflict")]
    Conflict,
    #[error("duplicate idempotency key")]
    DuplicateIdempotencyKey,
    #[error("storage failure: {0}")]
    Storage(#[source] BoxError),
}

pub trait LedgerRepository: Send + Sync + 'static {
    fn insert_account(
        &self,
        account: &Account,
    ) -> impl Future<Output = Result<(), RepoError>> + Send;

    fn find_account(
        &self,
        id: AccountId,
    ) -> impl Future<Output = Result<Option<Account>, RepoError>> + Send;

    fn system_account(
        &self,
        currency: Currency,
        new_id: AccountId,
    ) -> impl Future<Output = Result<Account, RepoError>> + Send;

    fn find_transfer(
        &self,
        id: TransferId,
    ) -> impl Future<Output = Result<Option<Transfer>, RepoError>> + Send;

    fn find_transfer_by_key(
        &self,
        key: &IdempotencyKey,
    ) -> impl Future<Output = Result<Option<Transfer>, RepoError>> + Send;

    fn commit_transfer(
        &self,
        transfer: &Transfer,
        from: &Account,
        to: &Account,
    ) -> impl Future<Output = Result<(), RepoError>> + Send;

    fn ping(&self) -> impl Future<Output = Result<(), RepoError>> + Send;
}

pub trait IdGenerator: Send + Sync + 'static {
    fn generate(&self) -> Uuid;
}
