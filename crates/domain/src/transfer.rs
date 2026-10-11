use std::fmt;

use uuid::Uuid;

use crate::{Account, AccountId, DomainError, Money};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransferId(Uuid);

impl TransferId {
    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl fmt::Display for TransferId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn parse(raw: &str) -> Result<Self, DomainError> {
        let ok = (1..=128).contains(&raw.len()) && raw.bytes().all(|b| b.is_ascii_graphic());
        if ok {
            Ok(Self(raw.to_owned()))
        } else {
            Err(DomainError::InvalidIdempotencyKey)
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransferKind {
    Deposit,
    Transfer,
}

impl TransferKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deposit => "deposit",
            Self::Transfer => "transfer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub account: AccountId,
    pub delta: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    id: TransferId,
    idempotency_key: IdempotencyKey,
    kind: TransferKind,
    from: AccountId,
    to: AccountId,
    amount: Money,
}

impl Transfer {
    pub fn restore(
        id: TransferId,
        idempotency_key: IdempotencyKey,
        kind: TransferKind,
        from: AccountId,
        to: AccountId,
        amount: Money,
    ) -> Self {
        Self {
            id,
            idempotency_key,
            kind,
            from,
            to,
            amount,
        }
    }

    pub fn id(&self) -> TransferId {
        self.id
    }
    pub fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }
    pub fn kind(&self) -> TransferKind {
        self.kind
    }
    pub fn from(&self) -> AccountId {
        self.from
    }
    pub fn to(&self) -> AccountId {
        self.to
    }
    pub fn amount(&self) -> Money {
        self.amount
    }

    pub fn entries(&self) -> [Entry; 2] {
        let m = self.amount.minor();
        [
            Entry {
                account: self.from,
                delta: -m,
            },
            Entry {
                account: self.to,
                delta: m,
            },
        ]
    }
}

pub fn post_transfer(
    id: TransferId,
    idempotency_key: IdempotencyKey,
    kind: TransferKind,
    from: &mut Account,
    to: &mut Account,
    amount: Money,
) -> Result<Transfer, DomainError> {
    if from.id() == to.id() {
        return Err(DomainError::SameAccount);
    }
    let new_from = from.balance_after_debit(amount)?;
    let new_to = to.balance_after_credit(amount)?;
    from.apply(new_from);
    to.apply(new_to);

    Ok(Transfer {
        id,
        idempotency_key,
        kind,
        from: from.id(),
        to: to.id(),
        amount,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccountKind, Currency};

    fn acc(n: u128, balance: i64, cur: &str) -> Account {
        Account::restore(
            AccountId::from_uuid(Uuid::from_u128(n)),
            AccountKind::Customer,
            "x".into(),
            Currency::parse(cur).unwrap(),
            balance,
            7,
        )
    }

    fn money(minor: i64, cur: &str) -> Money {
        Money::new(minor, Currency::parse(cur).unwrap()).unwrap()
    }

    fn post(from: &mut Account, to: &mut Account, amount: Money) -> Result<Transfer, DomainError> {
        post_transfer(
            TransferId::from_uuid(Uuid::nil()),
            IdempotencyKey::parse("k").unwrap(),
            TransferKind::Transfer,
            from,
            to,
            amount,
        )
    }

    #[test]
    fn moves_money_and_bumps_versions() {
        let (mut a, mut b) = (acc(1, 1_000, "TJS"), acc(2, 0, "TJS"));
        let t = post(&mut a, &mut b, money(300, "TJS")).unwrap();
        assert_eq!((a.balance(), b.balance()), (700, 300));
        assert_eq!((a.version(), a.loaded_version()), (8, 7));
        assert_eq!(t.entries().iter().map(|e| e.delta).sum::<i64>(), 0);
    }

    #[test]
    fn failed_transfer_changes_nothing() {
        let (mut a, mut b) = (acc(1, 100, "TJS"), acc(2, 0, "USD"));
        let before = (a.clone(), b.clone());
        assert!(matches!(
            post(&mut a, &mut b, money(50, "TJS")),
            Err(DomainError::CurrencyMismatch { .. })
        ));
        assert_eq!((a, b), before);
    }

    #[test]
    fn rejects_same_account_and_overdraft() {
        let mut a = acc(1, 100, "TJS");
        let mut a2 = a.clone();
        assert_eq!(
            post(&mut a, &mut a2, money(1, "TJS")),
            Err(DomainError::SameAccount)
        );
        let mut b = acc(2, 0, "TJS");
        assert!(matches!(
            post(&mut a, &mut b, money(101, "TJS")),
            Err(DomainError::InsufficientFunds { .. })
        ));
    }

    #[test]
    fn idempotency_key_validation() {
        assert!(IdempotencyKey::parse("order-42").is_ok());
        assert!(IdempotencyKey::parse("").is_err());
        assert!(IdempotencyKey::parse("has space").is_err());
        assert!(IdempotencyKey::parse(&"x".repeat(129)).is_err());
    }
}
