use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use super::cache::CacheManager;
use super::client::FalconClient;
use crate::error::ApiResult;

pub const FALCON_CARS_PATH: &str = "/api/cars";
pub const CACHE_KEY_CARS: &str = "falcon:cars";



/// Fetch cars from Falcon (cached for `ttl_seconds`).
///
/// Returns the parsed Falcon array (raw — caller decides how to project).
pub async fn fetch_cars_cached(
    falcon: &FalconClient,
    cache: &CacheManager,
    token: &str,
    ttl_seconds: u64,
) -> ApiResult<Value> {
    if let Some(hit) = cache.get_json::<Value>(CACHE_KEY_CARS).await? {
        tracing::debug!("falcon cars cache hit");
        return Ok(hit);
    }
    tracing::debug!("falcon cars cache miss");
    let v = falcon.get_json(FALCON_CARS_PATH, token).await?;
    cache.set_json(CACHE_KEY_CARS, &v, ttl_seconds).await?;
    Ok(v)
}

/// The only driver fields the maintenance app has a use for. Falcon's driver
/// object also carries the national ID number, document image names, loans
/// and expenses; none of that belongs in the garage app, its local database,
/// this cache or its backups.
const DRIVER_FIELDS: &[&str] = &[
    "ID", "id", "name", "mobile_number", "transporter",
    "id_license_expiration_date", "driver_license_expiration_date",
    "safety_license_expiration_date", "drug_test_expiration_date", "is_approved",
];

fn scrub_driver(d: &Value) -> Value {
    match d.as_object() {
        Some(o) => Value::Object(
            o.iter().filter(|(k, _)| DRIVER_FIELDS.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect(),
        ),
        None => Value::Null,
    }
}

/// Falcon dates arrive as "YYYY-MM-DD…" strings. Take the first ten
/// CHARACTERS: slicing bytes panicked on any non-ASCII value (Arabic-Indic
/// digits) and took /cache/vehicles down for every client.
fn parse_date(v: Option<&Value>) -> Option<chrono::NaiveDate> {
    let s: String = v?.as_str()?.trim().chars().take(10).collect();
    chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()
}

/// Falcon numbers as i32, or None when missing or out of range (a wrapped
/// odometer is worse than no odometer).
fn int32(v: Option<&Value>) -> Option<i32> {
    let v = v?;
    let n = v.as_i64().or_else(|| v.as_f64().filter(|f| f.is_finite()).map(|f| f.round() as i64))?;
    i32::try_from(n).ok()
}

fn float(v: Option<&Value>) -> Option<f64> {
    let v = v?;
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).filter(|f| f.is_finite())
}

fn row_id(v: &Value) -> Option<i32> {
    int32(v.get("ID")).or_else(|| int32(v.get("id"))).filter(|id| *id > 0)
}

/// Project Falcon's cars response into vehicles + drivers arrays, sorted by
/// id (a stable write order keeps two concurrent refreshes from deadlocking).
pub fn project_vehicles_and_drivers(raw: &Value) -> (Vec<Value>, Vec<Value>) {
    let arr = match raw.as_array() {
        Some(a) => a.clone(),
        None => raw.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default(),
    };

    let mut vehicles = Vec::with_capacity(arr.len());
    let mut drivers_by_id: std::collections::BTreeMap<i32, Value> = Default::default();

    for car in &arr {
        let mut car = car.clone();
        if let Some(d) = car.get("driver").filter(|d| d.is_object()) {
            let clean = scrub_driver(d);
            if let Some(id) = row_id(&clean) {
                drivers_by_id.entry(id).or_insert_with(|| clean.clone());
            }
            car["driver"] = clean;
        }
        vehicles.push(car);
    }
    vehicles.sort_by_key(|v| row_id(v).unwrap_or(i32::MAX));

    let drivers: Vec<Value> = drivers_by_id.into_values().collect();
    (vehicles, drivers)
}

/// Upsert vehicles + drivers into Postgres cache tables.
///
/// Idempotent: re-running with the same data is a no-op (last-write-wins per row).
/// Marks rows missing from the new payload as `source_deleted_at = now()`.
pub async fn upsert_cars_into_cache(
    pool: &PgPool,
    vehicles: &mut [Value],
    drivers: &[Value],
) -> ApiResult<()> {
    let mut tx = pool.begin().await?;

    // Build the set of seen ids so we can soft-delete missing rows.
    let mut seen_vehicle_ids: Vec<i32> = Vec::with_capacity(vehicles.len());

    for v in vehicles {
        let Some(id) = row_id(v) else {
            continue;
        };
        seen_vehicle_ids.push(id);

        let car_no_plate = v
            .get("car_no_plate")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let car_type = v.get("car_type").and_then(|x| x.as_str()).map(str::to_string);
        let transporter = v.get("transporter").and_then(|x| x.as_str()).map(str::to_string);
        let tank_capacity = int32(v.get("tank_capacity"));
        let json_compartments = v.get("json_compartments").cloned();
        let license_expiration_date = parse_date(v.get("license_expiration_date"));
        let calibration_expiration_date = parse_date(v.get("calibration_expiration_date"));
        let tank_license_expiration_date = parse_date(v.get("tank_license_expiration_date"));
        let is_in_trip = v.get("is_in_trip").and_then(|x| x.as_bool()).unwrap_or(false);
        let is_approved = v.get("is_approved").and_then(|x| x.as_bool()).unwrap_or(false);
        let location = v.get("location").and_then(|x| x.as_str()).map(str::to_string);
        let lat = float(v.get("lat"));
        let long = float(v.get("long"));
        let location_time_stamp = v
            .get("location_time_stamp")
            .and_then(|x| x.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc));
        let engine_status = v.get("engine_status").and_then(|x| x.as_str()).map(str::to_string);
        let speed = int32(v.get("speed"));
        let last_fuel_odometer = int32(v.get("last_fuel_odometer"));
        let last_oil_change_id = int32(v.get("last_oil_change_id"));
        // The mirror stays PURE Falcon data. Odometer overrides participate in
        // arbitration only through v_current_odometer (0014, where downward
        // corrections work via superseded_km) — merging them here was an
        // upward-only second arbitration rule that contradicted it and
        // contaminated raw_payload.
        // Falcon stores mileage as float64 (oil-change readings can carry a
        // fraction); as_i64 alone turned 123456.5 into NULL.
        let mileage = int32(v.get("mileage"));
        let driver_id = int32(v.get("driver_id"));
        let operating_company = v
            .get("operating_company")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        let operating_area = v.get("operating_area").and_then(|x| x.as_str()).map(str::to_string);
        let geo_fence = v.get("geo_fence").and_then(|x| x.as_str()).map(str::to_string);
        let slack_status = v.get("slack_status").and_then(|x| x.as_str()).map(str::to_string);
        let last_updated_slack_status = v
            .get("last_updated_slack_status")
            .and_then(|x| x.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc));
        let etit_car_id = v.get("etit_car_id").and_then(|x| x.as_str()).map(str::to_string);
        let car_license_url = v.get("car_license_url").and_then(|x| x.as_str()).map(str::to_string);
        let car_license_back_url = v
            .get("car_license_back_url")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        let calibration_license_url = v
            .get("calibration_license_url")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        let calibration_license_back_url = v
            .get("calibration_license_back_url")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        let tank_license_url = v.get("tank_license_url").and_then(|x| x.as_str()).map(str::to_string);
        let tank_license_back_url = v
            .get("tank_license_back_url")
            .and_then(|x| x.as_str())
            .map(str::to_string);

        let lat_dec = lat.map(|f| rust_decimal::Decimal::from_f64_retain(f).unwrap_or_default());
        let long_dec = long.map(|f| rust_decimal::Decimal::from_f64_retain(f).unwrap_or_default());

        sqlx::query(
            r#"
            INSERT INTO vehicles_cache (
                id, car_no_plate, car_type, transporter, tank_capacity, json_compartments,
                license_expiration_date, calibration_expiration_date, tank_license_expiration_date,
                is_in_trip, is_approved, location, lat, long, location_time_stamp,
                engine_status, speed, last_fuel_odometer, last_oil_change_id, mileage, driver_id,
                operating_company, operating_area, geo_fence, slack_status, last_updated_slack_status,
                etit_car_id, car_license_url, car_license_back_url,
                calibration_license_url, calibration_license_back_url,
                tank_license_url, tank_license_back_url,
                raw_payload, source_deleted_at, fetched_at
            ) VALUES (
                $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,
                $21,$22,$23,$24,$25,$26,$27,$28,$29,$30,$31,$32,$33,$34,NULL, now()
            )
            ON CONFLICT (id) DO UPDATE SET
                car_no_plate = EXCLUDED.car_no_plate,
                car_type = EXCLUDED.car_type,
                transporter = EXCLUDED.transporter,
                tank_capacity = EXCLUDED.tank_capacity,
                json_compartments = EXCLUDED.json_compartments,
                license_expiration_date = EXCLUDED.license_expiration_date,
                calibration_expiration_date = EXCLUDED.calibration_expiration_date,
                tank_license_expiration_date = EXCLUDED.tank_license_expiration_date,
                is_in_trip = EXCLUDED.is_in_trip,
                is_approved = EXCLUDED.is_approved,
                location = EXCLUDED.location,
                lat = EXCLUDED.lat,
                long = EXCLUDED.long,
                location_time_stamp = EXCLUDED.location_time_stamp,
                engine_status = EXCLUDED.engine_status,
                speed = EXCLUDED.speed,
                last_fuel_odometer = EXCLUDED.last_fuel_odometer,
                last_oil_change_id = EXCLUDED.last_oil_change_id,
                mileage = EXCLUDED.mileage,
                driver_id = EXCLUDED.driver_id,
                operating_company = EXCLUDED.operating_company,
                operating_area = EXCLUDED.operating_area,
                geo_fence = EXCLUDED.geo_fence,
                slack_status = EXCLUDED.slack_status,
                last_updated_slack_status = EXCLUDED.last_updated_slack_status,
                etit_car_id = EXCLUDED.etit_car_id,
                car_license_url = EXCLUDED.car_license_url,
                car_license_back_url = EXCLUDED.car_license_back_url,
                calibration_license_url = EXCLUDED.calibration_license_url,
                calibration_license_back_url = EXCLUDED.calibration_license_back_url,
                tank_license_url = EXCLUDED.tank_license_url,
                tank_license_back_url = EXCLUDED.tank_license_back_url,
                raw_payload = EXCLUDED.raw_payload,
                source_deleted_at = NULL,
                -- fetched_at is what the odometer views report as "observed":
                -- it moves when a reading changes, not on every poll (a parked
                -- truck used to look freshly read every few minutes).
                fetched_at = CASE
                    WHEN (vehicles_cache.last_fuel_odometer, vehicles_cache.mileage)
                         IS DISTINCT FROM (EXCLUDED.last_fuel_odometer, EXCLUDED.mileage)
                    THEN now() ELSE vehicles_cache.fetched_at END
            "#,
        )
        .bind(id)
        .bind(car_no_plate)
        .bind(car_type)
        .bind(transporter)
        .bind(tank_capacity)
        .bind(json_compartments)
        .bind(license_expiration_date)
        .bind(calibration_expiration_date)
        .bind(tank_license_expiration_date)
        .bind(is_in_trip)
        .bind(is_approved)
        .bind(location)
        .bind(lat_dec)
        .bind(long_dec)
        .bind(location_time_stamp)
        .bind(engine_status)
        .bind(speed)
        .bind(last_fuel_odometer)
        .bind(last_oil_change_id)
        .bind(mileage)
        .bind(driver_id)
        .bind(operating_company)
        .bind(operating_area)
        .bind(geo_fence)
        .bind(slack_status)
        .bind(last_updated_slack_status)
        .bind(etit_car_id)
        .bind(car_license_url)
        .bind(car_license_back_url)
        .bind(calibration_license_url)
        .bind(calibration_license_back_url)
        .bind(tank_license_url)
        .bind(tank_license_back_url)
        .bind(&*v)
        .execute(&mut *tx)
        .await?;
    }

    // Soft-delete vehicles missing from the new payload (Falcon-side delete reflection).
    if !seen_vehicle_ids.is_empty() {
        sqlx::query(
            r#"
            UPDATE vehicles_cache
               SET source_deleted_at = now()
             WHERE source_deleted_at IS NULL
               AND id <> ALL($1)
            "#,
        )
        .bind(&seen_vehicle_ids)
        .execute(&mut *tx)
        .await?;
    }

    // Drivers
    for d in drivers {
        let Some(id) = row_id(d) else {
            continue;
        };
        let name = d.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let mobile_number = d.get("mobile_number").and_then(|x| x.as_str()).map(str::to_string);
        let transporter = d.get("transporter").and_then(|x| x.as_str()).map(str::to_string);
        let id_license_expiration_date = parse_date(d.get("id_license_expiration_date"));
        let driver_license_expiration_date = parse_date(d.get("driver_license_expiration_date"));
        let safety_license_expiration_date = parse_date(d.get("safety_license_expiration_date"));
        let drug_test_expiration_date = parse_date(d.get("drug_test_expiration_date"));
        let is_approved = d.get("is_approved").and_then(|x| x.as_bool()).unwrap_or(false);

        sqlx::query(
            r#"
            INSERT INTO drivers_cache (
                id, name, mobile_number, transporter,
                id_license_expiration_date, driver_license_expiration_date,
                safety_license_expiration_date, drug_test_expiration_date,
                is_approved, raw_payload, fetched_at
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10, now())
            ON CONFLICT (id) DO UPDATE SET
                name = EXCLUDED.name,
                mobile_number = EXCLUDED.mobile_number,
                transporter = EXCLUDED.transporter,
                id_license_expiration_date = EXCLUDED.id_license_expiration_date,
                driver_license_expiration_date = EXCLUDED.driver_license_expiration_date,
                safety_license_expiration_date = EXCLUDED.safety_license_expiration_date,
                drug_test_expiration_date = EXCLUDED.drug_test_expiration_date,
                is_approved = EXCLUDED.is_approved,
                raw_payload = EXCLUDED.raw_payload,
                fetched_at = now()
            "#,
        )
        .bind(id)
        .bind(name)
        .bind(mobile_number)
        .bind(transporter)
        .bind(id_license_expiration_date)
        .bind(driver_license_expiration_date)
        .bind(safety_license_expiration_date)
        .bind(drug_test_expiration_date)
        .bind(is_approved)
        .bind(d)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dates_never_panic_on_non_ascii() {
        assert_eq!(parse_date(Some(&json!("2026-03-01T00:00:00Z"))), chrono::NaiveDate::from_ymd_opt(2026, 3, 1));
        assert_eq!(parse_date(Some(&json!("٢٠٢٦-٠٣-٠١"))), None);
        assert_eq!(parse_date(Some(&json!("غير محدد"))), None);
        assert_eq!(parse_date(Some(&json!(""))), None);
        assert_eq!(parse_date(None), None);
    }

    #[test]
    fn numbers_do_not_wrap() {
        assert_eq!(int32(Some(&json!(123456.5))), Some(123457));
        assert_eq!(int32(Some(&json!(9_999_999_999i64))), None);
        assert_eq!(float(Some(&json!("30.0444"))), Some(30.0444));
    }

    #[test]
    fn drivers_are_scrubbed_everywhere_and_ordered() {
        let raw = json!({"data": [
            {"ID": 9, "car_no_plate": "b", "driver": {"ID": 5, "name": "A", "social_security_number": "2990", "loans": [1]}},
            {"ID": 2, "car_no_plate": "a", "driver": {"ID": 3, "name": "B", "id_license_image_name": "x.jpg"}},
        ]});
        let (vehicles, drivers) = project_vehicles_and_drivers(&raw);
        assert_eq!(row_id(&vehicles[0]), Some(2));
        assert_eq!(drivers.iter().map(|d| row_id(d).unwrap()).collect::<Vec<_>>(), vec![3, 5]);
        let text = serde_json::to_string(&(&vehicles, &drivers)).unwrap();
        assert!(!text.contains("social_security") && !text.contains("loans") && !text.contains("image_name"));
        assert!(text.contains("\"name\":\"A\""));
    }
}
