-- =========================================================
-- chassis_layouts: one per vehicle
-- =========================================================
CREATE TABLE chassis_layouts (
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
CREATE INDEX idx_layouts_updated ON chassis_layouts(updated_at);

CREATE TABLE chassis_axles (
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
CREATE INDEX idx_axles_updated ON chassis_axles(updated_at);

CREATE TABLE chassis_positions (
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
CREATE INDEX idx_positions_layout  ON chassis_positions(layout_id);
CREATE INDEX idx_positions_axle    ON chassis_positions(axle_id);
CREATE INDEX idx_positions_updated ON chassis_positions(updated_at);
