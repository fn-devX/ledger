use std::{net::SocketAddr, time::Duration};

use anyhow::{Context, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Pretty,
    Json,
}

#[derive(Clone)]
pub struct AdminSeed {
    pub email: String,
    pub name: String,
    pub password: String,
}

impl std::fmt::Debug for AdminSeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminSeed")
            .field("email", &self.email)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct Config {
    pub http_addr: SocketAddr,
    pub database_url: Option<String>,
    pub db_max_connections: u32,
    pub request_timeout: Duration,
    pub log_format: LogFormat,
    pub jwt_secret: Option<String>,
    pub access_token_ttl: Duration,
    pub admin: Option<AdminSeed>,
    pub redis_url: Option<String>,
    pub nats_url: Option<String>,
    pub api_rate_limit_per_minute: u32,
    pub auth_rate_limit_per_minute: u32,
    pub trust_proxy_headers: bool,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("http_addr", &self.http_addr)
            .field("database", &self.database_url.is_some())
            .field("redis", &self.redis_url.is_some())
            .field("nats", &self.nats_url.is_some())
            .field("access_token_ttl", &self.access_token_ttl)
            .field("admin", &self.admin)
            .field("api_rate_limit_per_minute", &self.api_rate_limit_per_minute)
            .field(
                "auth_rate_limit_per_minute",
                &self.auth_rate_limit_per_minute,
            )
            .field("trust_proxy_headers", &self.trust_proxy_headers)
            .finish_non_exhaustive()
    }
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn number<T: std::str::FromStr>(name: &str, default: T) -> anyhow::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    var(name)
        .map(|v| v.trim().parse::<T>())
        .transpose()
        .with_context(|| format!("{name} must be a number"))
        .map(|v| v.unwrap_or(default))
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let http_addr = var("HTTP_ADDR")
            .unwrap_or_else(|| "0.0.0.0:8080".into())
            .parse()
            .context("HTTP_ADDR must be host:port")?;
        let log_format = match var("LOG_FORMAT").as_deref() {
            Some("json") => LogFormat::Json,
            _ => LogFormat::Pretty,
        };
        let jwt_secret = var("JWT_SECRET");
        if jwt_secret.as_ref().is_some_and(|s| s.len() < 32) {
            bail!("JWT_SECRET must be at least 32 characters");
        }
        let admin = match (var("ADMIN_EMAIL"), var("ADMIN_PASSWORD")) {
            (Some(email), Some(password)) => Some(AdminSeed {
                email,
                password,
                name: var("ADMIN_NAME").unwrap_or_else(|| "Administrator".into()),
            }),
            (None, None) => None,
            _ => bail!("ADMIN_EMAIL and ADMIN_PASSWORD must be set together"),
        };
        Ok(Self {
            http_addr,
            database_url: var("DATABASE_URL"),
            db_max_connections: number("DB_MAX_CONNECTIONS", 10)?,
            request_timeout: Duration::from_secs(number("REQUEST_TIMEOUT_SECS", 10)?),
            log_format,
            jwt_secret,
            access_token_ttl: Duration::from_secs(number("ACCESS_TOKEN_TTL_SECS", 3600)?),
            admin,
            redis_url: var("REDIS_URL"),
            nats_url: var("NATS_URL"),
            api_rate_limit_per_minute: number("API_RATE_LIMIT_PER_MINUTE", 120)?,
            auth_rate_limit_per_minute: number("AUTH_RATE_LIMIT_PER_MINUTE", 10)?,
            trust_proxy_headers: matches!(
                var("TRUST_PROXY_HEADERS").as_deref(),
                Some("true" | "1")
            ),
        })
    }
}
