use std::{net::SocketAddr, time::Duration};

use anyhow::Context;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Pretty,
    Json,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub http_addr: SocketAddr,
    pub database_url: Option<String>,
    pub db_max_connections: u32,
    pub request_timeout: Duration,
    pub log_format: LogFormat,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let http_addr = var("HTTP_ADDR")
            .unwrap_or_else(|| "0.0.0.0:8080".into())
            .parse()
            .context("HTTP_ADDR must be host:port")?;
        let db_max_connections = var("DB_MAX_CONNECTIONS")
            .map(|v| v.parse())
            .transpose()
            .context("DB_MAX_CONNECTIONS must be a number")?
            .unwrap_or(10);
        let timeout_secs: u64 = var("REQUEST_TIMEOUT_SECS")
            .map(|v| v.parse())
            .transpose()
            .context("REQUEST_TIMEOUT_SECS must be a number")?
            .unwrap_or(10);
        let log_format = match var("LOG_FORMAT").as_deref() {
            Some("json") => LogFormat::Json,
            _ => LogFormat::Pretty,
        };
        Ok(Self {
            http_addr,
            database_url: var("DATABASE_URL"),
            db_max_connections,
            request_timeout: Duration::from_secs(timeout_secs),
            log_format,
        })
    }
}
