#![no_main]
//! /sync/push deserializes a whole batch of client-authored operations from
//! the wire — arbitrary JSON a hostile or corrupt client could send. Parsing
//! the envelope and normalizing the server-owned fields (the oil flag) must
//! NEVER panic, and normalize must be idempotent and leave `flagged`
//! consistent with `liters`. (The SQL apply needs a live tx and is covered by
//! the live integration tests; here we fuzz the pure pre-apply path.)
use libfuzzer_sys::fuzz_target;
use maint_rust::services::side_effects::{normalize_insert, normalize_update, oil_flagged};
use maint_rust::services::sync_engine::{EntityType, PushBody};
use serde_json::Value;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);

    // 1. The envelope parse must never panic on any bytes.
    if let Ok(body) = serde_json::from_str::<PushBody>(&s) {
        for op in &body.operations {
            let mut p = op.payload.clone();
            normalize_insert(op.entity_type, &mut p);
            let once = p.clone();
            normalize_insert(op.entity_type, &mut p);
            assert_eq!(once, p, "normalize_insert not idempotent");
            if op.entity_type == EntityType::OilChanges {
                if let Some(l) = p.get("liters").and_then(|v| {
                    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                }) {
                    assert_eq!(
                        p.get("flagged"),
                        Some(&Value::Bool(oil_flagged(l))),
                        "flag inconsistent after normalize_insert"
                    );
                }
            }
            let mut p2 = op.payload.clone();
            normalize_update(op.entity_type, &mut p2);
        }
    }
});
