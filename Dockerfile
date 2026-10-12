# syntax=docker/dockerfile:1

FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --bin ledger-api --bin ledger-notifier --bin notify-watch

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /app/target/release/ledger-api /usr/local/bin/ledger-api
COPY --from=builder /app/target/release/ledger-notifier /usr/local/bin/ledger-notifier
COPY --from=builder /app/target/release/notify-watch /usr/local/bin/notify-watch
ENV HTTP_ADDR=0.0.0.0:8080
EXPOSE 8080 50051
ENTRYPOINT ["/usr/local/bin/ledger-api"]
