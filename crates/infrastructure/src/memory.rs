use std::{
    collections::HashMap,
    future::{Future, ready},
    sync::{Arc, Mutex, MutexGuard},
};

use ledger_application::ports::{
    EntryQuery, EntryRecord, LedgerRepository, RepoError, UserRepository,
};
use ledger_domain::{
    Account, AccountId, Currency, Email, Entry, IdempotencyKey, Transfer, TransferId, User, UserId,
};
use time::OffsetDateTime;

#[derive(Debug, Clone, Default)]
pub struct InMemoryLedger {
    state: Arc<Mutex<State>>,
}

#[derive(Debug, Default)]
struct State {
    accounts: HashMap<AccountId, Account>,
    system: HashMap<Currency, AccountId>,
    transfers: HashMap<TransferId, Transfer>,
    keys: HashMap<(Option<UserId>, String), TransferId>,
    users: HashMap<String, User>,
    entries: Vec<(AccountId, EntryRecord)>,
}

impl InMemoryLedger {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.lock()
            .entries
            .iter()
            .map(|(account, e)| Entry {
                account: *account,
                delta: e.delta,
            })
            .collect()
    }

    pub fn accounts(&self) -> Vec<Account> {
        self.lock().accounts.values().cloned().collect()
    }
}

fn persisted(a: &Account) -> Account {
    Account::restore(
        a.id(),
        a.kind(),
        a.holder(),
        a.owner().to_owned(),
        a.currency(),
        a.balance(),
        a.version(),
    )
}

impl State {
    fn system_account(&mut self, currency: Currency, new_id: AccountId) -> Account {
        let id = *self.system.entry(currency).or_insert(new_id);
        self.accounts
            .entry(id)
            .or_insert_with(|| Account::system(id, currency))
            .clone()
    }

    fn find_transfer_by_key(&self, initiator: UserId, key: &IdempotencyKey) -> Option<Transfer> {
        self.keys
            .get(&(Some(initiator), key.as_str().to_owned()))
            .and_then(|id| self.transfers.get(id))
            .cloned()
    }

    fn commit_transfer(
        &mut self,
        transfer: &Transfer,
        from: &Account,
        to: &Account,
    ) -> Result<(), RepoError> {
        for acc in [from, to] {
            let stored = self.accounts.get(&acc.id()).map(Account::version);
            if stored != Some(acc.loaded_version()) {
                return Err(RepoError::Conflict);
            }
        }
        let scoped_key = (
            transfer.initiator(),
            transfer.idempotency_key().as_str().to_owned(),
        );
        if self.keys.contains_key(&scoped_key) {
            return Err(RepoError::DuplicateIdempotencyKey);
        }
        self.accounts.insert(from.id(), persisted(from));
        self.accounts.insert(to.id(), persisted(to));
        self.keys.insert(scoped_key, transfer.id());
        self.transfers.insert(transfer.id(), transfer.clone());
        let created_at = OffsetDateTime::now_utc();
        for entry in transfer.entries() {
            let (balance_after, counterparty) = if entry.account == from.id() {
                (from.balance(), to.id())
            } else {
                (to.balance(), from.id())
            };
            let id = i64::try_from(self.entries.len()).unwrap_or(i64::MAX - 1) + 1;
            self.entries.push((
                entry.account,
                EntryRecord {
                    id,
                    transfer_id: transfer.id(),
                    kind: transfer.kind(),
                    counterparty,
                    delta: entry.delta,
                    balance_after,
                    created_at,
                },
            ));
        }
        Ok(())
    }

    fn list_entries(&self, query: EntryQuery) -> Vec<EntryRecord> {
        self.entries
            .iter()
            .rev()
            .filter(|(account, e)| {
                *account == query.account && query.before.is_none_or(|before| e.id < before)
            })
            .take(query.limit as usize)
            .map(|(_, e)| e.clone())
            .collect()
    }
}

impl LedgerRepository for InMemoryLedger {
    fn insert_account(
        &self,
        account: &Account,
    ) -> impl Future<Output = Result<(), RepoError>> + Send {
        self.lock()
            .accounts
            .insert(account.id(), persisted(account));
        ready(Ok(()))
    }

    fn find_account(
        &self,
        id: AccountId,
    ) -> impl Future<Output = Result<Option<Account>, RepoError>> + Send {
        ready(Ok(self.lock().accounts.get(&id).cloned()))
    }

    fn list_accounts(
        &self,
        holder: UserId,
    ) -> impl Future<Output = Result<Vec<Account>, RepoError>> + Send {
        let mut accounts: Vec<Account> = self
            .lock()
            .accounts
            .values()
            .filter(|a| a.is_held_by(holder))
            .cloned()
            .collect();
        accounts.sort_by_key(Account::id);
        ready(Ok(accounts))
    }

    fn system_account(
        &self,
        currency: Currency,
        new_id: AccountId,
    ) -> impl Future<Output = Result<Account, RepoError>> + Send {
        ready(Ok(self.lock().system_account(currency, new_id)))
    }

    fn find_transfer(
        &self,
        id: TransferId,
    ) -> impl Future<Output = Result<Option<Transfer>, RepoError>> + Send {
        ready(Ok(self.lock().transfers.get(&id).cloned()))
    }

    fn find_transfer_by_key(
        &self,
        initiator: UserId,
        key: &IdempotencyKey,
    ) -> impl Future<Output = Result<Option<Transfer>, RepoError>> + Send {
        ready(Ok(self.lock().find_transfer_by_key(initiator, key)))
    }

    fn commit_transfer(
        &self,
        transfer: &Transfer,
        from: &Account,
        to: &Account,
    ) -> impl Future<Output = Result<(), RepoError>> + Send {
        ready(self.lock().commit_transfer(transfer, from, to))
    }

    fn list_entries(
        &self,
        query: EntryQuery,
    ) -> impl Future<Output = Result<Vec<EntryRecord>, RepoError>> + Send {
        ready(Ok(self.lock().list_entries(query)))
    }

    fn ping(&self) -> impl Future<Output = Result<(), RepoError>> + Send {
        ready(Ok(()))
    }
}

impl UserRepository for InMemoryLedger {
    fn insert_user(&self, user: &User) -> impl Future<Output = Result<(), RepoError>> + Send {
        let mut state = self.lock();
        let result = if state.users.contains_key(user.email().as_str()) {
            Err(RepoError::DuplicateEmail)
        } else {
            state
                .users
                .insert(user.email().as_str().to_owned(), user.clone());
            Ok(())
        };
        ready(result)
    }

    fn find_user_by_email(
        &self,
        email: &Email,
    ) -> impl Future<Output = Result<Option<User>, RepoError>> + Send {
        ready(Ok(self.lock().users.get(email.as_str()).cloned()))
    }
}
