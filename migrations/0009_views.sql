-- =========================================================
-- v_current_odometer: the odometer truth per vehicle = MAX across every reading.
-- =========================================================
CREATE VIEW v_current_odometer AS
WITH readings AS (
    SELECT vc.id AS vehicle_id, vc.last_fuel_odometer AS km, 'falcon_fuel'::text AS src, vc.fetched_at AS observed_at
      FROM vehicles_cache vc
    UNION ALL
    SELECT o.vehicle_id, o.odometer, 'manual_override', o.set_at FROM vehicle_odometer_overrides o
    UNION ALL
    SELECT oc.vehicle_id, oc.odometer, 'oil_change', oc.performed_at
      FROM oil_changes oc WHERE oc.deleted_at IS NULL AND oc.odometer IS NOT NULL
    UNION ALL
    SELECT wo.vehicle_id, wo.odometer_at_open, 'work_order', wo.opened_at
      FROM work_orders wo WHERE wo.deleted_at IS NULL AND wo.odometer_at_open IS NOT NULL
),
ranked AS (
    SELECT vehicle_id, km, src, observed_at,
           ROW_NUMBER() OVER (PARTITION BY vehicle_id ORDER BY km DESC NULLS LAST) AS rn
      FROM readings WHERE km IS NOT NULL
)
SELECT vehicle_id, km AS current_odometer, src AS odometer_source, observed_at AS odometer_observed_at
  FROM ranked WHERE rn = 1;

-- =========================================================
-- v_maintenance_due: fleet-wide plan × vehicle, with computed status.
-- =========================================================
CREATE VIEW v_maintenance_due AS
WITH last_done AS (
    SELECT wo.vehicle_id, t.category_id,
           MAX(wo.opened_at)       AS last_at,
           MAX(wo.odometer_at_open) AS last_km
      FROM work_order_tasks t
      JOIN work_orders wo ON wo.id = t.work_order_id
     WHERE t.deleted_at IS NULL AND wo.deleted_at IS NULL AND t.category_id IS NOT NULL
     GROUP BY wo.vehicle_id, t.category_id
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

-- =========================================================
-- v_tires_list: each individual tire + where it is now (if mounted).
-- =========================================================
CREATE VIEW v_tires_list AS
SELECT
    t.*,
    ta.vehicle_id     AS mounted_vehicle_id,
    ta.position_id    AS mounted_position_id,
    p.position_code   AS mounted_position_code,
    ta.mounted_at     AS mounted_at
FROM tires t
LEFT JOIN tire_assignments ta ON ta.tire_id = t.id AND ta.dismounted_at IS NULL AND ta.deleted_at IS NULL
LEFT JOIN chassis_positions p ON p.id = ta.position_id
WHERE t.deleted_at IS NULL;
