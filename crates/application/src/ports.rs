use std::{collections::HashMap, future::Future, pin::Pin};

use ledger_domain::{
    Account, AccountId, Currency, Email, IdempotencyKey, Password, Transfer, TransferId,
    TransferKind, User, UserId,
};
use time::OffsetDateTime;
use uuid::Uuid;

pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("optimistic concurrency conflict")]
    Conflict,
    #[error("duplicate idempotency key")]
    DuplicateIdempotencyKey,
    #[error("duplicate email")]
    DuplicateEmail,
    #[error("storage failure: {0}")]
    Storage(#[source] BoxError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryRecord {
    pub id: i64,
    pub transfer_id: TransferId,
    pub kind: TransferKind,
    pub counterparty: AccountId,
    pub delta: i64,
    pub balance_after: i64,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryQuery {
    pub account: AccountId,
    pub before: Option<i64>,
    pub limit: u32,
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

    fn list_accounts(
        &self,
        holder: UserId,
    ) -> impl Future<Output = Result<Vec<Account>, RepoError>> + Send;

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
        initiator: UserId,
        key: &IdempotencyKey,
    ) -> impl Future<Output = Result<Option<Transfer>, RepoError>> + Send;

    fn commit_transfer(
        &self,
        transfer: &Transfer,
        from: &Account,
        to: &Account,
        event: &OutboxMessage,
    ) -> impl Future<Output = Result<(), RepoError>> + Send;

    fn list_entries(
        &self,
        query: EntryQuery,
    ) -> impl Future<Output = Result<Vec<EntryRecord>, RepoError>> + Send;

    fn ping(&self) -> impl Future<Output = Result<(), RepoError>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxMessage {
    pub event_id: Uuid,
    pub subject: String,
    pub payload: Vec<u8>,
    pub headers: HashMap<String, String>,
    pub created_at: OffsetDateTime,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait EventPublisher: Send + Sync + 'static {
    fn publish<'a>(&'a self, message: &'a OutboxMessage) -> BoxFuture<'a, Result<(), BoxError>>;
}

pub trait OutboxStore: Send + Sync + 'static {
    fn relay_batch<'a>(
        &'a self,
        publisher: &'a dyn EventPublisher,
        limit: u32,
    ) -> impl Future<Output = Result<usize, RepoError>> + Send + 'a;

    fn pending(&self) -> impl Future<Output = Result<u64, RepoError>> + Send;
}

pub trait UserRepository: Send + Sync + 'static {
    fn insert_user(&self, user: &User) -> impl Future<Output = Result<(), RepoError>> + Send;

    fn find_user_by_email(
        &self,
        email: &Email,
    ) -> impl Future<Output = Result<Option<User>, RepoError>> + Send;
}

pub trait PasswordHasher: Send + Sync + 'static {
    fn hash(&self, password: &Password) -> impl Future<Output = Result<String, BoxError>> + Send;

    fn verify(
        &self,
        password: &str,
        hash: &str,
    ) -> impl Future<Output = Result<bool, BoxError>> + Send;

    fn decoy_hash(&self) -> &str;
}

pub trait IdGenerator: Send + Sync + 'static {
    fn generate(&self) -> Uuid;
}
