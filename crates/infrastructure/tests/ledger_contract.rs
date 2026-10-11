use std::sync::Arc;

use ledger_application::{
    AppError, Deposit, LedgerService, ListEntries, OpenAccount, TransferFunds,
    ports::{EntryRecord, LedgerRepository},
};
use ledger_domain::DomainError;
use ledger_domain::TransferKind;
use ledger_infrastructure::{InMemoryLedger, PgLedgerRepository, UuidV7Generator};
use uuid::Uuid;

type Svc<R> = Arc<LedgerService<R, UuidV7Generator>>;

fn key() -> String {
    Uuid::now_v7().to_string()
}

fn currency() -> String {
    let n = Uuid::now_v7().as_u128();
    (0..3)
        .map(|i| char::from(b'A' + ((n >> (i * 5)) % 26) as u8))
        .collect()
}

async fn open<R: LedgerRepository>(svc: &Svc<R>, cur: &str) -> Uuid {
    let cmd = OpenAccount {
        owner: "test".into(),
        currency: cur.into(),
    };
    svc.open_account(cmd).await.unwrap().id().as_uuid()
}

async fn deposit<R: LedgerRepository>(svc: &Svc<R>, acc: Uuid, amount: i64, cur: &str) {
    let cmd = Deposit {
        idempotency_key: key(),
        account: acc,
        amount_minor: amount,
        currency: cur.into(),
    };
    svc.deposit(cmd).await.unwrap();
}

fn transfer_cmd(from: Uuid, to: Uuid, amount: i64, cur: &str, key: String) -> TransferFunds {
    TransferFunds {
        idempotency_key: key,
        from,
        to,
        amount_minor: amount,
        currency: cur.into(),
    }
}

async fn balance<R: LedgerRepository>(svc: &Svc<R>, id: Uuid) -> i64 {
    svc.get_account(id).await.unwrap().balance()
}

async fn deposit_and_transfer_move_money<R: LedgerRepository>(svc: Svc<R>) {
    let cur = currency();
    let (a, b) = (open(&svc, &cur).await, open(&svc, &cur).await);
    deposit(&svc, a, 10_000, &cur).await;

    let out = svc
        .transfer(transfer_cmd(a, b, 2_500, &cur, key()))
        .await
        .unwrap();
    assert!(!out.replayed);
    assert_eq!(balance(&svc, a).await, 7_500);
    assert_eq!(balance(&svc, b).await, 2_500);
    assert_eq!(
        svc.get_transfer(out.transfer.id().as_uuid()).await.unwrap(),
        out.transfer
    );
}

async fn idempotency<R: LedgerRepository>(svc: Svc<R>) {
    let cur = currency();
    let (a, b) = (open(&svc, &cur).await, open(&svc, &cur).await);
    deposit(&svc, a, 1_000, &cur).await;

    let k = key();
    let first = svc
        .transfer(transfer_cmd(a, b, 100, &cur, k.clone()))
        .await
        .unwrap();
    let again = svc
        .transfer(transfer_cmd(a, b, 100, &cur, k.clone()))
        .await
        .unwrap();
    assert!(again.replayed);
    assert_eq!(first.transfer.id(), again.transfer.id());
    assert_eq!(
        balance(&svc, a).await,
        900,
        "money must be moved exactly once"
    );

    let reused = svc.transfer(transfer_cmd(a, b, 999, &cur, k)).await;
    assert!(matches!(reused, Err(AppError::IdempotencyKeyReused)));
}

async fn business_rules<R: LedgerRepository>(svc: Svc<R>) {
    let cur = currency();
    let (a, b) = (open(&svc, &cur).await, open(&svc, &cur).await);
    let r = svc.transfer(transfer_cmd(a, b, 1, &cur, key())).await;
    assert!(matches!(
        r,
        Err(AppError::Domain(DomainError::InsufficientFunds { .. }))
    ));

    let r = svc
        .transfer(transfer_cmd(a, Uuid::now_v7(), 1, &cur, key()))
        .await;
    assert!(matches!(r, Err(AppError::AccountNotFound(_))));
}

async fn concurrent_transfers_never_overdraw<R: LedgerRepository>(svc: Svc<R>) {
    let cur = currency();
    let (a, b) = (open(&svc, &cur).await, open(&svc, &cur).await);
    deposit(&svc, a, 1_000, &cur).await;

    let tasks: Vec<_> = (0..50)
        .map(|_| {
            let svc = svc.clone();
            let cmd = transfer_cmd(a, b, 100, &cur, key());
            tokio::spawn(async move { svc.transfer(cmd).await })
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
    assert_eq!(balance(&svc, a).await, 0);
    assert_eq!(balance(&svc, b).await, 1_000);

    for (account, expected) in [(a, 11), (b, 10)] {
        let history = all_entries(&svc, account, 7).await;
        assert_eq!(history.len(), expected);
        assert_running_balance(&history);
    }
}

async fn all_entries<R: LedgerRepository>(
    svc: &Svc<R>,
    account: Uuid,
    limit: u32,
) -> Vec<EntryRecord> {
    let mut all = Vec::new();
    let mut before = None;
    loop {
        let page = svc
            .list_entries(ListEntries {
                account,
                before,
                limit: Some(limit),
            })
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

async fn entries_are_paginated_newest_first<R: LedgerRepository>(svc: Svc<R>) {
    let cur = currency();
    let (a, b) = (open(&svc, &cur).await, open(&svc, &cur).await);
    deposit(&svc, a, 10_000, &cur).await;
    for amount in [100, 200, 300] {
        svc.transfer(transfer_cmd(a, b, amount, &cur, key()))
            .await
            .unwrap();
    }
    svc.transfer(transfer_cmd(b, a, 50, &cur, key()))
        .await
        .unwrap();

    let first = svc
        .list_entries(ListEntries {
            account: a,
            before: None,
            limit: Some(2),
        })
        .await
        .unwrap();
    let deltas: Vec<_> = first
        .entries
        .iter()
        .map(|e| (e.delta, e.balance_after))
        .collect();
    assert_eq!(deltas, [(50, 9_450), (-300, 9_400)]);
    assert!(first.entries.iter().all(|e| e.counterparty.as_uuid() == b));
    assert_eq!(first.entries[0].balance_after, balance(&svc, a).await);

    let second = svc
        .list_entries(ListEntries {
            account: a,
            before: first.next_before,
            limit: Some(2),
        })
        .await
        .unwrap();
    let deltas: Vec<_> = second.entries.iter().map(|e| e.delta).collect();
    assert_eq!(deltas, [-200, -100]);

    let last = svc
        .list_entries(ListEntries {
            account: a,
            before: second.next_before,
            limit: Some(2),
        })
        .await
        .unwrap();
    assert_eq!(last.entries.len(), 1);
    assert_eq!(last.entries[0].kind, TransferKind::Deposit);
    assert_eq!(last.entries[0].balance_after, 10_000);
    assert_eq!(last.next_before, None);

    assert_running_balance(&all_entries(&svc, a, 100).await);
    assert_eq!(all_entries(&svc, b, 1).await.len(), 4);
}

async fn entries_validation<R: LedgerRepository>(svc: Svc<R>) {
    let cur = currency();
    let a = open(&svc, &cur).await;
    let empty = svc
        .list_entries(ListEntries {
            account: a,
            before: None,
            limit: None,
        })
        .await
        .unwrap();
    assert!(empty.entries.is_empty() && empty.next_before.is_none());

    for limit in [0, 101] {
        let r = svc
            .list_entries(ListEntries {
                account: a,
                before: None,
                limit: Some(limit),
            })
            .await;
        assert!(matches!(r, Err(AppError::InvalidPageSize { max: 100 })));
    }
    let r = svc
        .list_entries(ListEntries {
            account: Uuid::now_v7(),
            before: None,
            limit: None,
        })
        .await;
    assert!(matches!(r, Err(AppError::AccountNotFound(_))));
}

macro_rules! contract {
    ($($name:ident),* $(,)?) => {
        mod memory {
            use super::*;
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    let svc = LedgerService::new(InMemoryLedger::new(), UuidV7Generator)
                        .with_max_attempts(200);
                    super::$name(Arc::new(svc)).await;
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
                    let svc = LedgerService::new(repo, UuidV7Generator).with_max_attempts(200);
                    super::$name(Arc::new(svc)).await;
                }
            )*
        }
    };
}

contract!(
    deposit_and_transfer_move_money,
    idempotency,
    business_rules,
    concurrent_transfers_never_overdraw,
    entries_are_paginated_newest_first,
    entries_validation,
);

#[tokio::test]
async fn double_entry_invariant_holds_in_memory() {
    let repo = InMemoryLedger::new();
    let svc = Arc::new(LedgerService::new(repo.clone(), UuidV7Generator));
    let cur = currency();
    let (a, b) = (open(&svc, &cur).await, open(&svc, &cur).await);
    deposit(&svc, a, 5_000, &cur).await;
    deposit(&svc, b, 700, &cur).await;
    svc.transfer(transfer_cmd(a, b, 1_234, &cur, key()))
        .await
        .unwrap();

    assert_eq!(repo.entries().iter().map(|e| e.delta).sum::<i64>(), 0);
    assert_eq!(
        repo.accounts()
            .iter()
            .map(ledger_domain::Account::balance)
            .sum::<i64>(),
        0
    );
}
