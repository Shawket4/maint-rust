-- v_current_odometer: count Falcon's `cars.mileage` as a reading.
--
-- Falcon keeps `mileage` = MAX(fuel odometer_after, oil change
-- current_odometer, service invoice meter_reading) (Models.RecalculateMileage).
-- The cache mirrored it since 0011 but no view read it, so an oil change
-- recorded in Falcon (the oil change screen in apex-react) never moved the
-- truck's odometer here — only the last fuel reading did, and mileage plans
-- were judged against a stale number.
--
-- It follows the fuel reading's override rule: pinned while ≤ the value the
-- correction superseded. The client now records superseded_km as the larger
-- of the fuel reading and mileage, so a downward correction holds against both.
--
-- The client's SQLite mirror (src-tauri services/views/odometer.rs ODO_CTE)
-- must stay equivalent — change both together.

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
    SELECT vc.id, vc.mileage, 'falcon_mileage', vc.fetched_at
      FROM vehicles_cache vc
      LEFT JOIN ov ON ov.vehicle_id = vc.id
     WHERE vc.mileage IS NOT NULL
       AND vc.mileage > 0
       AND (ov.vehicle_id IS NULL OR vc.mileage > COALESCE(ov.superseded_km, -1))
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
    -- Ties go to the more specific source: an equal fuel reading names
    -- where the number came from better than the aggregate mileage does.
    SELECT vehicle_id, km, src, observed_at,
           ROW_NUMBER() OVER (PARTITION BY vehicle_id
                              ORDER BY km DESC NULLS LAST, (src = 'falcon_mileage')) AS rn
      FROM readings WHERE km IS NOT NULL
)
SELECT vehicle_id, km AS current_odometer, src AS odometer_source, observed_at AS odometer_observed_at
  FROM ranked WHERE rn = 1;
