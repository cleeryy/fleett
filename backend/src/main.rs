//! `hermes-fleet` entry point.

use clap::Parser;
use hermes_fleet::api::{self, AppState};
use hermes_fleet::config::{Config, DriverKind, VERSION};
use hermes_fleet::driver::compose::ComposeDriver;
use hermes_fleet::driver::mock::MockDriver;
use hermes_fleet::driver::Driver;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "hermes-fleet",
    version,
    about = "Control plane for fleet-managed Hermes agents"
)]
struct Cli {}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cfg = Config::from_env()?;

    if cfg.token.is_none() {
        tracing::warn!(
            bind = %cfg.bind,
            "FLEET_ALLOW_NO_AUTH=1: authentication is disabled, everyone who can reach the \
             port controls every agent"
        );
        eprintln!(
            "WARNING: authentication is disabled (FLEET_ALLOW_NO_AUTH=1).\n\
             WARNING: anyone who can reach {} can create and destroy agents.\n\
             WARNING: bind it to localhost or put it behind an authenticating proxy.",
            cfg.bind
        );
    }

    if let Some(path) = cfg.env_file_path().is_file().then(|| cfg.env_file_path()) {
        tracing::info!(path = %path.display(), "loaded environment defaults from fleet.env");
    }

    std::fs::create_dir_all(&cfg.root)?;

    let driver: Arc<dyn Driver> = match cfg.driver {
        DriverKind::Mock => {
            tracing::warn!("FLEET_DRIVER=mock: no container is actually created");
            Arc::new(MockDriver::with_image(cfg.image.clone()))
        }
        DriverKind::Compose => {
            let compose = ComposeDriver::new(&cfg);
            match compose.ensure_network().await {
                Ok(true) => tracing::info!(network = %cfg.network, "created the fleet network"),
                Ok(false) => {}
                Err(err) => {
                    tracing::warn!(%err, "could not verify the fleet docker network")
                }
            }
            Arc::new(compose)
        }
    };

    let state = AppState::bootstrap(cfg.clone(), driver)?;
    let app = api::router(state);

    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    tracing::info!(
        version = VERSION,
        bind = %cfg.bind,
        driver = %cfg.driver,
        root = %cfg.root.display(),
        image = %cfg.image,
        network = %cfg.network,
        "hermes-fleet listening"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("control plane stopped; agents keep running");
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
