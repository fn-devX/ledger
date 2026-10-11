use ledger_domain::{
    Account, AccountId, Currency, IdempotencyKey, Money, Transfer, TransferId, TransferKind,
    post_transfer,
};
use uuid::Uuid;

use crate::{
    AppError,
    ports::{IdGenerator, LedgerRepository, RepoError},
};

#[derive(Debug, Clone)]
pub struct OpenAccount {
    pub owner: String,
    pub currency: String,
}

#[derive(Debug, Clone)]
pub struct Deposit {
    pub idempotency_key: String,
    pub account: Uuid,
    pub amount_minor: i64,
    pub currency: String,
}

#[derive(Debug, Clone)]
pub struct TransferFunds {
    pub idempotency_key: String,
    pub from: Uuid,
    pub to: Uuid,
    pub amount_minor: i64,
    pub currency: String,
}

#[derive(Debug, Clone)]
pub struct TransferOutcome {
    pub transfer: Transfer,
    pub replayed: bool,
}

#[derive(Debug, Clone, Copy)]
enum Source {
    Account(AccountId),
    Treasury,
}

#[derive(Debug)]
pub struct LedgerService<R, G> {
    repo: R,
    ids: G,
    max_attempts: u32,
}

impl<R: LedgerRepository, G: IdGenerator> LedgerService<R, G> {
    pub fn new(repo: R, ids: G) -> Self {
        Self {
            repo,
            ids,
            max_attempts: 5,
        }
    }

    #[must_use]
    pub fn with_max_attempts(mut self, attempts: u32) -> Self {
        self.max_attempts = attempts.max(1);
        self
    }

    pub fn repository(&self) -> &R {
        &self.repo
    }

    #[tracing::instrument(skip(self), err(level = "warn"))]
    pub async fn open_account(&self, cmd: OpenAccount) -> Result<Account, AppError> {
        let currency = Currency::parse(&cmd.currency)?;
        let account = Account::open(
            AccountId::from_uuid(self.ids.generate()),
            &cmd.owner,
            currency,
        )?;
        self.repo.insert_account(&account).await?;
        tracing::info!(account_id = %account.id(), "account opened");
        Ok(account)
    }

    pub async fn get_account(&self, id: Uuid) -> Result<Account, AppError> {
        self.repo
            .find_account(AccountId::from_uuid(id))
            .await?
            .ok_or(AppError::AccountNotFound(id))
    }

    pub async fn get_transfer(&self, id: Uuid) -> Result<Transfer, AppError> {
        self.repo
            .find_transfer(TransferId::from_uuid(id))
            .await?
            .ok_or(AppError::TransferNotFound(id))
    }

    #[tracing::instrument(skip(self), err(level = "warn"))]
    pub async fn deposit(&self, cmd: Deposit) -> Result<TransferOutcome, AppError> {
        let key = IdempotencyKey::parse(&cmd.idempotency_key)?;
        let amount = Money::new(cmd.amount_minor, Currency::parse(&cmd.currency)?)?;
        self.post(
            key,
            TransferKind::Deposit,
            Source::Treasury,
            AccountId::from_uuid(cmd.account),
            amount,
        )
        .await
    }

    #[tracing::instrument(skip(self), err(level = "warn"))]
    pub async fn transfer(&self, cmd: TransferFunds) -> Result<TransferOutcome, AppError> {
        let key = IdempotencyKey::parse(&cmd.idempotency_key)?;
        let amount = Money::new(cmd.amount_minor, Currency::parse(&cmd.currency)?)?;
        let from = Source::Account(AccountId::from_uuid(cmd.from));
        self.post(
            key,
            TransferKind::Transfer,
            from,
            AccountId::from_uuid(cmd.to),
            amount,
        )
        .await
    }

    async fn post(
        &self,
        key: IdempotencyKey,
        kind: TransferKind,
        source: Source,
        to: AccountId,
        amount: Money,
    ) -> Result<TransferOutcome, AppError> {
        for attempt in 1..=self.max_attempts {
            if let Some(existing) = self.repo.find_transfer_by_key(&key).await? {
                return Self::replay(existing, kind, source, to, amount);
            }

            let mut from_acc = match source {
                Source::Account(id) => self.load(id).await?,
                Source::Treasury => {
                    let new_id = AccountId::from_uuid(self.ids.generate());
                    self.repo.system_account(amount.currency(), new_id).await?
                }
            };
            let mut to_acc = self.load(to).await?;
            if kind == TransferKind::Deposit
                && to_acc.kind() != ledger_domain::AccountKind::Customer
            {
                return Err(AppError::AccountNotFound(to.as_uuid()));
            }

            let transfer = post_transfer(
                TransferId::from_uuid(self.ids.generate()),
                key.clone(),
                kind,
                &mut from_acc,
                &mut to_acc,
                amount,
            )?;

            match self
                .repo
                .commit_transfer(&transfer, &from_acc, &to_acc)
                .await
            {
                Ok(()) => {
                    tracing::info!(transfer_id = %transfer.id(), %amount, attempt, "transfer posted");
                    return Ok(TransferOutcome {
                        transfer,
                        replayed: false,
                    });
                }
                Err(RepoError::Conflict | RepoError::DuplicateIdempotencyKey) => {
                    tracing::debug!(attempt, "concurrent modification, retrying");
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(AppError::Contention)
    }

    async fn load(&self, id: AccountId) -> Result<Account, AppError> {
        self.repo
            .find_account(id)
            .await?
            .ok_or(AppError::AccountNotFound(id.as_uuid()))
    }

    fn replay(
        existing: Transfer,
        kind: TransferKind,
        source: Source,
        to: AccountId,
        amount: Money,
    ) -> Result<TransferOutcome, AppError> {
        let same_source = match source {
            Source::Account(id) => existing.from() == id,
            Source::Treasury => true,
        };
        if existing.kind() == kind
            && same_source
            && existing.to() == to
            && existing.amount() == amount
        {
            Ok(TransferOutcome {
                transfer: existing,
                replayed: true,
            })
        } else {
            Err(AppError::IdempotencyKeyReused)
        }
    }
}
