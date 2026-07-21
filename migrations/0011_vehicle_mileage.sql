-- vehicles_cache.mileage — written by the Falcon car projection upsert
-- (falcon/cars.rs). Distinct from last_fuel_odometer; used as a Falcon odometer source.
ALTER TABLE vehicles_cache ADD COLUMN IF NOT EXISTS mileage INT;
