-- Work done before work orders existed counts toward the plans.
--
-- Until now maintenance history lived in Falcon's service invoices as free
-- text lines ("تغيير زيت موتور"). Work orders replace invoices from here on,
-- but a truck whose oil was changed last month must not show "never done".
--
-- legacy_service_events: one row per old invoice line that was recognised as
-- a maintenance type, imported from Falcon (read-only pool) by the importer
-- (services/legacy_import.rs). Recognition:
--   1. legacy_service_map — an exact decision for that text (a type, or
--      "ignore"), made by a person in the review screen; else
--   2. category_keywords — the text contains a keyword of a type.
-- Lines matching neither are left out and listed for review.

CREATE TABLE category_keywords (
    category_id TEXT NOT NULL REFERENCES maintenance_categories(id),
    keyword     TEXT NOT NULL,
    norm        TEXT NOT NULL,
    PRIMARY KEY (category_id, norm)
);

CREATE TABLE legacy_service_map (
    norm_text   TEXT PRIMARY KEY,
    sample_text TEXT NOT NULL,
    -- NULL = a person looked at it and it is not a maintenance type.
    category_id TEXT REFERENCES maintenance_categories(id),
    decided_by_user_id BIGINT,
    decided_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE legacy_service_lines (
    falcon_item_id    BIGINT PRIMARY KEY,
    falcon_invoice_id BIGINT NOT NULL,
    vehicle_id        INT NOT NULL REFERENCES vehicles_cache(id),
    service           TEXT NOT NULL,
    norm_text         TEXT NOT NULL,
    done_at           TIMESTAMPTZ NOT NULL,
    km                INT,
    -- Resolved type (NULL = not recognised yet, or ignored).
    category_id       TEXT REFERENCES maintenance_categories(id),
    imported_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_legacy_lines_vehicle ON legacy_service_lines(vehicle_id, category_id, done_at DESC);
CREATE INDEX idx_legacy_lines_norm    ON legacy_service_lines(norm_text) WHERE category_id IS NULL;

CREATE TRIGGER trg_legacy_lines_a_vehicle BEFORE INSERT OR UPDATE OF vehicle_id ON legacy_service_lines
    FOR EACH ROW EXECUTE FUNCTION maint_ensure_vehicle();

-- Which type a line of text is, by the two rules above.
CREATE OR REPLACE FUNCTION maint_legacy_category(p_norm TEXT) RETURNS TEXT
LANGUAGE sql STABLE AS $$
    SELECT CASE
        WHEN EXISTS (SELECT 1 FROM legacy_service_map m WHERE m.norm_text = p_norm)
            THEN (SELECT m.category_id FROM legacy_service_map m WHERE m.norm_text = p_norm)
        ELSE (SELECT k.category_id FROM category_keywords k
               WHERE position(k.norm IN p_norm) > 0
               ORDER BY length(k.norm) DESC, k.category_id LIMIT 1)
    END
$$;

-- Starting keywords, Egyptian garage wording. Longest match wins, so
-- "فلتر زيت" beats "زيت".
INSERT INTO category_keywords (category_id, keyword, norm)
SELECT c, k, maint_norm(k) FROM (VALUES
    ('oil_change',   'تغيير زيت موتور'), ('oil_change', 'زيت موتور'), ('oil_change', 'غيار زيت'), ('oil_change', 'تغيير زيت'),
    ('oil_filter',   'فلتر زيت'), ('oil_filter', 'فلتر الزيت'),
    ('air_filter',   'فلتر هواء'), ('air_filter', 'فلتر هوا'), ('air_filter', 'فلتر الهواء'),
    ('fuel_filter',  'فلتر سولار'), ('fuel_filter', 'فلتر جاز'), ('fuel_filter', 'فلتر وقود'),
    ('brakes',       'فرامل'), ('brakes', 'تيل'), ('brakes', 'فحمات'),
    ('coolant',      'مياه تبريد'), ('coolant', 'ميه تبريد'), ('coolant', 'ردياتير'),
    ('transmission', 'زيت فتيس'), ('transmission', 'زيت الفتيس'), ('transmission', 'زيت كرونة'),
    ('battery',      'بطارية'), ('battery', 'بطاريه'),
    ('greasing',     'تشحيم'),
    ('inspection',   'فحص دوري'),
    ('tires',        'كاوتش'), ('tires', 'اطار'), ('tires', 'إطار')
) AS v(c, k)
WHERE EXISTS (SELECT 1 FROM maintenance_categories mc WHERE mc.id = v.c)
ON CONFLICT DO NOTHING;

-- The due clock counts recognised old lines next to ticked tasks.
DROP VIEW IF EXISTS v_maintenance_due;
CREATE VIEW v_maintenance_due AS
WITH done AS (
    SELECT wo.vehicle_id, t.category_id, t.id::text AS task_id,
           COALESCE(t.done_at, wo.opened_at) AS done_at,
           wo.odometer_at_open               AS km
      FROM work_order_tasks t
      JOIN work_orders wo ON wo.id = t.work_order_id
     WHERE t.deleted_at IS NULL AND wo.deleted_at IS NULL
       AND t.category_id IS NOT NULL AND t.is_done
    UNION ALL
    SELECT l.vehicle_id, l.category_id, 'legacy-' || l.falcon_item_id, l.done_at, l.km
      FROM legacy_service_lines l
     WHERE l.category_id IS NOT NULL
),
last_at AS (
    SELECT DISTINCT ON (vehicle_id, category_id) vehicle_id, category_id, done_at
      FROM done
     ORDER BY vehicle_id, category_id, done_at DESC, task_id
),
last_km AS (
    SELECT DISTINCT ON (vehicle_id, category_id) vehicle_id, category_id, km
      FROM done WHERE km IS NOT NULL
     ORDER BY vehicle_id, category_id, done_at DESC, task_id
),
base AS (
    SELECT
        vca.vehicle_id,
        sp.id AS plan_id, sp.category_id,
        c.name_ar AS category_name_ar, c.name_en AS category_name_en,
        sp.trigger_type, sp.interval_km, sp.interval_days,
        sp.lead_warn_km, sp.lead_warn_days,
        la.done_at AS last_at, lk.km AS last_km,
        odo.current_odometer, odo.odometer_source, odo.odometer_observed_at,
        lk.km + sp.interval_km AS next_due_km,
        la.done_at + make_interval(days => sp.interval_days) AS next_due_at
    FROM vehicle_class_assignments vca
    JOIN vehicles_cache v ON v.id = vca.vehicle_id AND v.source_deleted_at IS NULL
    JOIN service_plans sp ON sp.class_id = vca.class_id AND sp.is_active AND sp.deleted_at IS NULL
    JOIN maintenance_categories c ON c.id = sp.category_id
    LEFT JOIN last_at la ON la.vehicle_id = vca.vehicle_id AND la.category_id = sp.category_id
    LEFT JOIN last_km lk ON lk.vehicle_id = vca.vehicle_id AND lk.category_id = sp.category_id
    LEFT JOIN v_current_odometer odo ON odo.vehicle_id = vca.vehicle_id
    WHERE vca.deleted_at IS NULL
),
graded AS (
    SELECT b.*,
        CASE
            WHEN interval_km IS NULL THEN NULL
            WHEN last_km IS NULL THEN 'never_done'
            WHEN current_odometer IS NULL THEN 'unknown'
            WHEN current_odometer >= next_due_km THEN 'overdue'
            WHEN current_odometer >= next_due_km - lead_warn_km THEN 'due_soon'
            ELSE 'ok'
        END AS km_status,
        CASE
            WHEN interval_days IS NULL THEN NULL
            WHEN last_at IS NULL THEN 'never_done'
            WHEN now() >= next_due_at THEN 'overdue'
            WHEN now() >= next_due_at - make_interval(days => LEAST(lead_warn_days, interval_days)) THEN 'due_soon'
            ELSE 'ok'
        END AS time_status
    FROM base b
)
SELECT
    vehicle_id, plan_id, category_id, category_name_ar, category_name_en,
    trigger_type, interval_km, interval_days, lead_warn_km, lead_warn_days,
    last_at, last_km, current_odometer, odometer_source, odometer_observed_at,
    next_due_km, next_due_at,
    CASE WHEN next_due_km IS NOT NULL AND current_odometer IS NOT NULL
         THEN next_due_km - current_odometer END AS km_remaining,
    CASE WHEN next_due_at IS NOT NULL
         THEN (next_due_at::date - (now() AT TIME ZONE 'Africa/Cairo')::date) END AS days_remaining,
    CASE
        WHEN last_at IS NULL THEN 'never_done'
        WHEN 'overdue'  IN (km_status, time_status) THEN 'overdue'
        WHEN 'due_soon' IN (km_status, time_status) THEN 'due_soon'
        WHEN time_status IS NULL AND km_status = 'never_done' THEN 'never_done'
        WHEN time_status IS NULL AND km_status = 'unknown' THEN 'unknown'
        ELSE 'ok'
    END AS status
FROM graded;
