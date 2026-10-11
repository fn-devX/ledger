# Ledger — a payment core in Rust

A money ledger service: accounts, deposits and transfers with **double-entry bookkeeping**,
**idempotency** and **optimistic locking**. Built with a hexagonal (clean) architecture.

## Architecture

```mermaid
flowchart LR
    client([HTTP client]) --> api

    subgraph api["crates/api — outer layer"]
        direction TB
        http["HTTP (Axum)<br/>DTOs, RFC 9457 errors, middleware"]
        root["main.rs — composition root<br/>config, tracing, graceful shutdown"]
    end

    subgraph app["crates/application — use cases"]
        svc["LedgerService<br/>open_account / deposit / transfer"]
        ports{{"ports:<br/>LedgerRepository, IdGenerator"}}
    end

    subgraph dom["crates/domain — core"]
        model["Money, Currency, Account,<br/>Transfer, post_transfer()"]
    end

    subgraph infra["crates/infrastructure — adapters"]
        pg["PgLedgerRepository (sqlx)"]
        mem["InMemoryLedger"]
        ids["UuidV7Generator"]
    end

    http --> svc
    svc --> model
    svc --> ports
    pg -. implements .-> ports
    mem -. implements .-> ports
    ids -. implements .-> ports
    root --> infra
    pg --> db[(PostgreSQL)]
```

**Dependency rule:** arrows point inwards only.

| Crate | Depends on | Knows nothing about |
|---|---|---|
| `domain` | nothing (only `uuid`, `thiserror`) | async, database, HTTP, serde |
| `application` | `domain` | Postgres, Axum |
| `infrastructure` | `application`, `domain`, `sqlx` | HTTP |
| `api` | everything (wires the app together) | — |

The domain is tested in milliseconds, and the storage can be swapped without touching business logic.

### How a transfer works

1. **API** validates the JSON and the `Idempotency-Key` header, then builds a command.
2. **Use case** looks up a transfer with that key: same parameters → returns it (`200`, `Idempotent-Replayed: true`); different parameters → `409`.
3. Loads both accounts and calls the **pure domain function** `post_transfer`: currency, sufficient funds, overflow. All checks run before any mutation.
4. **Repository**, in a single transaction: `UPDATE ... WHERE version = loaded_version` (ordered by id to avoid deadlocks), then `INSERT` the transfer and its two entries.
5. A version conflict or a race on the key → retry from step 2 (up to N attempts).

### Key decisions

- **Money is `i64` in minor units** (dirams, cents) — never `f64`.
- **Double entry:** a deposit is a transfer from the currency's system account. The sum of balances per currency is always `0`, and so is the sum of entries. Verified by tests and by reconciliation in the database.
- **Defense in depth:** even with a bug in the code, the database rejects a negative customer balance (`CHECK`) and a duplicate key (`UNIQUE`).
- **Native `async fn` in traits** instead of `async_trait`; static dispatch through generics.
- **UUIDv7** — time-ordered, index-friendly ids.
- **Errors:** `thiserror` per layer; the API returns `application/problem+json` and never leaks internal details.
- **Keyset (cursor) pagination** for statements: `WHERE id < cursor ORDER BY id DESC` runs as a backward index scan on `(account_id, id)`, so page 1000 is as fast as page 1 and rows inserted meanwhile never shift pages (unlike `OFFSET`). The cursor is opaque to clients.
- **Running balance** is stored on each entry (`balance_after`) at write time; the migration that added it backfilled existing rows with a window function.
- **Observability:** `tracing` spans for every request with `x-request-id`; JSON logs in production.

## Running (Windows / Linux / macOS)

Requires Rust (stable) and Docker Desktop.

```bash
docker compose up -d postgres      # PostgreSQL on localhost:5432
cp .env.example .env               # PowerShell: copy .env.example .env
cargo run -p ledger-api            # http://localhost:8080
```

Without `DATABASE_URL` the service starts with in-memory storage — handy for quick experiments.
Everything in containers: `docker compose --profile full up --build`.

On Windows you can just double-click `scripts\run.cmd` (starts the service on port 8090),
then `scripts\demo.cmd` — it walks through deposits, transfers, idempotent retries, error cases
and 20 parallel transfers, then prints a paginated account statement.

Sample requests are in `requests.http` (RustRover or the VS Code REST Client extension).

## API

| Method | Path | Description |
|---|---|---|
| `POST` | `/v1/accounts` | open an account `{owner, currency}` |
| `GET` | `/v1/accounts/{id}` | account and balance |
| `GET` | `/v1/accounts/{id}/entries?limit=&cursor=` | statement: entries newest first with running balance, cursor pagination |
| `POST` | `/v1/accounts/{id}/deposits` | deposit `{amount_minor, currency}` + `Idempotency-Key` |
| `POST` | `/v1/transfers` | transfer `{from, to, amount_minor, currency}` + `Idempotency-Key` |
| `GET` | `/v1/transfers/{id}` | transfer |
| `GET` | `/health/live`, `/health/ready` | Kubernetes probes |

## Quality

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings   # clippy::pedantic enabled
cargo test --workspace                                  # in-memory
DATABASE_URL=postgres://ledger:ledger@localhost:5432/ledger cargo test --workspace  # + Postgres
```

Contract tests (`crates/infrastructure/tests`) run **the same scenarios** against both
adapters, including 50 concurrent transfers from one account: exactly as many succeed as the
balance allows, not a single diram more. CI (GitHub Actions) runs all of this against Postgres
and builds the Docker image.

## Roadmap

- [x] Paginated account entry history, `GET /v1/accounts/{id}/entries`
- [ ] `sqlx` compile-time checked queries (`query!` + offline mode)
- [ ] OpenAPI spec (`utoipa`) and Swagger UI
- [ ] Authentication (JWT), rate limiting (Redis)
- [ ] Outbox pattern → Kafka/NATS, notification service over gRPC (`tonic`)
- [ ] Prometheus metrics, OpenTelemetry traces → Grafana/Jaeger
- [ ] Property tests (`proptest`), load testing (k6)
- [ ] Kubernetes manifests / Helm
- [ ] Sharding the system account (hotspot under heavy deposit load)
