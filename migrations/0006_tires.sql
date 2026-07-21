-- =========================================================
-- tires: individual, tracked tire assets.
-- An asset is born the moment identity is captured: first mount from
-- fungible stock, or legacy intake. Fungible NEW stock is NOT here
-- (it lives as a count in Falcon, mirrored in tire_stock_cache).
-- =========================================================
CREATE TABLE tires (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    dot_code                TEXT,                       -- nullable: may be unknown for legacy
    brand                   TEXT NOT NULL,
    model                   TEXT,
    size                    TEXT,
    status                  tire_status NOT NULL DEFAULT 'in_stock_used',
    origin                  tire_origin NOT NULL,
    total_km                INT NOT NULL DEFAULT 0,      -- accumulated across all mounts
    parent_tire_id          UUID REFERENCES tires(id),  -- retread lineage
    retread_count           SMALLINT NOT NULL DEFAULT 0,
    notes_ar                TEXT,
    notes_en                TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1
);
CREATE UNIQUE INDEX idx_tires_dot ON tires(dot_code) WHERE dot_code IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX idx_tires_status  ON tires(status) WHERE deleted_at IS NULL;
CREATE INDEX idx_tires_updated ON tires(updated_at);

-- =========================================================
-- tire_assignments: the mount ledger + current-mount invariant.
-- An open row (dismounted_at IS NULL) = the tire is currently mounted.
-- Two partial unique indexes enforce: one active mount per tire, one per position.
-- =========================================================
CREATE TABLE tire_assignments (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tire_id                 UUID NOT NULL REFERENCES tires(id),
    position_id             UUID NOT NULL REFERENCES chassis_positions(id),
    vehicle_id              INT NOT NULL,
    work_order_id           UUID REFERENCES work_orders(id) ON DELETE SET NULL,
    mounted_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    mounted_odometer        INT,
    mount_reason            mount_reason NOT NULL DEFAULT 'new',
    dismounted_at           TIMESTAMPTZ,
    dismounted_odometer     INT,
    dismount_reason         dismount_reason,
    notes                   TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1
);
CREATE UNIQUE INDEX uq_active_tire     ON tire_assignments(tire_id)     WHERE dismounted_at IS NULL AND deleted_at IS NULL;
CREATE UNIQUE INDEX uq_active_position ON tire_assignments(position_id) WHERE dismounted_at IS NULL AND deleted_at IS NULL;
CREATE INDEX idx_ta_tire     ON tire_assignments(tire_id);
CREATE INDEX idx_ta_vehicle  ON tire_assignments(vehicle_id);
CREATE INDEX idx_ta_wo       ON tire_assignments(work_order_id);
CREATE INDEX idx_ta_updated  ON tire_assignments(updated_at);

-- =========================================================
-- tire_status_events: non-mount lifecycle (scrap / retread / repair / restock / intake).
-- Mount/dismount history lives in tire_assignments; this is the rest of the story.
-- =========================================================
CREATE TABLE tire_status_events (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tire_id                 UUID NOT NULL REFERENCES tires(id),
    event_type              tire_event_type NOT NULL,
    work_order_id           UUID REFERENCES work_orders(id) ON DELETE SET NULL,
    from_status             tire_status,
    to_status               tire_status,
    notes                   TEXT,
    occurred_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1
);
CREATE INDEX idx_tse_tire    ON tire_status_events(tire_id);
CREATE INDEX idx_tse_updated ON tire_status_events(updated_at);
