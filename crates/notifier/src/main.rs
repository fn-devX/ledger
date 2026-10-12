use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, bail};
use ledger_auth::JwtKeys;
use ledger_notifier::{
    grpc::{self, NotificationsService},
    inbox::Inbox,
    projection,
};
use ledger_telemetry::{TelemetryConfig, metrics::Metrics};
use tokio::net::TcpListener;

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let telemetry = ledger_telemetry::init(TelemetryConfig::from_env(
        "ledger-notifier",
        "info,ledger_notifier=debug",
    ))
    .map_err(|e| anyhow::anyhow!(e))
    .context("initialise telemetry")?;
    let result = run().await;
    tokio::task::spawn_blocking(move || telemetry.shutdown()).await?;
    result
}

async fn serve_metrics(addr: SocketAddr, metrics: Metrics) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind metrics endpoint {addr}"))?;
    let app = axum::Router::new().route(
        "/metrics",
        axum::routing::get(move || std::future::ready(metrics.render())),
    );
    tracing::info!(%addr, "Prometheus metrics at /metrics");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "metrics endpoint stopped");
        }
    });
    Ok(())
}

async fn run() -> anyhow::Result<()> {
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
    let metrics_addr: SocketAddr = var("METRICS_ADDR")
        .unwrap_or_else(|| "0.0.0.0:9464".into())
        .parse()
        .context("METRICS_ADDR must be host:port")?;
    serve_metrics(metrics_addr, ledger_telemetry::metrics::install()).await?;

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
