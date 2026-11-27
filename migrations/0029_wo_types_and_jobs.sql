-- Work-order tasks: a maintenance type, then one of its jobs (2026-11-27).
--
-- Each type had one standard job of the same name, so the two pickers were
-- the same list twice. Now a type holds its jobs ("تشحيم" → "تشحيم صنيه",
-- "تشحيم مقطوره", "تشحيم بلي (الاجناب)"); the old one-per-type jobs leave the
-- list (tasks that used them keep their text).
--
-- Both can be typed:
--   * a type nobody has yet is created from the task that names it (the app
--     mints its id offline and sends the name in category_name_ar; a name an
--     existing type already has means that type);
--   * a job typed under a type is added to that type's jobs.

ALTER TABLE work_order_tasks ADD COLUMN category_name_ar TEXT;

UPDATE standard_jobs j SET is_active = FALSE, updated_at = now()
  FROM maintenance_categories c
 WHERE j.category_id = c.id AND j.norm_key = maint_norm(c.name_ar);

INSERT INTO standard_jobs (name_ar, category_id, norm_key)
SELECT v.name_ar, v.category_id, maint_norm(v.name_ar)
  FROM (VALUES
    ('تشحيم صنيه', 'greasing'), ('تشحيم مقطوره', 'greasing'), ('تشحيم بلي (الاجناب)', 'greasing'),
    ('تغيير زيت موتور', 'oil_change'), ('تكملة زيت موتور', 'oil_change'),
    ('تغيير فلتر زيت', 'oil_filter'),
    ('تغيير فلتر سولار', 'fuel_filter'), ('تغيير فلتر فاصل المياه', 'fuel_filter'),
    ('تغيير فلتر هوا', 'air_filter'), ('تنظيف فلتر هوا', 'air_filter'),
    ('تغيير تيل أمامي', 'brakes'), ('تغيير تيل خلفي', 'brakes'), ('تغيير تيل مقطوره', 'brakes'),
    ('خرط طنابير', 'brakes'), ('ظبط فرامل', 'brakes'), ('تغيير كاسات فرامل', 'brakes'),
    ('تغيير ميه تبريد', 'coolant'), ('تكملة ميه تبريد', 'coolant'), ('غسيل رادياتير', 'coolant'),
    ('تغيير زيت فتيس', 'transmission'), ('تغيير زيت دفرنس', 'transmission'), ('تكملة زيت فتيس', 'transmission'),
    ('تغيير بطارية', 'battery'), ('شحن بطارية', 'battery'), ('تنظيف أقطاب البطارية', 'battery'),
    ('فحص دوري شامل', 'inspection'), ('فحص فرامل', 'inspection'), ('فحص كهربا', 'inspection'), ('فحص عفشة', 'inspection'),
    ('تركيب كاوتش', 'tires'), ('فك كاوتش', 'tires'), ('ترصيص', 'tires'), ('ظبط زوايا', 'tires'),
    ('تبديل أماكن الكاوتش', 'tires'), ('إصلاح بنشر', 'tires')
  ) AS v(name_ar, category_id)
  JOIN maintenance_categories c ON c.id = v.category_id
ON CONFLICT (norm_key) DO UPDATE SET category_id = excluded.category_id, is_active = TRUE, updated_at = now();

-- Named to sort before trg_wot_link, so the job exists when the task is linked.
CREATE OR REPLACE FUNCTION maint_task_type_and_job() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    name TEXT := NULLIF(btrim(NEW.category_name_ar), '');
    same TEXT;
BEGIN
    IF NEW.category_id IS NOT NULL
       AND NOT EXISTS (SELECT 1 FROM maintenance_categories WHERE id = NEW.category_id) THEN
        IF name IS NULL THEN
            RAISE EXCEPTION 'unknown maintenance type %', NEW.category_id USING ERRCODE = '23503';
        END IF;
        SELECT id INTO same FROM maintenance_categories WHERE maint_norm(name_ar) = maint_norm(name) LIMIT 1;
        IF same IS NOT NULL THEN
            NEW.category_id := same;
        ELSE
            INSERT INTO maintenance_categories (id, name_ar, name_en, sort_order)
            VALUES (NEW.category_id, name, name,
                    (SELECT COALESCE(MAX(sort_order), 0) + 1 FROM maintenance_categories));
        END IF;
    END IF;
    -- A task with only a type carries the type's name: not a job of its own.
    IF NEW.category_id IS NOT NULL AND maint_norm(NEW.description_ar) <> ''
       AND maint_norm(NEW.description_ar) <> (SELECT maint_norm(name_ar) FROM maintenance_categories WHERE id = NEW.category_id) THEN
        INSERT INTO standard_jobs (name_ar, category_id, norm_key, created_by_user_id)
        VALUES (btrim(NEW.description_ar), NEW.category_id, maint_norm(NEW.description_ar), NEW.updated_by_user_id)
        ON CONFLICT (norm_key) DO NOTHING;
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER trg_wot_a_type_job BEFORE INSERT OR UPDATE OF category_id, category_name_ar, description_ar
    ON work_order_tasks FOR EACH ROW EXECUTE FUNCTION maint_task_type_and_job();
