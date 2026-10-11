ALTER TABLE ledger_entries ADD COLUMN balance_after BIGINT;

UPDATE ledger_entries AS e
SET balance_after = running.total
FROM (
    SELECT id, SUM(delta) OVER (PARTITION BY account_id ORDER BY id) AS total
    FROM ledger_entries
) AS running
WHERE e.id = running.id;

ALTER TABLE ledger_entries ALTER COLUMN balance_after SET NOT NULL;
