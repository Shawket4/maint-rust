-- =========================================================
-- vehicle_classes: maint-owned maintenance classification (NOT Falcon's car_type).
-- =========================================================
CREATE TABLE vehicle_classes (
    id          TEXT PRIMARY KEY,       -- e.g. 'tractor', 'tanker_trailer'
    name_ar     TEXT NOT NULL,
    name_en     TEXT NOT NULL,
    sort_order  INT NOT NULL DEFAULT 0
);

-- Maps each vehicle to a class. Syncable (maint owns classification).
CREATE TABLE vehicle_class_assignments (
    vehicle_id              INT PRIMARY KEY,
    class_id                TEXT NOT NULL REFERENCES vehicle_classes(id),
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1
);
CREATE INDEX idx_vca_updated ON vehicle_class_assignments(updated_at);

-- =========================================================
-- service_plans: fleet-wide preventive rule, per class + category.
-- trigger is mileage XOR time (CHECK enforced).
-- =========================================================
CREATE TABLE service_plans (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    class_id                TEXT NOT NULL REFERENCES vehicle_classes(id),
    category_id             TEXT NOT NULL REFERENCES maintenance_categories(id),
    trigger_type            plan_trigger NOT NULL,
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
        (trigger_type = 'mileage' AND interval_km   IS NOT NULL AND interval_days IS NULL)
     OR (trigger_type = 'time'    AND interval_days IS NOT NULL AND interval_km   IS NULL)
    ),
    UNIQUE (class_id, category_id)
);
CREATE INDEX idx_plans_class   ON service_plans(class_id);
CREATE INDEX idx_plans_updated ON service_plans(updated_at);

-- =========================================================
-- vehicle_odometer_overrides: manual reading that beats Falcon. Hard-delete (no deleted_at).
-- =========================================================
CREATE TABLE vehicle_odometer_overrides (
    vehicle_id              INT PRIMARY KEY,
    odometer                INT NOT NULL,
    set_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    notes                   TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1
);
CREATE INDEX idx_odo_updated ON vehicle_odometer_overrides(updated_at);
