-- =========================================================
-- vehicles_cache: mirror of Falcon /api/cars
-- =========================================================
CREATE TABLE vehicles_cache (
    id                              INT PRIMARY KEY,
    car_no_plate                    TEXT NOT NULL,
    car_type                        TEXT,
    transporter                     TEXT,
    tank_capacity                   INT,
    json_compartments               JSONB,
    license_expiration_date         DATE,
    calibration_expiration_date     DATE,
    tank_license_expiration_date    DATE,
    is_in_trip                      BOOLEAN NOT NULL DEFAULT FALSE,
    is_approved                     BOOLEAN NOT NULL DEFAULT FALSE,
    location                        TEXT,
    lat                             NUMERIC(10, 7),
    long                            NUMERIC(10, 7),
    location_time_stamp             TIMESTAMPTZ,
    engine_status                   TEXT,
    speed                           INT,
    last_fuel_odometer              INT,
    last_oil_change_id              INT,
    driver_id                       INT,
    operating_company               TEXT,
    operating_area                  TEXT,
    geo_fence                       TEXT,
    slack_status                    TEXT,
    last_updated_slack_status       TIMESTAMPTZ,
    etit_car_id                     TEXT,
    car_license_url                 TEXT,
    car_license_back_url            TEXT,
    calibration_license_url         TEXT,
    calibration_license_back_url    TEXT,
    tank_license_url                TEXT,
    tank_license_back_url           TEXT,
    raw_payload                     JSONB NOT NULL,
    source_created_at               TIMESTAMPTZ,
    source_updated_at               TIMESTAMPTZ,
    source_deleted_at               TIMESTAMPTZ,
    fetched_at                      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_vehicles_cache_plate  ON vehicles_cache(car_no_plate);
CREATE INDEX idx_vehicles_cache_active ON vehicles_cache(id) WHERE source_deleted_at IS NULL;

-- =========================================================
-- drivers_cache: extracted from nested driver field
-- =========================================================
CREATE TABLE drivers_cache (
    id                                  INT PRIMARY KEY,
    name                                TEXT NOT NULL,
    mobile_number                       TEXT,
    transporter                         TEXT,
    id_license_expiration_date          DATE,
    driver_license_expiration_date      DATE,
    safety_license_expiration_date      DATE,
    drug_test_expiration_date           DATE,
    is_approved                         BOOLEAN NOT NULL DEFAULT FALSE,
    raw_payload                         JSONB NOT NULL,
    source_created_at                   TIMESTAMPTZ,
    source_updated_at                   TIMESTAMPTZ,
    fetched_at                          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- =========================================================
-- service_invoices_cache: mirror of Falcon /api/service-invoices
-- =========================================================
CREATE TABLE service_invoices_cache (
    id                          INT PRIMARY KEY,
    car_id                      INT NOT NULL,
    driver_name                 TEXT,
    date                        TIMESTAMPTZ,
    meter_reading               INT,
    plate_number                TEXT,
    supervisor                  TEXT,
    operating_region            TEXT,
    raw_payload                 JSONB NOT NULL,
    source_created_at           TIMESTAMPTZ,
    source_updated_at           TIMESTAMPTZ,
    source_deleted_at           TIMESTAMPTZ,
    fetched_at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_invoices_cache_car   ON service_invoices_cache(car_id);
CREATE INDEX idx_invoices_cache_date  ON service_invoices_cache(date DESC);

CREATE TABLE service_invoice_items_cache (
    id                          INT PRIMARY KEY,
    service_invoice_id          INT NOT NULL,
    service                     TEXT NOT NULL,
    notes                       TEXT,
    item_order                  INT,
    raw_payload                 JSONB NOT NULL,
    source_created_at           TIMESTAMPTZ,
    source_updated_at           TIMESTAMPTZ,
    fetched_at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_invoice_items_invoice ON service_invoice_items_cache(service_invoice_id);
