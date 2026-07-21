-- =========================================================
-- oil_changes: maint-owned consumption event. Debits oil stock.
-- Any liter amount accepted; `flagged` set when outside the 34-36 L band.
-- =========================================================
CREATE TABLE oil_changes (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    work_order_id           UUID REFERENCES work_orders(id) ON DELETE SET NULL,
    vehicle_id              INT NOT NULL,
    oil_type                TEXT NOT NULL,
    liters                  NUMERIC(8,2) NOT NULL,
    odometer                INT,
    flagged                 BOOLEAN NOT NULL DEFAULT FALSE,
    notes                   TEXT,
    performed_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1
);
CREATE INDEX idx_oil_vehicle ON oil_changes(vehicle_id);
CREATE INDEX idx_oil_wo      ON oil_changes(work_order_id);
CREATE INDEX idx_oil_updated ON oil_changes(updated_at);

-- =========================================================
-- Stock CACHES: read-only mirrors of Falcon's authoritative credit ledger.
-- No sync columns (like vehicles_cache). Debits reflected locally, pushed to Falcon S2S.
-- =========================================================
CREATE TABLE tire_stock_cache (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    brand           TEXT NOT NULL,
    model           TEXT,
    size            TEXT,
    on_hand_qty     INT NOT NULL DEFAULT 0,
    falcon_stock_id INT,
    fetched_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (brand, model, size)
);

CREATE TABLE oil_stock_cache (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    oil_type        TEXT NOT NULL UNIQUE,
    liters_on_hand  NUMERIC(10,2) NOT NULL DEFAULT 0,
    falcon_stock_id INT,
    fetched_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- =========================================================
-- stock_movements: local debit ledger. Every consumption records a movement;
-- a background reconciler pushes un-mirrored rows to Falcon (S2S) and stamps
-- mirrored_to_falcon_at. Append-only, server-owned (not in the offline outbox;
-- generated server-side when a mount/oil op applies).
-- =========================================================
CREATE TABLE stock_movements (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind                    stock_move_kind NOT NULL,
    brand                   TEXT,
    model                   TEXT,
    size                    TEXT,
    oil_type                TEXT,
    qty                     INT,
    liters                  NUMERIC(10,2),
    work_order_id           UUID,
    ref_id                  UUID,               -- tire_assignment or oil_change id (idempotency key upstream)
    note                    TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    mirrored_to_falcon_at   TIMESTAMPTZ
);
CREATE INDEX idx_sm_unmirrored ON stock_movements(created_at) WHERE mirrored_to_falcon_at IS NULL;
