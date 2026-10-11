use std::sync::Arc;

use ledger_application::{
    AppError, Deposit, LedgerService, OpenAccount, TransferFunds, ports::LedgerRepository,
};
use ledger_domain::DomainError;
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
