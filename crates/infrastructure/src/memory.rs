use std::{
    collections::HashMap,
    future::{Future, ready},
    sync::{Arc, Mutex, MutexGuard},
};

use ledger_application::ports::{LedgerRepository, RepoError};
use ledger_domain::{Account, AccountId, Currency, Entry, IdempotencyKey, Transfer, TransferId};

#[derive(Debug, Clone, Default)]
pub struct InMemoryLedger {
    state: Arc<Mutex<State>>,
}

#[derive(Debug, Default)]
struct State {
    accounts: HashMap<AccountId, Account>,
    system: HashMap<Currency, AccountId>,
    transfers: HashMap<TransferId, Transfer>,
    keys: HashMap<String, TransferId>,
    entries: Vec<Entry>,
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
        self.lock().entries.clone()
    }

    pub fn accounts(&self) -> Vec<Account> {
        self.lock().accounts.values().cloned().collect()
    }
}

fn persisted(a: &Account) -> Account {
    Account::restore(
        a.id(),
        a.kind(),
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

    fn find_transfer_by_key(&self, key: &IdempotencyKey) -> Option<Transfer> {
        self.keys
            .get(key.as_str())
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
        if self.keys.contains_key(transfer.idempotency_key().as_str()) {
            return Err(RepoError::DuplicateIdempotencyKey);
        }
        self.accounts.insert(from.id(), persisted(from));
        self.accounts.insert(to.id(), persisted(to));
        self.keys.insert(
            transfer.idempotency_key().as_str().to_owned(),
            transfer.id(),
        );
        self.transfers.insert(transfer.id(), transfer.clone());
        self.entries.extend(transfer.entries());
        Ok(())
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
        key: &IdempotencyKey,
    ) -> impl Future<Output = Result<Option<Transfer>, RepoError>> + Send {
        ready(Ok(self.lock().find_transfer_by_key(key)))
    }

    fn commit_transfer(
        &self,
        transfer: &Transfer,
        from: &Account,
        to: &Account,
    ) -> impl Future<Output = Result<(), RepoError>> + Send {
        ready(self.lock().commit_transfer(transfer, from, to))
    }

    fn ping(&self) -> impl Future<Output = Result<(), RepoError>> + Send {
        ready(Ok(()))
    }
}
