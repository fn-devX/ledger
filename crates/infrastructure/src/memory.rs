use std::{
    collections::HashMap,
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

impl LedgerRepository for InMemoryLedger {
    async fn insert_account(&self, account: &Account) -> Result<(), RepoError> {
        self.lock()
            .accounts
            .insert(account.id(), persisted(account));
        Ok(())
    }

    async fn find_account(&self, id: AccountId) -> Result<Option<Account>, RepoError> {
        Ok(self.lock().accounts.get(&id).cloned())
    }

    async fn system_account(
        &self,
        currency: Currency,
        new_id: AccountId,
    ) -> Result<Account, RepoError> {
        let mut s = self.lock();
        let id = *s.system.entry(currency).or_insert(new_id);
        Ok(s.accounts
            .entry(id)
            .or_insert_with(|| Account::system(id, currency))
            .clone())
    }

    async fn find_transfer(&self, id: TransferId) -> Result<Option<Transfer>, RepoError> {
        Ok(self.lock().transfers.get(&id).cloned())
    }

    async fn find_transfer_by_key(
        &self,
        key: &IdempotencyKey,
    ) -> Result<Option<Transfer>, RepoError> {
        let s = self.lock();
        Ok(s.keys
            .get(key.as_str())
            .and_then(|id| s.transfers.get(id))
            .cloned())
    }

    async fn commit_transfer(
        &self,
        transfer: &Transfer,
        from: &Account,
        to: &Account,
    ) -> Result<(), RepoError> {
        let mut s = self.lock();
        for acc in [from, to] {
            let stored = s.accounts.get(&acc.id()).map(Account::version);
            if stored != Some(acc.loaded_version()) {
                return Err(RepoError::Conflict);
            }
        }
        if s.keys.contains_key(transfer.idempotency_key().as_str()) {
            return Err(RepoError::DuplicateIdempotencyKey);
        }
        s.accounts.insert(from.id(), persisted(from));
        s.accounts.insert(to.id(), persisted(to));
        s.keys.insert(
            transfer.idempotency_key().as_str().to_owned(),
            transfer.id(),
        );
        s.transfers.insert(transfer.id(), transfer.clone());
        s.entries.extend(transfer.entries());
        Ok(())
    }

    async fn ping(&self) -> Result<(), RepoError> {
        Ok(())
    }
}
