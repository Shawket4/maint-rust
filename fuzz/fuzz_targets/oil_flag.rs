#![no_main]
//! The oil-flag band decides whether a fill is anomalous — server authority.
//! For any finite liters it must equal the inclusive 34–36 L test and never
//! panic (NaN/inf included).
use libfuzzer_sys::fuzz_target;
use maint_rust::services::side_effects::{oil_flagged, OIL_FLAG_MAX, OIL_FLAG_MIN};

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let mut b = [0u8; 8];
    b.copy_from_slice(&data[..8]);
    let liters = f64::from_le_bytes(b);
    let flagged = oil_flagged(liters);
    if liters.is_finite() {
        let inside = (OIL_FLAG_MIN..=OIL_FLAG_MAX).contains(&liters);
        assert_eq!(flagged, !inside, "flag disagrees with band at {liters}");
    }
});
