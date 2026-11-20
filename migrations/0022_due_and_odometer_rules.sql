-- Due + odometer rules (2026-11 audit). The client's SQLite mirrors
-- (src-tauri services/views/odometer.rs + due.rs) must stay equivalent.
--
-- 1. Odometer override: a Falcon reading used to be ignored while it was
--    <= superseded_km (the Falcon value at the moment of correction). After
--    correcting a typo'd 4,123,000 down to 412,300, a genuine 418,000 fuel
--    reading was ignored indefinitely — the odometer froze. And superseded_km
--    holds ONE number while Falcon has two readings (fuel and mileage), so
--    the smaller stale one could still win over the correction.
--    New rule, the same one oil-change and work-order readings already
--    follow: a Falcon reading counts only when it was observed AFTER the
--    correction. Each of the two readings gets its own "last changed" time
--    (fuel_odometer_changed_at, mileage_changed_at, kept by falcon/cars.rs),
--    so a stale value stays ignored and the first new reading counts
--    whatever its size.
--
-- 2. Due baseline: "last done" is when the task was ticked (done_at), not the
--    day its work order was opened. The km baseline is the newest done record
--    that HAS a reading — a later record without km no longer hides it.
--
-- 3. A plan may carry a km interval, a day interval, or both ("10,000 km or
--    6 months, whichever first"). trigger_type is kept for older clients:
--    'mileage' when a km interval exists, else 'time'.
--
-- 4. A mileage-only plan on a vehicle with no odometer reading reported 'ok'.
--    It is 'unknown' now — nothing can be said about it.

ALTER TABLE service_plans DROP CONSTRAINT IF EXISTS service_plans_check;
ALTER TABLE service_plans
    ADD CONSTRAINT ck_plan_intervals CHECK (
        (interval_km IS NOT NULL OR interval_days IS NOT NULL)
        AND (interval_km   IS NULL OR interval_km   > 0)
        AND (interval_days IS NULL OR interval_days > 0)
        AND lead_warn_km >= 0 AND lead_warn_days >= 0
    ) NOT VALID;

ALTER TABLE vehicles_cache
    ADD COLUMN fuel_odometer_changed_at TIMESTAMPTZ,
    ADD COLUMN mileage_changed_at       TIMESTAMPTZ;
-- Unknown history: assume each reading is as old as the last poll, except on
-- a corrected vehicle whose reading is still at or below the value that was
-- corrected — date that one before the correction so it stays ignored.
UPDATE vehicles_cache SET fuel_odometer_changed_at = fetched_at, mileage_changed_at = fetched_at;
UPDATE vehicles_cache vc
   SET fuel_odometer_changed_at = o.set_at - interval '1 second'
  FROM vehicle_odometer_overrides o
 WHERE o.vehicle_id = vc.id AND vc.fuel_odometer_changed_at > o.set_at
   AND COALESCE(vc.last_fuel_odometer, 0) <= COALESCE(o.superseded_km, 2147483647);
UPDATE vehicles_cache vc
   SET mileage_changed_at = o.set_at - interval '1 second'
  FROM vehicle_odometer_overrides o
 WHERE o.vehicle_id = vc.id AND vc.mileage_changed_at > o.set_at
   AND COALESCE(vc.mileage, 0) <= COALESCE(o.superseded_km, 2147483647);

CREATE OR REPLACE VIEW v_current_odometer AS
WITH ov AS (
    SELECT vehicle_id, odometer, set_at, superseded_km
      FROM vehicle_odometer_overrides
     WHERE odometer IS NOT NULL
),
readings AS (
    SELECT vc.id AS vehicle_id, vc.last_fuel_odometer AS km,
           'falcon_fuel'::text AS src, COALESCE(vc.fuel_odometer_changed_at, vc.fetched_at) AS observed_at
      FROM vehicles_cache vc
      LEFT JOIN ov ON ov.vehicle_id = vc.id
     WHERE vc.last_fuel_odometer IS NOT NULL
       AND vc.last_fuel_odometer > 0
       AND (ov.vehicle_id IS NULL OR COALESCE(vc.fuel_odometer_changed_at, vc.fetched_at) > ov.set_at)
    UNION ALL
    SELECT vc.id, vc.mileage, 'falcon_mileage', COALESCE(vc.mileage_changed_at, vc.fetched_at)
      FROM vehicles_cache vc
      LEFT JOIN ov ON ov.vehicle_id = vc.id
     WHERE vc.mileage IS NOT NULL
       AND vc.mileage > 0
       AND (ov.vehicle_id IS NULL OR COALESCE(vc.mileage_changed_at, vc.fetched_at) > ov.set_at)
    UNION ALL
    SELECT oc.vehicle_id, oc.odometer, 'oil_change', oc.performed_at
      FROM oil_changes oc
      LEFT JOIN ov ON ov.vehicle_id = oc.vehicle_id
     WHERE oc.deleted_at IS NULL AND oc.odometer IS NOT NULL
       AND (ov.vehicle_id IS NULL OR oc.performed_at >= ov.set_at)
    UNION ALL
    SELECT wo.vehicle_id, wo.odometer_at_open, 'work_order', wo.opened_at
      FROM work_orders wo
      LEFT JOIN ov ON ov.vehicle_id = wo.vehicle_id
     WHERE wo.deleted_at IS NULL AND wo.odometer_at_open IS NOT NULL
       AND (ov.vehicle_id IS NULL OR wo.opened_at >= ov.set_at)
    UNION ALL
    SELECT vehicle_id, odometer, 'manual_override', set_at FROM ov
),
ranked AS (
    SELECT vehicle_id, km, src, observed_at,
           ROW_NUMBER() OVER (PARTITION BY vehicle_id
                              ORDER BY km DESC NULLS LAST, (src = 'falcon_mileage'), src) AS rn
      FROM readings WHERE km IS NOT NULL
)
SELECT vehicle_id, km AS current_odometer, src AS odometer_source, observed_at AS odometer_observed_at
  FROM ranked WHERE rn = 1;

DROP VIEW IF EXISTS v_maintenance_due;
CREATE VIEW v_maintenance_due AS
WITH done AS (
    SELECT wo.vehicle_id, t.category_id, t.id AS task_id,
           COALESCE(t.done_at, wo.opened_at) AS done_at,
           wo.odometer_at_open               AS km
      FROM work_order_tasks t
      JOIN work_orders wo ON wo.id = t.work_order_id
     WHERE t.deleted_at IS NULL AND wo.deleted_at IS NULL
       AND t.category_id IS NOT NULL AND t.is_done
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
    -- Worst of the two clocks. A plan nobody ever recorded is never_done; a
    -- km clock with no baseline defers to the time clock when there is one.
    CASE
        WHEN last_at IS NULL THEN 'never_done'
        WHEN 'overdue'  IN (km_status, time_status) THEN 'overdue'
        WHEN 'due_soon' IN (km_status, time_status) THEN 'due_soon'
        WHEN time_status IS NULL AND km_status = 'never_done' THEN 'never_done'
        WHEN time_status IS NULL AND km_status = 'unknown' THEN 'unknown'
        ELSE 'ok'
    END AS status
FROM graded;
