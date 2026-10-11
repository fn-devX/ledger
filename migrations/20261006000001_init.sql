CREATE TABLE accounts (
    id          UUID        PRIMARY KEY,
    kind        TEXT        NOT NULL CHECK (kind IN ('customer', 'system')),
    owner       TEXT        NOT NULL,
    currency    TEXT        NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    balance     BIGINT      NOT NULL DEFAULT 0,
    version     BIGINT      NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT customer_balance_non_negative CHECK (kind = 'system' OR balance >= 0)
);

CREATE UNIQUE INDEX accounts_one_system_per_currency ON accounts (currency) WHERE kind = 'system';

CREATE TABLE transfers (
    id               UUID        PRIMARY KEY,
    idempotency_key  TEXT        NOT NULL UNIQUE,
    kind             TEXT        NOT NULL CHECK (kind IN ('deposit', 'transfer')),
    from_account     UUID        NOT NULL REFERENCES accounts (id),
    to_account       UUID        NOT NULL REFERENCES accounts (id),
    amount           BIGINT      NOT NULL CHECK (amount > 0),
    currency         TEXT        NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (from_account <> to_account)
);

CREATE TABLE ledger_entries (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    transfer_id  UUID        NOT NULL REFERENCES transfers (id),
    account_id   UUID        NOT NULL REFERENCES accounts (id),
    delta        BIGINT      NOT NULL CHECK (delta <> 0),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX ledger_entries_account_idx ON ledger_entries (account_id, id);
