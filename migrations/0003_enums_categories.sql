-- =========================================================
-- Shared enums for the maintenance domain
-- =========================================================
CREATE TYPE work_order_status AS ENUM ('open', 'closed');
CREATE TYPE wo_task_kind      AS ENUM ('free', 'tire', 'oil');

CREATE TYPE tire_status       AS ENUM ('mounted', 'in_stock_used', 'in_repair', 'retreading', 'scrapped');
CREATE TYPE tire_origin       AS ENUM ('shipment', 'legacy');
CREATE TYPE tire_event_type   AS ENUM ('scrap', 'retread', 'repair_out', 'repair_in', 'restock', 'intake');

CREATE TYPE mount_reason      AS ENUM ('new', 'rotation', 'replacement', 'repair_return', 'onboard');
CREATE TYPE dismount_reason   AS ENUM ('worn', 'damaged', 'rotation', 'puncture', 'retread', 'other');

CREATE TYPE plan_trigger      AS ENUM ('mileage', 'time');
CREATE TYPE stock_move_kind   AS ENUM ('tire_debit', 'oil_debit', 'tire_credit', 'oil_credit', 'adjustment');

-- Chassis enums
CREATE TYPE chassis_section AS ENUM ('tractor', 'trailer');
CREATE TYPE axle_type       AS ENUM ('single', 'dual');
CREATE TYPE position_side   AS ENUM ('left', 'right');
CREATE TYPE position_depth  AS ENUM ('single', 'inner', 'outer');

-- =========================================================
-- maintenance_categories: TEXT PK lookup
-- =========================================================
CREATE TABLE maintenance_categories (
    id          TEXT PRIMARY KEY,
    name_ar     TEXT NOT NULL,
    name_en     TEXT NOT NULL,
    icon        TEXT,
    color       TEXT,
    sort_order  INT NOT NULL DEFAULT 0
);
