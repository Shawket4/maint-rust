//! maint-rust as a library.
//!
//! The binary (main.rs) is a thin shell over this lib. The split exists so
//! integration tests (tests/) can exercise the sync engine and helpers against
//! a live Postgres without spinning up the HTTP server or minting JWTs.

pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod falcon;
pub mod handlers;
pub mod models;
pub mod services;
pub mod utils;
