-- A DOT code identifies a manufacturing batch (plant + size + week/year), not
-- an individual tire — several tires bought together legitimately share one.
-- The unique index was wrong: it dead-lettered real mounts and cascaded an FK
-- failure onto their assignments. Identity of the ASSET is the row's UUID.
DROP INDEX IF EXISTS idx_tires_dot;
