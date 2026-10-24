-- One-time reset of the pre-rebuild (May 2026) maintenance schema.
--
-- The 2026-07-20 domain rebuild rewrote migrations 0003+ in place, so a DB
-- that applied the old set can never boot the new binary ("migration 3 was
-- previously applied but has been modified"). The owner chose to discard the
-- old data (backed up by deploy.sh first) and let the new migrations build
-- the schema from scratch.
--
-- Self-guarding: it only acts when _sqlx_migrations still records the OLD
-- migration 3 ("maintenance tables"); on any other DB it is a no-op, so
-- running it twice or on a fresh box is safe. Only objects in `public` that
-- are not owned by an extension are dropped (pgcrypto stays). Run with
-- psql --single-transaction -v ON_ERROR_STOP=1.

DO $$
DECLARE
    r record;
BEGIN
    IF to_regclass('public._sqlx_migrations') IS NULL
       OR NOT EXISTS (SELECT 1 FROM public._sqlx_migrations
                      WHERE version = 3 AND description = 'maintenance tables') THEN
        RAISE NOTICE 'reset_legacy_schema: no legacy schema found, nothing to do';
        RETURN;
    END IF;

    RAISE NOTICE 'reset_legacy_schema: legacy schema found, dropping it';

    -- Views first (they depend on tables), then tables, then what is left.
    FOR r IN
        SELECT c.relname, c.relkind FROM pg_class c
        WHERE c.relnamespace = 'public'::regnamespace
          AND c.relkind IN ('v', 'm', 'r', 'p', 'S')
          AND NOT EXISTS (SELECT 1 FROM pg_depend d
                          WHERE d.classid = 'pg_class'::regclass
                            AND d.objid = c.oid AND d.deptype = 'e')
        ORDER BY CASE c.relkind WHEN 'm' THEN 0 WHEN 'v' THEN 1 WHEN 'S' THEN 3 ELSE 2 END
    LOOP
        -- An earlier CASCADE may already have taken this one.
        IF to_regclass(format('public.%I', r.relname)) IS NULL THEN
            CONTINUE;
        END IF;
        EXECUTE format('DROP %s public.%I CASCADE',
            CASE r.relkind WHEN 'v' THEN 'VIEW' WHEN 'm' THEN 'MATERIALIZED VIEW'
                           WHEN 'S' THEN 'SEQUENCE' ELSE 'TABLE' END,
            r.relname);
    END LOOP;

    FOR r IN
        SELECT p.oid::regprocedure AS sig, p.prokind FROM pg_proc p
        WHERE p.pronamespace = 'public'::regnamespace
          AND NOT EXISTS (SELECT 1 FROM pg_depend d
                          WHERE d.classid = 'pg_proc'::regclass
                            AND d.objid = p.oid AND d.deptype = 'e')
    LOOP
        EXECUTE format('DROP %s %s CASCADE',
            CASE r.prokind WHEN 'p' THEN 'PROCEDURE' WHEN 'a' THEN 'AGGREGATE' ELSE 'FUNCTION' END,
            r.sig);
    END LOOP;

    -- Standalone enums, domains and composite types (not table row types).
    FOR r IN
        SELECT t.typname FROM pg_type t
        WHERE t.typnamespace = 'public'::regnamespace
          AND t.typtype IN ('e', 'd', 'c')
          AND (t.typtype <> 'c'
               OR (SELECT relkind FROM pg_class WHERE oid = t.typrelid) = 'c')
          AND NOT EXISTS (SELECT 1 FROM pg_depend d
                          WHERE d.classid = 'pg_type'::regclass
                            AND d.objid = t.oid AND d.deptype = 'e')
    LOOP
        EXECUTE format('DROP TYPE IF EXISTS public.%I CASCADE', r.typname);
    END LOOP;
END
$$;
