use ledger_application::ports::{EntryQuery, EntryRecord, LedgerRepository, RepoError};
use ledger_domain::{
    Account, AccountId, AccountKind, Currency, IdempotencyKey, Money, Transfer, TransferId,
    TransferKind,
};
use sqlx::{PgPool, postgres::PgPoolOptions};
use time::OffsetDateTime;
use uuid::Uuid;

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
}

#[derive(sqlx::FromRow)]
struct AccountRow {
    id: Uuid,
    kind: String,
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
            r.owner,
            currency,
            r.balance,
            r.version,
        ))
    }
}

#[derive(sqlx::FromRow)]
struct TransferRow {
    id: Uuid,
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
            IdempotencyKey::parse(&r.idempotency_key).map_err(storage)?,
            kind,
            AccountId::from_uuid(r.from_account),
            AccountId::from_uuid(r.to_account),
            Money::new(r.amount, currency).map_err(storage)?,
        ))
    }
}

#[derive(sqlx::FromRow)]
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

const ACCOUNT_COLS: &str = "id, kind, owner, currency, balance, version";
const TRANSFER_COLS: &str = "id, idempotency_key, kind, from_account, to_account, amount, currency";

impl LedgerRepository for PgLedgerRepository {
    async fn insert_account(&self, a: &Account) -> Result<(), RepoError> {
        sqlx::query(
            "INSERT INTO accounts (id, kind, owner, currency, balance, version)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(a.id().as_uuid())
        .bind(a.kind().as_str())
        .bind(a.owner())
        .bind(a.currency().as_str())
        .bind(a.balance())
        .bind(a.version())
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(())
    }

    async fn find_account(&self, id: AccountId) -> Result<Option<Account>, RepoError> {
        sqlx::query_as::<_, AccountRow>(&format!(
            "SELECT {ACCOUNT_COLS} FROM accounts WHERE id = $1"
        ))
        .bind(id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .map(Account::try_from)
        .transpose()
    }

    async fn system_account(
        &self,
        currency: Currency,
        new_id: AccountId,
    ) -> Result<Account, RepoError> {
        sqlx::query(
            "INSERT INTO accounts (id, kind, owner, currency) VALUES ($1, 'system', $2, $3)
             ON CONFLICT (currency) WHERE kind = 'system' DO NOTHING",
        )
        .bind(new_id.as_uuid())
        .bind(format!("system:{currency}"))
        .bind(currency.as_str())
        .execute(&self.pool)
        .await
        .map_err(storage)?;

        let row = sqlx::query_as::<_, AccountRow>(&format!(
            "SELECT {ACCOUNT_COLS} FROM accounts WHERE kind = 'system' AND currency = $1"
        ))
        .bind(currency.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(storage)?;
        row.try_into()
    }

    async fn find_transfer(&self, id: TransferId) -> Result<Option<Transfer>, RepoError> {
        sqlx::query_as::<_, TransferRow>(&format!(
            "SELECT {TRANSFER_COLS} FROM transfers WHERE id = $1"
        ))
        .bind(id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .map(Transfer::try_from)
        .transpose()
    }

    async fn find_transfer_by_key(
        &self,
        key: &IdempotencyKey,
    ) -> Result<Option<Transfer>, RepoError> {
        sqlx::query_as::<_, TransferRow>(&format!(
            "SELECT {TRANSFER_COLS} FROM transfers WHERE idempotency_key = $1"
        ))
        .bind(key.as_str())
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
    ) -> Result<(), RepoError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;

        let mut accounts = [from, to];
        accounts.sort_by_key(|a| a.id());
        for acc in accounts {
            let res = sqlx::query(
                "UPDATE accounts SET balance = $1, version = $2, updated_at = now()
                 WHERE id = $3 AND version = $4",
            )
            .bind(acc.balance())
            .bind(acc.version())
            .bind(acc.id().as_uuid())
            .bind(acc.loaded_version())
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
            if res.rows_affected() != 1 {
                return Err(RepoError::Conflict);
            }
        }

        let inserted = sqlx::query(
            "INSERT INTO transfers (id, idempotency_key, kind, from_account, to_account, amount, currency)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(t.id().as_uuid())
        .bind(t.idempotency_key().as_str())
        .bind(t.kind().as_str())
        .bind(t.from().as_uuid())
        .bind(t.to().as_uuid())
        .bind(t.amount().minor())
        .bind(t.amount().currency().as_str())
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
            sqlx::query(
                "INSERT INTO ledger_entries (transfer_id, account_id, delta, balance_after)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(t.id().as_uuid())
            .bind(entry.account.as_uuid())
            .bind(entry.delta)
            .bind(balance_after)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        }

        tx.commit().await.map_err(storage)
    }

    async fn list_entries(&self, query: EntryQuery) -> Result<Vec<EntryRecord>, RepoError> {
        sqlx::query_as::<_, EntryRow>(
            "SELECT e.id, e.transfer_id, t.kind,
                    CASE WHEN t.from_account = e.account_id THEN t.to_account
                         ELSE t.from_account END AS counterparty,
                    e.delta, e.balance_after, e.created_at
             FROM ledger_entries e
             JOIN transfers t ON t.id = e.transfer_id
             WHERE e.account_id = $1 AND ($2::BIGINT IS NULL OR e.id < $2)
             ORDER BY e.id DESC
             LIMIT $3",
        )
        .bind(query.account.as_uuid())
        .bind(query.before)
        .bind(i64::from(query.limit))
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?
        .into_iter()
        .map(EntryRecord::try_from)
        .collect()
    }

    async fn ping(&self) -> Result<(), RepoError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(())
    }
}
