-- =========================================================
-- 0009_enrich_views.sql: Add category details (name, icon) to maintenance views
-- =========================================================

-- 1. Update v_maintenance_due
DROP VIEW IF EXISTS v_maintenance_due;
CREATE VIEW v_maintenance_due AS
SELECT
    t.id              AS template_id,
    t.vehicle_id,
    vc.car_no_plate    AS plate_number,
    t.category_id,
    t.name_ar, t.name_en,
    t.trigger_type,
    t.interval_km, t.interval_days,
    t.lead_warn_km, t.lead_warn_days,
    r.performed_at        AS last_done_at,
    r.odometer_at_service AS last_done_km,
    r.next_due_at,
    r.next_due_km,
    GREATEST(
        COALESCE(vc.last_fuel_odometer, 0),
        COALESCE(voo.odometer, 0)
    ) AS current_odometer,
    CASE
        WHEN COALESCE(voo.odometer, 0) > COALESCE(vc.last_fuel_odometer, 0)
            THEN 'manual'
        ELSE 'falcon'
    END AS odometer_source,
    CASE
      WHEN r.id IS NULL THEN 'never_done'
      WHEN t.trigger_type = 'mileage' AND r.next_due_km IS NOT NULL THEN
        CASE
          WHEN GREATEST(COALESCE(vc.last_fuel_odometer, 0), COALESCE(voo.odometer, 0)) >= r.next_due_km
            THEN 'overdue'
          WHEN GREATEST(COALESCE(vc.last_fuel_odometer, 0), COALESCE(voo.odometer, 0)) >= r.next_due_km - t.lead_warn_km
            THEN 'due_soon'
          ELSE 'ok'
        END
      WHEN t.trigger_type = 'time' AND r.next_due_at IS NOT NULL THEN
        CASE
          WHEN now() >= r.next_due_at THEN 'overdue'
          WHEN now() >= r.next_due_at - (t.lead_warn_days || ' days')::INTERVAL THEN 'due_soon'
          ELSE 'ok'
        END
      ELSE 'never_done'
    END AS status,
    c.icon AS category_icon
FROM maintenance_templates t
LEFT JOIN LATERAL (
    SELECT * FROM maintenance_records
    WHERE template_id = t.id AND deleted_at IS NULL
    ORDER BY performed_at DESC LIMIT 1
) r ON TRUE
JOIN vehicles_cache vc                    ON vc.id = t.vehicle_id
JOIN maintenance_categories c             ON c.id = t.category_id
LEFT JOIN vehicle_odometer_overrides voo  ON voo.vehicle_id = t.vehicle_id
WHERE t.deleted_at IS NULL
  AND t.is_active
  AND vc.source_deleted_at IS NULL;

-- 2. Update v_maintenance_records
DROP VIEW IF EXISTS v_maintenance_records;
CREATE VIEW v_maintenance_records AS
SELECT
    r.*,
    vc.car_no_plate AS plate_number,
    c.name_ar,
    c.name_en,
    c.icon AS category_icon
FROM maintenance_records r
JOIN vehicles_cache vc ON vc.id = r.vehicle_id
JOIN maintenance_categories c ON c.id = r.category_id;

-- 3. Update v_maintenance_templates
DROP VIEW IF EXISTS v_maintenance_templates;
CREATE VIEW v_maintenance_templates AS
SELECT
    t.*,
    vc.car_no_plate AS plate_number,
    c.name_ar AS category_name_ar,
    c.name_en AS category_name_en,
    c.icon AS category_icon
FROM maintenance_templates t
JOIN vehicles_cache vc ON vc.id = t.vehicle_id
JOIN maintenance_categories c ON c.id = t.category_id;
