-- A manual odometer override is an explicit human correction, not just one
-- more reading competing by MAX — that rule made downward corrections
-- silently lose to the very reading being corrected.
--
-- New arbitration:
--   * the override itself always competes;
--   * oil-change / work-order readings recorded BEFORE the correction are
--     discarded (they are what was being corrected);
--   * the Falcon fuel reading is discarded while it is ≤ the value it showed
--     when the correction was made (superseded_km) — it re-engages only when
--     the truck genuinely drives past the corrected number.

ALTER TABLE vehicle_odometer_overrides ADD COLUMN superseded_km INT;

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
