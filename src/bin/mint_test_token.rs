//! Dev-only: mint a maint-rust JWT for API fuzzing / local curl against a
//! DISPOSABLE server. NEVER a production path — it signs with whatever
//! JWT_SECRET is in the environment and always stamps user_type=admin_user
//! (the type FalconGo's Verify — and our middleware — require).
//!
//!   JWT_SECRET=… cargo run --bin mint_test_token [user_id] [permission]
use maint_rust::auth::token::mint;

fn main() {
    let secret = std::env::var("JWT_SECRET").expect("JWT_SECRET required");
    let mut args = std::env::args().skip(1);
    let user_id: i64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    let permission: i32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);
    let (token, _exp) = mint(&secret, user_id, permission, "admin_user", 1).expect("mint");
    print!("{token}");
}
