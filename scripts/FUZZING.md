# Fuzzing & property testing

Three layers, mirroring the Madar core's approach.

## 1. Property tests (proptest) — run in `cargo test`
`src/services/side_effects.rs` (backend) and `src-tauri/src/services/proptests.rs`
(client) assert invariants over thousands of generated inputs: the oil-flag
band, dismount-status totality, chassis-planner uniqueness, backoff curve,
digit normalization, FTS5 safety, JSON→SQLite coercion. No setup; part of the
normal suite.

## 2. Coverage-guided fuzzing (cargo-fuzz) — backend only
`fuzz/fuzz_targets/` — `push_payload` (the /sync/push deserialize+normalize
path) and `oil_flag`. Needs nightly + cargo-fuzz:

    cargo install cargo-fuzz
    cargo +nightly fuzz run push_payload -- -max_total_time=60
    cargo +nightly fuzz run oil_flag     -- -max_total_time=60

(The client uses proptest instead of cargo-fuzz: `src-tauri` is a Tauri app
crate whose WebKit/wry stack won't link under libfuzzer's ASAN on macOS. The
fix is a pure-core-crate split — deferred.)

## 3. API property fuzzing (schemathesis) — against a disposable DB
`scripts/api-fuzz.sh` seeds a scratch `maint_fuzz` Postgres, boots the real
server, mints a token (`mint_test_token`), and hammers every endpoint from
`api-fuzz/openapi.yaml`, asserting the server's own contract (no 5xx, valid
JSON, declared status codes).

    python3 -m venv .fuzzvenv && .fuzzvenv/bin/pip install schemathesis
    scripts/api-fuzz.sh                          # all checks
    CHECKS=not_a_server_error scripts/api-fuzz.sh   # CI: no-5xx gate only

This layer found four real 500s on first run (invalid enum, malformed
payload crash, NUL bytes, FK violation) — all now clean 4xx via the central
error mapping in error.rs.
