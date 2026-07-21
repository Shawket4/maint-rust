-- A dismount can happen under a different work order than the mount that
-- created the assignment (work_order_id belongs to the MOUNT). Without its own
-- attribution, a dismount performed in WO-B on a tire mounted in WO-A (or
-- onboarded with no WO at all) never appears in WO-B's tire work.
ALTER TABLE tire_assignments
    ADD COLUMN dismount_work_order_id UUID REFERENCES work_orders(id) ON DELETE SET NULL;
CREATE INDEX idx_ta_dismount_wo ON tire_assignments(dismount_work_order_id);
