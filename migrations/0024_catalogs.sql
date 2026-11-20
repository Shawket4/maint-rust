-- Catalogs: what used to be typed is now a row.
--
-- Tire brand/model/size and oil type were free text on every tire, oil change,
-- stock line and stock movement. One typo ("Pireli") made a second stock line
-- and a tire nobody could match to it. Now:
--
--   tire_products / oil_products  — one row per real product;
--   standard_jobs                 — the jobs a garage does over and over.
--
-- The TEXT stays on each row (older apps send only text, and Falcon's stock
-- ledger is keyed by text), and a trigger on every table that carries it
-- resolves the text to a catalog row — creating the row the first time a new
-- product is seen. So the catalog learns by itself, a picker can offer it,
-- and nothing has to be typed the same way twice. Two spellings of one
-- product are MERGED (merged_into): every later arrival of the old spelling
-- resolves to the surviving product and is rewritten to its spelling.

-- Text → comparison key: case, spacing, punctuation, Arabic letter variants
-- and digit scripts do not make a different product.
CREATE OR REPLACE FUNCTION maint_norm(t TEXT) RETURNS TEXT
LANGUAGE sql IMMUTABLE AS $$
    SELECT regexp_replace(
             lower(translate(COALESCE(t, ''),
                             'أإآٱةىؤئ٠١٢٣٤٥٦٧٨٩۰۱۲۳۴۵۶۷۸۹',
                             'ااااهيوي01234567890123456789')),
             '[^0-9a-z' || chr(1569) || '-' || chr(1610) || ']+', '', 'g')
$$;

CREATE TABLE tire_products (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    brand               TEXT NOT NULL CHECK (btrim(brand) <> ''),
    model               TEXT NOT NULL DEFAULT '',
    size                TEXT NOT NULL DEFAULT '',
    norm_key            TEXT NOT NULL UNIQUE,
    merged_into         UUID REFERENCES tire_products(id),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by_user_id  BIGINT
);

CREATE TABLE oil_products (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name                TEXT NOT NULL CHECK (btrim(name) <> ''),
    norm_key            TEXT NOT NULL UNIQUE,
    merged_into         UUID REFERENCES oil_products(id),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by_user_id  BIGINT
);

CREATE TABLE standard_jobs (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name_ar             TEXT NOT NULL CHECK (btrim(name_ar) <> ''),
    name_en             TEXT,
    -- Ticking a task of this job restarts this maintenance's clock.
    category_id         TEXT REFERENCES maintenance_categories(id),
    norm_key            TEXT NOT NULL UNIQUE,
    is_active           BOOLEAN NOT NULL DEFAULT TRUE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by_user_id  BIGINT
);

-- Find the product for a spelling, following merges; create it when new.
CREATE OR REPLACE FUNCTION maint_tire_product(p_brand TEXT, p_model TEXT, p_size TEXT, p_user BIGINT)
RETURNS UUID LANGUAGE plpgsql AS $$
DECLARE
    k TEXT := maint_norm(p_brand) || '|' || maint_norm(p_model) || '|' || maint_norm(p_size);
    found UUID;
    hops INT := 0;
BEGIN
    IF maint_norm(p_brand) = '' THEN RETURN NULL; END IF;
    INSERT INTO tire_products (brand, model, size, norm_key, created_by_user_id)
    VALUES (btrim(p_brand), COALESCE(btrim(p_model), ''), COALESCE(btrim(p_size), ''), k, p_user)
    ON CONFLICT (norm_key) DO NOTHING;
    SELECT id INTO found FROM tire_products WHERE norm_key = k;
    WHILE hops < 8 LOOP
        EXIT WHEN (SELECT merged_into FROM tire_products WHERE id = found) IS NULL;
        SELECT merged_into INTO found FROM tire_products WHERE id = found;
        hops := hops + 1;
    END LOOP;
    RETURN found;
END $$;

CREATE OR REPLACE FUNCTION maint_oil_product(p_name TEXT, p_user BIGINT)
RETURNS UUID LANGUAGE plpgsql AS $$
DECLARE
    k TEXT := maint_norm(p_name);
    found UUID;
    hops INT := 0;
BEGIN
    IF k = '' THEN RETURN NULL; END IF;
    INSERT INTO oil_products (name, norm_key, created_by_user_id)
    VALUES (btrim(p_name), k, p_user)
    ON CONFLICT (norm_key) DO NOTHING;
    SELECT id INTO found FROM oil_products WHERE norm_key = k;
    WHILE hops < 8 LOOP
        EXIT WHEN (SELECT merged_into FROM oil_products WHERE id = found) IS NULL;
        SELECT merged_into INTO found FROM oil_products WHERE id = found;
        hops := hops + 1;
    END LOOP;
    RETURN found;
END $$;

-- ── links ───────────────────────────────────────────────────────────────────
ALTER TABLE tires            ADD COLUMN tire_product_id UUID REFERENCES tire_products(id);
ALTER TABLE oil_changes      ADD COLUMN oil_product_id  UUID REFERENCES oil_products(id);
ALTER TABLE work_order_tasks ADD COLUMN standard_job_id UUID REFERENCES standard_jobs(id);
ALTER TABLE stock_movements  ADD COLUMN tire_product_id UUID REFERENCES tire_products(id),
                             ADD COLUMN oil_product_id  UUID REFERENCES oil_products(id);
ALTER TABLE tire_stock_cache ADD COLUMN tire_product_id UUID REFERENCES tire_products(id);
ALTER TABLE oil_stock_cache  ADD COLUMN oil_product_id  UUID REFERENCES oil_products(id);
CREATE INDEX idx_tires_product ON tires(tire_product_id);
CREATE INDEX idx_oil_product   ON oil_changes(oil_product_id);
CREATE INDEX idx_wot_job       ON work_order_tasks(standard_job_id);

-- Text in, link out — and the surviving product's spelling back onto the row.
CREATE OR REPLACE FUNCTION maint_link_tire_product() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    uid BIGINT := NULLIF(to_jsonb(NEW) ->> 'updated_by_user_id', '')::bigint;
    p tire_products%ROWTYPE;
BEGIN
    IF NEW.brand IS NULL OR btrim(NEW.brand) = '' THEN
        NEW.tire_product_id := NULL;
        RETURN NEW;
    END IF;
    NEW.tire_product_id := maint_tire_product(NEW.brand, NEW.model, NEW.size, uid);
    SELECT * INTO p FROM tire_products WHERE id = NEW.tire_product_id;
    IF FOUND THEN
        NEW.brand := p.brand;
        NEW.model := NULLIF(p.model, '');
        NEW.size  := NULLIF(p.size, '');
    END IF;
    RETURN NEW;
END $$;

-- The stock cache keys on ('' not NULL) for model/size.
CREATE OR REPLACE FUNCTION maint_link_tire_stock() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE p tire_products%ROWTYPE;
BEGIN
    NEW.tire_product_id := maint_tire_product(NEW.brand, NEW.model, NEW.size, NULL);
    SELECT * INTO p FROM tire_products WHERE id = NEW.tire_product_id;
    IF FOUND THEN
        NEW.brand := p.brand; NEW.model := p.model; NEW.size := p.size;
    END IF;
    RETURN NEW;
END $$;

CREATE OR REPLACE FUNCTION maint_link_oil_product() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    uid BIGINT := NULLIF(to_jsonb(NEW) ->> 'updated_by_user_id', '')::bigint;
    canonical TEXT;
BEGIN
    IF NEW.oil_type IS NULL OR btrim(NEW.oil_type) = '' THEN
        NEW.oil_product_id := NULL;
        RETURN NEW;
    END IF;
    NEW.oil_product_id := maint_oil_product(NEW.oil_type, uid);
    SELECT name INTO canonical FROM oil_products WHERE id = NEW.oil_product_id;
    IF canonical IS NOT NULL THEN NEW.oil_type := canonical; END IF;
    RETURN NEW;
END $$;

CREATE OR REPLACE FUNCTION maint_link_movement() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE p tire_products%ROWTYPE; canonical TEXT;
BEGIN
    IF NEW.kind IN ('tire_debit', 'tire_credit') AND NEW.brand IS NOT NULL AND btrim(NEW.brand) <> '' THEN
        NEW.tire_product_id := maint_tire_product(NEW.brand, NEW.model, NEW.size, NEW.user_id);
        SELECT * INTO p FROM tire_products WHERE id = NEW.tire_product_id;
        IF FOUND THEN NEW.brand := p.brand; NEW.model := p.model; NEW.size := p.size; END IF;
    ELSIF NEW.kind IN ('oil_debit', 'oil_credit') AND NEW.oil_type IS NOT NULL AND btrim(NEW.oil_type) <> '' THEN
        NEW.oil_product_id := maint_oil_product(NEW.oil_type, NEW.user_id);
        SELECT name INTO canonical FROM oil_products WHERE id = NEW.oil_product_id;
        IF canonical IS NOT NULL THEN NEW.oil_type := canonical; END IF;
    END IF;
    RETURN NEW;
END $$;

-- A task whose text IS a standard job is linked to it. A plain sentence stays
-- a plain sentence: jobs are added on purpose, not learned from every note.
CREATE OR REPLACE FUNCTION maint_link_job() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    NEW.standard_job_id := (
        SELECT id FROM standard_jobs
         WHERE is_active AND norm_key = maint_norm(NEW.description_ar) AND maint_norm(NEW.description_ar) <> ''
         LIMIT 1);
    RETURN NEW;
END $$;

-- Named to sort before trg_*_stamp (migration 0021), so the link is in place
-- when the row is stamped.
CREATE TRIGGER trg_tires_link    BEFORE INSERT OR UPDATE OF brand, model, size ON tires
    FOR EACH ROW EXECUTE FUNCTION maint_link_tire_product();
CREATE TRIGGER trg_oil_link      BEFORE INSERT OR UPDATE OF oil_type ON oil_changes
    FOR EACH ROW EXECUTE FUNCTION maint_link_oil_product();
CREATE TRIGGER trg_wot_link      BEFORE INSERT OR UPDATE OF description_ar ON work_order_tasks
    FOR EACH ROW EXECUTE FUNCTION maint_link_job();
CREATE TRIGGER trg_sm_link       BEFORE INSERT ON stock_movements
    FOR EACH ROW EXECUTE FUNCTION maint_link_movement();
CREATE TRIGGER trg_tsc_link      BEFORE INSERT OR UPDATE OF brand, model, size ON tire_stock_cache
    FOR EACH ROW EXECUTE FUNCTION maint_link_tire_stock();
CREATE TRIGGER trg_osc_link      BEFORE INSERT OR UPDATE OF oil_type ON oil_stock_cache
    FOR EACH ROW EXECUTE FUNCTION maint_link_oil_product();

-- ── bring what is already stored into the catalog ───────────────────────────
-- Rewriting a column to itself fires the link triggers.
UPDATE tires       SET brand = brand;
UPDATE oil_changes SET oil_type = oil_type;
UPDATE work_order_tasks SET description_ar = description_ar WHERE description_ar IS NOT NULL;

UPDATE stock_movements m
   SET tire_product_id = maint_tire_product(m.brand, m.model, m.size, m.user_id)
 WHERE m.kind IN ('tire_debit', 'tire_credit') AND m.brand IS NOT NULL AND btrim(m.brand) <> '';
UPDATE stock_movements m
   SET oil_product_id = maint_oil_product(m.oil_type, m.user_id)
 WHERE m.kind IN ('oil_debit', 'oil_credit') AND m.oil_type IS NOT NULL AND btrim(m.oil_type) <> '';

-- Stock lines: two spellings of one product fold into one line.
CREATE TEMP TABLE _tsc ON COMMIT DROP AS
    SELECT maint_tire_product(brand, model, size, NULL) AS pid, SUM(on_hand_qty)::int AS qty, MAX(fetched_at) AS at
      FROM tire_stock_cache WHERE btrim(brand) <> '' GROUP BY 1;
DELETE FROM tire_stock_cache;
INSERT INTO tire_stock_cache (brand, model, size, on_hand_qty, fetched_at)
    SELECT p.brand, p.model, p.size, t.qty, t.at FROM _tsc t JOIN tire_products p ON p.id = t.pid;

CREATE TEMP TABLE _osc ON COMMIT DROP AS
    SELECT maint_oil_product(oil_type, NULL) AS pid, SUM(liters_on_hand) AS liters, MAX(fetched_at) AS at
      FROM oil_stock_cache WHERE btrim(oil_type) <> '' GROUP BY 1;
DELETE FROM oil_stock_cache;
INSERT INTO oil_stock_cache (oil_type, liters_on_hand, fetched_at)
    SELECT p.name, o.liters, o.at FROM _osc o JOIN oil_products p ON p.id = o.pid;

CREATE UNIQUE INDEX uq_tire_stock_product ON tire_stock_cache(tire_product_id);
CREATE UNIQUE INDEX uq_oil_stock_product  ON oil_stock_cache(oil_product_id);

-- A starting list of jobs: one per maintenance type. Garages add their own.
INSERT INTO standard_jobs (name_ar, name_en, category_id, norm_key)
    SELECT c.name_ar, c.name_en, c.id, maint_norm(c.name_ar)
      FROM maintenance_categories c
    ON CONFLICT (norm_key) DO NOTHING;
UPDATE work_order_tasks SET description_ar = description_ar WHERE description_ar IS NOT NULL;

-- ── vehicles are rows, not loose numbers ────────────────────────────────────
-- Every owned table names a vehicle by Falcon's id with nothing behind it.
-- vehicles_cache is the local vehicles table; make the reference real. A
-- vehicle the mirror has not seen yet (a truck added in Falcon minutes ago)
-- gets a placeholder row, filled in by the next fleet refresh — a work order
-- must never be refused because the mirror is a few minutes behind.
CREATE OR REPLACE FUNCTION maint_ensure_vehicle() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.vehicle_id IS NOT NULL THEN
        INSERT INTO vehicles_cache (id, car_no_plate, raw_payload)
        VALUES (NEW.vehicle_id, '#' || NEW.vehicle_id, '{"placeholder": true}'::jsonb)
        ON CONFLICT (id) DO NOTHING;
    END IF;
    RETURN NEW;
END $$;

DO $$
DECLARE t TEXT;
BEGIN
    FOREACH t IN ARRAY ARRAY[
        'work_orders', 'oil_changes', 'tire_assignments', 'chassis_layouts',
        'vehicle_class_assignments', 'vehicle_odometer_overrides'
    ] LOOP
        EXECUTE format(
            'INSERT INTO vehicles_cache (id, car_no_plate, raw_payload)
               SELECT DISTINCT x.vehicle_id, ''#'' || x.vehicle_id, ''{"placeholder": true}''::jsonb
                 FROM %I x WHERE x.vehicle_id IS NOT NULL
             ON CONFLICT (id) DO NOTHING', t);
        EXECUTE format(
            'CREATE TRIGGER trg_%s_a_vehicle BEFORE INSERT OR UPDATE OF vehicle_id ON %I
               FOR EACH ROW EXECUTE FUNCTION maint_ensure_vehicle()', t, t);
        EXECUTE format(
            'ALTER TABLE %I ADD CONSTRAINT fk_%s_vehicle FOREIGN KEY (vehicle_id) REFERENCES vehicles_cache(id)', t, t);
    END LOOP;
END $$;

-- An assignment's vehicle is the vehicle its position belongs to. (Checked
-- in code for new mounts since 0021; this makes it a fact of the schema.)
ALTER TABLE stock_movements
    ADD CONSTRAINT fk_sm_work_order FOREIGN KEY (work_order_id) REFERENCES work_orders(id) NOT VALID;
