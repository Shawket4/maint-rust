use anyhow::{Context, Result};
use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub redis_url: Option<String>,
    pub jwt_secret: String,
    pub falcon_base_url: String,
    pub port: u16,
    pub bind_addr: String,
    pub falcon_cars_cache_ttl: u64,
    pub falcon_invoices_cache_ttl: u64,
    pub dev_login: bool,
    /// Read-only connection to Falcon's Postgres (service history + AI SQL tool).
    pub falcon_database_url: Option<String>,
    pub anthropic_api_key: Option<String>,
    pub ai_model: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        // dotenvy is best-effort — if .env doesn't exist (prod/systemd) we don't fail.
        let _ = dotenvy::dotenv();

        let jwt_secret = env::var("JWT_SECRET").context("JWT_SECRET must be set")?;
        // Refuse to boot on the committed placeholder — a deploy that adopts it
        // (e.g. `.env` auto-created from `.env.example`) would come up "healthy"
        // while every real JWT 401s. Fail loud instead of shipping green-broken.
        if jwt_secret == "must_match_falcon_jwt_secret" {
            anyhow::bail!("JWT_SECRET is the placeholder — set the real Falcon secret");
        }
        Ok(Self {
            database_url: env::var("DATABASE_URL").context("DATABASE_URL must be set")?,
            redis_url: env::var("REDIS_URL").ok(),
            jwt_secret,
            falcon_base_url: env::var("FALCON_BASE_URL")
                .unwrap_or_else(|_| "https://apextransport.ddns.net/api/go".to_string()),
            port: env::var("PORT")
                .unwrap_or_else(|_| "8090".to_string())
                .parse()
                .context("PORT must be a u16")?,
            bind_addr: env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1".to_string()),
            falcon_cars_cache_ttl: env::var("FALCON_CARS_CACHE_TTL")
                .unwrap_or_else(|_| "30".to_string())
                .parse()
                .unwrap_or(30),
            falcon_invoices_cache_ttl: env::var("FALCON_INVOICES_CACHE_TTL")
                .unwrap_or_else(|_| "30".to_string())
                .parse()
                .unwrap_or(30),
            dev_login: env::var("MAINT_DEV_LOGIN")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            falcon_database_url: env::var("FALCON_DATABASE_URL").ok(),
            anthropic_api_key: env::var("ANTHROPIC_API_KEY").ok(),
            ai_model: env::var("AI_MODEL").unwrap_or_else(|_| "claude-haiku-4-5-20251001".to_string()),
        })
    }
}
