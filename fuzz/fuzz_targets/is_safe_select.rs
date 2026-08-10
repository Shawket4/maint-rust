#![no_main]
//! `is_safe_select` is the first of three layers guarding the model-driven
//! `run_sql` tool (the others being a READ ONLY tx + statement_timeout). It is
//! fed whatever SQL Claude emits — effectively arbitrary text steered by an
//! end-user's prompt. Two properties must hold for ALL input:
//!   1. It never panics (arbitrary bytes, invalid UTF-8, NUL, huge unicode).
//!   2. If it returns true, the query really does start with `select`/`with`
//!      after trimming — the invariant the run_sql wrapper relies on. A pass
//!      that didn't start with a read verb would let the subquery wrapper build
//!      malformed/again-injectable SQL.
use libfuzzer_sys::fuzz_target;
use maint_rust::handlers::ai::is_safe_select;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);

    // 1. Never panics, and is a pure predicate (calling twice agrees).
    let a = is_safe_select(&s);
    let b = is_safe_select(&s);
    assert_eq!(a, b, "is_safe_select is not deterministic");

    // 2. A pass implies the trimmed, lowercased text begins with a read verb.
    if a {
        let t = s.trim_start().to_lowercase();
        assert!(
            t.starts_with("select") || t.starts_with("with"),
            "is_safe_select accepted a query that is not a SELECT/WITH: {s:?}"
        );
    }
});
