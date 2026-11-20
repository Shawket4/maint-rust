-- The server-side service-invoice mirror was write-only: nothing ever read
-- service_invoices_cache / service_invoice_items_cache (service history is
-- read straight from Falcon), the endpoint that filled them was never called,
-- and its sync could neither stop nor reflect Falcon deletes. Removed with its
-- code rather than left as a second, wrong copy of Falcon's data.
DROP TABLE IF EXISTS service_invoice_items_cache;
DROP TABLE IF EXISTS service_invoices_cache;

-- Redundant with the primary key.
DROP INDEX IF EXISTS idx_vehicles_cache_active;

-- Driver rows carried Falcon's whole driver object (national ID number,
-- document image names, loans). The mirror keeps named columns only.
UPDATE drivers_cache SET raw_payload = '{}'::jsonb;
UPDATE vehicles_cache SET raw_payload = raw_payload - 'driver' WHERE raw_payload ? 'driver';
