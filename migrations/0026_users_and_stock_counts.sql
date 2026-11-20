-- People are rows, not bare numbers.
--
-- Every owned row says who created and last changed it, as a Falcon user id.
-- users_cache is the local mirror of those people (name + permission), filled
-- at sign-in and hourly from Falcon; the id columns now reference it. An id
-- the mirror has not seen yet gets a placeholder row with no name, filled in
-- by the next refresh — a write is never refused because of it.
CREATE TABLE users_cache (
    id         BIGINT PRIMARY KEY,
    name       TEXT,
    permission INT,
    fetched_at TIMESTAMPTZ
);

CREATE OR REPLACE FUNCTION maint_ensure_user() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO users_cache (id)
    SELECT DISTINCT x FROM unnest(ARRAY[NEW.created_by_user_id, NEW.updated_by_user_id]) x
     WHERE x IS NOT NULL
    ON CONFLICT (id) DO NOTHING;
    RETURN NEW;
END $$;

DO $$
DECLARE t TEXT;
BEGIN
    FOR t IN
        SELECT c.table_name
          FROM information_schema.columns c
          JOIN information_schema.tables tb
            ON tb.table_schema = c.table_schema AND tb.table_name = c.table_name AND tb.table_type = 'BASE TABLE'
         WHERE c.table_schema = current_schema() AND c.column_name = 'created_by_user_id'
           AND EXISTS (SELECT 1 FROM information_schema.columns u
                        WHERE u.table_schema = c.table_schema AND u.table_name = c.table_name
                          AND u.column_name = 'updated_by_user_id')
         ORDER BY 1
    LOOP
        EXECUTE format(
            'INSERT INTO users_cache (id)
               SELECT DISTINCT u FROM (SELECT created_by_user_id AS u FROM %1$I
                                       UNION SELECT updated_by_user_id FROM %1$I) s
                WHERE u IS NOT NULL
             ON CONFLICT (id) DO NOTHING', t);
        EXECUTE format(
            'CREATE TRIGGER trg_%1$s_a_user BEFORE INSERT OR UPDATE OF created_by_user_id, updated_by_user_id ON %1$I
               FOR EACH ROW EXECUTE FUNCTION maint_ensure_user()', t);
        EXECUTE format(
            'ALTER TABLE %1$I ADD CONSTRAINT fk_%1$s_created_by FOREIGN KEY (created_by_user_id) REFERENCES users_cache(id)', t);
        EXECUTE format(
            'ALTER TABLE %1$I ADD CONSTRAINT fk_%1$s_updated_by FOREIGN KEY (updated_by_user_id) REFERENCES users_cache(id)', t);
    END LOOP;
END $$;

-- Who recorded a stock movement (nullable: old rows have none).
INSERT INTO users_cache (id) SELECT DISTINCT user_id FROM stock_movements WHERE user_id IS NOT NULL ON CONFLICT DO NOTHING;
CREATE OR REPLACE FUNCTION maint_ensure_sm_user() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.user_id IS NOT NULL THEN
        INSERT INTO users_cache (id) VALUES (NEW.user_id) ON CONFLICT (id) DO NOTHING;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_stock_movements_a_user BEFORE INSERT OR UPDATE OF user_id ON stock_movements
    FOR EACH ROW EXECUTE FUNCTION maint_ensure_sm_user();
ALTER TABLE stock_movements ADD CONSTRAINT fk_stock_movements_user FOREIGN KEY (user_id) REFERENCES users_cache(id);

-- A stock count: "we counted N on the shelf". Recorded as the credit or debit
-- that brings the book figure to N, marked so the ledger can say why.
ALTER TABLE stock_movements ADD COLUMN is_count BOOLEAN NOT NULL DEFAULT false;
