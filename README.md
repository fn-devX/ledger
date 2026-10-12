# Ledger — a payment core in Rust

A money ledger service: accounts, deposits and transfers with **double-entry bookkeeping**,
**idempotency** and **optimistic locking**, behind **JWT authentication** and **Redis rate limiting**.
Every posted transfer becomes an event (**transactional outbox → NATS JetStream**), and a separate
**notification service** turns events into per-user notifications served over **gRPC** (`tonic`).
Built with a hexagonal (clean) architecture.

## Architecture

```mermaid
flowchart LR
    client([HTTP client]) --> api
    watcher([gRPC client]) --> notifier

    subgraph api["crates/api — HTTP service"]
        direction TB
        http["Axum: DTOs, RFC 9457 errors,<br/>JWT and rate limit middleware"]
        root["main.rs — composition root,<br/>spawns the outbox relay"]
    end

    subgraph app["crates/application — use cases"]
        svc["LedgerService + AuthService<br/>authorization, builds events"]
        ports{{"ports: LedgerRepository, UserRepository,<br/>OutboxStore, EventPublisher,<br/>PasswordHasher, RateLimiter"}}
    end

    subgraph dom["crates/domain — core"]
        model["Money, Account, Transfer,<br/>User, post_transfer()"]
    end

    subgraph infra["crates/infrastructure — adapters"]
        pg["PgLedgerRepository (sqlx)<br/>+ outbox relay"]
        js["JetStreamPublisher"]
        redis["RedisRateLimiter (GCRA, Lua)"]
        argon["Argon2Hasher"]
    end

    subgraph contracts["shared contracts"]
        events["crates/events<br/>TransferPosted v1"]
        auth["crates/auth<br/>JWT"]
    end

    subgraph notifier["crates/notifier — gRPC service"]
        proj["projection from JetStream"]
        grpc["Notifications: List, Subscribe (stream)"]
    end

    http --> svc
    svc --> model
    svc --> ports
    svc -. emits .-> events
    pg -. implements .-> ports
    js -. implements .-> ports
    redis -. implements .-> ports
    argon -. implements .-> ports
    root --> infra
    pg --> db[(PostgreSQL)]
    redis --> rds[(Redis)]
    js --> nats[(NATS JetStream)]
    nats --> proj
    proj --> grpc
    events -.- proj
    auth -.- http
    auth -.- grpc
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
5. The same transaction inserts a `transfer.posted` event into the **outbox** and fires `NOTIFY`.
6. A version conflict or a race on the key → retry from step 2 (up to N attempts).

### Events and notifications

```mermaid
sequenceDiagram
    participant API as ledger-api
    participant PG as PostgreSQL
    participant R as outbox relay
    participant JS as NATS JetStream
    participant N as ledger-notifier
    participant C as gRPC client
    API->>PG: BEGIN, update balances, insert transfer, entries, outbox row, NOTIFY, COMMIT
    PG-->>R: NOTIFY ledger_outbox
    R->>PG: SELECT ... FOR UPDATE SKIP LOCKED
    R->>JS: publish (Nats-Msg-Id = event id)
    JS-->>R: ack
    R->>PG: mark published, COMMIT
    JS->>N: ordered consumer
    N->>N: project into per-user notifications
    N-->>C: Subscribe stream: "You received 250.00 TJS from Firuz"
```

- **Transactional outbox.** The money and the event are committed together, so there is never a transfer without an event or an event without a transfer — no dual-write problem.
- **Relay.** Wakes up on Postgres `LISTEN/NOTIFY` (milliseconds, measured ~20 ms from HTTP 201 to the gRPC client), polls as a fallback, claims rows with `FOR UPDATE SKIP LOCKED` so several relays can run side by side, and backs off exponentially while the broker is down. The API keeps accepting transfers during a broker outage; events wait in the outbox.
- **Effectively once.** Delivery is at-least-once, and JetStream drops redeliveries of the same `Nats-Msg-Id` within a 10-minute window; the notifier is idempotent by event id on top of that.
- **Contracts are code.** `crates/events` holds the versioned envelope (`id`, `type`, `version`, `occurred_at`, `data`) and `TransferPosted v1`; both services compile against it. `crates/auth` holds JWT verification, so the notifier accepts the same tokens as the API.
- **The notifier owns no database.** It replays the stream from the beginning on start (an ordered consumer) and rebuilds its read model, then follows new events live.
- **gRPC.** `ledger.notifications.v1.Notifications` with `List` and a server-streaming `Subscribe`, authenticated with the same bearer token; each user only ever sees their own notifications. Server reflection is on, so `grpcurl` and Postman discover the API. `protoc` is vendored, so the build needs nothing installed.

### Key decisions

- **Money is `i64` in minor units** (dirams, cents) — never `f64`.
- **Double entry:** a deposit is a transfer from the currency's system account. The sum of balances per currency is always `0`, and so is the sum of entries. Verified by tests and by reconciliation in the database.
- **Defense in depth:** even with a bug in the code, the database rejects a negative customer balance (`CHECK`) and a duplicate key (`UNIQUE`).
- **Compile-time checked SQL:** every query uses `sqlx::query!` / `query_as!`, so a typo in a column name, a wrong parameter type or a nullable column read into a non-`Option` field is a **compile error**, not a production incident. Query metadata is cached in `.sqlx/` (committed), and `.cargo/config.toml` sets `SQLX_OFFLINE=true`, so building never needs a running database — on a laptop, in Docker or in CI. CI fails if the cache is stale.
- **API contract generated from code:** the OpenAPI spec is built by `utoipa` from the handlers and DTOs (`#[utoipa::path]`, `ToSchema`), so it cannot drift from the implementation. Tests check that every documented operation is actually routed and that the committed snapshot is current. Swagger UI is vendored — no network access at build time.
- **Authentication:** passwords are hashed with Argon2id off the async runtime (`spawn_blocking`). Login returns an HS256 JWT (`sub`, `role`, `iss`, `exp`); the signature, issuer and expiry are validated, `alg=none` and foreign keys are rejected. Logging in with an unknown email verifies against a decoy hash, so it takes as long and answers the same as a wrong password — emails cannot be enumerated.
- **Authorization lives in the use cases**, not in HTTP: customers spend and see only their accounts, only admins (the cash desk) can deposit money. Idempotency keys are scoped per user, so one user's key never collides with another's.
- **Rate limiting with GCRA** (a token bucket without a background refill): one Lua script in Redis reads the bucket, decides and updates it atomically using Redis' own clock, so all instances share one limit. Auth endpoints are limited per client IP (brute-force protection), the rest of the API per user. Responses carry `RateLimit-*` headers, `429` carries `Retry-After`. If Redis is down the API fails open and logs a warning rather than going down with it.
- **Native `async fn` in traits** instead of `async_trait`; static dispatch through generics.
- **UUIDv7** — time-ordered, index-friendly ids.
- **Errors:** `thiserror` per layer; the API returns `application/problem+json` and never leaks internal details.
- **Keyset (cursor) pagination** for statements: `WHERE id < cursor ORDER BY id DESC` runs as a backward index scan on `(account_id, id)`, so page 1000 is as fast as page 1 and rows inserted meanwhile never shift pages (unlike `OFFSET`). The cursor is opaque to clients.
- **Running balance** is stored on each entry (`balance_after`) at write time; the migration that added it backfilled existing rows with a window function.
- **Observability:** `tracing` spans for every request with `x-request-id`; JSON logs in production.

## Running (Windows / Linux / macOS)

Requires Rust (stable) and Docker Desktop.

```bash
docker compose up -d postgres redis nats
cp .env.example .env               # PowerShell: copy .env.example .env
cargo run -p ledger-api            # http://localhost:8080/swagger-ui/
cargo run -p ledger-notifier       # gRPC on 127.0.0.1:50051
```

Watch your notifications live (log in through the API first and pass the access token):

```bash
cargo run -p ledger-notifier --bin notify-watch -- --token <JWT>
grpcurl -plaintext -H "authorization: Bearer <JWT>" 127.0.0.1:50051 ledger.notifications.v1.Notifications/Subscribe
```

On start the service applies migrations and creates the admin from `ADMIN_EMAIL` / `ADMIN_PASSWORD`.
Log in through `POST /v1/auth/login`, then press **Authorize** in Swagger UI and paste the token.

Every dependency is optional for local experiments: without `DATABASE_URL` storage is in-memory,
without `REDIS_URL` rate limits are per process, without `NATS_URL` events stay in the outbox,
without `JWT_SECRET` a random key is generated (tokens stop working after a restart; the notifier
requires the same `JWT_SECRET` as the API).
Everything in containers: `docker compose --profile full up --build`.

On Windows you can just double-click `scripts\run.cmd` (starts the service on port 8090),
then `scripts\demo.cmd` — it walks through deposits, transfers, idempotent retries, error cases
and 20 parallel transfers, then prints a paginated account statement.

Sample requests are in `requests.http` (RustRover or the VS Code REST Client extension).

## API

Interactive docs: **`/swagger-ui/`** (e.g. http://localhost:8080/swagger-ui/). The OpenAPI 3.1 spec is
served at `/api-docs/openapi.json` and committed as [`crates/api/openapi.json`](crates/api/openapi.json),
so API changes show up in code review and clients can be generated from it.

| Method | Path | Who | Description |
|---|---|---|---|
| `POST` | `/v1/auth/register` | anyone | create a customer `{email, name, password}` |
| `POST` | `/v1/auth/login` | anyone | get an access token `{email, password}` |
| `GET` | `/v1/auth/me` | user | the caller, from the token |
| `GET` | `/v1/accounts` | user | the caller's accounts |
| `POST` | `/v1/accounts` | user | open an account `{currency}` for the caller |
| `GET` | `/v1/accounts/{id}` | holder, admin | account and balance |
| `GET` | `/v1/accounts/{id}/entries?limit=&cursor=` | holder, admin | statement: entries newest first with running balance, cursor pagination |
| `POST` | `/v1/accounts/{id}/deposits` | admin | deposit `{amount_minor, currency}` + `Idempotency-Key` |
| `POST` | `/v1/transfers` | holder of `from` | transfer `{from, to, amount_minor, currency}` + `Idempotency-Key` |
| `GET` | `/v1/transfers/{id}` | both sides, admin | transfer |
| `GET` | `/health/live`, `/health/ready` | anyone | Kubernetes probes |

Foreign accounts and transfers answer `404`, not `403`, so their ids cannot be probed.

## Quality

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings   # clippy::pedantic enabled
cargo test --workspace                                  # in-memory
DATABASE_URL=postgres://ledger:ledger@localhost:5432/ledger REDIS_URL=redis://localhost:6379 NATS_URL=nats://localhost:4222 cargo test --workspace  # + Postgres + Redis + NATS
```

### Changing SQL

Queries are verified against the real schema at compile time. After you add or change a query
(or a migration), refresh the offline cache with Postgres running and commit `.sqlx/`:

```bash
cargo install sqlx-cli --version 0.8.6 --locked --no-default-features --features postgres,rustls
docker compose up -d postgres
sqlx migrate run
cargo sqlx prepare --workspace -- --all-targets
```

If you forget, the build tells you: `there is no cached data for this query, run cargo sqlx prepare`.

### Changing the API

After changing a handler or a DTO, refresh the committed OpenAPI snapshot:

```bash
UPDATE_OPENAPI=1 cargo test -p ledger-api --test openapi      # PowerShell: $env:UPDATE_OPENAPI=1; cargo test -p ledger-api --test openapi; Remove-Item Env:UPDATE_OPENAPI
```

Contract tests (`crates/infrastructure/tests`) run **the same scenarios** against both
adapters, including 50 concurrent transfers from one account: exactly as many succeed as the
balance allows, not a single diram more. CI (GitHub Actions) runs all of this against Postgres
and builds the Docker image.

## Roadmap

- [x] Paginated account entry history, `GET /v1/accounts/{id}/entries`
- [x] `sqlx` compile-time checked queries (`query!` + offline mode)
- [x] OpenAPI spec (`utoipa`) and Swagger UI
- [x] Authentication (JWT), rate limiting (Redis)
- [x] Outbox pattern → NATS JetStream, notification service over gRPC (`tonic`)
- [ ] Prometheus metrics, OpenTelemetry traces → Grafana/Jaeger
- [ ] Property tests (`proptest`), load testing (k6)
- [ ] Kubernetes manifests / Helm
- [ ] Sharding the system account (hotspot under heavy deposit load)
