-- Garage attendance (2026-11-27).
--
-- The supervisor marks each garage worker's day in the app. Workers are typed
-- in by name (free form) or picked from Falcon's employees; only the name is
-- required. One row per worker per day; the row id is a UUID v5 of
-- "<worker_id>:<YYYY-MM-DD>" (namespace in services::rules), so two devices
-- marking the same day offline meet on one id: the first mark stays and the
-- second device is told.
--
-- Settings are one row (id = 1). With no row, the defaults below apply.

CREATE TYPE attendance_status AS ENUM ('present', 'absent', 'leave', 'sick', 'off');

CREATE TABLE attendance_workers (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name                TEXT NOT NULL CHECK (btrim(name) <> ''),
    job                 TEXT,
    phone               TEXT,
    falcon_employee_id  INT CHECK (falcon_employee_id > 0),
    joined_on           DATE,
    left_on             DATE,
    sort_order          INT NOT NULL DEFAULT 0,
    notes               TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at          TIMESTAMPTZ,
    created_by_user_id  BIGINT NOT NULL REFERENCES users_cache(id),
    updated_by_user_id  BIGINT NOT NULL REFERENCES users_cache(id),
    sync_version        BIGINT NOT NULL DEFAULT 1,
    CONSTRAINT attendance_workers_dates CHECK (left_on IS NULL OR joined_on IS NULL OR left_on >= joined_on)
);
CREATE UNIQUE INDEX uq_attendance_workers_falcon
    ON attendance_workers(falcon_employee_id) WHERE deleted_at IS NULL AND falcon_employee_id IS NOT NULL;
CREATE INDEX idx_attendance_workers_feed ON attendance_workers(updated_at, id);

CREATE TABLE attendance_days (
    id                  UUID PRIMARY KEY,
    worker_id           UUID NOT NULL REFERENCES attendance_workers(id),
    work_date           DATE NOT NULL,
    status              attendance_status NOT NULL,
    check_in_at         TIMESTAMPTZ,
    check_out_at        TIMESTAMPTZ,
    note                TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at          TIMESTAMPTZ,
    created_by_user_id  BIGINT NOT NULL REFERENCES users_cache(id),
    updated_by_user_id  BIGINT NOT NULL REFERENCES users_cache(id),
    sync_version        BIGINT NOT NULL DEFAULT 1,
    -- Only a present day has times.
    CONSTRAINT attendance_days_times_only_present
        CHECK (status = 'present' OR (check_in_at IS NULL AND check_out_at IS NULL)),
    -- A check-out comes after the check-in, within 18 hours (a late night
    -- still belongs to the day it started).
    CONSTRAINT attendance_days_out_after_in
        CHECK (check_out_at IS NULL OR check_in_at IS NULL
               OR (check_out_at > check_in_at AND check_out_at <= check_in_at + interval '18 hours'))
);
CREATE UNIQUE INDEX uq_attendance_days_live ON attendance_days(worker_id, work_date) WHERE deleted_at IS NULL;
CREATE INDEX idx_attendance_days_date ON attendance_days(work_date) WHERE deleted_at IS NULL;
CREATE INDEX idx_attendance_days_feed ON attendance_days(updated_at, id);

-- A day in the future (Cairo's calendar) is refused.
CREATE OR REPLACE FUNCTION maint_attendance_day_not_future() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.work_date > (now() AT TIME ZONE 'Africa/Cairo')::date THEN
        RAISE EXCEPTION 'work_date is in the future' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_attendance_days_not_future BEFORE INSERT OR UPDATE OF work_date ON attendance_days
    FOR EACH ROW EXECUTE FUNCTION maint_attendance_day_not_future();

CREATE TABLE attendance_settings (
    id                  INT PRIMARY KEY CHECK (id = 1),
    work_start          TEXT NOT NULL DEFAULT '08:00' CHECK (work_start ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
    work_end            TEXT NOT NULL DEFAULT '17:00' CHECK (work_end ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
    late_grace_minutes  INT NOT NULL DEFAULT 15 CHECK (late_grace_minutes BETWEEN 0 AND 240),
    -- ISO weekdays (1 = Monday … 7 = Sunday), comma separated; '5' = Friday.
    weekly_off_days     TEXT NOT NULL DEFAULT '5' CHECK (weekly_off_days ~ '^([1-7](,[1-7])*)?$'),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at          TIMESTAMPTZ,
    created_by_user_id  BIGINT NOT NULL REFERENCES users_cache(id),
    updated_by_user_id  BIGINT NOT NULL REFERENCES users_cache(id),
    sync_version        BIGINT NOT NULL DEFAULT 1
);
CREATE INDEX idx_attendance_settings_feed ON attendance_settings(updated_at, id);

-- Commit-ordered updated_at (0021) and the users_cache placeholder (0026).
DO $$
DECLARE t TEXT;
BEGIN
    FOREACH t IN ARRAY ARRAY['attendance_workers', 'attendance_days', 'attendance_settings'] LOOP
        EXECUTE format(
            'CREATE TRIGGER trg_%1$s_stamp BEFORE INSERT OR UPDATE ON %1$I
               FOR EACH ROW EXECUTE FUNCTION maint_stamp_row()', t);
        EXECUTE format(
            'CREATE TRIGGER trg_%1$s_a_user BEFORE INSERT OR UPDATE OF created_by_user_id, updated_by_user_id ON %1$I
               FOR EACH ROW EXECUTE FUNCTION maint_ensure_user()', t);
    END LOOP;
END $$;
