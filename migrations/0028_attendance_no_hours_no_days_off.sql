-- Attendance without working hours or days off (2026-11-27).
--
-- The garage works every day: no working-hours settings (start, end, grace),
-- no lateness, no weekly day off and no "off" day. A day is present, absent,
-- leave or sick; a present day keeps its check-in and check-out times. Days
-- already marked off become absent; the update stamps them, so every device
-- pulls the change.

UPDATE attendance_days SET status = 'absent' WHERE status = 'off';

-- The times check names the enum; recreate it around the type swap.
ALTER TABLE attendance_days DROP CONSTRAINT attendance_days_times_only_present;
ALTER TYPE attendance_status RENAME TO attendance_status_old;
CREATE TYPE attendance_status AS ENUM ('present', 'absent', 'leave', 'sick');
ALTER TABLE attendance_days
    ALTER COLUMN status TYPE attendance_status USING status::text::attendance_status;
DROP TYPE attendance_status_old;
ALTER TABLE attendance_days ADD CONSTRAINT attendance_days_times_only_present
    CHECK (status = 'present' OR (check_in_at IS NULL AND check_out_at IS NULL));

DROP TABLE attendance_settings;
