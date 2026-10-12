use std::collections::HashMap;

use ledger_domain::{
    Account, AccountId, AccountKind, Currency, IdempotencyKey, Money, Transfer, TransferId,
    TransferKind, post_transfer,
};
use ledger_events::{Envelope, Event, Party, TransferPosted};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    Actor, AppError,
    ports::{EntryQuery, EntryRecord, IdGenerator, LedgerRepository, OutboxMessage, RepoError},
};

pub const DEFAULT_PAGE_SIZE: u32 = 20;
pub const MAX_PAGE_SIZE: u32 = 100;

#[derive(Debug, Clone)]
pub struct OpenAccount {
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
pub struct ListEntries {
    pub account: Uuid,
    pub before: Option<i64>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct EntryPage {
    pub account: Account,
    pub entries: Vec<EntryRecord>,
    pub next_before: Option<i64>,
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

    #[tracing::instrument(skip(self), fields(user = %actor.user_id), err(level = "warn"))]
    pub async fn open_account(&self, actor: &Actor, cmd: OpenAccount) -> Result<Account, AppError> {
        let currency = Currency::parse(&cmd.currency)?;
        let account = Account::open(
            AccountId::from_uuid(self.ids.generate()),
            actor.user_id,
            &actor.name,
            currency,
        )?;
        self.repo.insert_account(&account).await?;
        tracing::info!(account_id = %account.id(), "account opened");
        Ok(account)
    }

    pub async fn list_accounts(&self, actor: &Actor) -> Result<Vec<Account>, AppError> {
        Ok(self.repo.list_accounts(actor.user_id).await?)
    }

    pub async fn get_account(&self, actor: &Actor, id: Uuid) -> Result<Account, AppError> {
        let account = self.load(AccountId::from_uuid(id)).await?;
        if actor.can_view(&account) {
            Ok(account)
        } else {
            Err(AppError::AccountNotFound(id))
        }
    }

    pub async fn get_transfer(&self, actor: &Actor, id: Uuid) -> Result<Transfer, AppError> {
        let transfer = self
            .repo
            .find_transfer(TransferId::from_uuid(id))
            .await?
            .ok_or(AppError::TransferNotFound(id))?;
        if actor.is_admin() {
            return Ok(transfer);
        }
        for side in [transfer.from(), transfer.to()] {
            if let Some(account) = self.repo.find_account(side).await?
                && account.is_held_by(actor.user_id)
            {
                return Ok(transfer);
            }
        }
        Err(AppError::TransferNotFound(id))
    }

    pub async fn list_entries(
        &self,
        actor: &Actor,
        query: ListEntries,
    ) -> Result<EntryPage, AppError> {
        let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);
        if !(1..=MAX_PAGE_SIZE).contains(&limit) {
            return Err(AppError::InvalidPageSize { max: MAX_PAGE_SIZE });
        }
        let account = self.get_account(actor, query.account).await?;
        let mut entries = self
            .repo
            .list_entries(EntryQuery {
                account: account.id(),
                before: query.before,
                limit: limit + 1,
            })
            .await?;
        let page_len = limit as usize;
        let next_before = if entries.len() > page_len {
            entries.truncate(page_len);
            entries.last().map(|e| e.id)
        } else {
            None
        };
        Ok(EntryPage {
            account,
            entries,
            next_before,
        })
    }

    #[tracing::instrument(skip(self), fields(user = %actor.user_id), err(level = "warn"))]
    pub async fn deposit(&self, actor: &Actor, cmd: Deposit) -> Result<TransferOutcome, AppError> {
        if !actor.is_admin() {
            return Err(AppError::Forbidden("only admins can deposit money"));
        }
        let key = IdempotencyKey::parse(&cmd.idempotency_key)?;
        let amount = Money::new(cmd.amount_minor, Currency::parse(&cmd.currency)?)?;
        self.post(
            actor,
            key,
            TransferKind::Deposit,
            Source::Treasury,
            AccountId::from_uuid(cmd.account),
            amount,
        )
        .await
    }

    #[tracing::instrument(skip(self), fields(user = %actor.user_id), err(level = "warn"))]
    pub async fn transfer(
        &self,
        actor: &Actor,
        cmd: TransferFunds,
    ) -> Result<TransferOutcome, AppError> {
        let key = IdempotencyKey::parse(&cmd.idempotency_key)?;
        let amount = Money::new(cmd.amount_minor, Currency::parse(&cmd.currency)?)?;
        let from = AccountId::from_uuid(cmd.from);
        if !self.load(from).await?.is_held_by(actor.user_id) {
            return Err(AppError::AccountNotFound(cmd.from));
        }
        self.post(
            actor,
            key,
            TransferKind::Transfer,
            Source::Account(from),
            AccountId::from_uuid(cmd.to),
            amount,
        )
        .await
    }

    async fn post(
        &self,
        actor: &Actor,
        key: IdempotencyKey,
        kind: TransferKind,
        source: Source,
        to: AccountId,
        amount: Money,
    ) -> Result<TransferOutcome, AppError> {
        for attempt in 1..=self.max_attempts {
            if let Some(existing) = self.repo.find_transfer_by_key(actor.user_id, &key).await? {
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
            if to_acc.kind() != AccountKind::Customer {
                return Err(AppError::AccountNotFound(to.as_uuid()));
            }

            let transfer = post_transfer(
                TransferId::from_uuid(self.ids.generate()),
                actor.user_id,
                key.clone(),
                kind,
                &mut from_acc,
                &mut to_acc,
                amount,
            )?;

            let event = self.transfer_posted(&transfer, &from_acc, &to_acc)?;
            match self
                .repo
                .commit_transfer(&transfer, &from_acc, &to_acc, &event)
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

    fn transfer_posted(
        &self,
        transfer: &Transfer,
        from: &Account,
        to: &Account,
    ) -> Result<OutboxMessage, AppError> {
        let party = |a: &Account| Party {
            account_id: a.id().as_uuid(),
            holder_id: a.holder().map(|h| h.as_uuid()),
            owner: a.owner().to_owned(),
            balance_after_minor: a.balance(),
        };
        let envelope = Envelope::new(
            self.ids.generate(),
            OffsetDateTime::now_utc(),
            TransferPosted {
                transfer_id: transfer.id().as_uuid(),
                kind: transfer.kind().as_str().to_owned(),
                amount_minor: transfer.amount().minor(),
                currency: transfer.amount().currency().to_string(),
                initiated_by: transfer.initiator().map(|u| u.as_uuid()),
                from: party(from),
                to: party(to),
            },
        );
        let payload = envelope
            .to_json()
            .map_err(|e| RepoError::Storage(Box::new(e)))?;
        Ok(OutboxMessage {
            event_id: envelope.id,
            subject: TransferPosted::SUBJECT.to_owned(),
            payload,
            headers: HashMap::new(),
            created_at: envelope.occurred_at,
        })
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
