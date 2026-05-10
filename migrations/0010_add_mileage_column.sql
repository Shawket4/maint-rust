-- =========================================================
-- 0010_add_mileage_column.sql: Add mileage column to vehicles_cache
-- =========================================================
ALTER TABLE vehicles_cache ADD COLUMN IF NOT EXISTS mileage INT;

-- Update existing mileage from last_fuel_odometer
UPDATE vehicles_cache SET mileage = last_fuel_odometer WHERE mileage IS NULL;
