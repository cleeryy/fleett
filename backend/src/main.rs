//! `fleett` entry point.

use clap::{Parser, Subcommand};
use fleett::api::{self, AppState};
use fleett::config::{Config, DriverKind, VERSION};
use fleett::driver::compose::ComposeDriver;
use fleett::driver::mock::MockDriver;
use fleett::driver::Driver;
use fleett::migrate;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "fleett",
    version,
    about = "Control plane for fleet-managed Hermes agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Serve the control plane (default when no subcommand is given).
    Serve,
    /// Rename legacy compose projects to the current `fleett-<id>` prefix.
    Migrate {
        /// Name of the product that created the legacy projects.
        #[arg(long, default_value = migrate::SOURCE)]
        from: String,
        /// Rewrite the projects and restart the ones that were running.
        #[arg(long)]
        apply: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    match cli.command.unwrap_or(Commands::Serve) {
        Commands::Serve => serve(Config::from_env()?).await,
        Commands::Migrate { from, apply } => {
            // Migration never serves HTTP: no bearer token is needed.
            let cfg = Config::from_env_for_maintenance()?;
            migrate::run(&cfg, &from, apply).await
        }
    }
}

async fn serve(cfg: Config) -> anyhow::Result<()> {
    if cfg.token.is_none() {
        tracing::warn!(
            bind = %cfg.bind,
            "FLEETT_ALLOW_NO_AUTH=1: authentication is disabled, everyone who can reach the \
             port controls every agent"
        );
        eprintln!(
            "WARNING: authentication is disabled (FLEETT_ALLOW_NO_AUTH=1).\n\
             WARNING: anyone who can reach {} can create and destroy agents.\n\
             WARNING: bind it to localhost or put it behind an authenticating proxy.",
            cfg.bind
        );
    }

    if cfg.env_file_path().is_file() {
        tracing::info!(
            path = %cfg.env_file_path().display(),
            "loaded environment defaults"
        );
    }

    std::fs::create_dir_all(&cfg.root)?;

    warn_about_legacy_projects(&cfg);

    let driver: Arc<dyn Driver> = match cfg.driver {
        DriverKind::Mock => {
            tracing::warn!("FLEETT_DRIVER=mock: no container is actually created");
            Arc::new(MockDriver::with_image(cfg.image.clone()))
        }
        DriverKind::Compose => {
            let compose = ComposeDriver::new(&cfg);
            match compose.ensure_network().await {
                Ok(true) => tracing::info!(network = %cfg.network, "created the docker network"),
                Ok(false) => {}
                Err(err) => {
                    tracing::warn!(%err, "could not verify the docker network")
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
        "fleett listening"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("control plane stopped; agents keep running");
    Ok(())
}

/// One warning (not one per instance) when legacy projects are still around.
fn warn_about_legacy_projects(cfg: &Config) {
    let legacy = migrate::plan(cfg);
    if legacy.is_empty() {
        return;
    }
    tracing::warn!(
        count = legacy.len(),
        "found {} compose project(s) still declared as hermes-<id>; run \
         `fleett migrate --from {} --apply` to rename them to fleett-<id>",
        legacy.len(),
        migrate::SOURCE
    );
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
