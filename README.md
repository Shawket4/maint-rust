# maint-rust

Backend for Apex Maintenance, the offline-first garage app
([apex-maintenance-tauri](https://github.com/Shawket4/apex-maintenance-tauri)).
It is the record of truth for work orders, tasks, oil changes, tires, tire
mounts, chassis layouts, vehicle classes, service plans and odometer
corrections.

```
desktop / Android app ──HTTPS──▶ maint-rust ──HTTPS──▶ Falcon (fleet list, stock, login)
                                    │
                                    ├─▶ Postgres  apex_maint            (everything this service owns)
                                    ├─▶ Postgres  Falcon, read-only     (optional: service history, AI search)
                                    └─▶ Redis                           (optional: short cache of Falcon reads)
```

Falcon owns vehicles, drivers, service invoices and the stock counts. This
service mirrors the fleet list (`vehicles_cache`) and stock (`*_stock_cache`)
and never writes to Falcon except to report stock movements.

## Run locally

```bash
cp .env.example .env        # set DATABASE_URL and JWT_SECRET at least
createdb apex_maint
cargo run                   # 127.0.0.1:8095; migrations run on boot
```

`MAINT_DEV_LOGIN=1` (debug builds only) lets `/auth/login` hand out a local
token when Falcon is not reachable.

## Configuration

| Variable | Required | Default | Notes |
|---|---|---|---|
| `DATABASE_URL` | yes | | `postgres://user:pass@host/apex_maint` |
| `JWT_SECRET` | yes | | Falcon's HS256 secret. The placeholder from `.env.example` refuses to boot. |
| `FALCON_BASE_URL` | no | `https://apextransport.ddns.net/api/go` | No trailing slash. |
| `PORT` / `BIND_ADDR` | no | `8095` / `127.0.0.1` | |
| `REDIS_URL` | no | none | Without it every Falcon read goes upstream. |
| `FALCON_CARS_CACHE_TTL`, `FALCON_INVOICES_CACHE_TTL` | no | `30` | Seconds. |
| `FALCON_DATABASE_URL` | no | none | Read-only role on Falcon's database. Turns on service history, AI search and live permission checks. |
| `ANTHROPIC_API_KEY`, `AI_MODEL` | no | none / `claude-haiku-4-5-20251001` | AI search. |
| `MIN_CLIENT_VERSION` | no | `0.0.0` | Reported by `/health`; the app asks for an update below it. |
| `MAINT_DEV_LOGIN`, `MAINT_DEV_USER_ID` | no | off | Debug builds only. |

`FALCON_DATABASE_URL` must be a role with `SELECT` on `service_invoices`,
`inspection_items` and `users (id, permission)` and nothing else. The AI
endpoint also checks every query's plan against those two tables, but the
role is the boundary that does not depend on this code being right.

## Who may do what

Every route except `/health` and `/auth/login` needs `Authorization: Bearer`.

- Only staff tokens (`user_type = admin_user`) are accepted. Falcon's
  driver-app tokens are signed with the same secret and are refused.
- Permission 1 or higher is needed for anything. Service plans and vehicle
  classes need 3, AI search 3, stock credits 4.
- With `FALCON_DATABASE_URL` set the permission is re-read from Falcon (cached
  for a minute), so a removed or demoted user loses access without waiting for
  the token to expire. Without it the token's own claim is trusted for its
  31 days.
- A write that carries `X-Via-AI` (the advisor replaying an approved
  suggestion) needs permission 4 and is recorded in `ai_audit_log`.

`/auth/login` checks the credentials with Falcon and mints this service's own
token. That means this service holds Falcon's signing secret; treat its `.env`
like Falcon's.

## Endpoints (`/api/maint`)

| | |
|---|---|
| `GET /health` | Liveness, version, `min_client_version`, which optional parts are on. |
| `POST /auth/login` | `{email, password}` → `{jwt, exp, user_id, permission, name}`. |
| `POST /sync/push`, `GET /sync/pull` | The sync protocol, below. |
| `GET /records/{entity_type}/{id}` | One synced row and its `sync_version`. |
| `GET /cache/vehicles` | Fleet list from Falcon; also refreshes `vehicles_cache`. |
| `GET /cache/employees` | Falcon's employees (id, name, type, active only), for the garage's attendance list, from Falcon's `/api/employee-directory` (permission 1). A token Falcon refuses gets an empty list with `available: false`. |
| `GET /office/attendance?month=YYYY-MM` | The garage's attendance for one month (Cairo calendar): days, one status per worker per day (present, absent, leave, sick; no hours, no days off), totals. |
| `GET /cache/service-invoices`, `…/search` | Paged proxy to Falcon. |
| `GET /vehicles/{id}/service-events` | A vehicle's service history (needs `FALCON_DATABASE_URL`). |
| `GET /categories`, `/vehicle-classes`, `/vehicle-class-assignments`, `/vehicles` | Reference data. |
| `POST /vehicle-classes` | Create or rename a class. |
| `GET /service-plans`, `POST /service-plans`, `DELETE /service-plans/{id}` | Preventive plans per class and category. |
| `GET /due?vehicle_id=` | `v_maintenance_due`. |
| `GET /stock/tires`, `/stock/oil` | Stock mirror. `X-Stock-Fresh: false` when Falcon could not be asked. |
| `POST /stock/tires/credit`, `/stock/oil/credit` | Add stock. Send a `ref_id`; the same one twice credits once. |
| `GET /stock/movements?kind=&from=&limit=&offset=` | The in/out ledger. |
| `GET /work-orders`, `/work-orders/{id}`, `/tires`, `/tires/{id}/history` | Server-side reads (the app reads its local mirror instead). |
| `POST /ai/query` | Natural-language search over Falcon's service history. |
| `GET /ai-audit` | The AI write trail. |

Errors are `{"error": "<code>", "message": "<text safe to show>"}`. Database
and upstream error text is logged, never returned.

## Sync

Entities: `work_orders`, `work_order_tasks`, `tires`, `tire_assignments`,
`tire_status_events`, `oil_changes`, `chassis_layouts`, `chassis_axles`,
`chassis_positions`, `vehicle_class_assignments`, `vehicle_odometer_overrides`.

### Push

```json
{ "operations": [ {
    "op_id": "…uuid…",          "group_id": "…uuid, optional…",
    "entity_type": "work_order_tasks", "entity_id": "…",
    "operation": "insert | update | delete",
    "sync_version": 0,          "payload": { … }
} ] }
```

One result per op, in request order: `applied` (with `new_sync_version`),
`conflict` (with `server_row`), or `error` (with a stable `code` and a
`message`).

- **`op_id` makes an op exactly-once.** An op that was applied is remembered;
  sending it again returns the first answer. A client must give every op its
  own id and must not change an op after sending it.
- **`group_id` makes adjacent ops one action.** They apply together or not at
  all. The op that was refused carries its own result; the others report
  `group_rolled_back`.
- `update` and `delete` carry the `sync_version` the client last saw. A
  different version on the server is a `conflict`.
- A `503` means "send the same request again later": the database was busy.
  It says nothing about the ops.
- Clients older than `op_id` (app ≤ 0.2.3) are still accepted; their replays
  are recognised by version and content.

What the server decides for itself, whatever a client sends: tire `status` and
`retread_count` (from mounts, dismounts and status events), the oil flag
(outside 34–36 L), a work order's `closed_at`, a task's `done_at`, and every
stock movement. Timestamps in the future are set to now.

Error codes: `validation`, `not_found`, `deleted`, `duplicate`,
`missing_reference`, `position_occupied`, `tire_already_mounted`,
`tire_not_available`, `tire_mounted`, `position_wrong_vehicle`,
`chassis_exists`, `chassis_in_use`, `work_order_not_empty`,
`group_rolled_back`, `internal`.

### Pull

`GET /sync/pull?entity_type=tires&since=<iso>&after_id=<pk>&limit=200` returns
rows ordered by `(updated_at, pk)`, tombstones included, with `next_cursor`,
`next_cursor_id` and `has_more`. Pass both cursor values back. `updated_at` is
stamped at write time under one lock (migration 0021), so a row can never
commit behind a cursor that has already moved past it.

## Rules worth knowing

- **Odometer** (`v_current_odometer`): the highest of Falcon's fuel reading,
  Falcon's mileage, oil-change and work-order readings, and a manual
  correction. After a correction, readings recorded before it are ignored, and
  the Falcon value that was corrected is ignored until it changes.
- **Due** (`v_maintenance_due`): a plan has a km interval, a day interval, or
  both; it is due on whichever comes first. "Last done" is the newest ticked
  task of that category (`done_at`), and the km baseline is the newest one
  that has a reading. Statuses: `overdue`, `due_soon`, `ok`, `never_done`,
  `unknown` (a km-only plan on a vehicle with no reading).
- **Tire km** is derived from mounts: base km at intake plus every
  mount-to-dismount distance on a road position, plus the open mount up to the
  current odometer. Spare positions do not count.
- **Stock** may go negative. A movement is recorded once per `ref_id`, names
  the user, adjusts the mirror at once and is sent to Falcon in the
  background. A movement Falcon refuses is parked with the reason
  (`mirror_error`) instead of retrying forever.

## Tests

```bash
cargo test                                                  # unit tests; live suites skip
MAINT_TEST_DATABASE_URL=postgres://…/throwaway cargo test   # everything
```

CI runs the second form against a Postgres service and fails if the database
is missing. `scripts/FUZZING.md` covers the proptest, cargo-fuzz and
schemathesis suites.

## Deploy

A push to `main` runs the tests, builds in an Ubuntu 22.04 container and
deploys to `/opt/maint-rust` on the VPS (`.github/workflows/deploy.yml`):

1. Stage the new files and take a `pg_dump` while the old service is still
   running. A failed dump stops the deploy.
2. Stop, swap the binary, start. Migrations run on boot.
3. Check `/health` on loopback and through nginx. If the service does not
   start, the previous binary is put back.

Never edit a migration that has been applied; add a new one. The binary boots
on a database that is ahead of it, so a rollback after a migration still
starts.

Backups: nightly and pre-deploy dumps in `/opt/maint-rust/backups/pg` (14
kept). They live on the same disk as the database; copy them off the box.
