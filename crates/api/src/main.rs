use std::sync::Arc;

use anyhow::Context;
use ledger_api::{config::Config, http, telemetry};
use ledger_application::{
    LedgerService,
    ports::{IdGenerator, LedgerRepository},
};
use ledger_infrastructure::{InMemoryLedger, PgLedgerRepository, UuidV7Generator};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let cfg = Config::from_env()?;
    telemetry::init(cfg.log_format);

    if let Some(url) = &cfg.database_url {
        let repo = PgLedgerRepository::connect(url, cfg.db_max_connections)
            .await
            .context("connect to postgres")?;
        repo.migrate().await.context("run migrations")?;
        tracing::info!("using PostgreSQL storage");
        serve(&cfg, LedgerService::new(repo, UuidV7Generator)).await
    } else {
        tracing::warn!("DATABASE_URL is not set: using in-memory storage, data is lost on restart");
        serve(
            &cfg,
            LedgerService::new(InMemoryLedger::new(), UuidV7Generator),
        )
        .await
    }
}

async fn serve<R: LedgerRepository, G: IdGenerator>(
    cfg: &Config,
    service: LedgerService<R, G>,
) -> anyhow::Result<()> {
    let app = http::router(Arc::new(service), cfg.request_timeout);
    let listener = TcpListener::bind(cfg.http_addr)
        .await
        .with_context(|| format!("bind {}", cfg.http_addr))?;
    tracing::info!(addr = %cfg.http_addr, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
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
