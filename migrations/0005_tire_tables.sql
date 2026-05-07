-- =========================================================
-- tires: first-class assets, lifecycle tracked
-- =========================================================
CREATE TYPE tire_status AS ENUM
    ('in_stock', 'mounted', 'in_repair', 'retreading', 'scrapped');

CREATE TABLE tires (
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
CREATE INDEX idx_tires_dot              ON tires(dot_code);
CREATE INDEX idx_tires_status_brand     ON tires(status, brand) WHERE deleted_at IS NULL;
CREATE INDEX idx_tires_stock_location   ON tires(stock_location)
    WHERE deleted_at IS NULL AND status = 'in_stock';
CREATE INDEX idx_tires_production       ON tires(production_year, production_week);
CREATE INDEX idx_tires_updated          ON tires(updated_at);

-- =========================================================
-- tire_assignments: every mount/dismount event
-- =========================================================
CREATE TYPE mount_reason AS ENUM (
    'new_install', 'rotation', 'replacement',
    'puncture', 'wear', 'damage', 'retread_return'
);

CREATE TABLE tire_assignments (
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
CREATE INDEX idx_assignments_tire     ON tire_assignments(tire_id);
CREATE INDEX idx_assignments_position ON tire_assignments(position_id);
CREATE INDEX idx_assignments_vehicle  ON tire_assignments(vehicle_id);
CREATE INDEX idx_assignments_updated  ON tire_assignments(updated_at);

-- A position holds at most one tire at a time
CREATE UNIQUE INDEX idx_assignments_one_active_per_position
    ON tire_assignments(position_id) WHERE dismounted_at IS NULL AND deleted_at IS NULL;
-- A tire is mounted in at most one place
CREATE UNIQUE INDEX idx_assignments_one_active_per_tire
    ON tire_assignments(tire_id) WHERE dismounted_at IS NULL AND deleted_at IS NULL;
