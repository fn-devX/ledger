use anyhow::{Context, bail};
use futures::StreamExt;
use ledger_notifier::{
    inbox::format_money,
    proto::{
        Direction, ListRequest, Notification, SubscribeRequest,
        notifications_client::NotificationsClient,
    },
};
use tonic::{
    Request,
    metadata::{Ascii, MetadataValue},
};

fn usage() -> anyhow::Error {
    anyhow::anyhow!(
        "usage: notify-watch --token <JWT> [--addr http://127.0.0.1:50051]\n\
         (or set LEDGER_TOKEN and NOTIFY_ADDR)"
    )
}

fn authorized<T>(message: T, bearer: &MetadataValue<Ascii>) -> Request<T> {
    let mut request = Request::new(message);
    request
        .metadata_mut()
        .insert("authorization", bearer.clone());
    request
}

fn print(n: &Notification) {
    let at = n
        .occurred_at
        .as_ref()
        .and_then(|t| time::OffsetDateTime::from_unix_timestamp(t.seconds).ok())
        .map_or_else(
            || "--:--:--".to_owned(),
            |t| format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second()),
        );
    let sign = if n.direction() == Direction::Debit {
        "-"
    } else {
        "+"
    };
    println!(
        "{at} UTC  {sign}{:<16} {}  (balance {})",
        format_money(n.amount_minor, &n.currency),
        n.text,
        format_money(n.balance_after_minor, &n.currency)
    );
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut token = std::env::var("LEDGER_TOKEN").ok();
    let mut addr = std::env::var("NOTIFY_ADDR").unwrap_or_else(|_| "http://127.0.0.1:50051".into());
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--token" => token = Some(args.next().ok_or_else(usage)?),
            "--addr" => addr = args.next().ok_or_else(usage)?,
            _ => return Err(usage()),
        }
    }
    let Some(token) = token else {
        return Err(usage());
    };
    let bearer: MetadataValue<Ascii> =
        format!("Bearer {token}").parse().context("invalid token")?;

    let mut client = NotificationsClient::connect(addr.clone())
        .await
        .with_context(|| format!("connect to {addr}"))?;

    let history = client
        .list(authorized(ListRequest { limit: 10 }, &bearer))
        .await?
        .into_inner()
        .notifications;
    println!("--- last {} notifications ---", history.len());
    for n in history.iter().rev() {
        print(n);
    }

    println!("--- waiting for new notifications (Ctrl+C to stop) ---");
    let mut live = client
        .subscribe(authorized(SubscribeRequest {}, &bearer))
        .await?
        .into_inner();
    while let Some(item) = live.next().await {
        match item {
            Ok(n) => print(&n),
            Err(status) => bail!("stream closed: {}", status.message()),
        }
    }
    Ok(())
}
