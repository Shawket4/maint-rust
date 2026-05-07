use anyhow::{Context, Result};
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

pub type PgPool = sqlx::PgPool;

pub async fn init_pool(database_url: &str) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .min_connections(2)
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Some(Duration::from_secs(600)))
        .connect(database_url)
        .await
        .context("failed to connect to Postgres")?;

    tracing::info!("postgres pool ready");
    Ok(pool)
}
