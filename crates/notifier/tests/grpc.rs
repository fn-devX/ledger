use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use ledger_auth::JwtKeys;
use ledger_domain::{Email, Role, User, UserId};
use ledger_events::{Envelope, Event, Party, TransferPosted};
use ledger_notifier::{
    grpc::{NotificationsService, serve_on},
    inbox::Inbox,
    projection,
    proto::{Direction, ListRequest, SubscribeRequest, notifications_client::NotificationsClient},
};
use time::OffsetDateTime;
use tonic::{Code, Request, transport::Channel};
use uuid::Uuid;

struct Harness {
    inbox: Arc<Inbox>,
    jwt: Arc<JwtKeys>,
    client: NotificationsClient<Channel>,
}

impl Harness {
    async fn start() -> Self {
        let inbox = Arc::new(Inbox::new());
        let jwt = Arc::new(JwtKeys::random(Duration::from_secs(60)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve_on(
            listener,
            NotificationsService::new(inbox.clone(), jwt.clone()),
        ));
        let client = NotificationsClient::connect(format!("http://{addr}"))
            .await
            .unwrap();
        Self { inbox, jwt, client }
    }

    fn token(&self, user: Uuid) -> String {
        let user = User::restore(
            UserId::from_uuid(user),
            Email::parse("user@ledger.test").unwrap(),
            "User".into(),
            Role::Customer,
            String::new(),
        );
        self.jwt.issue(&user).unwrap().token
    }

    fn request<T>(&self, message: T, user: Uuid) -> Request<T> {
        let mut request = Request::new(message);
        let bearer = format!("Bearer {}", self.token(user)).parse().unwrap();
        request.metadata_mut().insert("authorization", bearer);
        request
    }
}

fn party(owner: &str, holder: Option<Uuid>, balance: i64) -> Party {
    Party {
        account_id: Uuid::now_v7(),
        holder_id: holder,
        owner: owner.into(),
        balance_after_minor: balance,
    }
}

fn transfer(from: Party, to: Party, amount: i64) -> Envelope<TransferPosted> {
    Envelope::new(
        Uuid::now_v7(),
        OffsetDateTime::now_utc(),
        TransferPosted {
            transfer_id: Uuid::now_v7(),
            kind: "transfer".into(),
            amount_minor: amount,
            currency: "TJS".into(),
            initiated_by: from.holder_id,
            from,
            to,
        },
    )
}

#[tokio::test]
async fn requires_a_valid_token() {
    let mut h = Harness::start().await;
    let err = h.client.list(ListRequest { limit: 5 }).await.unwrap_err();
    assert_eq!(err.code(), Code::Unauthenticated);

    let mut forged = Request::new(ListRequest { limit: 5 });
    let other = JwtKeys::random(Duration::from_secs(60));
    let user = User::restore(
        UserId::from_uuid(Uuid::now_v7()),
        Email::parse("x@ledger.test").unwrap(),
        "X".into(),
        Role::Admin,
        String::new(),
    );
    let bearer = format!("Bearer {}", other.issue(&user).unwrap().token)
        .parse()
        .unwrap();
    forged.metadata_mut().insert("authorization", bearer);
    assert_eq!(
        h.client.list(forged).await.unwrap_err().code(),
        Code::Unauthenticated
    );

    let err = h.client.subscribe(SubscribeRequest {}).await.unwrap_err();
    assert_eq!(err.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn each_user_lists_only_their_notifications() {
    let mut h = Harness::start().await;
    let (firuz, nigina, stranger) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    h.inbox.apply(&transfer(
        party("Firuz", Some(firuz), 75_000),
        party("Nigina", Some(nigina), 25_000),
        25_000,
    ));

    let list = |h: &Harness, user| h.request(ListRequest { limit: 0 }, user);
    let mine = h
        .client
        .list(list(&h, firuz))
        .await
        .unwrap()
        .into_inner()
        .notifications;
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].direction(), Direction::Debit);
    assert_eq!(mine[0].text, "You sent 250.00 TJS to Nigina");

    let theirs = h
        .client
        .list(list(&h, nigina))
        .await
        .unwrap()
        .into_inner()
        .notifications;
    assert_eq!(theirs[0].text, "You received 250.00 TJS from Firuz");
    assert_eq!(theirs[0].balance_after_minor, 25_000);

    let nothing = h
        .client
        .list(list(&h, stranger))
        .await
        .unwrap()
        .into_inner()
        .notifications;
    assert_eq!(nothing, []);

    let too_many = h.request(ListRequest { limit: 1_000 }, firuz);
    assert_eq!(
        h.client.list(too_many).await.unwrap_err().code(),
        Code::InvalidArgument
    );
}

#[tokio::test]
async fn subscribe_streams_only_the_callers_live_notifications() {
    let mut h = Harness::start().await;
    let (firuz, nigina) = (Uuid::now_v7(), Uuid::now_v7());
    let mut live = h
        .client
        .subscribe(h.request(SubscribeRequest {}, nigina))
        .await
        .unwrap()
        .into_inner();

    h.inbox.apply(&transfer(
        party("Firuz", Some(firuz), 0),
        party("Someone", Some(Uuid::now_v7()), 1),
        1,
    ));
    h.inbox.apply(&transfer(
        party("Firuz", Some(firuz), 0),
        party("Nigina", Some(nigina), 5_000),
        5_000,
    ));

    let first = tokio::time::timeout(Duration::from_secs(2), live.next())
        .await
        .expect("a notification arrives")
        .unwrap()
        .unwrap();
    assert_eq!(
        first.text, "You received 50.00 TJS from Firuz",
        "other users' events are filtered out"
    );
}

#[tokio::test]
async fn end_to_end_through_jetstream() {
    let Ok(url) = std::env::var("NATS_URL") else {
        eprintln!("NATS_URL not set, skipping");
        return;
    };
    let mut h = Harness::start().await;
    let js = async_nats::jetstream::new(async_nats::connect(&url).await.unwrap());
    ledger_events::jetstream::ensure_stream(&js).await.unwrap();
    tokio::spawn(projection::run(js.clone(), h.inbox.clone()));

    let nigina = Uuid::now_v7();
    let event = transfer(
        party("Firuz", Some(Uuid::now_v7()), 0),
        party("Nigina", Some(nigina), 7_700),
        7_700,
    );
    for _ in 0..2 {
        js.send_publish(
            TransferPosted::SUBJECT,
            async_nats::jetstream::message::PublishMessage::build()
                .payload(event.to_json().unwrap().into())
                .message_id(event.id.to_string()),
        )
        .await
        .unwrap()
        .await
        .unwrap();
    }

    let mut found = Vec::new();
    for _ in 0..50 {
        found = h
            .client
            .list(h.request(ListRequest { limit: 10 }, nigina))
            .await
            .unwrap()
            .into_inner()
            .notifications;
        if !found.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(found.len(), 1, "published twice, delivered once");
    assert_eq!(found[0].text, "You received 77.00 TJS from Firuz");
}
