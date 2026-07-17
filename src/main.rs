use actix_cors::Cors;
use actix_web::{middleware::Logger, web, App, HttpServer};
use anyhow::Context;
use tracing_actix_web::TracingLogger;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use maint_rust::{auth, config, db, falcon, handlers};

use auth::JwtAuth;
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
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("running migrations")?;
    tracing::info!("migrations up to date");

    let falcon = FalconClient::new(cfg.falcon_base_url.clone());
    let cache = CacheManager::new(cfg.redis_url.as_deref()).await;

    let app_state = AppState {
        pool: pool.clone(),
        falcon,
        cache,
        falcon_cars_ttl: cfg.falcon_cars_cache_ttl,
        falcon_invoices_ttl: cfg.falcon_invoices_cache_ttl,
    };

    let bind = (cfg.bind_addr.clone(), cfg.port);
    let jwt_secret = cfg.jwt_secret.clone();

    HttpServer::new(move || {
        let auth_mw = JwtAuth::new(jwt_secret.clone());
        App::new()
            .app_data(web::Data::new(app_state.clone()))
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::JsonConfig::default().limit(4 * 1024 * 1024)) // 4 MB
            .wrap(Logger::default())
            .wrap(TracingLogger::default())
            .wrap(Cors::permissive())
            .service(
                web::scope("/api/maint")
                    .configure(handlers::health::configure)
                    .service(
                        web::scope("")
                            .wrap(auth_mw)
                            .configure(handlers::cache_vehicles::configure)
                            .configure(handlers::cache_invoices::configure)
                            .configure(handlers::templates::configure)
                            .configure(handlers::records::configure)
                            .configure(handlers::due::configure)
                            .configure(handlers::overrides::configure)
                            .configure(handlers::chassis::configure)
                            .configure(handlers::tires::configure)
                            .configure(handlers::assignments::configure)
                            .configure(handlers::sync::configure),
                    ),
            )
    })
    .bind(bind)?
    .run()
    .await?;

    Ok(())
}
