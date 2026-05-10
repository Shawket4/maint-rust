-- =========================================================================
-- maint-rust / apex_maint — single-shot setup script
-- =========================================================================
--
-- Run against a FRESH `apex_maint` database (separate from Falcon's database
-- per ARCHITECTURE.md §2). Idempotent: every CREATE uses IF NOT EXISTS and
-- the seed inserts use ON CONFLICT DO NOTHING.
--
-- Usage:
--   createdb -U maint apex_maint
--   psql -U maint -d apex_maint -f setup.sql
--
-- This is exactly equivalent to running migrations 0001-0007 in order via
-- sqlx-cli, but in a form that can be executed with plain psql.
-- =========================================================================

\echo '== 0001 init_extensions =='
CREATE EXTENSION IF NOT EXISTS pgcrypto;

\echo '== 0002 cache_tables =='

CREATE TABLE IF NOT EXISTS vehicles_cache (
    id                              INT PRIMARY KEY,
    car_no_plate                    TEXT NOT NULL,
    car_type                        TEXT,
    transporter                     TEXT,
    tank_capacity                   INT,
    json_compartments               JSONB,
    license_expiration_date         DATE,
    calibration_expiration_date     DATE,
    tank_license_expiration_date    DATE,
    is_in_trip                      BOOLEAN NOT NULL DEFAULT FALSE,
    is_approved                     BOOLEAN NOT NULL DEFAULT FALSE,
    location                        TEXT,
    lat                             NUMERIC(10, 7),
    long                            NUMERIC(10, 7),
    location_time_stamp             TIMESTAMPTZ,
    engine_status                   TEXT,
    speed                           INT,
    last_fuel_odometer              INT,
    last_oil_change_id              INT,
    mileage                         INT,
    driver_id                       INT,
    operating_company               TEXT,
    operating_area                  TEXT,
    geo_fence                       TEXT,
    slack_status                    TEXT,
    last_updated_slack_status       TIMESTAMPTZ,
    etit_car_id                     TEXT,
    car_license_url                 TEXT,
    car_license_back_url            TEXT,
    calibration_license_url         TEXT,
    calibration_license_back_url    TEXT,
    tank_license_url                TEXT,
    tank_license_back_url           TEXT,
    raw_payload                     JSONB NOT NULL,
    source_created_at               TIMESTAMPTZ,
    source_updated_at               TIMESTAMPTZ,
    source_deleted_at               TIMESTAMPTZ,
    fetched_at                      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_vehicles_cache_plate  ON vehicles_cache(car_no_plate);
CREATE INDEX IF NOT EXISTS idx_vehicles_cache_active ON vehicles_cache(id) WHERE source_deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS drivers_cache (
    id                                  INT PRIMARY KEY,
    name                                TEXT NOT NULL,
    mobile_number                       TEXT,
    transporter                         TEXT,
    id_license_expiration_date          DATE,
    driver_license_expiration_date      DATE,
    safety_license_expiration_date      DATE,
    drug_test_expiration_date           DATE,
    is_approved                         BOOLEAN NOT NULL DEFAULT FALSE,
    raw_payload                         JSONB NOT NULL,
    source_created_at                   TIMESTAMPTZ,
    source_updated_at                   TIMESTAMPTZ,
    fetched_at                          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS service_invoices_cache (
    id                          INT PRIMARY KEY,
    car_id                      INT NOT NULL,
    driver_name                 TEXT,
    date                        TIMESTAMPTZ,
    meter_reading               INT,
    plate_number                TEXT,
    supervisor                  TEXT,
    operating_region            TEXT,
    raw_payload                 JSONB NOT NULL,
    source_created_at           TIMESTAMPTZ,
    source_updated_at           TIMESTAMPTZ,
    source_deleted_at           TIMESTAMPTZ,
    fetched_at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_invoices_cache_car   ON service_invoices_cache(car_id);
CREATE INDEX IF NOT EXISTS idx_invoices_cache_date  ON service_invoices_cache(date DESC);

CREATE TABLE IF NOT EXISTS service_invoice_items_cache (
    id                          INT PRIMARY KEY,
    service_invoice_id          INT NOT NULL,
    service                     TEXT NOT NULL,
    notes                       TEXT,
    item_order                  INT,
    raw_payload                 JSONB NOT NULL,
    source_created_at           TIMESTAMPTZ,
    source_updated_at           TIMESTAMPTZ,
    fetched_at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_invoice_items_invoice ON service_invoice_items_cache(service_invoice_id);

\echo '== 0003 maintenance_tables =='

CREATE TABLE IF NOT EXISTS maintenance_categories (
    id                      TEXT PRIMARY KEY,
    name_ar                 TEXT NOT NULL,
    name_en                 TEXT NOT NULL,
    icon                    TEXT,
    color                   TEXT,
    sort_order              SMALLINT NOT NULL DEFAULT 0
);

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'maintenance_trigger') THEN
        CREATE TYPE maintenance_trigger AS ENUM ('mileage', 'time');
    END IF;
END
$$;

CREATE TABLE IF NOT EXISTS maintenance_templates (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    vehicle_id              INT NOT NULL,
    category_id             TEXT NOT NULL REFERENCES maintenance_categories(id),
    name_ar                 TEXT NOT NULL,
    name_en                 TEXT NOT NULL,
    notes_ar                TEXT,
    notes_en                TEXT,
    trigger_type            maintenance_trigger NOT NULL,
    interval_km             INT,
    interval_days           INT,
    lead_warn_km            INT NOT NULL DEFAULT 500,
    lead_warn_days          INT NOT NULL DEFAULT 14,
    is_active               BOOLEAN NOT NULL DEFAULT TRUE,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1,
    CHECK (
        (trigger_type = 'mileage' AND interval_km  IS NOT NULL AND interval_km  > 0) OR
        (trigger_type = 'time'    AND interval_days IS NOT NULL AND interval_days > 0)
    )
);
CREATE INDEX IF NOT EXISTS idx_templates_vehicle ON maintenance_templates(vehicle_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_templates_updated ON maintenance_templates(updated_at);

CREATE TABLE IF NOT EXISTS maintenance_records (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_id             UUID REFERENCES maintenance_templates(id),
    vehicle_id              INT NOT NULL,
    category_id             TEXT NOT NULL REFERENCES maintenance_categories(id),
    performed_at            TIMESTAMPTZ NOT NULL,
    odometer_at_service     INT,
    next_due_at             TIMESTAMPTZ,
    next_due_km             INT,
    cost                    NUMERIC(12, 2),
    vendor                  TEXT,
    performed_by            TEXT,
    notes                   TEXT,
    attachments             JSONB NOT NULL DEFAULT '[]',
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS idx_records_vehicle_date ON maintenance_records(vehicle_id, performed_at DESC);
CREATE INDEX IF NOT EXISTS idx_records_template     ON maintenance_records(template_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_records_updated      ON maintenance_records(updated_at);

CREATE TABLE IF NOT EXISTS vehicle_odometer_overrides (
    vehicle_id              INT PRIMARY KEY,
    odometer                INT NOT NULL CHECK (odometer >= 0),
    set_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    notes                   TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS idx_overrides_updated ON vehicle_odometer_overrides(updated_at);

\echo '== 0004 chassis_tables =='

CREATE TABLE IF NOT EXISTS chassis_layouts (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    vehicle_id              INT NOT NULL UNIQUE,
    name_ar                 TEXT,
    name_en                 TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS idx_layouts_updated ON chassis_layouts(updated_at);

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'chassis_section') THEN
        CREATE TYPE chassis_section AS ENUM ('tractor', 'trailer');
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'axle_type') THEN
        CREATE TYPE axle_type AS ENUM ('single', 'dual');
    END IF;
END
$$;

CREATE TABLE IF NOT EXISTS chassis_axles (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    layout_id               UUID NOT NULL REFERENCES chassis_layouts(id) ON DELETE CASCADE,
    section                 chassis_section NOT NULL,
    section_index           SMALLINT NOT NULL,
    axle_type               axle_type NOT NULL,
    label_ar                TEXT,
    label_en                TEXT,
    is_steering             BOOLEAN NOT NULL DEFAULT FALSE,
    is_lifted               BOOLEAN NOT NULL DEFAULT FALSE,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1,
    UNIQUE (layout_id, section, section_index)
);
CREATE INDEX IF NOT EXISTS idx_axles_updated ON chassis_axles(updated_at);

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'position_side') THEN
        CREATE TYPE position_side AS ENUM ('left', 'right');
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'position_depth') THEN
        CREATE TYPE position_depth AS ENUM ('single', 'inner', 'outer');
    END IF;
END
$$;

CREATE TABLE IF NOT EXISTS chassis_positions (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    layout_id               UUID NOT NULL REFERENCES chassis_layouts(id) ON DELETE CASCADE,
    axle_id                 UUID REFERENCES chassis_axles(id) ON DELETE CASCADE,
    side                    position_side,
    depth                   position_depth,
    is_spare                BOOLEAN NOT NULL DEFAULT FALSE,
    spare_index             SMALLINT,
    position_code           TEXT NOT NULL,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1,
    UNIQUE (layout_id, position_code),
    CHECK (
        (is_spare = FALSE AND axle_id IS NOT NULL AND side IS NOT NULL AND depth IS NOT NULL)
     OR (is_spare = TRUE  AND axle_id IS NULL     AND spare_index IS NOT NULL)
    )
);
CREATE INDEX IF NOT EXISTS idx_positions_layout  ON chassis_positions(layout_id);
CREATE INDEX IF NOT EXISTS idx_positions_axle    ON chassis_positions(axle_id);
CREATE INDEX IF NOT EXISTS idx_positions_updated ON chassis_positions(updated_at);

\echo '== 0005 tire_tables =='

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'tire_status') THEN
        CREATE TYPE tire_status AS ENUM
            ('in_stock', 'mounted', 'in_repair', 'retreading', 'scrapped');
    END IF;
END
$$;

CREATE TABLE IF NOT EXISTS tires (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    dot_code                TEXT NOT NULL UNIQUE,
    internal_serial         TEXT,
    brand                   TEXT NOT NULL,
    model                   TEXT,
    purchase_date           DATE,
    purchase_cost           NUMERIC(12, 2),
    supplier                TEXT,
    production_week         SMALLINT CHECK (production_week  BETWEEN 1 AND 53),
    production_year         SMALLINT CHECK (production_year  BETWEEN 2000 AND 2099),
    production_date         DATE,
    is_retread              BOOLEAN NOT NULL DEFAULT FALSE,
    retread_count           SMALLINT NOT NULL DEFAULT 0,
    parent_tire_id          UUID REFERENCES tires(id),
    status                  tire_status NOT NULL DEFAULT 'in_stock',
    stock_location          TEXT,
    scrap_reason            TEXT,
    scrap_date              DATE,
    notes                   TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1,
    CHECK ((production_week IS NULL) = (production_year IS NULL))
);
CREATE INDEX IF NOT EXISTS idx_tires_dot              ON tires(dot_code);
CREATE INDEX IF NOT EXISTS idx_tires_status_brand     ON tires(status, brand) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_tires_stock_location   ON tires(stock_location)
    WHERE deleted_at IS NULL AND status = 'in_stock';
CREATE INDEX IF NOT EXISTS idx_tires_production       ON tires(production_year, production_week);
CREATE INDEX IF NOT EXISTS idx_tires_updated          ON tires(updated_at);

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'mount_reason') THEN
        CREATE TYPE mount_reason AS ENUM (
            'new_install', 'rotation', 'replacement',
            'puncture', 'wear', 'damage', 'retread_return'
        );
    END IF;
END
$$;

CREATE TABLE IF NOT EXISTS tire_assignments (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tire_id                 UUID NOT NULL REFERENCES tires(id),
    position_id             UUID NOT NULL REFERENCES chassis_positions(id),
    vehicle_id              INT NOT NULL,
    mounted_at              TIMESTAMPTZ NOT NULL,
    mounted_odometer        INT,
    dismounted_at           TIMESTAMPTZ,
    dismounted_odometer     INT,
    mount_reason            mount_reason NOT NULL,
    dismount_reason         mount_reason,
    notes                   TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1,
    CHECK (dismounted_at IS NULL OR dismounted_at >= mounted_at),
    CHECK (
        dismounted_odometer IS NULL OR mounted_odometer IS NULL
        OR dismounted_odometer >= mounted_odometer
    )
);
CREATE INDEX IF NOT EXISTS idx_assignments_tire     ON tire_assignments(tire_id);
CREATE INDEX IF NOT EXISTS idx_assignments_position ON tire_assignments(position_id);
CREATE INDEX IF NOT EXISTS idx_assignments_vehicle  ON tire_assignments(vehicle_id);
CREATE INDEX IF NOT EXISTS idx_assignments_updated  ON tire_assignments(updated_at);

CREATE UNIQUE INDEX IF NOT EXISTS idx_assignments_one_active_per_position
    ON tire_assignments(position_id) WHERE dismounted_at IS NULL AND deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_assignments_one_active_per_tire
    ON tire_assignments(tire_id) WHERE dismounted_at IS NULL AND deleted_at IS NULL;

\echo '== 0006 views =='

CREATE OR REPLACE VIEW v_maintenance_due AS
SELECT
    t.id              AS template_id,
    t.vehicle_id,
    t.category_id,
    t.name_ar, t.name_en,
    t.trigger_type,
    t.interval_km, t.interval_days,
    t.lead_warn_km, t.lead_warn_days,
    r.performed_at        AS last_done_at,
    r.odometer_at_service AS last_done_km,
    r.next_due_at,
    r.next_due_km,
    GREATEST(
        COALESCE(vc.mileage, 0),
        COALESCE(vc.last_fuel_odometer, 0),
        COALESCE(voo.odometer, 0)
    ) AS current_odometer,
    CASE
        WHEN COALESCE(voo.odometer, 0) > GREATEST(COALESCE(vc.mileage, 0), COALESCE(vc.last_fuel_odometer, 0))
            THEN 'manual'
        ELSE 'falcon'
    END AS odometer_source,
    CASE
      WHEN r.id IS NULL THEN 'never_done'
      WHEN t.trigger_type = 'mileage' AND r.next_due_km IS NOT NULL THEN
        CASE
          WHEN GREATEST(COALESCE(vc.mileage, 0), COALESCE(vc.last_fuel_odometer, 0), COALESCE(voo.odometer, 0)) >= r.next_due_km
            THEN 'overdue'
          WHEN GREATEST(COALESCE(vc.mileage, 0), COALESCE(vc.last_fuel_odometer, 0), COALESCE(voo.odometer, 0)) >= r.next_due_km - t.lead_warn_km
            THEN 'due_soon'
          ELSE 'ok'
        END
      WHEN t.trigger_type = 'time' AND r.next_due_at IS NOT NULL THEN
        CASE
          WHEN now() >= r.next_due_at THEN 'overdue'
          WHEN now() >= r.next_due_at - (t.lead_warn_days || ' days')::INTERVAL THEN 'due_soon'
          ELSE 'ok'
        END
      ELSE 'never_done'
    END AS status
FROM maintenance_templates t
LEFT JOIN LATERAL (
    SELECT * FROM maintenance_records
    WHERE template_id = t.id AND deleted_at IS NULL
    ORDER BY performed_at DESC LIMIT 1
) r ON TRUE
JOIN vehicles_cache vc                    ON vc.id = t.vehicle_id
LEFT JOIN vehicle_odometer_overrides voo  ON voo.vehicle_id = t.vehicle_id
WHERE t.deleted_at IS NULL
  AND t.is_active
  AND vc.source_deleted_at IS NULL;

CREATE OR REPLACE VIEW v_tire_lifetime AS
WITH per_assignment_km AS (
    SELECT
        ta.id,
        ta.tire_id,
        ta.vehicle_id,
        ta.mounted_at,
        CASE
          WHEN ta.dismounted_odometer IS NOT NULL AND ta.mounted_odometer IS NOT NULL
            THEN ta.dismounted_odometer - ta.mounted_odometer
          WHEN ta.dismounted_at IS NULL AND ta.mounted_odometer IS NOT NULL
            THEN GREATEST(
                   COALESCE(vc.last_fuel_odometer, ta.mounted_odometer),
                   COALESCE(voo.odometer,         ta.mounted_odometer)
                 ) - ta.mounted_odometer
          ELSE 0
        END AS km_on_position
    FROM tire_assignments ta
    LEFT JOIN vehicles_cache vc               ON vc.id  = ta.vehicle_id
    LEFT JOIN vehicle_odometer_overrides voo  ON voo.vehicle_id = ta.vehicle_id
    WHERE ta.deleted_at IS NULL
)
SELECT
    t.id                                    AS tire_id,
    t.dot_code,
    t.brand,
    t.status,
    t.purchase_cost,
    COALESCE(SUM(pa.km_on_position), 0)::INT AS lifetime_km,
    COUNT(pa.id)::INT                        AS installment_count,
    MIN(pa.mounted_at)                       AS first_mounted_at,
    CASE
      WHEN t.purchase_cost IS NOT NULL AND COALESCE(SUM(pa.km_on_position), 0) > 0
        THEN ROUND(t.purchase_cost / NULLIF(SUM(pa.km_on_position), 0), 4)
      ELSE NULL
    END                                      AS cost_per_km
FROM tires t
LEFT JOIN per_assignment_km pa ON pa.tire_id = t.id
WHERE t.deleted_at IS NULL
GROUP BY t.id, t.dot_code, t.brand, t.status, t.purchase_cost;

CREATE OR REPLACE VIEW v_tires_list AS
SELECT
    t.id,
    t.dot_code,
    t.internal_serial,
    t.brand,
    t.model,
    t.status,
    t.production_week,
    t.production_year,
    t.production_date,
    t.purchase_date,
    t.purchase_cost,
    t.stock_location,
    t.is_retread,
    t.retread_count,
    CASE
      WHEN t.status = 'mounted' THEN
        (SELECT vc.car_no_plate || ' / ' || cp.position_code
           FROM tire_assignments ta
           JOIN chassis_positions cp ON cp.id = ta.position_id
           JOIN vehicles_cache vc    ON vc.id = ta.vehicle_id
          WHERE ta.tire_id = t.id AND ta.dismounted_at IS NULL AND ta.deleted_at IS NULL
          LIMIT 1)
      WHEN t.status = 'in_stock' THEN COALESCE(t.stock_location, 'Unassigned')
      ELSE t.status::TEXT
    END AS current_location_label,
    EXISTS (
        SELECT 1 FROM tire_assignments ta
        WHERE ta.tire_id = t.id AND ta.deleted_at IS NULL
    ) AS has_history,
    tl.lifetime_km,
    tl.installment_count,
    tl.cost_per_km,
    t.created_at,
    t.updated_at
FROM tires t
LEFT JOIN v_tire_lifetime tl ON tl.tire_id = t.id
WHERE t.deleted_at IS NULL;

\echo '== 0007 seeds =='

INSERT INTO maintenance_categories (id, name_ar, name_en, icon, color, sort_order) VALUES
  ('oil_change',   'تغيير زيت',  'Oil Change',          'droplet',     'amber',  10),
  ('air_filter',   'فلتر هواء',  'Air Filter',          'wind',        'sky',    20),
  ('fuel_filter',  'فلتر سولار', 'Fuel Filter',         'fuel',        'orange', 30),
  ('oil_filter',   'فلتر زيت',   'Oil Filter',          'filter',      'amber',  40),
  ('brakes',       'فرامل',     'Brakes',              'octagon',     'red',    50),
  ('coolant',      'مياه تبريد', 'Coolant',             'thermometer', 'cyan',   60),
  ('transmission', 'زيت فتيس',   'Transmission Oil',    'cog',         'violet', 70),
  ('battery',      'بطارية',    'Battery',             'battery',     'green',  80),
  ('inspection',   'فحص دوري',  'Periodic Inspection', 'clipboard',   'slate',  90),
  ('greasing',     'تشحيم',     'Greasing',            'wrench',      'zinc',   100)
ON CONFLICT (id) DO NOTHING;

\echo '== done =='
