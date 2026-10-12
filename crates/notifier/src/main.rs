use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, bail};
use ledger_auth::JwtKeys;
use ledger_notifier::{
    grpc::{self, NotificationsService},
    inbox::Inbox,
    projection,
};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ledger_notifier=debug"));
    let registry = tracing_subscriber::registry().with(filter);
    if var("LOG_FORMAT").as_deref() == Some("json") {
        registry
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        registry.with(tracing_subscriber::fmt::layer()).init();
    }

    let nats_url = var("NATS_URL").unwrap_or_else(|| "nats://127.0.0.1:4222".into());
    let grpc_addr: SocketAddr = var("GRPC_ADDR")
        .unwrap_or_else(|| "0.0.0.0:50051".into())
        .parse()
        .context("GRPC_ADDR must be host:port")?;
    let Some(secret) = var("JWT_SECRET") else {
        bail!("JWT_SECRET must be set to the same value as the ledger API");
    };
    if secret.len() < 32 {
        bail!("JWT_SECRET must be at least 32 characters");
    }

    let client = async_nats::connect(&nats_url)
        .await
        .with_context(|| format!("connect to NATS at {nats_url}"))?;
    let inbox = Arc::new(Inbox::new());
    tokio::spawn(projection::run(
        async_nats::jetstream::new(client),
        inbox.clone(),
    ));

    let service = NotificationsService::new(
        inbox,
        Arc::new(JwtKeys::new(secret.as_bytes(), Duration::ZERO)),
    );
    tracing::info!(addr = %grpc_addr, "gRPC notifications service listening");
    grpc::serve(grpc_addr, service, shutdown_signal())
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    tracing::info!("shut down gracefully");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
