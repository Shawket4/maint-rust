# maint-rust

Backend microservice for Apex Maintenance — the offline-first desktop fleet
maintenance system described in `ARCHITECTURE.md`. Owns the Postgres
truth-of-record for maintenance templates/records, chassis layouts, tires, and
tire assignments. Proxies vehicle and service-invoice reads from the existing
Falcon Go backend with Redis-backed caching.

```
Tauri client  ─HTTPS─▶  maint-rust  ─HTTPS─▶  Falcon Go
                          │
                          ├─▶ Postgres (apex_maint)
                          └─▶ Redis (key prefix maint:)
```

---

## Quick start

```bash
# 1. Install toolchain + sqlx-cli
rustup default stable                   # Rust >= 1.75
cargo install sqlx-cli --no-default-features --features postgres,rustls

# 2. Configure
cp .env.example .env
$EDITOR .env                            # set DATABASE_URL, JWT_SECRET, FALCON_BASE_URL, etc.

# 3. Create DB + run migrations
sqlx database create
sqlx migrate run

# 4. Run
cargo run                               # listens on $BIND_ADDR:$PORT (default 127.0.0.1:8090)
```

The service auto-runs migrations on startup, so step 3 is mostly for first-time
DB creation. If you already have an `apex_maint` database, step 3 is just
`sqlx migrate run`.

If you don't want to use sqlx-cli at all, `psql apex_maint -f setup.sql`
(shipped alongside this README) creates everything in one shot.

---

## Configuration

All configuration is via environment variables (or a `.env` file in the working
directory). See `.env.example` for the full list.

| Variable                       | Required | Default                                          | Notes |
|--------------------------------|----------|--------------------------------------------------|-------|
| `DATABASE_URL`                 | yes      | —                                                | `postgres://user:pass@host:port/apex_maint` |
| `JWT_SECRET`                   | yes      | —                                                | Must byte-match Falcon's `JWT_SECRET` (HS256). |
| `FALCON_BASE_URL`              | no       | `https://apextransport.ddns.net/api/go`          | No trailing slash. |
| `REDIS_URL`                    | no       | (none — runs without cache)                      | When unset, Falcon proxies hit upstream every time. |
| `PORT`                         | no       | `8090`                                           | |
| `BIND_ADDR`                    | no       | `127.0.0.1`                                      | Use `0.0.0.0` only behind a reverse proxy. |
| `RUST_LOG`                     | no       | `info,maint_rust=debug`                          | tracing-subscriber filter. |
| `FALCON_CARS_CACHE_TTL`        | no       | `30`                                             | Seconds. |
| `FALCON_INVOICES_CACHE_TTL`    | no       | `30`                                             | Seconds. |

---

## Production build & deploy

```bash
# On dev machine:
cargo build --release
scp target/release/maint-rust apex@vps:/opt/maint-rust/

# On VPS, first-time setup:
sudo mkdir -p /opt/maint-rust
sudo chown apex:apex /opt/maint-rust
sudo cp deploy/maint-rust.service /etc/systemd/system/
sudo cp deploy/nginx-maint.conf /etc/nginx/snippets/
# Edit /etc/nginx/sites-available/<your_site> to `include snippets/nginx-maint.conf;`
# Drop your tuned .env at /opt/maint-rust/.env (mode 0600, owned by apex)

sudo systemctl daemon-reload
sudo systemctl enable --now maint-rust
sudo nginx -t && sudo systemctl reload nginx

# Health check
curl https://apextransport.ddns.net/api/maint/health
```

Subsequent deploys are: `cargo build --release` → `scp` → `sudo systemctl
restart maint-rust`. Identical pattern to the existing apex-rust deploy.

---

## Endpoints

All routes live under `/api/maint`. Every route except `/health` requires
`Authorization: Bearer <jwt>` where the JWT is HS256-signed with the same
secret as Falcon.

### Health

```
GET /api/maint/health        — no auth
```

### Falcon proxies (Phase 2)

```
GET /api/maint/cache/vehicles
GET /api/maint/cache/service-invoices?page=N&limit=M
GET /api/maint/cache/service-invoices/sync?full=true
GET /api/maint/cache/service-invoices/search?query=...&page=N&limit=M
```

### Maintenance domain (Phase 3)

```
POST   /api/maint/templates
GET    /api/maint/templates?vehicle_id=N
GET    /api/maint/templates/{id}
PUT    /api/maint/templates/{id}
DELETE /api/maint/templates/{id}

POST   /api/maint/records
GET    /api/maint/records?vehicle_id=N&template_id=UUID
GET    /api/maint/records/{id}
PUT    /api/maint/records/{id}
DELETE /api/maint/records/{id}

GET    /api/maint/due?vehicle_id=N&status=overdue|due_soon|ok|never_done

PUT    /api/maint/overrides/{vehicle_id}
GET    /api/maint/overrides/{vehicle_id}
DELETE /api/maint/overrides/{vehicle_id}
```

### Chassis & tires (Phase 4)

```
POST   /api/maint/chassis/layouts
GET    /api/maint/chassis/layouts
GET    /api/maint/chassis/layouts/{id}
PUT    /api/maint/chassis/layouts/{id}
DELETE /api/maint/chassis/layouts/{id}

POST   /api/maint/chassis/axles      (server auto-creates positions per §9.2)
GET    /api/maint/chassis/axles?layout_id=UUID
PUT    /api/maint/chassis/axles/{id}
DELETE /api/maint/chassis/axles/{id}

POST   /api/maint/chassis/spares
GET    /api/maint/chassis/positions?layout_id=UUID&axle_id=UUID
DELETE /api/maint/chassis/positions/{id}      (only spare positions)

GET    /api/maint/chassis/{vehicle_id}/full   (full diagram payload, §9.1)

POST   /api/maint/tires
GET    /api/maint/tires?status=...&brand=...
GET    /api/maint/tires/{id}
GET    /api/maint/tires/{id}/history
PUT    /api/maint/tires/{id}
DELETE /api/maint/tires/{id}                   (refused if status=mounted)

POST   /api/maint/tires/{tire_id}/mount
POST   /api/maint/tires/assignments/{assignment_id}/dismount
POST   /api/maint/tires/bulk

GET    /api/maint/assignments?tire_id=...&vehicle_id=...&active_only=true
```

### Sync (Phase 5)

```
POST   /api/maint/sync/push
GET    /api/maint/sync/pull?entity_type=tires&since=<iso>&limit=200
```

`entity_type` is one of: `maintenance_templates`, `maintenance_records`,
`chassis_layouts`, `chassis_axles`, `chassis_positions`, `tires`,
`tire_assignments`, `vehicle_odometer_overrides`.

A push operation looks like:

```json
{
  "operations": [
    {
      "entity_type": "maintenance_records",
      "entity_id": "f8a1...",
      "operation": "insert",
      "sync_version": 0,
      "payload": { "id": "f8a1...", "vehicle_id": 12, "...": "..." }
    }
  ]
}
```

The response gives one result per op:

```json
{
  "results": [
    { "status": "applied", "entity_id": "f8a1...", "new_sync_version": 1 },
    { "status": "conflict", "entity_id": "9b2c...", "server_row": { ... } },
    { "status": "error", "entity_id": "...", "message": "..." }
  ]
}
```

---

## Curl smoke tests per phase

Replace `$JWT` with a token from `POST /api/go/api/login`. Prepend
`https://apextransport.ddns.net` for tests against prod, or
`http://127.0.0.1:8090` for local.

### Phase 1 — Foundation

```bash
# Health (no auth)
curl -s "$BASE/api/maint/health"

# Anything else without a token → 401
curl -i "$BASE/api/maint/due"
```

### Phase 2 — Falcon proxies

```bash
curl -s -H "Authorization: Bearer $JWT" "$BASE/api/maint/cache/vehicles" | jq '.vehicles | length'
curl -s -H "Authorization: Bearer $JWT" "$BASE/api/maint/cache/service-invoices?page=1&limit=5" | jq
curl -s -H "Authorization: Bearer $JWT" "$BASE/api/maint/cache/service-invoices/sync"
curl -s -H "Authorization: Bearer $JWT" "$BASE/api/maint/cache/service-invoices/search?query=%D9%81%D8%B1%D8%A7%D9%85%D9%84&limit=10" | jq '.results | length'
```

### Phase 3 — Maintenance

```bash
# Create a mileage-trigger template (oil change every 15,000 km)
TEMPLATE_ID=$(uuidgen)
curl -s -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -X POST "$BASE/api/maint/templates" \
  -d "{
    \"id\": \"$TEMPLATE_ID\",
    \"vehicle_id\": 1,
    \"category_id\": \"oil_change\",
    \"name_ar\": \"تغيير زيت رئيسي\",
    \"name_en\": \"Main oil change\",
    \"trigger_type\": \"mileage\",
    \"interval_km\": 15000
  }" | jq

# Negative: time trigger missing interval_days → 400
curl -i -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -X POST "$BASE/api/maint/templates" \
  -d '{"vehicle_id":1,"category_id":"brakes","name_ar":"x","name_en":"x","trigger_type":"time"}'

# Mark done
curl -s -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -X POST "$BASE/api/maint/records" \
  -d "{
    \"template_id\": \"$TEMPLATE_ID\",
    \"vehicle_id\": 1,
    \"category_id\": \"oil_change\",
    \"performed_at\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",
    \"odometer_at_service\": 150000,
    \"cost\": \"850.00\",
    \"vendor\": \"Workshop A\"
  }" | jq

# Negative: performed_at in the future → 400
curl -i -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -X POST "$BASE/api/maint/records" \
  -d "{\"template_id\":\"$TEMPLATE_ID\",\"vehicle_id\":1,\"category_id\":\"oil_change\",\"performed_at\":\"2099-01-01T00:00:00Z\"}"

# Due engine
curl -s -H "Authorization: Bearer $JWT" "$BASE/api/maint/due?vehicle_id=1" | jq

# Odometer override
curl -s -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -X PUT "$BASE/api/maint/overrides/1" \
  -d '{"vehicle_id":1,"odometer":165000,"notes":"Manual reading"}' | jq

curl -s -H "Authorization: Bearer $JWT" "$BASE/api/maint/due?vehicle_id=1" | jq '.[0].current_odometer, .[0].odometer_source'
```

### Phase 4 — Chassis & tires

```bash
# Create layout for a vehicle
LAYOUT_ID=$(uuidgen)
curl -s -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -X POST "$BASE/api/maint/chassis/layouts" \
  -d "{\"id\":\"$LAYOUT_ID\",\"vehicle_id\":1,\"name_en\":\"Default 6x4 + 3-axle trailer\"}" | jq

# Add axles. Server auto-generates position codes.
curl -s -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -X POST "$BASE/api/maint/chassis/axles" \
  -d "{\"layout_id\":\"$LAYOUT_ID\",\"section\":\"tractor\",\"section_index\":1,\"axle_type\":\"single\",\"is_steering\":true}" | jq '.positions[].position_code'
# expect: ["H1L","H1R"]

curl -s -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -X POST "$BASE/api/maint/chassis/axles" \
  -d "{\"layout_id\":\"$LAYOUT_ID\",\"section\":\"tractor\",\"section_index\":2,\"axle_type\":\"dual\"}" | jq '.positions[].position_code'
# expect: ["H2LO","H2LI","H2RI","H2RO"]

# Create a spare slot
curl -s -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -X POST "$BASE/api/maint/chassis/spares" \
  -d "{\"layout_id\":\"$LAYOUT_ID\",\"spare_index\":1}" | jq '.position_code'
# expect: "SP1"

# Create a tire (DOT WWYY parsed server-side)
TIRE_ID=$(uuidgen)
curl -s -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -X POST "$BASE/api/maint/tires" \
  -d "{
    \"id\": \"$TIRE_ID\",
    \"dot_code\": \"DOT XYZ4523ABC\",
    \"brand\": \"Michelin\",
    \"model\": \"XZA-2\",
    \"dot_date_code\": \"4523\",
    \"purchase_cost\": \"4500.00\"
  }" | jq '{id, brand, production_year, production_week, production_date}'

# Negative: malformed DOT code → 400
curl -i -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -X POST "$BASE/api/maint/tires" \
  -d '{"dot_code":"DOT BAD","brand":"X","dot_date_code":"5499"}'

# Mount on H1L
POS_ID=$(curl -s -H "Authorization: Bearer $JWT" "$BASE/api/maint/chassis/positions?layout_id=$LAYOUT_ID" | jq -r '.[] | select(.position_code=="H1L") | .id')
curl -s -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -X POST "$BASE/api/maint/tires/$TIRE_ID/mount" \
  -d "{\"position_id\":\"$POS_ID\",\"mounted_at\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"mounted_odometer\":150000,\"mount_reason\":\"new_install\"}" | jq

# Negative: try to mount the same tire twice → 409
curl -i -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -X POST "$BASE/api/maint/tires/$TIRE_ID/mount" \
  -d "{\"position_id\":\"$POS_ID\",\"mounted_at\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"mount_reason\":\"new_install\"}"

# Full chassis with the mounted tire visible
curl -s -H "Authorization: Bearer $JWT" "$BASE/api/maint/chassis/1/full" | jq '.sections[0].axles[0].positions[] | {position_code, tire: .tire.dot_code}'

# Bulk: dismount + mount in one transaction (rotation pattern)
ASSIGN_ID=$(curl -s -H "Authorization: Bearer $JWT" "$BASE/api/maint/assignments?tire_id=$TIRE_ID&active_only=true" | jq -r '.[0].id')
curl -s -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -X POST "$BASE/api/maint/tires/bulk" \
  -d "{
    \"operations\": [
      {\"kind\":\"dismount\",\"assignment_id\":\"$ASSIGN_ID\",\"dismounted_at\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"dismounted_odometer\":160000,\"dismount_reason\":\"rotation\",\"destination\":\"in_stock\"}
    ]
  }" | jq
```

### Phase 5 — Sync

```bash
# Pull all maintenance_records since epoch
curl -s -H "Authorization: Bearer $JWT" \
  "$BASE/api/maint/sync/pull?entity_type=maintenance_records&limit=50" | jq '{rows: (.rows|length), next_cursor, has_more}'

# Push a new record (offline-style — client generates the UUID)
NEW_ID=$(uuidgen)
curl -s -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -X POST "$BASE/api/maint/sync/push" \
  -d "{
    \"operations\": [
      {
        \"entity_type\": \"maintenance_records\",
        \"entity_id\": \"$NEW_ID\",
        \"operation\": \"insert\",
        \"sync_version\": 0,
        \"payload\": {
          \"id\": \"$NEW_ID\",
          \"vehicle_id\": 1,
          \"category_id\": \"greasing\",
          \"performed_at\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",
          \"odometer_at_service\": 165500
        }
      }
    ]
  }" | jq

# Push with a stale sync_version → 409 Conflict, server_row attached per result
curl -s -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -X POST "$BASE/api/maint/sync/push" \
  -d "{
    \"operations\": [
      {
        \"entity_type\": \"maintenance_records\",
        \"entity_id\": \"$NEW_ID\",
        \"operation\": \"update\",
        \"sync_version\": 999,
        \"payload\": {\"notes\": \"trying with stale sync_version\"}
      }
    ]
  }" | jq
```

---

## Implementation notes

- **sqlx macros (`sqlx::query!`) vs runtime queries.** Compile-time-checked
  macros require `DATABASE_URL` set at compile time *or* an `.sqlx/` offline
  cache. To keep `cargo build --release` working cold on the CI runner without
  a live DB, this codebase uses `sqlx::query` and `sqlx::query_as` (runtime
  checked). To switch to compile-time checking: bring up a DB, run migrations,
  then `cargo sqlx prepare` and rewrite the hot queries to the macro form.
- **Audit columns** (`created_by_user_id`, `updated_by_user_id`) are stamped
  server-side from `claims.user_id` at every handler — never trusted from
  request body, in either direct CRUD or sync push (§3.6).
- **Soft delete only.** No `DELETE FROM` for owned-data tables in handler code.
  The single exception is `vehicle_odometer_overrides`, which has no
  `deleted_at` column — its delete handler does a hard `DELETE`, audited via
  the structured log.
- **Sync version conflicts** return HTTP 409 with the server's current row in
  `server_row`. The desktop's conflict UX (§7.5) reads from there.
- **`chassis_axles` and `chassis_positions` audit columns.** §5.3's table
  definitions don't include `created_at` / `updated_at` / `deleted_at` /
  `sync_version`, but §7.3 lists both as syncable entities and sync requires
  `updated_at` + `sync_version`. Migrations add those columns. If the
  desktop client wants to drop them on its SQLite mirror, that's fine — the
  protocol contract doesn't require them client-side.
- **Position codes are server-generated only.** Sync push payloads for
  `chassis_positions` may include `position_code`, but the only canonical
  generator is `services/chassis_builder.rs` (§9.2). Client-supplied codes are
  trusted on insert, so well-behaved clients should generate them with the
  same algorithm to avoid drift.
- **Falcon `iss` quirk.** Falcon stamps `iss = user_id.to_string()` rather than
  a fixed issuer. The middleware does not validate `iss` (§3.4 covers this).

---

## Tests

```bash
cargo test
```

Unit tests live alongside their modules (`services/dot_parser.rs`,
`services/chassis_builder.rs`). Integration tests in `tests/` can be added
once a Postgres test fixture is wired up.

---

## License

Internal — Apex Transport.
