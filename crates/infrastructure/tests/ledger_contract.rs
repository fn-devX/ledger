use std::{
    future::{Future, ready},
    sync::{Arc, Mutex, OnceLock},
};

use ledger_application::{
    Actor, AppError, AuthService, Deposit, LedgerService, ListEntries, OpenAccount, Register,
    TransferFunds, TransferOutcome,
    ports::{
        BoxError, BoxFuture, EntryRecord, EventPublisher, LedgerRepository, OutboxMessage,
        OutboxStore, PasswordHasher, UserRepository,
    },
};
use ledger_domain::{DomainError, Password, Role, TransferKind};
use ledger_events::{RawEnvelope, TransferPosted};
use ledger_infrastructure::{InMemoryLedger, PgLedgerRepository, UuidV7Generator};
use uuid::Uuid;

#[derive(Debug, Clone)]
struct PlainHasher;

impl PasswordHasher for PlainHasher {
    fn hash(&self, password: &Password) -> impl Future<Output = Result<String, BoxError>> + Send {
        ready(Ok(format!("plain:{}", password.expose())))
    }

    fn verify(
        &self,
        password: &str,
        hash: &str,
    ) -> impl Future<Output = Result<bool, BoxError>> + Send {
        ready(Ok(hash == format!("plain:{password}")))
    }

    fn decoy_hash(&self) -> &'static str {
        "plain:\u{0}decoy"
    }
}

trait Repo: LedgerRepository + UserRepository + OutboxStore + Clone {}
impl<T: LedgerRepository + UserRepository + OutboxStore + Clone> Repo for T {}

#[derive(Default)]
struct Collector(Mutex<Vec<OutboxMessage>>);

fn collector() -> &'static Collector {
    static SHARED: OnceLock<Collector> = OnceLock::new();
    SHARED.get_or_init(Collector::default)
}

impl EventPublisher for Collector {
    fn publish<'a>(&'a self, message: &'a OutboxMessage) -> BoxFuture<'a, Result<(), BoxError>> {
        self.0.lock().unwrap().push(message.clone());
        Box::pin(ready(Ok(())))
    }
}

struct Broken;

impl EventPublisher for Broken {
    fn publish<'a>(&'a self, _: &'a OutboxMessage) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(ready(Err("broker is down".into())))
    }
}

struct World<R> {
    repo: R,
    ledger: Arc<LedgerService<R, UuidV7Generator>>,
    auth: AuthService<R, PlainHasher, UuidV7Generator>,
    admin: Actor,
}

fn key() -> String {
    Uuid::now_v7().to_string()
}

fn email() -> String {
    format!("user-{}@ledger.test", Uuid::now_v7())
}

fn currency() -> String {
    let n = Uuid::now_v7().as_u128();
    (0..3)
        .map(|i| char::from(b'A' + ((n >> (i * 5)) % 26) as u8))
        .collect()
}

impl<R: Repo> World<R> {
    async fn new(repo: R) -> Self {
        let ledger =
            Arc::new(LedgerService::new(repo.clone(), UuidV7Generator).with_max_attempts(200));
        let auth = AuthService::new(repo.clone(), PlainHasher, UuidV7Generator);
        let admin = auth
            .ensure_admin(&email(), "Cashier", "admin-password")
            .await
            .unwrap();
        Self {
            repo,
            ledger,
            auth,
            admin: Actor::from(&admin),
        }
    }

    async fn events_for(&self, currency: &str, expected: usize) -> Vec<TransferPosted> {
        let mut mine = Vec::new();
        for _ in 0..50 {
            while self.repo.relay_batch(collector(), 500).await.unwrap() > 0 {}
            mine = collector()
                .0
                .lock()
                .unwrap()
                .iter()
                .map(|m| RawEnvelope::parse(&m.payload).unwrap())
                .filter(RawEnvelope::is::<TransferPosted>)
                .map(|raw| raw.decode::<TransferPosted>().unwrap().data)
                .filter(|event| event.currency == currency)
                .collect();
            if mine.len() >= expected {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        mine
    }

    async fn customer(&self, name: &str) -> Actor {
        let user = self
            .auth
            .register(Register {
                email: email(),
                name: name.into(),
                password: "password123".into(),
            })
            .await
            .unwrap();
        Actor::from(&user)
    }

    async fn open(&self, who: &Actor, cur: &str) -> Uuid {
        let cmd = OpenAccount {
            currency: cur.into(),
        };
        self.ledger
            .open_account(who, cmd)
            .await
            .unwrap()
            .id()
            .as_uuid()
    }

    async fn deposit(&self, account: Uuid, amount: i64, cur: &str) {
        let cmd = Deposit {
            idempotency_key: key(),
            account,
            amount_minor: amount,
            currency: cur.into(),
        };
        self.ledger.deposit(&self.admin, cmd).await.unwrap();
    }

    async fn transfer(
        &self,
        who: &Actor,
        from: Uuid,
        to: Uuid,
        amount: i64,
        cur: &str,
        key: String,
    ) -> Result<TransferOutcome, AppError> {
        let cmd = TransferFunds {
            idempotency_key: key,
            from,
            to,
            amount_minor: amount,
            currency: cur.into(),
        };
        self.ledger.transfer(who, cmd).await
    }

    async fn balance(&self, id: Uuid) -> i64 {
        self.ledger
            .get_account(&self.admin, id)
            .await
            .unwrap()
            .balance()
    }

    async fn all_entries(&self, who: &Actor, account: Uuid, limit: u32) -> Vec<EntryRecord> {
        let mut all = Vec::new();
        let mut before = None;
        loop {
            let page = self
                .ledger
                .list_entries(
                    who,
                    ListEntries {
                        account,
                        before,
                        limit: Some(limit),
                    },
                )
                .await
                .unwrap();
            assert!(page.entries.len() <= limit as usize);
            all.extend(page.entries);
            match page.next_before {
                Some(cursor) => before = Some(cursor),
                None => return all,
            }
        }
    }
}

fn assert_running_balance(newest_first: &[EntryRecord]) {
    let mut balance = 0;
    for e in newest_first.iter().rev() {
        balance += e.delta;
        assert_eq!(
            e.balance_after, balance,
            "entry {} has a broken running balance",
            e.id
        );
    }
    assert!(
        newest_first.windows(2).all(|w| w[0].id > w[1].id),
        "entries must be newest first"
    );
}

async fn deposit_and_transfer_move_money<R: Repo>(w: World<R>) {
    let cur = currency();
    let (firuz, nigina) = (w.customer("Firuz").await, w.customer("Nigina").await);
    let (a, b) = (w.open(&firuz, &cur).await, w.open(&nigina, &cur).await);
    w.deposit(a, 10_000, &cur).await;

    let out = w.transfer(&firuz, a, b, 2_500, &cur, key()).await.unwrap();
    assert!(!out.replayed);
    assert_eq!(out.transfer.initiator(), Some(firuz.user_id));
    assert_eq!(w.balance(a).await, 7_500);
    assert_eq!(w.balance(b).await, 2_500);

    let id = out.transfer.id().as_uuid();
    assert_eq!(
        w.ledger.get_transfer(&firuz, id).await.unwrap(),
        out.transfer
    );
    assert_eq!(
        w.ledger.get_transfer(&nigina, id).await.unwrap(),
        out.transfer
    );
    assert_eq!(
        w.ledger.get_transfer(&w.admin, id).await.unwrap(),
        out.transfer
    );
}

async fn idempotency<R: Repo>(w: World<R>) {
    let cur = currency();
    let (firuz, nigina) = (w.customer("Firuz").await, w.customer("Nigina").await);
    let (a, b) = (w.open(&firuz, &cur).await, w.open(&nigina, &cur).await);
    w.deposit(a, 1_000, &cur).await;
    w.deposit(b, 1_000, &cur).await;

    let k = key();
    let first = w
        .transfer(&firuz, a, b, 100, &cur, k.clone())
        .await
        .unwrap();
    let again = w
        .transfer(&firuz, a, b, 100, &cur, k.clone())
        .await
        .unwrap();
    assert!(again.replayed);
    assert_eq!(first.transfer.id(), again.transfer.id());
    assert_eq!(w.balance(a).await, 900, "money must be moved exactly once");

    let reused = w.transfer(&firuz, a, b, 999, &cur, k.clone()).await;
    assert!(matches!(reused, Err(AppError::IdempotencyKeyReused)));

    let other_user = w.transfer(&nigina, b, a, 50, &cur, k).await.unwrap();
    assert!(!other_user.replayed, "keys are scoped per user");
    assert_eq!(w.balance(a).await, 950);
}

async fn business_rules<R: Repo>(w: World<R>) {
    let cur = currency();
    let firuz = w.customer("Firuz").await;
    let (a, b) = (w.open(&firuz, &cur).await, w.open(&firuz, &cur).await);
    let r = w.transfer(&firuz, a, b, 1, &cur, key()).await;
    assert!(matches!(
        r,
        Err(AppError::Domain(DomainError::InsufficientFunds { .. }))
    ));

    let r = w.transfer(&firuz, a, Uuid::now_v7(), 1, &cur, key()).await;
    assert!(matches!(r, Err(AppError::AccountNotFound(_))));
}

async fn authorization<R: Repo>(w: World<R>) {
    let cur = currency();
    let (firuz, nigina, stranger) = (
        w.customer("Firuz").await,
        w.customer("Nigina").await,
        w.customer("Stranger").await,
    );
    let (a, b) = (w.open(&firuz, &cur).await, w.open(&nigina, &cur).await);
    w.deposit(a, 5_000, &cur).await;

    let r = w.ledger.get_account(&nigina, a).await;
    assert!(
        matches!(r, Err(AppError::AccountNotFound(_))),
        "foreign accounts look nonexistent"
    );
    let r = w
        .ledger
        .list_entries(
            &nigina,
            ListEntries {
                account: a,
                before: None,
                limit: None,
            },
        )
        .await;
    assert!(matches!(r, Err(AppError::AccountNotFound(_))));

    let r = w.transfer(&nigina, a, b, 1_000, &cur, key()).await;
    assert!(
        matches!(r, Err(AppError::AccountNotFound(_))),
        "cannot spend someone else's money"
    );
    assert_eq!(w.balance(a).await, 5_000);

    let cmd = Deposit {
        idempotency_key: key(),
        account: b,
        amount_minor: 1_000_000,
        currency: cur.clone(),
    };
    assert!(matches!(
        w.ledger.deposit(&nigina, cmd).await,
        Err(AppError::Forbidden(_))
    ));
    assert_eq!(w.balance(b).await, 0, "customers cannot print money");

    let posted = w.transfer(&firuz, a, b, 700, &cur, key()).await.unwrap();
    let id = posted.transfer.id().as_uuid();
    assert!(w.ledger.get_transfer(&nigina, id).await.is_ok());
    assert!(matches!(
        w.ledger.get_transfer(&stranger, id).await,
        Err(AppError::TransferNotFound(_))
    ));

    assert!(
        w.ledger.get_account(&w.admin, a).await.is_ok(),
        "admins see every account"
    );
    let mine: Vec<_> = w
        .ledger
        .list_accounts(&firuz)
        .await
        .unwrap()
        .iter()
        .map(|acc| acc.id().as_uuid())
        .collect();
    assert_eq!(mine, [a], "list shows only the caller's accounts");
    assert_eq!(w.ledger.list_accounts(&stranger).await.unwrap(), []);
    assert_eq!(w.all_entries(&w.admin, b, 10).await.len(), 1);

    let entries = w.all_entries(&firuz, a, 10).await;
    let system = entries.last().unwrap().counterparty.as_uuid();
    let r = w.transfer(&firuz, a, system, 1, &cur, key()).await;
    assert!(
        matches!(r, Err(AppError::AccountNotFound(_))),
        "system accounts are not transfer targets"
    );
}

async fn authentication<R: Repo>(w: World<R>) {
    let email = email();
    let user = w
        .auth
        .register(Register {
            email: email.to_uppercase(),
            name: "  Malika ".into(),
            password: "password123".into(),
        })
        .await
        .unwrap();
    assert_eq!(user.email().as_str(), email);
    assert_eq!(user.name(), "Malika");
    assert_eq!(user.role(), Role::Customer);

    let logged_in = w.auth.login(&email, "password123").await.unwrap();
    assert_eq!(logged_in.id(), user.id());

    for (login, password) in [
        (email.as_str(), "wrong-password"),
        ("nobody@ledger.test", "password123"),
        ("not-an-email", "x"),
    ] {
        assert!(matches!(
            w.auth.login(login, password).await,
            Err(AppError::InvalidCredentials)
        ));
    }

    let dup = w
        .auth
        .register(Register {
            email: email.to_uppercase(),
            name: "Again".into(),
            password: "password123".into(),
        })
        .await;
    assert!(matches!(dup, Err(AppError::EmailTaken)));

    let weak = w
        .auth
        .register(Register {
            email: self::email(),
            name: "Weak".into(),
            password: "short".into(),
        })
        .await;
    assert!(matches!(
        weak,
        Err(AppError::Domain(DomainError::WeakPassword { .. }))
    ));

    let bad = w
        .auth
        .register(Register {
            email: "nope".into(),
            name: "Bad".into(),
            password: "password123".into(),
        })
        .await;
    assert!(matches!(
        bad,
        Err(AppError::Domain(DomainError::InvalidEmail))
    ));

    let admin_email = self::email();
    let first = w
        .auth
        .ensure_admin(&admin_email, "Boss", "admin-password")
        .await
        .unwrap();
    let second = w
        .auth
        .ensure_admin(&admin_email, "Boss", "admin-password")
        .await
        .unwrap();
    assert_eq!(first.id(), second.id());
    assert_eq!(first.role(), Role::Admin);
}

async fn concurrent_transfers_never_overdraw<R: Repo>(w: World<R>) {
    let cur = currency();
    let (firuz, nigina) = (w.customer("Firuz").await, w.customer("Nigina").await);
    let (a, b) = (w.open(&firuz, &cur).await, w.open(&nigina, &cur).await);
    w.deposit(a, 1_000, &cur).await;

    let w = Arc::new(w);
    let tasks: Vec<_> = (0..50)
        .map(|_| {
            let (w, firuz, cur) = (w.clone(), firuz.clone(), cur.clone());
            tokio::spawn(async move { w.transfer(&firuz, a, b, 100, &cur, key()).await })
        })
        .collect();

    let mut ok = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(_) => ok += 1,
            Err(AppError::Domain(DomainError::InsufficientFunds { .. })) => {}
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert_eq!(ok, 10);
    assert_eq!(w.balance(a).await, 0);
    assert_eq!(w.balance(b).await, 1_000);

    for (who, account, expected) in [(&firuz, a, 11), (&nigina, b, 10)] {
        let history = w.all_entries(who, account, 7).await;
        assert_eq!(history.len(), expected);
        assert_running_balance(&history);
    }
}

async fn entries_are_paginated_newest_first<R: Repo>(w: World<R>) {
    let cur = currency();
    let (firuz, nigina) = (w.customer("Firuz").await, w.customer("Nigina").await);
    let (a, b) = (w.open(&firuz, &cur).await, w.open(&nigina, &cur).await);
    w.deposit(a, 10_000, &cur).await;
    for amount in [100, 200, 300] {
        w.transfer(&firuz, a, b, amount, &cur, key()).await.unwrap();
    }
    w.transfer(&nigina, b, a, 50, &cur, key()).await.unwrap();

    let page = |before| ListEntries {
        account: a,
        before,
        limit: Some(2),
    };
    let first = w.ledger.list_entries(&firuz, page(None)).await.unwrap();
    let deltas: Vec<_> = first
        .entries
        .iter()
        .map(|e| (e.delta, e.balance_after))
        .collect();
    assert_eq!(deltas, [(50, 9_450), (-300, 9_400)]);
    assert!(first.entries.iter().all(|e| e.counterparty.as_uuid() == b));
    assert_eq!(first.entries[0].balance_after, w.balance(a).await);

    let second = w
        .ledger
        .list_entries(&firuz, page(first.next_before))
        .await
        .unwrap();
    let deltas: Vec<_> = second.entries.iter().map(|e| e.delta).collect();
    assert_eq!(deltas, [-200, -100]);

    let last = w
        .ledger
        .list_entries(&firuz, page(second.next_before))
        .await
        .unwrap();
    assert_eq!(last.entries.len(), 1);
    assert_eq!(last.entries[0].kind, TransferKind::Deposit);
    assert_eq!(last.entries[0].balance_after, 10_000);
    assert_eq!(last.next_before, None);

    assert_running_balance(&w.all_entries(&firuz, a, 100).await);
    assert_eq!(w.all_entries(&nigina, b, 1).await.len(), 4);
}

async fn entries_validation<R: Repo>(w: World<R>) {
    let cur = currency();
    let firuz = w.customer("Firuz").await;
    let a = w.open(&firuz, &cur).await;
    let q = |limit| ListEntries {
        account: a,
        before: None,
        limit,
    };
    let empty = w.ledger.list_entries(&firuz, q(None)).await.unwrap();
    assert_eq!(empty.entries, []);
    assert_eq!(empty.next_before, None);

    for limit in [0, 101] {
        let r = w.ledger.list_entries(&firuz, q(Some(limit))).await;
        assert!(matches!(r, Err(AppError::InvalidPageSize { max: 100 })));
    }
    let r = w
        .ledger
        .list_entries(
            &firuz,
            ListEntries {
                account: Uuid::now_v7(),
                before: None,
                limit: None,
            },
        )
        .await;
    assert!(matches!(r, Err(AppError::AccountNotFound(_))));
}

async fn outbox_records_exactly_the_committed_transfers<R: Repo>(w: World<R>) {
    let cur = currency();
    let (firuz, nigina) = (w.customer("Firuz").await, w.customer("Nigina").await);
    let (a, b) = (w.open(&firuz, &cur).await, w.open(&nigina, &cur).await);
    w.deposit(a, 1_000, &cur).await;
    let k = key();
    let posted = w
        .transfer(&firuz, a, b, 300, &cur, k.clone())
        .await
        .unwrap();
    assert!(
        w.transfer(&firuz, a, b, 300, &cur, k)
            .await
            .unwrap()
            .replayed
    );
    assert!(
        w.transfer(&firuz, a, b, 999_999, &cur, key())
            .await
            .is_err()
    );

    let events = w.events_for(&cur, 2).await;
    assert_eq!(
        events.len(),
        2,
        "one deposit and one transfer, no replay, no failure"
    );
    let transfer = events
        .iter()
        .find(|e| e.transfer_id == posted.transfer.id().as_uuid())
        .expect("transfer event");
    assert_eq!(transfer.kind, "transfer");
    assert_eq!(transfer.amount_minor, 300);
    assert_eq!(transfer.initiated_by, Some(firuz.user_id.as_uuid()));
    assert_eq!(
        (
            transfer.from.owner.as_str(),
            transfer.from.balance_after_minor
        ),
        ("Firuz", 700)
    );
    assert_eq!(
        (transfer.to.owner.as_str(), transfer.to.balance_after_minor),
        ("Nigina", 300)
    );
    assert_eq!(transfer.to.holder_id, Some(nigina.user_id.as_uuid()));

    assert_eq!(
        w.events_for(&cur, 0).await,
        events,
        "published events are not sent again"
    );
}

async fn outbox_survives_a_broker_outage<R: Repo>(w: World<R>) {
    let cur = currency();
    let firuz = w.customer("Firuz").await;
    let a = w.open(&firuz, &cur).await;
    w.deposit(a, 1_000, &cur).await;

    assert!(w.repo.relay_batch(&Broken, 100).await.is_err());
    assert_eq!(
        w.events_for(&cur, 1).await.len(),
        1,
        "the event waited for the broker"
    );
}

async fn outbox_matches_concurrent_commits<R: Repo>(w: World<R>) {
    let cur = currency();
    let (firuz, nigina) = (w.customer("Firuz").await, w.customer("Nigina").await);
    let (a, b) = (w.open(&firuz, &cur).await, w.open(&nigina, &cur).await);
    w.deposit(a, 1_000, &cur).await;
    let w = Arc::new(w);
    let tasks: Vec<_> = (0..30)
        .map(|_| {
            let (w, firuz, cur) = (w.clone(), firuz.clone(), cur.clone());
            tokio::spawn(async move { w.transfer(&firuz, a, b, 100, &cur, key()).await.is_ok() })
        })
        .collect();
    let mut ok = 0;
    for t in tasks {
        ok += usize::from(t.await.unwrap());
    }
    assert_eq!(ok, 10);
    let events = w.events_for(&cur, 11).await;
    assert_eq!(
        events.len(),
        11,
        "retried and rejected attempts leave no events"
    );
    let mut last_balances: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "transfer")
        .map(|e| e.from.balance_after_minor)
        .collect();
    last_balances.sort_unstable();
    assert_eq!(last_balances, (0..10).map(|i| i * 100).collect::<Vec<_>>());
}

macro_rules! contract {
    ($($name:ident),* $(,)?) => {
        mod memory {
            use super::*;
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::$name(World::new(InMemoryLedger::new()).await).await;
                }
            )*
        }

        mod postgres {
            use super::*;
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    let Ok(url) = std::env::var("DATABASE_URL") else {
                        eprintln!("DATABASE_URL not set, skipping");
                        return;
                    };
                    let repo = PgLedgerRepository::connect(&url, 20).await.unwrap();
                    repo.migrate().await.unwrap();
                    super::$name(World::new(repo).await).await;
                }
            )*
        }
    };
}

contract!(
    deposit_and_transfer_move_money,
    idempotency,
    business_rules,
    authorization,
    authentication,
    concurrent_transfers_never_overdraw,
    entries_are_paginated_newest_first,
    entries_validation,
    outbox_records_exactly_the_committed_transfers,
    outbox_survives_a_broker_outage,
    outbox_matches_concurrent_commits,
);

#[tokio::test]
async fn double_entry_invariant_holds_in_memory() {
    let repo = InMemoryLedger::new();
    let w = World::new(repo.clone()).await;
    let cur = currency();
    let (firuz, nigina) = (w.customer("Firuz").await, w.customer("Nigina").await);
    let (a, b) = (w.open(&firuz, &cur).await, w.open(&nigina, &cur).await);
    w.deposit(a, 5_000, &cur).await;
    w.deposit(b, 700, &cur).await;
    w.transfer(&firuz, a, b, 1_234, &cur, key()).await.unwrap();

    assert_eq!(repo.entries().iter().map(|e| e.delta).sum::<i64>(), 0);
    assert_eq!(
        repo.accounts()
            .iter()
            .map(ledger_domain::Account::balance)
            .sum::<i64>(),
        0
    );
}
