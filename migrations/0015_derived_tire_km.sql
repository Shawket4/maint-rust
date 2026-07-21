-- Tire km becomes DERIVED, never accrued (decision 2026-07-21).
--
-- tires.total_km now holds only the BASE — the estimated km entered at
-- legacy intake/onboarding (0 for shipment tires). Lifetime km is computed
-- wherever it is read:
--     base + Σ (dismounted − mounted) over closed road assignments
--          + (current odometer − mounted) for the open assignment, if any.
-- Editing any historical odometer (or correcting the truck's odometer)
-- repropagates everywhere instantly, because nothing is baked.

-- Recover the base from already-accrued rows: subtract the exact deltas the
-- old dismount side-effect added (closed, both odometers, road position).
UPDATE tires t
   SET total_km = GREATEST(0, COALESCE(t.total_km, 0) - COALESCE((
        SELECT SUM(ta.dismounted_odometer - ta.mounted_odometer)
          FROM tire_assignments ta
          LEFT JOIN chassis_positions p ON p.id = ta.position_id
         WHERE ta.tire_id = t.id
           AND ta.deleted_at IS NULL
           AND ta.dismounted_at IS NOT NULL
           AND ta.mounted_odometer IS NOT NULL
           AND ta.dismounted_odometer IS NOT NULL
           AND ta.dismounted_odometer > ta.mounted_odometer
           AND COALESCE(p.is_spare, false) = false
   ), 0));

COMMENT ON COLUMN tires.total_km IS
  'BASE km at intake/onboarding only. Lifetime km is DERIVED from assignments (0015) — never accrue here.';

DROP VIEW IF EXISTS v_tires_list;
CREATE VIEW v_tires_list AS
SELECT
    t.id, t.dot_code, t.brand, t.model, t.size, t.status, t.origin,
    COALESCE(t.total_km, 0)
      + COALESCE((
            SELECT SUM(x.dismounted_odometer - x.mounted_odometer)
              FROM tire_assignments x
              LEFT JOIN chassis_positions xp ON xp.id = x.position_id
             WHERE x.tire_id = t.id AND x.deleted_at IS NULL
               AND x.dismounted_at IS NOT NULL
               AND x.mounted_odometer IS NOT NULL AND x.dismounted_odometer IS NOT NULL
               AND x.dismounted_odometer > x.mounted_odometer
               AND COALESCE(xp.is_spare, false) = false
        ), 0)
      + CASE WHEN ta.id IS NOT NULL AND ta.mounted_odometer IS NOT NULL
                  AND COALESCE(cp.is_spare, false) = false
                  AND odo.current_odometer > ta.mounted_odometer
             THEN odo.current_odometer - ta.mounted_odometer ELSE 0 END
      AS total_km,
    t.parent_tire_id, t.retread_count, t.notes_ar, t.notes_en,
    t.created_at, t.updated_at, t.deleted_at,
    t.created_by_user_id, t.updated_by_user_id, t.sync_version,
    ta.vehicle_id     AS mounted_vehicle_id,
    ta.position_id    AS mounted_position_id,
    cp.position_code  AS mounted_position_code,
    ta.mounted_at     AS mounted_at
FROM tires t
LEFT JOIN tire_assignments ta ON ta.tire_id = t.id AND ta.dismounted_at IS NULL AND ta.deleted_at IS NULL
LEFT JOIN chassis_positions cp ON cp.id = ta.position_id
LEFT JOIN v_current_odometer odo ON odo.vehicle_id = ta.vehicle_id
WHERE t.deleted_at IS NULL;
