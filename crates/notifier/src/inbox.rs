use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Mutex, MutexGuard},
};

use ledger_events::{Envelope, Party, TransferPosted};
use ledger_telemetry::propagation::{self, Carrier};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::proto::{Direction, Notification};

const PER_USER: usize = 200;
#[cfg(test)]
const PER_USER_I64: i64 = 200;

#[derive(Debug, Clone)]
pub struct Delivered {
    pub user_id: Uuid,
    pub notification: Notification,
    pub trace: Carrier,
}

#[derive(Debug)]
pub struct Inbox {
    state: Mutex<State>,
    live: broadcast::Sender<Delivered>,
}

#[derive(Debug, Default)]
struct State {
    by_user: HashMap<Uuid, VecDeque<Notification>>,
    seen: HashSet<Uuid>,
}

impl Default for Inbox {
    fn default() -> Self {
        Self::new()
    }
}

pub fn format_money(minor: i64, currency: &str) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    let whole = (abs / 100).to_string();
    let mut grouped = String::new();
    for (i, ch) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!("{sign}{grouped}.{:02} {currency}", abs % 100)
}

fn timestamp(at: time::OffsetDateTime) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: at.unix_timestamp(),
        nanos: i32::try_from(at.nanosecond()).unwrap_or(0),
    }
}

impl Inbox {
    pub fn new() -> Self {
        let (live, _) = broadcast::channel(1024);
        Self {
            state: Mutex::default(),
            live,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Delivered> {
        self.live.subscribe()
    }

    pub fn list(&self, user_id: Uuid, limit: usize) -> Vec<Notification> {
        self.lock()
            .by_user
            .get(&user_id)
            .map(|items| items.iter().take(limit).cloned().collect())
            .unwrap_or_default()
    }

    pub fn apply(&self, event: &Envelope<TransferPosted>) -> usize {
        let delivered = {
            let mut state = self.lock();
            if !state.seen.insert(event.id) {
                return 0;
            }
            let notifications = Self::notifications_for(event);
            for d in &notifications {
                let items = state.by_user.entry(d.user_id).or_default();
                items.push_front(d.notification.clone());
                items.truncate(PER_USER);
            }
            notifications
        };
        let count = delivered.len();
        let trace = propagation::current();
        for mut d in delivered {
            d.trace.clone_from(&trace);
            let _ = self.live.send(d);
        }
        count
    }

    fn notifications_for(event: &Envelope<TransferPosted>) -> Vec<Delivered> {
        let data = &event.data;
        let amount = format_money(data.amount_minor, &data.currency);
        let deposit = data.kind == "deposit";
        let make = |party: &Party, other: &Party, direction: Direction| {
            let user_id = party.holder_id?;
            let text = match (direction, deposit) {
                (Direction::Credit, true) => format!("Your account was topped up with {amount}"),
                (Direction::Credit, false) => format!("You received {amount} from {}", other.owner),
                _ => format!("You sent {amount} to {}", other.owner),
            };
            let suffix = if direction == Direction::Credit {
                "credit"
            } else {
                "debit"
            };
            Some(Delivered {
                trace: Carrier::new(),
                user_id,
                notification: Notification {
                    id: format!("{}:{suffix}", event.id),
                    transfer_id: data.transfer_id.to_string(),
                    account_id: party.account_id.to_string(),
                    direction: direction.into(),
                    kind: data.kind.clone(),
                    amount_minor: data.amount_minor,
                    currency: data.currency.clone(),
                    counterparty: if deposit {
                        "cash desk".into()
                    } else {
                        other.owner.clone()
                    },
                    balance_after_minor: party.balance_after_minor,
                    text,
                    occurred_at: Some(timestamp(event.occurred_at)),
                },
            })
        };
        [
            make(&data.from, &data.to, Direction::Debit),
            make(&data.to, &data.from, Direction::Credit),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    fn party(owner: &str, holder: Option<u128>, balance: i64) -> Party {
        Party {
            account_id: Uuid::now_v7(),
            holder_id: holder.map(Uuid::from_u128),
            owner: owner.into(),
            balance_after_minor: balance,
        }
    }

    fn event(kind: &str, from: Party, to: Party, amount: i64) -> Envelope<TransferPosted> {
        Envelope::new(
            Uuid::now_v7(),
            datetime!(2026-10-12 12:00 UTC),
            TransferPosted {
                transfer_id: Uuid::now_v7(),
                kind: kind.into(),
                amount_minor: amount,
                currency: "TJS".into(),
                initiated_by: None,
                from,
                to,
            },
        )
    }

    #[test]
    fn money_is_formatted_for_people() {
        assert_eq!(format_money(25_000, "TJS"), "250.00 TJS");
        assert_eq!(format_money(123_456_789, "TJS"), "1,234,567.89 TJS");
        assert_eq!(format_money(5, "USD"), "0.05 USD");
        assert_eq!(format_money(-100_000, "TJS"), "-1,000.00 TJS");
    }

    #[test]
    fn transfer_notifies_both_sides() {
        let inbox = Inbox::new();
        let e = event(
            "transfer",
            party("Firuz", Some(1), 75_000),
            party("Nigina", Some(2), 25_000),
            25_000,
        );
        assert_eq!(inbox.apply(&e), 2);

        let firuz = inbox.list(Uuid::from_u128(1), 10);
        assert_eq!(firuz[0].text, "You sent 250.00 TJS to Nigina");
        assert_eq!(firuz[0].direction(), Direction::Debit);
        assert_eq!(firuz[0].balance_after_minor, 75_000);

        let nigina = inbox.list(Uuid::from_u128(2), 10);
        assert_eq!(nigina[0].text, "You received 250.00 TJS from Firuz");
        assert_eq!(nigina[0].direction(), Direction::Credit);
    }

    #[test]
    fn deposit_notifies_only_the_customer_and_duplicates_are_ignored() {
        let inbox = Inbox::new();
        let e = event(
            "deposit",
            party("system:TJS", None, -100_000),
            party("Firuz", Some(1), 100_000),
            100_000,
        );
        assert_eq!(inbox.apply(&e), 1);
        assert_eq!(inbox.apply(&e), 0, "redelivery is idempotent");
        let firuz = inbox.list(Uuid::from_u128(1), 10);
        assert_eq!(firuz.len(), 1);
        assert_eq!(
            firuz[0].text,
            "Your account was topped up with 1,000.00 TJS"
        );
        assert_eq!(firuz[0].counterparty, "cash desk");
    }

    #[test]
    fn newest_first_and_capped() {
        let inbox = Inbox::new();
        for amount in 1..=(PER_USER_I64 + 5) {
            inbox.apply(&event(
                "transfer",
                party("A", Some(1), 0),
                party("B", Some(2), amount),
                amount,
            ));
        }
        let items = inbox.list(Uuid::from_u128(2), 1000);
        assert_eq!(items.len(), PER_USER);
        assert_eq!(items[0].amount_minor, PER_USER_I64 + 5);
        assert_eq!(inbox.list(Uuid::from_u128(2), 3).len(), 3);
        assert_eq!(inbox.list(Uuid::from_u128(9), 10), []);
    }
}
