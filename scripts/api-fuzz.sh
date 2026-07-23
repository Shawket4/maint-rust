#!/usr/bin/env bash
#
# Schemathesis property fuzzing against a DISPOSABLE maint_fuzz DB + the real
# server binary. Cross-platform (no x86 VM — unlike RESTler). Same shape as the
# Madar restler-run.sh: seed a scratch DB, boot the real server, mint a token,
# fuzz, tear down.
#
# One-time:  python3 -m venv .fuzzvenv && .fuzzvenv/bin/pip install schemathesis
# Usage:     scripts/api-fuzz.sh            (all checks)
#            CHECKS=not_a_server_error scripts/api-fuzz.sh   (CI: no 5xx only)
#            scripts/api-fuzz.sh /sync/pull (one endpoint by path substring)
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"

DB="${FUZZ_DB:-maint_fuzz}"
PORT="${FUZZ_PORT:-8097}"
SECRET="${JWT_SECRET:-fuzz-secret-not-for-prod}"
PGHOST="${PGHOST:-127.0.0.1}"
PGUSER="${PGUSER:-maint}"
BASE="http://127.0.0.1:$PORT/api/maint"

# Refuse to touch anything that isn't a throwaway fuzz DB.
case "$DB" in *fuzz*) : ;; *) echo "REFUSING: '$DB' is not a *fuzz* DB" >&2; exit 1;; esac

SCHEMA="${SCHEMATHESIS:-.fuzzvenv/bin/schemathesis}"
[ -x "$SCHEMA" ] || SCHEMA="$(command -v schemathesis || true)"
[ -n "$SCHEMA" ] || { echo "schemathesis not found — see the header for setup" >&2; exit 1; }

echo "▶ scratch DB $DB"
dropdb -h "$PGHOST" -U "$PGUSER" --if-exists "$DB" 2>/dev/null || dropdb -h "$PGHOST" --if-exists "$DB"
createdb -h "$PGHOST" "$DB" -O "$PGUSER" 2>/dev/null || createdb -h "$PGHOST" "$DB"

echo "▶ build server + token minter"
cargo build --quiet --bin maint-rust --bin mint_test_token

echo "▶ boot server on :$PORT (Falcon pointed at a dead port → pure local)"
DATABASE_URL="postgres://$PGUSER:secret@$PGHOST:5432/$DB" \
JWT_SECRET="$SECRET" PORT="$PORT" BIND_ADDR=127.0.0.1 \
FALCON_BASE_URL="http://127.0.0.1:1" MAINT_DEV_LOGIN=0 \
  "$ROOT/target/debug/maint-rust" > /tmp/maint-fuzz-server.log 2>&1 &
SRV=$!
cleanup() {
  kill "$SRV" 2>/dev/null || true
  dropdb -h "$PGHOST" -U "$PGUSER" --if-exists "$DB" 2>/dev/null || dropdb -h "$PGHOST" --if-exists "$DB" 2>/dev/null || true
}
trap cleanup EXIT

for _ in $(seq 1 60); do curl -sf "$BASE/health" >/dev/null 2>&1 && break; sleep 0.5; done
curl -sf "$BASE/health" >/dev/null || { echo "server never became healthy"; tail /tmp/maint-fuzz-server.log; exit 1; }
echo "  healthy"

TOKEN="$(JWT_SECRET="$SECRET" ./target/debug/mint_test_token 1 5)"

# Schemathesis: generate requests from the spec, assert the server's own
# contract on every response — no 500s, valid JSON, declared status codes,
# response conforms to schema. --hypothesis-* controls fuzz depth.
FILTER=()
if [ $# -gt 0 ]; then FILTER=(--include-path-regex "$1"); fi

echo "▶ schemathesis fuzz ${1:-all endpoints}"
"$SCHEMA" run api-fuzz/openapi.yaml \
  --url "$BASE" \
  --header "Authorization: Bearer $TOKEN" \
  --checks "${CHECKS:-all}" \
  --max-examples "${MAX_EXAMPLES:-40}" \
  --suppress-health-check=filter_too_much \
  "${FILTER[@]+"${FILTER[@]}"}" || { echo "▶ schemathesis found issues (see above)"; exit 1; }

echo "▶ clean — no contract violations"
