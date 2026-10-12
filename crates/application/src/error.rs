use ledger_domain::DomainError;
use uuid::Uuid;

use crate::ports::RepoError;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error("account {0} not found")]
    AccountNotFound(Uuid),
    #[error("transfer {0} not found")]
    TransferNotFound(Uuid),
    #[error("invalid email or password")]
    InvalidCredentials,
    #[error("{0}")]
    Forbidden(&'static str),
    #[error("a user with this email already exists")]
    EmailTaken,
    #[error("idempotency key was already used with different parameters")]
    IdempotencyKeyReused,
    #[error("page size must be between 1 and {max}")]
    InvalidPageSize { max: u32 },
    #[error("too much contention, retry later")]
    Contention,
    #[error(transparent)]
    Repository(#[from] RepoError),
}
