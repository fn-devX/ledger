use crate::money::Currency;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    #[error("invalid currency code `{0}`: expected 3 uppercase latin letters (ISO 4217)")]
    InvalidCurrency(String),
    #[error("amount must be positive")]
    NonPositiveAmount,
    #[error("owner must be 1..=200 characters")]
    InvalidOwner,
    #[error("idempotency key must be 1..=128 visible ASCII characters")]
    InvalidIdempotencyKey,
    #[error("cannot transfer to the same account")]
    SameAccount,
    #[error("currency mismatch: account is in {expected}, operation is in {actual}")]
    CurrencyMismatch {
        expected: Currency,
        actual: Currency,
    },
    #[error("insufficient funds: available {available}, requested {requested}")]
    InsufficientFunds { available: i64, requested: i64 },
    #[error("balance overflow")]
    BalanceOverflow,
    #[error("invalid email address")]
    InvalidEmail,
    #[error("password must be {min}..={max} characters")]
    WeakPassword { min: usize, max: usize },
}
