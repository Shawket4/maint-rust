-- =========================================================
-- 0011_odometer_truth.sql
--
-- current_odometer was GREATEST(vc.last_fuel_odometer, voo.odometer) — i.e. the
-- odometer at the last FUEL-UP, max'd with a manual override. Two problems:
--
--  1. maintenance_records.odometer_at_service never contributed. A mechanic
--     records an oil change at 486,320 while the last fuel-up was 480,000, and
--     the due engine still believes 480,000. The reading exists and is thrown
--     away — and the error is always in the direction that makes work look LESS
--     due than it is, which is how things get missed.
--
--  2. Migration 0010 added vehicles_cache.mileage and falcon/cars.rs syncs it
--     (already override-corrected at cars.rs:148), but this view never read it.
--     Falcon's truer reading was fetched, corrected, stored, then ignored.
--
-- An odometer is monotonic: it only counts up. So the best estimate of "now" is
-- the MAX of every observation we hold, whatever its source. This view takes
-- that max across all four, and reports which source won so the UI can show
-- provenance (and so a stale reading is visible rather than silently trusted).
--
-- NOTE the override still only wins by being HIGHER — both here and in
-- cars.rs:148 ("override replaces fetched mileage if override > fetched"). A
-- reading can therefore never be corrected DOWNWARD, so a fat-fingered high
-- value is permanent. That's a real hazard now that service records feed in
-- too; fixing it needs a product decision, not just SQL.
-- =========================================================

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
    odo.current_odometer,
    odo.odometer_source,
    odo.odometer_observed_at,
    CASE
      WHEN r.id IS NULL THEN 'never_done'
      WHEN t.trigger_type = 'mileage' AND r.next_due_km IS NOT NULL THEN
        CASE
          WHEN odo.current_odometer >= r.next_due_km                    THEN 'overdue'
          WHEN odo.current_odometer >= r.next_due_km - t.lead_warn_km   THEN 'due_soon'
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
-- The highest reading we hold for this vehicle, from ANY source, with its
-- provenance. One lateral, reused by both current_odometer and the status CASE.
LEFT JOIN LATERAL (
    SELECT
        MAX(o.reading)                                          AS current_odometer,
        (ARRAY_AGG(o.src        ORDER BY o.reading DESC))[1]    AS odometer_source,
        (ARRAY_AGG(o.observed_at ORDER BY o.reading DESC))[1]   AS odometer_observed_at
    FROM (
        SELECT COALESCE(vc.mileage, 0)::INT             AS reading,
               'falcon_mileage'::TEXT                   AS src,
               vc.fetched_at                            AS observed_at
        UNION ALL
        SELECT COALESCE(vc.last_fuel_odometer, 0)::INT,
               'falcon_fuel',
               vc.fetched_at
        UNION ALL
        SELECT COALESCE(voo.odometer, 0)::INT,
               'manual',
               voo.set_at
        UNION ALL
        SELECT COALESCE(MAX(mr.odometer_at_service), 0)::INT,
               'service',
               MAX(mr.performed_at)
        FROM maintenance_records mr
        WHERE mr.vehicle_id = t.vehicle_id
          AND mr.deleted_at IS NULL
    ) o
) odo ON TRUE
WHERE t.deleted_at IS NULL
  AND t.is_active
  AND vc.source_deleted_at IS NULL;
