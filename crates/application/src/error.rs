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
    #[error("idempotency key was already used with different parameters")]
    IdempotencyKeyReused,
    #[error("too much contention, retry later")]
    Contention,
    #[error(transparent)]
    Repository(#[from] RepoError),
}
