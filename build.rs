// sqlx::migrate! embeds the migrations dir at COMPILE time, but adding a new
// migration file doesn't dirty any Rust source — cargo skips the rebuild and
// the binary silently ships without it ("migrations up to date" while the new
// file sits unapplied on disk). This makes cargo watch the directory.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
