INSERT INTO maintenance_categories (id, name_ar, name_en, icon, color, sort_order) VALUES
  ('oil_change',   'تغيير زيت',  'Oil Change',          'droplet',     'amber',  10),
  ('air_filter',   'فلتر هواء',  'Air Filter',          'wind',        'sky',    20),
  ('fuel_filter',  'فلتر سولار', 'Fuel Filter',         'fuel',        'orange', 30),
  ('oil_filter',   'فلتر زيت',   'Oil Filter',          'filter',      'amber',  40),
  ('brakes',       'فرامل',     'Brakes',              'octagon',     'red',    50),
  ('coolant',      'مياه تبريد', 'Coolant',             'thermometer', 'cyan',   60),
  ('transmission', 'زيت فتيس',   'Transmission Oil',    'cog',         'violet', 70),
  ('battery',      'بطارية',    'Battery',             'battery',     'green',  80),
  ('inspection',   'فحص دوري',  'Periodic Inspection', 'clipboard',   'slate',  90),
  ('greasing',     'تشحيم',     'Greasing',            'wrench',      'zinc',   100)
ON CONFLICT (id) DO NOTHING;
