-- =========================================================
-- maintenance_categories: lookup, bilingual at row level
-- =========================================================
CREATE TABLE maintenance_categories (
    id                      TEXT PRIMARY KEY,
    name_ar                 TEXT NOT NULL,
    name_en                 TEXT NOT NULL,
    icon                    TEXT,
    color                   TEXT,
    sort_order              SMALLINT NOT NULL DEFAULT 0
);

-- =========================================================
-- maintenance_templates: the rule
-- =========================================================
CREATE TYPE maintenance_trigger AS ENUM ('mileage', 'time');

CREATE TABLE maintenance_templates (
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
CREATE INDEX idx_templates_vehicle ON maintenance_templates(vehicle_id) WHERE deleted_at IS NULL;
CREATE INDEX idx_templates_updated ON maintenance_templates(updated_at);

-- =========================================================
-- maintenance_records: the event
-- =========================================================
CREATE TABLE maintenance_records (
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
CREATE INDEX idx_records_vehicle_date ON maintenance_records(vehicle_id, performed_at DESC);
CREATE INDEX idx_records_template     ON maintenance_records(template_id) WHERE deleted_at IS NULL;
CREATE INDEX idx_records_updated      ON maintenance_records(updated_at);

-- =========================================================
-- vehicle_odometer_overrides: manual override per vehicle
-- =========================================================
CREATE TABLE vehicle_odometer_overrides (
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
CREATE INDEX idx_overrides_updated ON vehicle_odometer_overrides(updated_at);
