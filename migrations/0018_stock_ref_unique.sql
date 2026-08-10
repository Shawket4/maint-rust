-- Exactly-once stock movements: the ref_id (the consuming event's UUID, or a
-- client idempotency key on a credit) must be unique. The old check-then-insert
-- had a concurrency race — two retries with the same ref_id both saw "not
-- found" and both debited/credited. The unique index + ON CONFLICT DO NOTHING
-- makes the database the single source of exactly-once truth.
CREATE UNIQUE INDEX IF NOT EXISTS idx_stock_movements_ref
    ON stock_movements (ref_id) WHERE ref_id IS NOT NULL;
