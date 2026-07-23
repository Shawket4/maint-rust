-- Three due/odometer arbitration fixes (2026-07-23 review). The client's
-- SQLite mirrors (src-tauri services/views.rs ODO_CTE + DUE_TAIL) must stay
-- byte-for-byte equivalent in semantics — change both together.
--
-- 1. v_current_odometer: discard zero fuel readings. The client already
--    guards `last_fuel_odometer > 0` (a 0 is a sensor artifact, not a
--    reading); the server view didn't, so a vehicle whose only reading was a
--    zero fuel odometer computed current_odometer = 0 here and NULL there,
--    flipping due statuses between the two sides.
--
-- 2. v_maintenance_due.last_done: only DONE tasks reset the due clock
--    (is_done was never filtered — merely planning a task, or unchecking it,
--    counted as performed), and last_at/last_km must come from the SAME work
--    order (independent MAX() over each column could pair one WO's date with
--    another's odometer).
--
-- 3. v_maintenance_due.status: a mileage plan whose latest record carries no
--    odometer reported 'ok' forever (NULL >= NULL is NULL → ELSE). There is
--    no usable km baseline, which is what never_done means for a mileage
--    plan — surface it instead of silently disarming the plan.

CREATE OR REPLACE VIEW v_current_odometer AS
WITH ov AS (
    SELECT vehicle_id, odometer, set_at, superseded_km
      FROM vehicle_odometer_overrides
     WHERE odometer IS NOT NULL
),
readings AS (
    SELECT vc.id AS vehicle_id, vc.last_fuel_odometer AS km,
           'falcon_fuel'::text AS src, vc.fetched_at AS observed_at
      FROM vehicles_cache vc
      LEFT JOIN ov ON ov.vehicle_id = vc.id
     WHERE vc.last_fuel_odometer IS NOT NULL
       AND vc.last_fuel_odometer > 0
       AND (ov.vehicle_id IS NULL OR vc.last_fuel_odometer > COALESCE(ov.superseded_km, -1))
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
           ROW_NUMBER() OVER (PARTITION BY vehicle_id ORDER BY km DESC NULLS LAST) AS rn
      FROM readings WHERE km IS NOT NULL
)
SELECT vehicle_id, km AS current_odometer, src AS odometer_source, observed_at AS odometer_observed_at
  FROM ranked WHERE rn = 1;

DROP VIEW IF EXISTS v_maintenance_due;
CREATE VIEW v_maintenance_due AS
WITH last_done AS (
    SELECT DISTINCT ON (wo.vehicle_id, t.category_id)
           wo.vehicle_id, t.category_id,
           wo.opened_at         AS last_at,
           wo.odometer_at_open  AS last_km
      FROM work_order_tasks t
      JOIN work_orders wo ON wo.id = t.work_order_id
     WHERE t.deleted_at IS NULL AND wo.deleted_at IS NULL
       AND t.category_id IS NOT NULL AND t.is_done
     ORDER BY wo.vehicle_id, t.category_id, wo.opened_at DESC
)
SELECT
    vca.vehicle_id,
    sp.id                      AS plan_id,
    sp.category_id,
    c.name_ar                  AS category_name_ar,
    c.name_en                  AS category_name_en,
    sp.trigger_type,
    sp.interval_km,
    sp.interval_days,
    sp.lead_warn_km,
    sp.lead_warn_days,
    ld.last_at,
    ld.last_km,
    odo.current_odometer,
    odo.odometer_source,
    odo.odometer_observed_at,
    CASE WHEN sp.trigger_type = 'mileage' THEN ld.last_km + sp.interval_km END       AS next_due_km,
    CASE WHEN sp.trigger_type = 'time'    THEN ld.last_at + (sp.interval_days || ' days')::interval END AS next_due_at,
    CASE
        WHEN ld.last_at IS NULL THEN 'never_done'
        WHEN sp.trigger_type = 'mileage' THEN
            CASE
                WHEN ld.last_km IS NULL THEN 'never_done'
                WHEN odo.current_odometer IS NULL THEN 'ok'
                WHEN odo.current_odometer >= ld.last_km + sp.interval_km THEN 'overdue'
                WHEN odo.current_odometer >= ld.last_km + sp.interval_km - sp.lead_warn_km THEN 'due_soon'
                ELSE 'ok'
            END
        ELSE
            CASE
                WHEN now() >= ld.last_at + (sp.interval_days || ' days')::interval THEN 'overdue'
                WHEN now() >= ld.last_at + ((sp.interval_days - sp.lead_warn_days) || ' days')::interval THEN 'due_soon'
                ELSE 'ok'
            END
    END AS status
FROM vehicle_class_assignments vca
JOIN service_plans sp ON sp.class_id = vca.class_id AND sp.is_active AND sp.deleted_at IS NULL
JOIN maintenance_categories c ON c.id = sp.category_id
LEFT JOIN last_done ld ON ld.vehicle_id = vca.vehicle_id AND ld.category_id = sp.category_id
LEFT JOIN v_current_odometer odo ON odo.vehicle_id = vca.vehicle_id
WHERE vca.deleted_at IS NULL;
