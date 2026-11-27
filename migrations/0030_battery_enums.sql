-- Batteries: their own stock item, like tires and oil (2026-11-27).
--
-- The new enum types and values come first, in a migration of their own: a
-- value added to an enum cannot be used in the transaction that adds it, and
-- each migration runs in one transaction.

-- Where a battery is. 'installed' is never written by a garage app: an open
-- install says it (side_effects.rs keeps the two in step).
CREATE TYPE battery_status AS ENUM ('installed', 'in_stock_used', 'charging', 'warranty', 'scrapped');
-- 'stock': new, taken from the shelf (one unit out of the count);
-- 'legacy': registered already on a truck (never on the shelf).
CREATE TYPE battery_origin AS ENUM ('stock', 'legacy');
CREATE TYPE battery_removal_reason AS ENUM ('dead', 'weak', 'damaged', 'moved', 'warranty', 'other');

ALTER TYPE stock_move_kind ADD VALUE IF NOT EXISTS 'battery_debit';
ALTER TYPE stock_move_kind ADD VALUE IF NOT EXISTS 'battery_credit';
-- The hidden done task a fitted battery adds, so the battery due date
-- restarts (as an oil change does for oil).
ALTER TYPE wo_task_kind ADD VALUE IF NOT EXISTS 'battery';
