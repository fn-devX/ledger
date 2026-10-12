use std::{net::SocketAddr, sync::Arc};

use anyhow::Context;
use ledger_api::{
    config::Config,
    http::{self, AppState, HttpSettings, JwtKeys, Store},
    telemetry,
};
use ledger_application::{
    AuthService, LedgerService,
    ratelimit::{RateLimit, RateLimiter},
};
use ledger_infrastructure::{
    Argon2Hasher, InMemoryLedger, InMemoryRateLimiter, PgLedgerRepository, RedisRateLimiter,
    UuidV7Generator,
};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let cfg = Config::from_env()?;
    telemetry::init(cfg.log_format);
    tracing::debug!(config = ?cfg, "configuration loaded");

    if let Some(url) = &cfg.database_url {
        let repo = PgLedgerRepository::connect(url, cfg.db_max_connections)
            .await
            .context("connect to postgres")?;
        repo.migrate().await.context("run migrations")?;
        tracing::info!("using PostgreSQL storage");
        serve(&cfg, repo).await
    } else {
        tracing::warn!("DATABASE_URL is not set: using in-memory storage, data is lost on restart");
        serve(&cfg, InMemoryLedger::new()).await
    }
}

async fn rate_limiter(cfg: &Config) -> anyhow::Result<Arc<dyn RateLimiter>> {
    if let Some(url) = &cfg.redis_url {
        let limiter = RedisRateLimiter::connect(url, "ledger:ratelimit:")
            .await
            .context("connect to redis")?;
        tracing::info!("using Redis rate limiter");
        Ok(Arc::new(limiter))
    } else {
        tracing::warn!("REDIS_URL is not set: rate limits are per process");
        Ok(Arc::new(InMemoryRateLimiter::new()))
    }
}

async fn serve<R: Store + Clone>(cfg: &Config, repo: R) -> anyhow::Result<()> {
    let hasher = Argon2Hasher::new().map_err(|e| anyhow::anyhow!(e))?;
    let auth = AuthService::new(repo.clone(), hasher, UuidV7Generator);
    if let Some(admin) = &cfg.admin {
        let user = auth
            .ensure_admin(&admin.email, &admin.name, &admin.password)
            .await
            .context("create admin user")?;
        tracing::info!(email = %user.email().as_str(), "admin user is ready");
    }

    let jwt = if let Some(secret) = &cfg.jwt_secret {
        JwtKeys::new(secret.as_bytes(), cfg.access_token_ttl)
    } else {
        tracing::warn!("JWT_SECRET is not set: using a random key, tokens die on restart");
        JwtKeys::random(cfg.access_token_ttl)
    };

    let state = Arc::new(AppState {
        ledger: LedgerService::new(repo, UuidV7Generator),
        auth,
        jwt: Arc::new(jwt),
    });
    let settings = HttpSettings {
        request_timeout: cfg.request_timeout,
        limiter: rate_limiter(cfg).await?,
        api_limit: RateLimit::per_minute(cfg.api_rate_limit_per_minute),
        auth_limit: RateLimit::per_minute(cfg.auth_rate_limit_per_minute),
        trust_proxy_headers: cfg.trust_proxy_headers,
    };
    let app = http::router(state, &settings);

    let listener = TcpListener::bind(cfg.http_addr)
        .await
        .with_context(|| format!("bind {}", cfg.http_addr))?;
    tracing::info!(addr = %cfg.http_addr, "listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
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
