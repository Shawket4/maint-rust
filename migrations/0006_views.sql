-- =========================================================
-- v_maintenance_due: live status per active template
-- =========================================================
CREATE OR REPLACE VIEW v_maintenance_due AS
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
    END AS status
FROM maintenance_templates t
LEFT JOIN LATERAL (
    SELECT * FROM maintenance_records
    WHERE template_id = t.id AND deleted_at IS NULL
    ORDER BY performed_at DESC LIMIT 1
) r ON TRUE
JOIN vehicles_cache vc                    ON vc.id = t.vehicle_id
LEFT JOIN vehicle_odometer_overrides voo  ON voo.vehicle_id = t.vehicle_id
WHERE t.deleted_at IS NULL
  AND t.is_active
  AND vc.source_deleted_at IS NULL;

-- =========================================================
-- v_tire_lifetime: aggregate metrics per tire
-- =========================================================
CREATE OR REPLACE VIEW v_tire_lifetime AS
WITH per_assignment_km AS (
    SELECT
        ta.id,
        ta.tire_id,
        ta.vehicle_id,
        ta.mounted_at,
        CASE
          WHEN ta.dismounted_odometer IS NOT NULL AND ta.mounted_odometer IS NOT NULL
            THEN ta.dismounted_odometer - ta.mounted_odometer
          WHEN ta.dismounted_at IS NULL AND ta.mounted_odometer IS NOT NULL
            THEN GREATEST(
                   COALESCE(vc.last_fuel_odometer, ta.mounted_odometer),
                   COALESCE(voo.odometer,         ta.mounted_odometer)
                 ) - ta.mounted_odometer
          ELSE 0
        END AS km_on_position
    FROM tire_assignments ta
    LEFT JOIN vehicles_cache vc               ON vc.id  = ta.vehicle_id
    LEFT JOIN vehicle_odometer_overrides voo  ON voo.vehicle_id = ta.vehicle_id
    WHERE ta.deleted_at IS NULL
)
SELECT
    t.id                                    AS tire_id,
    t.dot_code,
    t.brand,
    t.status,
    t.purchase_cost,
    COALESCE(SUM(pa.km_on_position), 0)::INT AS lifetime_km,
    COUNT(pa.id)::INT                        AS installment_count,
    MIN(pa.mounted_at)                       AS first_mounted_at,
    CASE
      WHEN t.purchase_cost IS NOT NULL AND COALESCE(SUM(pa.km_on_position), 0) > 0
        THEN ROUND(t.purchase_cost / NULLIF(SUM(pa.km_on_position), 0), 4)
      ELSE NULL
    END                                      AS cost_per_km
FROM tires t
LEFT JOIN per_assignment_km pa ON pa.tire_id = t.id
WHERE t.deleted_at IS NULL
GROUP BY t.id, t.dot_code, t.brand, t.status, t.purchase_cost;

-- =========================================================
-- v_tires_list: list view with current location + lifetime metrics
-- =========================================================
CREATE OR REPLACE VIEW v_tires_list AS
SELECT
    t.id,
    t.dot_code,
    t.internal_serial,
    t.brand,
    t.model,
    t.status,
    t.production_week,
    t.production_year,
    t.production_date,
    t.purchase_date,
    t.purchase_cost,
    t.stock_location,
    t.is_retread,
    t.retread_count,
    CASE
      WHEN t.status = 'mounted' THEN
        (SELECT vc.car_no_plate || ' / ' || cp.position_code
           FROM tire_assignments ta
           JOIN chassis_positions cp ON cp.id = ta.position_id
           JOIN vehicles_cache vc    ON vc.id = ta.vehicle_id
          WHERE ta.tire_id = t.id AND ta.dismounted_at IS NULL AND ta.deleted_at IS NULL
          LIMIT 1)
      WHEN t.status = 'in_stock' THEN COALESCE(t.stock_location, 'Unassigned')
      ELSE t.status::TEXT
    END AS current_location_label,
    EXISTS (
        SELECT 1 FROM tire_assignments ta
        WHERE ta.tire_id = t.id AND ta.deleted_at IS NULL
    ) AS has_history,
    tl.lifetime_km,
    tl.installment_count,
    tl.cost_per_km,
    t.created_at,
    t.updated_at
FROM tires t
LEFT JOIN v_tire_lifetime tl ON tl.tire_id = t.id
WHERE t.deleted_at IS NULL;

-- =========================================================
-- v_maintenance_records: history with vehicle details
-- =========================================================
CREATE OR REPLACE VIEW v_maintenance_records AS
SELECT
    r.*,
    vc.car_no_plate AS plate_number
FROM maintenance_records r
JOIN vehicles_cache vc ON vc.id = r.vehicle_id;

-- =========================================================
-- v_maintenance_templates: templates with vehicle details
-- =========================================================
CREATE OR REPLACE VIEW v_maintenance_templates AS
SELECT
    t.*,
    vc.car_no_plate AS plate_number
FROM maintenance_templates t
JOIN vehicles_cache vc ON vc.id = t.vehicle_id;
