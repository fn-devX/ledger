CREATE TABLE users (
    id             UUID        PRIMARY KEY,
    email          TEXT        NOT NULL UNIQUE CHECK (email = lower(email)),
    name           TEXT        NOT NULL,
    role           TEXT        NOT NULL CHECK (role IN ('customer', 'admin')),
    password_hash  TEXT        NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE accounts ADD COLUMN holder_id UUID REFERENCES users (id);
CREATE INDEX accounts_holder_idx ON accounts (holder_id);

ALTER TABLE transfers ADD COLUMN initiated_by UUID REFERENCES users (id);
ALTER TABLE transfers DROP CONSTRAINT transfers_idempotency_key_key;
CREATE UNIQUE INDEX transfers_initiator_key_idx ON transfers (initiated_by, idempotency_key);
