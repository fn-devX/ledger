use std::{sync::Arc, time::Duration};

use ledger_application::ports::{
    EntryQuery, EntryRecord, EventPublisher, LedgerRepository, OutboxMessage, OutboxStore,
    RepoError, UserRepository,
};
use ledger_domain::{
    Account, AccountId, AccountKind, Currency, Email, IdempotencyKey, Money, Role, Transfer,
    TransferId, TransferKind, User, UserId,
};
use sqlx::{
    PgPool,
    postgres::{PgListener, PgPoolOptions},
};
use time::OffsetDateTime;
use tokio::{sync::Notify, task::JoinHandle};
use uuid::Uuid;

const OUTBOX_CHANNEL: &str = "ledger_outbox";

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

#[derive(Debug, Clone)]
pub struct PgLedgerRepository {
    pool: PgPool,
}

fn storage<E: std::error::Error + Send + Sync + 'static>(e: E) -> RepoError {
    RepoError::Storage(Box::new(e))
}

impl PgLedgerRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(url: &str, max_connections: u32) -> Result<Self, RepoError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(url)
            .await
            .map_err(storage)?;
        Ok(Self::new(pool))
    }

    pub async fn migrate(&self) -> Result<(), RepoError> {
        MIGRATOR.run(&self.pool).await.map_err(storage)
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn spawn_outbox_listener(&self, signal: Arc<Notify>) -> JoinHandle<()> {
        let pool = self.pool.clone();
        tokio::spawn(async move {
            loop {
                match PgListener::connect_with(&pool).await {
                    Ok(mut listener) => match listener.listen(OUTBOX_CHANNEL).await {
                        Ok(()) => {
                            signal.notify_one();
                            while listener.recv().await.is_ok() {
                                signal.notify_one();
                            }
                            tracing::warn!("outbox listener disconnected");
                        }
                        Err(e) => tracing::warn!(error = %e, "cannot LISTEN on outbox channel"),
                    },
                    Err(e) => tracing::warn!(error = %e, "cannot connect outbox listener"),
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
    }
}

struct AccountRow {
    id: Uuid,
    kind: String,
    holder_id: Option<Uuid>,
    owner: String,
    currency: String,
    balance: i64,
    version: i64,
}

impl TryFrom<AccountRow> for Account {
    type Error = RepoError;
    fn try_from(r: AccountRow) -> Result<Self, RepoError> {
        let kind = match r.kind.as_str() {
            "customer" => AccountKind::Customer,
            "system" => AccountKind::System,
            other => return Err(corrupt(format!("unknown account kind {other}"))),
        };
        let currency = Currency::parse(&r.currency).map_err(storage)?;
        Ok(Account::restore(
            AccountId::from_uuid(r.id),
            kind,
            r.holder_id.map(UserId::from_uuid),
            r.owner,
            currency,
            r.balance,
            r.version,
        ))
    }
}

struct TransferRow {
    id: Uuid,
    initiated_by: Option<Uuid>,
    idempotency_key: String,
    kind: String,
    from_account: Uuid,
    to_account: Uuid,
    amount: i64,
    currency: String,
}

impl TryFrom<TransferRow> for Transfer {
    type Error = RepoError;
    fn try_from(r: TransferRow) -> Result<Self, RepoError> {
        let kind = parse_transfer_kind(&r.kind)?;
        let currency = Currency::parse(&r.currency).map_err(storage)?;
        Ok(Transfer::restore(
            TransferId::from_uuid(r.id),
            r.initiated_by.map(UserId::from_uuid),
            IdempotencyKey::parse(&r.idempotency_key).map_err(storage)?,
            kind,
            AccountId::from_uuid(r.from_account),
            AccountId::from_uuid(r.to_account),
            Money::new(r.amount, currency).map_err(storage)?,
        ))
    }
}

struct EntryRow {
    id: i64,
    transfer_id: Uuid,
    kind: String,
    counterparty: Uuid,
    delta: i64,
    balance_after: i64,
    created_at: OffsetDateTime,
}

impl TryFrom<EntryRow> for EntryRecord {
    type Error = RepoError;
    fn try_from(r: EntryRow) -> Result<Self, RepoError> {
        Ok(EntryRecord {
            id: r.id,
            transfer_id: TransferId::from_uuid(r.transfer_id),
            kind: parse_transfer_kind(&r.kind)?,
            counterparty: AccountId::from_uuid(r.counterparty),
            delta: r.delta,
            balance_after: r.balance_after,
            created_at: r.created_at,
        })
    }
}

fn parse_transfer_kind(raw: &str) -> Result<TransferKind, RepoError> {
    match raw {
        "deposit" => Ok(TransferKind::Deposit),
        "transfer" => Ok(TransferKind::Transfer),
        other => Err(corrupt(format!("unknown transfer kind {other}"))),
    }
}

fn corrupt(msg: String) -> RepoError {
    RepoError::Storage(msg.into())
}

impl LedgerRepository for PgLedgerRepository {
    async fn insert_account(&self, a: &Account) -> Result<(), RepoError> {
        let currency = a.currency();
        sqlx::query!(
            "INSERT INTO accounts (id, kind, holder_id, owner, currency, balance, version)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            a.id().as_uuid(),
            a.kind().as_str(),
            a.holder().map(|h| h.as_uuid()),
            a.owner(),
            currency.as_str(),
            a.balance(),
            a.version(),
        )
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(())
    }

    async fn find_account(&self, id: AccountId) -> Result<Option<Account>, RepoError> {
        sqlx::query_as!(
            AccountRow,
            "SELECT id, kind, holder_id, owner, currency, balance, version FROM accounts WHERE id = $1",
            id.as_uuid(),
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .map(Account::try_from)
        .transpose()
    }

    async fn list_accounts(&self, holder: UserId) -> Result<Vec<Account>, RepoError> {
        sqlx::query_as!(
            AccountRow,
            "SELECT id, kind, holder_id, owner, currency, balance, version
             FROM accounts WHERE holder_id = $1 ORDER BY id",
            holder.as_uuid(),
        )
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?
        .into_iter()
        .map(Account::try_from)
        .collect()
    }

    async fn system_account(
        &self,
        currency: Currency,
        new_id: AccountId,
    ) -> Result<Account, RepoError> {
        sqlx::query!(
            "INSERT INTO accounts (id, kind, owner, currency) VALUES ($1, 'system', $2, $3)
             ON CONFLICT (currency) WHERE kind = 'system' DO NOTHING",
            new_id.as_uuid(),
            format!("system:{currency}"),
            currency.as_str(),
        )
        .execute(&self.pool)
        .await
        .map_err(storage)?;

        sqlx::query_as!(
            AccountRow,
            "SELECT id, kind, holder_id, owner, currency, balance, version
             FROM accounts WHERE kind = 'system' AND currency = $1",
            currency.as_str(),
        )
        .fetch_one(&self.pool)
        .await
        .map_err(storage)?
        .try_into()
    }

    async fn find_transfer(&self, id: TransferId) -> Result<Option<Transfer>, RepoError> {
        sqlx::query_as!(
            TransferRow,
            "SELECT id, initiated_by, idempotency_key, kind, from_account, to_account, amount, currency
             FROM transfers WHERE id = $1",
            id.as_uuid(),
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .map(Transfer::try_from)
        .transpose()
    }

    async fn find_transfer_by_key(
        &self,
        initiator: UserId,
        key: &IdempotencyKey,
    ) -> Result<Option<Transfer>, RepoError> {
        sqlx::query_as!(
            TransferRow,
            "SELECT id, initiated_by, idempotency_key, kind, from_account, to_account, amount, currency
             FROM transfers WHERE initiated_by = $1 AND idempotency_key = $2",
            initiator.as_uuid(),
            key.as_str(),
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .map(Transfer::try_from)
        .transpose()
    }

    async fn commit_transfer(
        &self,
        t: &Transfer,
        from: &Account,
        to: &Account,
        event: &OutboxMessage,
    ) -> Result<(), RepoError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;

        let mut accounts = [from, to];
        accounts.sort_by_key(|a| a.id());
        for acc in accounts {
            let res = sqlx::query!(
                "UPDATE accounts SET balance = $1, version = $2, updated_at = now()
                 WHERE id = $3 AND version = $4",
                acc.balance(),
                acc.version(),
                acc.id().as_uuid(),
                acc.loaded_version(),
            )
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
            if res.rows_affected() != 1 {
                return Err(RepoError::Conflict);
            }
        }

        let currency = t.amount().currency();
        let inserted = sqlx::query!(
            "INSERT INTO transfers
                 (id, initiated_by, idempotency_key, kind, from_account, to_account, amount, currency)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            t.id().as_uuid(),
            t.initiator().map(|u| u.as_uuid()),
            t.idempotency_key().as_str(),
            t.kind().as_str(),
            t.from().as_uuid(),
            t.to().as_uuid(),
            t.amount().minor(),
            currency.as_str(),
        )
        .execute(&mut *tx)
        .await;
        if let Err(e) = inserted {
            return Err(match &e {
                sqlx::Error::Database(db) if db.is_unique_violation() => {
                    RepoError::DuplicateIdempotencyKey
                }
                _ => storage(e),
            });
        }

        for entry in t.entries() {
            let balance_after = if entry.account == from.id() {
                from.balance()
            } else {
                to.balance()
            };
            sqlx::query!(
                "INSERT INTO ledger_entries (transfer_id, account_id, delta, balance_after)
                 VALUES ($1, $2, $3, $4)",
                t.id().as_uuid(),
                entry.account.as_uuid(),
                entry.delta,
                balance_after,
            )
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        }

        let payload: serde_json::Value = serde_json::from_slice(&event.payload).map_err(storage)?;
        sqlx::query!(
            "INSERT INTO outbox (event_id, subject, payload) VALUES ($1, $2, $3)",
            event.event_id,
            event.subject,
            payload,
        )
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        sqlx::query("SELECT pg_notify('ledger_outbox', '')")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;

        tx.commit().await.map_err(storage)
    }

    async fn list_entries(&self, query: EntryQuery) -> Result<Vec<EntryRecord>, RepoError> {
        sqlx::query_as!(
            EntryRow,
            r#"SELECT e.id, e.transfer_id, t.kind,
                      CASE WHEN t.from_account = e.account_id THEN t.to_account
                           ELSE t.from_account END AS "counterparty!",
                      e.delta, e.balance_after, e.created_at
               FROM ledger_entries e
               JOIN transfers t ON t.id = e.transfer_id
               WHERE e.account_id = $1 AND ($2::BIGINT IS NULL OR e.id < $2)
               ORDER BY e.id DESC
               LIMIT $3"#,
            query.account.as_uuid(),
            query.before,
            i64::from(query.limit),
        )
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?
        .into_iter()
        .map(EntryRecord::try_from)
        .collect()
    }

    async fn ping(&self) -> Result<(), RepoError> {
        sqlx::query!("SELECT 1 AS ok")
            .fetch_one(&self.pool)
            .await
            .map_err(storage)?;
        Ok(())
    }
}

struct UserRow {
    id: Uuid,
    email: String,
    name: String,
    role: String,
    password_hash: String,
}

impl TryFrom<UserRow> for User {
    type Error = RepoError;
    fn try_from(r: UserRow) -> Result<Self, RepoError> {
        let role =
            Role::parse(&r.role).ok_or_else(|| corrupt(format!("unknown role {}", r.role)))?;
        Ok(User::restore(
            UserId::from_uuid(r.id),
            Email::parse(&r.email).map_err(storage)?,
            r.name,
            role,
            r.password_hash,
        ))
    }
}

impl UserRepository for PgLedgerRepository {
    async fn insert_user(&self, user: &User) -> Result<(), RepoError> {
        sqlx::query!(
            "INSERT INTO users (id, email, name, role, password_hash) VALUES ($1, $2, $3, $4, $5)",
            user.id().as_uuid(),
            user.email().as_str(),
            user.name(),
            user.role().as_str(),
            user.password_hash(),
        )
        .execute(&self.pool)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => RepoError::DuplicateEmail,
            _ => storage(e),
        })?;
        Ok(())
    }

    async fn find_user_by_email(&self, email: &Email) -> Result<Option<User>, RepoError> {
        sqlx::query_as!(
            UserRow,
            "SELECT id, email, name, role, password_hash FROM users WHERE email = $1",
            email.as_str(),
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .map(User::try_from)
        .transpose()
    }
}

impl OutboxStore for PgLedgerRepository {
    async fn relay_batch<'a>(
        &'a self,
        publisher: &'a dyn EventPublisher,
        limit: u32,
    ) -> Result<usize, RepoError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let rows = sqlx::query!(
            "SELECT id, event_id, subject, payload FROM outbox
             WHERE published_at IS NULL
             ORDER BY id
             LIMIT $1
             FOR UPDATE SKIP LOCKED",
            i64::from(limit),
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(storage)?;

        let mut sent = Vec::with_capacity(rows.len());
        let mut failure = None;
        for row in rows {
            let message = OutboxMessage {
                event_id: row.event_id,
                subject: row.subject,
                payload: serde_json::to_vec(&row.payload).map_err(storage)?,
            };
            match publisher.publish(&message).await {
                Ok(()) => sent.push(row.id),
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }

        if !sent.is_empty() {
            sqlx::query!(
                "UPDATE outbox SET published_at = now() WHERE id = ANY($1)",
                &sent,
            )
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        }
        tx.commit().await.map_err(storage)?;
        match failure {
            Some(e) => Err(RepoError::Storage(e)),
            None => Ok(sent.len()),
        }
    }
}
