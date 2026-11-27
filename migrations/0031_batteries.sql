-- Batteries (2026-11-27), mirroring tires:
--
--   * on the shelf they are counted per product (brand, model, Ah, volts) in
--     maint-rust alone: stock_movements of kind battery_credit/battery_debit,
--     summed by v_battery_stock (no Falcon ledger);
--   * a battery gets its own record (serial, date code, warranty) when it is
--     fitted from stock (one unit off the count) or registered as already on
--     a truck;
--   * a truck has numbered battery slots (battery_layouts, default 2);
--     battery_installs is the ledger of which battery sat in which slot.

CREATE TABLE battery_products (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    brand               TEXT NOT NULL CHECK (btrim(brand) <> ''),
    model               TEXT NOT NULL DEFAULT '',
    capacity_ah         INT CHECK (capacity_ah BETWEEN 1 AND 2000),
    voltage             SMALLINT CHECK (voltage BETWEEN 2 AND 48),
    -- Cold cranking amps, when the garage records it (learned, not a key).
    cca                 INT CHECK (cca BETWEEN 1 AND 5000),
    norm_key            TEXT NOT NULL UNIQUE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by_user_id  BIGINT
);

-- The same battery typed two ways is one product: brand and model compared as
-- maint_norm compares names, Ah and volts as numbers.
CREATE OR REPLACE FUNCTION maint_battery_product(p_brand TEXT, p_model TEXT, p_ah INT, p_v INT, p_cca INT, p_user BIGINT)
RETURNS UUID LANGUAGE plpgsql AS $$
DECLARE
    k TEXT := maint_norm(p_brand) || '|' || maint_norm(p_model) || '|' || COALESCE(p_ah::text, '') || '|' || COALESCE(p_v::text, '');
    found UUID;
BEGIN
    IF maint_norm(p_brand) = '' THEN RETURN NULL; END IF;
    INSERT INTO battery_products (brand, model, capacity_ah, voltage, cca, norm_key, created_by_user_id)
    VALUES (btrim(p_brand), COALESCE(btrim(p_model), ''), p_ah, p_v, p_cca, k, p_user)
    ON CONFLICT (norm_key) DO UPDATE
       SET cca = COALESCE(battery_products.cca, EXCLUDED.cca),
           updated_at = CASE WHEN battery_products.cca IS NULL AND EXCLUDED.cca IS NOT NULL
                             THEN now() ELSE battery_products.updated_at END
    RETURNING id INTO found;
    RETURN found;
END $$;

-- How many battery slots a truck has. One row per vehicle (an insert upserts,
-- like vehicle_class_assignments); no row = 2.
CREATE TABLE battery_layouts (
    vehicle_id              INT PRIMARY KEY,
    slots                   SMALLINT NOT NULL DEFAULT 2 CHECK (slots BETWEEN 1 AND 12),
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1
);
CREATE INDEX idx_battery_layouts_feed ON battery_layouts(updated_at, vehicle_id);

CREATE TABLE batteries (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    battery_product_id      UUID REFERENCES battery_products(id),
    brand                   TEXT NOT NULL CHECK (btrim(brand) <> ''),
    model                   TEXT,
    capacity_ah             INT CHECK (capacity_ah BETWEEN 1 AND 2000),
    voltage                 SMALLINT CHECK (voltage BETWEEN 2 AND 48),
    cca                     INT CHECK (cca BETWEEN 1 AND 5000),
    serial                  TEXT,
    date_code               TEXT,
    warranty_months         SMALLINT CHECK (warranty_months BETWEEN 0 AND 120),
    status                  battery_status NOT NULL DEFAULT 'in_stock_used',
    origin                  battery_origin NOT NULL,
    notes_ar                TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1
);
CREATE INDEX idx_batteries_feed ON batteries(updated_at, id);
CREATE INDEX idx_batteries_product ON batteries(battery_product_id);

CREATE TABLE battery_installs (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    battery_id              UUID NOT NULL REFERENCES batteries(id),
    vehicle_id              INT NOT NULL,
    slot                    SMALLINT NOT NULL CHECK (slot BETWEEN 1 AND 12),
    work_order_id           UUID REFERENCES work_orders(id) ON DELETE SET NULL,
    installed_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    installed_odometer      INT,
    removed_at              TIMESTAMPTZ,
    removed_odometer        INT,
    removal_reason          battery_removal_reason,
    -- Where the battery went: back to stock as used, charging, a warranty
    -- claim, or scrap.
    removal_outcome         battery_status CHECK (removal_outcome IS DISTINCT FROM 'installed'),
    removal_work_order_id   UUID REFERENCES work_orders(id) ON DELETE SET NULL,
    notes                   TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1
);
CREATE UNIQUE INDEX uq_active_battery      ON battery_installs(battery_id)      WHERE removed_at IS NULL AND deleted_at IS NULL;
CREATE UNIQUE INDEX uq_active_battery_slot ON battery_installs(vehicle_id, slot) WHERE removed_at IS NULL AND deleted_at IS NULL;
CREATE INDEX idx_battery_installs_battery ON battery_installs(battery_id);
CREATE INDEX idx_battery_installs_vehicle ON battery_installs(vehicle_id);
CREATE INDEX idx_battery_installs_feed ON battery_installs(updated_at, id);

-- A movement of battery stock names its product (the ledger sets it).
ALTER TABLE stock_movements ADD COLUMN battery_product_id UUID REFERENCES battery_products(id);
CREATE INDEX idx_stock_movements_battery ON stock_movements(battery_product_id) WHERE battery_product_id IS NOT NULL;

-- A battery's text is the wire truth; its product follows (as tires do).
CREATE OR REPLACE FUNCTION maint_link_battery_product() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE p battery_products%ROWTYPE;
BEGIN
    NEW.battery_product_id := maint_battery_product(NEW.brand, NEW.model, NEW.capacity_ah, NEW.voltage, NEW.cca, NEW.updated_by_user_id);
    SELECT * INTO p FROM battery_products WHERE id = NEW.battery_product_id;
    IF FOUND THEN
        NEW.brand := p.brand;
        NEW.model := NULLIF(p.model, '');
    END IF;
    RETURN NEW;
END $$;
-- Named to sort before trg_batteries_stamp.
CREATE TRIGGER trg_batteries_link BEFORE INSERT OR UPDATE OF brand, model, capacity_ah, voltage, cca ON batteries
    FOR EACH ROW EXECUTE FUNCTION maint_link_battery_product();

-- New batteries on the shelf, per product: what came in minus what went out.
CREATE VIEW v_battery_stock AS
SELECT p.id, p.brand, p.model, p.capacity_ah, p.voltage, p.cca,
       COALESCE(SUM(CASE m.kind WHEN 'battery_credit' THEN m.qty WHEN 'battery_debit' THEN -m.qty ELSE 0 END), 0)::int AS on_hand_qty,
       (SELECT COUNT(*) FROM batteries b WHERE b.battery_product_id = p.id AND b.deleted_at IS NULL)::int AS batteries
  FROM battery_products p
  LEFT JOIN stock_movements m ON m.battery_product_id = p.id
 GROUP BY p.id;

-- Commit-ordered updated_at (0021), the users_cache placeholder (0026), and
-- the vehicles_cache placeholder + reference (0024) on the synced tables.
DO $$
DECLARE t TEXT;
BEGIN
    FOREACH t IN ARRAY ARRAY['battery_layouts', 'batteries', 'battery_installs'] LOOP
        EXECUTE format(
            'CREATE TRIGGER trg_%1$s_stamp BEFORE INSERT OR UPDATE ON %1$I
               FOR EACH ROW EXECUTE FUNCTION maint_stamp_row()', t);
        EXECUTE format(
            'CREATE TRIGGER trg_%1$s_a_user BEFORE INSERT OR UPDATE OF created_by_user_id, updated_by_user_id ON %1$I
               FOR EACH ROW EXECUTE FUNCTION maint_ensure_user()', t);
    END LOOP;
    FOREACH t IN ARRAY ARRAY['battery_layouts', 'battery_installs'] LOOP
        EXECUTE format(
            'CREATE TRIGGER trg_%1$s_a_vehicle BEFORE INSERT OR UPDATE OF vehicle_id ON %1$I
               FOR EACH ROW EXECUTE FUNCTION maint_ensure_vehicle()', t);
        EXECUTE format(
            'ALTER TABLE %1$I ADD CONSTRAINT fk_%1$s_vehicle FOREIGN KEY (vehicle_id) REFERENCES vehicles_cache(id)', t);
    END LOOP;
END $$;
