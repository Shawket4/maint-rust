use actix_cors::Cors;
use actix_web::{middleware::Logger, web, App, HttpServer};
use anyhow::Context;
use tracing_actix_web::TracingLogger;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use maint_rust::{config, db, falcon, handlers};

use config::Config;
use falcon::{CacheManager, FalconClient};
use handlers::AppState;

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    // Tracing setup
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,maint_rust=debug"));
    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_target(true).with_ansi(true))
        .init();

    let cfg = Config::from_env().context("loading config")?;
    tracing::info!(
        bind = %cfg.bind_addr,
        port = cfg.port,
        falcon = %cfg.falcon_base_url,
        "starting maint-rust"
    );

    let pool = db::init_pool(&cfg.database_url).await?;

    // Run migrations on startup. This makes the deploy story painless on a fresh box.
    // ignore_missing: a database that is AHEAD of this binary (a newer build
    // migrated it, then was rolled back) must still boot. Without it the
    // previous binary refuses to start and a failed deploy cannot roll back.
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    migrator.run(&pool).await.context("running migrations")?;
    tracing::info!("migrations up to date");

    // Optional read-only pool to Falcon's Postgres (service history + AI SQL tool).
    let falcon_pool = match &cfg.falcon_database_url {
        Some(url) => match db::init_pool(url).await {
            Ok(p) => {
                tracing::info!("connected read-only pool to Falcon Postgres");
                Some(p)
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not connect Falcon Postgres pool");
                None
            }
        },
        None => None,
    };

    let falcon = FalconClient::new(cfg.falcon_base_url.clone());
    let cache = CacheManager::new(cfg.redis_url.as_deref()).await;

    let app_state = AppState {
        pool: pool.clone(),
        falcon,
        cache,
        falcon_cars_ttl: cfg.falcon_cars_cache_ttl,
        falcon_invoices_ttl: cfg.falcon_invoices_cache_ttl,
        jwt_secret: cfg.jwt_secret.clone(),
        dev_login: cfg.dev_login,
        falcon_pool,
        anthropic_api_key: cfg.anthropic_api_key.clone(),
        ai_model: cfg.ai_model.clone(),
        permissions: Default::default(),
        mirror_busy: Default::default(),
        http: handlers::ai::http_client(),
        ai_limiter: Default::default(),
    };

    // Old Falcon service invoices → maintenance history (no-op without the
    // read-only Falcon database).
    maint_rust::services::legacy_import::spawn_periodic(app_state.pool.clone(), app_state.falcon_pool.clone());

    let bind = (cfg.bind_addr.clone(), cfg.port);
    let jwt_secret = cfg.jwt_secret.clone();

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(app_state.clone()))
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::JsonConfig::default().limit(4 * 1024 * 1024)) // 4 MB
            .wrap(Logger::default())
            .wrap(TracingLogger::default())
            .wrap(Cors::permissive())
            .service(handlers::api_scope(jwt_secret.clone()))
    })
    .bind(bind)?
    .run()
    .await?;

    Ok(())
}
