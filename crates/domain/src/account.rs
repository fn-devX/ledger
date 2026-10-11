use std::fmt;

use uuid::Uuid;

use crate::{Currency, DomainError, Money};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountId(Uuid);

impl AccountId {
    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountKind {
    Customer,
    System,
}

impl AccountKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Customer => "customer",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    id: AccountId,
    kind: AccountKind,
    owner: String,
    currency: Currency,
    balance: i64,
    version: i64,
    loaded_version: i64,
}

impl Account {
    pub fn open(id: AccountId, owner: &str, currency: Currency) -> Result<Self, DomainError> {
        let owner = owner.trim();
        if owner.is_empty() || owner.chars().count() > 200 {
            return Err(DomainError::InvalidOwner);
        }
        Ok(Self::restore(
            id,
            AccountKind::Customer,
            owner.to_owned(),
            currency,
            0,
            0,
        ))
    }

    pub fn system(id: AccountId, currency: Currency) -> Self {
        Self::restore(
            id,
            AccountKind::System,
            format!("system:{currency}"),
            currency,
            0,
            0,
        )
    }

    pub fn restore(
        id: AccountId,
        kind: AccountKind,
        owner: String,
        currency: Currency,
        balance: i64,
        version: i64,
    ) -> Self {
        Self {
            id,
            kind,
            owner,
            currency,
            balance,
            version,
            loaded_version: version,
        }
    }

    pub fn id(&self) -> AccountId {
        self.id
    }
    pub fn kind(&self) -> AccountKind {
        self.kind
    }
    pub fn owner(&self) -> &str {
        &self.owner
    }
    pub fn currency(&self) -> Currency {
        self.currency
    }
    pub fn balance(&self) -> i64 {
        self.balance
    }
    pub fn version(&self) -> i64 {
        self.version
    }
    pub fn loaded_version(&self) -> i64 {
        self.loaded_version
    }

    fn ensure_currency(&self, amount: Money) -> Result<(), DomainError> {
        if self.currency == amount.currency() {
            Ok(())
        } else {
            Err(DomainError::CurrencyMismatch {
                expected: self.currency,
                actual: amount.currency(),
            })
        }
    }

    pub(crate) fn balance_after_debit(&self, amount: Money) -> Result<i64, DomainError> {
        self.ensure_currency(amount)?;
        let new = self
            .balance
            .checked_sub(amount.minor())
            .ok_or(DomainError::BalanceOverflow)?;
        if self.kind == AccountKind::Customer && new < 0 {
            return Err(DomainError::InsufficientFunds {
                available: self.balance,
                requested: amount.minor(),
            });
        }
        Ok(new)
    }

    pub(crate) fn balance_after_credit(&self, amount: Money) -> Result<i64, DomainError> {
        self.ensure_currency(amount)?;
        self.balance
            .checked_add(amount.minor())
            .ok_or(DomainError::BalanceOverflow)
    }

    pub(crate) fn apply(&mut self, new_balance: i64) {
        self.balance = new_balance;
        self.version += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tjs() -> Currency {
        Currency::parse("TJS").unwrap()
    }

    #[test]
    fn open_validates_owner() {
        let id = AccountId::from_uuid(Uuid::nil());
        assert_eq!(
            Account::open(id, "   ", tjs()),
            Err(DomainError::InvalidOwner)
        );
        let acc = Account::open(id, "  Rustam ", tjs()).unwrap();
        assert_eq!(acc.owner(), "Rustam");
        assert_eq!(acc.balance(), 0);
    }

    #[test]
    fn customer_cannot_go_negative_but_system_can() {
        let id = AccountId::from_uuid(Uuid::nil());
        let amount = Money::new(100, tjs()).unwrap();
        let customer = Account::open(id, "a", tjs()).unwrap();
        assert!(matches!(
            customer.balance_after_debit(amount),
            Err(DomainError::InsufficientFunds {
                available: 0,
                requested: 100
            })
        ));
        assert_eq!(
            Account::system(id, tjs()).balance_after_debit(amount),
            Ok(-100)
        );
    }
}
