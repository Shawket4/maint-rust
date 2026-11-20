-- Sync integrity (2026-11 audit).
--
-- 1. Exactly-once pushes: the client stamps every outbox op with a UUID and
--    the server remembers the applied result. A replay (the ack was lost)
--    returns the stored result instead of guessing from sync_version
--    arithmetic, which silently dropped edits (replayed insert) or turned a
--    lost ack into a false conflict (replayed update).
--
-- 2. A change feed that cannot skip rows: updated_at used to be now(), the
--    transaction START time. A slow push committed rows stamped earlier than
--    a cursor another device had already advanced past, and those rows were
--    never pulled. Now every write to a synced table takes one advisory
--    transaction lock and stamps clock_timestamp() — writers are serialized,
--    so commit order equals updated_at order.
--
-- 3. Constraints that were missing or wrong.

-- ── 1. applied-op ledger ────────────────────────────────────────────────────
CREATE TABLE sync_applied_ops (
    op_id        UUID PRIMARY KEY,
    user_id      BIGINT NOT NULL,
    entity_type  TEXT NOT NULL,
    entity_id    TEXT NOT NULL,
    result       JSONB NOT NULL,
    applied_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_sync_applied_ops_at ON sync_applied_ops(applied_at);

-- ── 2. commit-ordered updated_at ────────────────────────────────────────────
CREATE OR REPLACE FUNCTION maint_stamp_row() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    -- One lock for every synced write: held to the end of the transaction, so
    -- no later-stamped row can commit before an earlier-stamped one.
    PERFORM pg_advisory_xact_lock(736100121);
    NEW.updated_at := clock_timestamp();
    RETURN NEW;
END $$;

DO $$
DECLARE t TEXT;
BEGIN
    FOREACH t IN ARRAY ARRAY[
        'work_orders', 'work_order_tasks', 'tires', 'tire_assignments',
        'tire_status_events', 'oil_changes', 'chassis_layouts', 'chassis_axles',
        'chassis_positions', 'vehicle_class_assignments',
        'vehicle_odometer_overrides', 'service_plans'
    ] LOOP
        EXECUTE format(
            'CREATE TRIGGER trg_%s_stamp BEFORE INSERT OR UPDATE ON %I
               FOR EACH ROW EXECUTE FUNCTION maint_stamp_row()', t, t);
    END LOOP;
END $$;

-- Keyset pull reads (updated_at, id); the old single-column indexes cannot
-- serve the ORDER BY.
CREATE INDEX idx_wo_feed    ON work_orders(updated_at, id);
CREATE INDEX idx_wot_feed   ON work_order_tasks(updated_at, id);
CREATE INDEX idx_tires_feed ON tires(updated_at, id);
CREATE INDEX idx_ta_feed    ON tire_assignments(updated_at, id);
CREATE INDEX idx_tse_feed   ON tire_status_events(updated_at, id);
CREATE INDEX idx_oil_feed   ON oil_changes(updated_at, id);
CREATE INDEX idx_layouts_feed   ON chassis_layouts(updated_at, id);
CREATE INDEX idx_axles_feed     ON chassis_axles(updated_at, id);
CREATE INDEX idx_positions_feed ON chassis_positions(updated_at, id);

-- ── 3. constraints ──────────────────────────────────────────────────────────
-- A deleted chassis must not block its rebuild: uniqueness is for LIVE rows.
ALTER TABLE chassis_layouts DROP CONSTRAINT chassis_layouts_vehicle_id_key;
CREATE UNIQUE INDEX uq_chassis_layouts_vehicle_live
    ON chassis_layouts(vehicle_id) WHERE deleted_at IS NULL;

ALTER TABLE chassis_axles DROP CONSTRAINT chassis_axles_layout_id_section_section_index_key;
CREATE UNIQUE INDEX uq_chassis_axles_live
    ON chassis_axles(layout_id, section, section_index) WHERE deleted_at IS NULL;

ALTER TABLE chassis_positions DROP CONSTRAINT chassis_positions_layout_id_position_code_key;
CREATE UNIQUE INDEX uq_chassis_positions_live
    ON chassis_positions(layout_id, position_code) WHERE deleted_at IS NULL;

-- Sanity bounds. NOT VALID: enforced for every new or changed row without
-- failing the deploy on a historical typo that is already stored.
ALTER TABLE work_orders
    ADD CONSTRAINT ck_wo_odometer CHECK (odometer_at_open IS NULL OR odometer_at_open BETWEEN 0 AND 9999999) NOT VALID,
    ADD CONSTRAINT ck_wo_closed   CHECK ((status = 'closed') = (closed_at IS NOT NULL)) NOT VALID;
ALTER TABLE oil_changes
    ADD CONSTRAINT ck_oil_liters   CHECK (liters > 0 AND liters <= 1000) NOT VALID,
    ADD CONSTRAINT ck_oil_odometer CHECK (odometer IS NULL OR odometer BETWEEN 0 AND 9999999) NOT VALID;
ALTER TABLE tire_assignments
    ADD CONSTRAINT ck_ta_mounted_odo    CHECK (mounted_odometer IS NULL OR mounted_odometer BETWEEN 0 AND 9999999) NOT VALID,
    ADD CONSTRAINT ck_ta_dismounted_odo CHECK (dismounted_odometer IS NULL OR dismounted_odometer BETWEEN 0 AND 9999999) NOT VALID,
    ADD CONSTRAINT ck_ta_odo_order      CHECK (mounted_odometer IS NULL OR dismounted_odometer IS NULL
                                               OR dismounted_odometer >= mounted_odometer) NOT VALID,
    ADD CONSTRAINT ck_ta_dismount_pair  CHECK (dismounted_at IS NOT NULL OR
                                               (dismounted_odometer IS NULL AND dismount_reason IS NULL)) NOT VALID;
ALTER TABLE tires
    ADD CONSTRAINT ck_tires_km      CHECK (total_km BETWEEN 0 AND 9999999) NOT VALID,
    ADD CONSTRAINT ck_tires_retread CHECK (retread_count >= 0) NOT VALID,
    ADD CONSTRAINT ck_tires_brand   CHECK (btrim(brand) <> '') NOT VALID;
ALTER TABLE vehicle_odometer_overrides
    ADD CONSTRAINT ck_override_odometer CHECK (odometer BETWEEN 0 AND 9999999) NOT VALID;

-- Adding a task is planning; only ticking it says the work happened. The
-- TRUE default made every task created outside the desktop app (advisor,
-- direct API) reset the due clock on insert.
ALTER TABLE work_order_tasks ALTER COLUMN is_done SET DEFAULT FALSE;
-- When the task was actually ticked. The due clock reads this, not the day
-- the work order happened to be opened.
ALTER TABLE work_order_tasks ADD COLUMN done_at TIMESTAMPTZ;
UPDATE work_order_tasks t
   SET done_at = wo.opened_at
  FROM work_orders wo
 WHERE wo.id = t.work_order_id AND t.is_done AND t.done_at IS NULL;

-- ── stock ledger ────────────────────────────────────────────────────────────
-- Who caused the movement, and a terminal state for the Falcon mirror: a row
-- Falcon rejects (4xx) used to retry forever and, 100 of them later, block
-- every other movement from mirroring.
ALTER TABLE stock_movements
    ADD COLUMN user_id        BIGINT,
    ADD COLUMN mirror_attempts INT NOT NULL DEFAULT 0,
    ADD COLUMN mirror_error   TEXT;
DROP INDEX IF EXISTS idx_sm_unmirrored;
CREATE INDEX idx_sm_unmirrored ON stock_movements(created_at)
    WHERE mirrored_to_falcon_at IS NULL AND mirror_error IS NULL;
CREATE INDEX idx_sm_created ON stock_movements(created_at DESC, id DESC);

-- The cache keys were nullable inside a UNIQUE — two (brand, NULL, NULL) rows
-- could coexist and a debit matched neither.
UPDATE tire_stock_cache SET model = COALESCE(model, ''), size = COALESCE(size, '');
ALTER TABLE tire_stock_cache
    ALTER COLUMN model SET DEFAULT '', ALTER COLUMN model SET NOT NULL,
    ALTER COLUMN size  SET DEFAULT '', ALTER COLUMN size  SET NOT NULL;
