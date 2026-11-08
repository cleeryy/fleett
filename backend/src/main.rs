//! `fleett` entry point.

use clap::{Parser, Subcommand};
use fleett::api::{self, AppState};
use fleett::config::{Config, DriverKind, EgressMode, VERSION};
use fleett::db::Db;
use fleett::driver::compose::ComposeDriver;
use fleett::driver::mock::MockDriver;
use fleett::driver::Driver;
use fleett::egress;
use fleett::migrate;
use fleett::secrets;
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
    /// Manage the host firewall that keeps agent egress off the LAN.
    ///
    /// Runs on the Docker host, as root or with CAP_NET_ADMIN; no bearer token
    /// is needed. See docs/EGRESS.md.
    Egress {
        #[command(subcommand)]
        action: EgressCommands,
    },
    /// Manage the key that encrypts instance secrets at rest.
    ///
    /// No bearer token is needed. See docs/SECRETS.md.
    Secrets {
        #[command(subcommand)]
        action: SecretsCommands,
    },
}

#[derive(Subcommand, Clone, Copy, PartialEq, Eq)]
enum SecretsCommands {
    /// Generate the key file (0640); refuses to overwrite an existing one.
    Init,
    /// Show the key source and the encrypted vs legacy instance counts.
    Status,
    /// Encrypt every legacy secrets.env into the database.
    Migrate {
        /// Delete the plaintext files once they are encrypted.
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand, Clone, Copy, PartialEq, Eq)]
enum EgressCommands {
    /// Install the DOCKER-USER rules for the current fleett subnet (idempotent).
    Install,
    /// Delete the rules tagged `fleett-egress`, then the marker.
    Remove,
    /// Report whether every rule is installed (exit 0 when it is).
    Status,
    /// Print the exact iptables commands without running them.
    Print,
}

impl From<EgressCommands> for egress::Action {
    fn from(command: EgressCommands) -> Self {
        match command {
            EgressCommands::Install => Self::Install,
            EgressCommands::Remove => Self::Remove,
            EgressCommands::Status => Self::Status,
            EgressCommands::Print => Self::Print,
        }
    }
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
        Commands::Egress { action } => {
            // Firewall commands never serve HTTP: no bearer token is needed.
            let cfg = Config::from_env_for_maintenance()?;
            let code = egress::run(action.into(), &cfg)?;
            std::process::exit(code);
        }
        Commands::Secrets { action } => {
            // Key management never serves HTTP: no bearer token is needed.
            let cfg = Config::from_env_for_maintenance()?;
            match action {
                SecretsCommands::Init => secrets::run_init(&cfg),
                SecretsCommands::Status => secrets::run_status(&cfg),
                SecretsCommands::Migrate { apply } => secrets::run_migrate(&cfg, apply),
            }
        }
    }
}

async fn serve(cfg: Config) -> anyhow::Result<()> {
    if cfg.allow_no_auth {
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
    if cfg.allow_no_auth && cfg.auth.oidc_enabled() {
        tracing::warn!(
            "FLEETT_ALLOW_NO_AUTH=1 bypasses the OIDC login: any request without a session is \
             treated as an anonymous admin"
        );
    }
    tracing::info!(
        auth = %cfg.auth,
        oidc = cfg.auth.oidc_enabled(),
        "authentication configured"
    );

    if cfg.env_file_path().is_file() {
        tracing::info!(
            path = %cfg.env_file_path().display(),
            "loaded environment defaults"
        );
    }

    std::fs::create_dir_all(&cfg.root)?;

    // One database handle shared by the index, the compose driver (which reads
    // the encrypted secrets) and the HTTP state.
    let db = Db::open(&cfg.db_path())?;
    secrets::startup(&cfg, &db);

    warn_about_legacy_projects(&cfg);

    let driver: Arc<dyn Driver> = match cfg.driver {
        DriverKind::Mock => {
            tracing::warn!("FLEETT_DRIVER=mock: no container is actually created");
            Arc::new(MockDriver::with_image(cfg.image.clone()))
        }
        DriverKind::Compose => {
            let compose = ComposeDriver::new(&cfg).with_secrets(db.clone());
            match compose.ensure_network_named(&cfg.network).await {
                Ok(true) => tracing::info!(network = %cfg.network, "created the docker network"),
                Ok(false) => {}
                Err(err) => {
                    tracing::warn!(%err, "could not verify the docker network")
                }
            }
            Arc::new(compose)
        }
    };

    warn_about_egress(&cfg, driver.clone()).await;

    let state = AppState::bootstrap_with_db(cfg.clone(), driver, db)?;
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

/// Warn once when the host egress firewall is not in force.
///
/// `FLEETT_EGRESS=restrict` (the default) is only meaningful when the operator
/// ran `fleett egress install`; `off` is always reported.
async fn warn_about_egress(cfg: &Config, driver: Arc<dyn Driver>) {
    match cfg.egress {
        EgressMode::Off => {
            tracing::warn!(
                "FLEETT_EGRESS=off: agents are not isolated from the LAN; set \
                 FLEETT_EGRESS=restrict and run `sudo fleett egress install`"
            );
        }
        EgressMode::Restrict => {
            let marker = egress::read_marker(&cfg.egress_marker);
            let subnet = driver.network_subnet(&cfg.network).await.ok().flatten();
            if let Err(detail) = egress::check_marker(&marker, subnet.as_deref()) {
                tracing::warn!(
                    marker = %cfg.egress_marker.display(),
                    %detail,
                    "creating a restricted instance will be refused until the firewall is installed"
                );
            }
        }
    }
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
