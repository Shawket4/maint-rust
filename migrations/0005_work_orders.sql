-- =========================================================
-- work_orders: the central object. Many can be open per vehicle.
-- =========================================================
CREATE TABLE work_orders (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    vehicle_id              INT NOT NULL,
    status                  work_order_status NOT NULL DEFAULT 'open',
    title                   TEXT,
    odometer_at_open        INT,
    opened_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    closed_at               TIMESTAMPTZ,
    notes_ar                TEXT,
    notes_en                TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1
);
CREATE INDEX idx_wo_vehicle ON work_orders(vehicle_id);
CREATE INDEX idx_wo_status  ON work_orders(status) WHERE deleted_at IS NULL;
CREATE INDEX idx_wo_updated ON work_orders(updated_at);

-- =========================================================
-- work_order_tasks: rows on a work order.
--   kind = free -> just a description
--   kind = tire -> mirrors a tire_assignment / tire_status_event
--   kind = oil  -> mirrors an oil_change
-- =========================================================
CREATE TABLE work_order_tasks (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    work_order_id           UUID NOT NULL REFERENCES work_orders(id) ON DELETE CASCADE,
    kind                    wo_task_kind NOT NULL DEFAULT 'free',
    category_id             TEXT REFERENCES maintenance_categories(id),
    description_ar          TEXT,
    description_en          TEXT,
    is_done                 BOOLEAN NOT NULL DEFAULT TRUE,
    sort_order              INT NOT NULL DEFAULT 0,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ,
    created_by_user_id      BIGINT NOT NULL,
    updated_by_user_id      BIGINT NOT NULL,
    sync_version            BIGINT NOT NULL DEFAULT 1
);
CREATE INDEX idx_wot_wo      ON work_order_tasks(work_order_id);
CREATE INDEX idx_wot_updated ON work_order_tasks(updated_at);
