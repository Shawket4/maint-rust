-- 0015 rewrote tires.total_km (accrued → base) WITHOUT bumping
-- updated_at/sync_version, so any client whose pull cursor was already past
-- those rows keeps the old accrued figure forever — and its locally derived
-- lifetime km double-counts the assignment deltas 0015 subtracted.
-- Re-announce every tire once; migrations are append-only so the fix cannot
-- go into 0015 itself.
UPDATE tires
   SET updated_at   = now(),
       sync_version = sync_version + 1;
