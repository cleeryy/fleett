//! `fleett` entry point.

use clap::{Parser, Subcommand};
use fleett::api::{self, AppState};
use fleett::backup;
use fleett::cli;
use fleett::config::{Config, DriverKind, EgressMode, VERSION};
use fleett::db::Db;
use fleett::driver::compose::ComposeDriver;
use fleett::driver::mock::MockDriver;
use fleett::driver::Driver;
use fleett::egress;
use fleett::migrate;
use fleett::secrets;
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "fleett",
    version,
    about = "Control plane for fleet-managed Hermes agents"
)]
struct Cli {
    /// Machine-readable JSON output (client commands).
    #[arg(long, global = true)]
    json: bool,
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
    /// Write a consistent, rotating backup of the whole fleet.
    ///
    /// Runs on the host, no bearer token is needed. See docs/BACKUP.md.
    Backup {
        /// Directory receiving the archive (default `<root>/backups`).
        #[arg(long)]
        dest: Option<PathBuf>,
        /// Archives kept after the new one is written (default 7).
        #[arg(long, default_value_t = backup::DEFAULT_KEEP)]
        keep: usize,
    },
    /// List or restore existing backups.
    ///
    /// No bearer token is needed. Stop the control plane before a restore.
    Backups {
        #[command(subcommand)]
        action: BackupCommands,
    },
    /// List the instances of the fleet (client mode).
    Ls,
    /// Show the running instance containers (client mode).
    Ps {
        /// Include stopped instances.
        #[arg(long)]
        all: bool,
    },
    /// Print the logs of one instance (client mode).
    Logs {
        /// Instance id.
        id: String,
        /// Number of trailing lines (1..=2000).
        #[arg(long, default_value_t = cli::DEFAULT_LOG_TAIL)]
        tail: u32,
        /// Docker duration (30s, 10m, 2h) or an RFC 3339 timestamp.
        #[arg(long)]
        since: Option<String>,
        /// Only keep the lines matching this regex.
        #[arg(long)]
        grep: Option<String>,
        /// Only keep `error` or `warn` lines (`warn` keeps warnings and above).
        #[arg(long)]
        level: Option<String>,
    },
    /// Create an instance (client mode).
    Create {
        /// Instance id (`[a-z0-9-]{2,32}`).
        #[arg(long)]
        id: String,
        /// Display name (defaults to the id).
        #[arg(long)]
        name: Option<String>,
        /// Template id (default `guest`).
        #[arg(long)]
        template: Option<String>,
        /// Model served by the gateway.
        #[arg(long)]
        model: Option<String>,
        /// Provider base URL.
        #[arg(long)]
        base_url: Option<String>,
        /// Environment variable holding the provider key.
        #[arg(long)]
        api_key_env: Option<String>,
        /// Value for `HERMES_GATE_PROFILE_TOKEN` and `api_key_env`.
        #[arg(long)]
        gate_token: Option<String>,
        /// Messaging platform: `telegram`, `discord` or `none`.
        #[arg(long)]
        channel: Option<String>,
        /// Bot token of the platform.
        #[arg(long)]
        channel_token: Option<String>,
        /// Attach the instance to the unrestricted LAN network.
        #[arg(long)]
        allow_lan: bool,
        /// CPU limit, in CPUs (0.1..=8.0).
        #[arg(long)]
        cpu: Option<f64>,
        /// Memory limit, in MB (256..=8192).
        #[arg(long)]
        mem: Option<u32>,
        /// Free-form operator note.
        #[arg(long)]
        note: Option<String>,
    },
    /// Start an instance (client mode).
    Start {
        /// Instance id.
        id: String,
    },
    /// Stop an instance (client mode).
    Stop {
        /// Instance id.
        id: String,
    },
    /// Restart an instance (client mode).
    Restart {
        /// Instance id.
        id: String,
    },
    /// Delete an instance (client mode).
    Delete {
        /// Instance id.
        id: String,
        /// Also erase the instance data directory.
        #[arg(long)]
        purge: bool,
    },
    /// Show the CPU, memory and disk usage of the fleet (client mode).
    Usage,
    /// List the container images present on the host and their instances
    /// (client mode).
    Images,
    /// Move every agent to another container image, rolling back the ones
    /// that never become healthy (client mode). See docs/UPGRADE.md.
    Upgrade {
        /// Image tag every instance is moved to.
        #[arg(long)]
        image: String,
        /// Instances upgraded at once (default 1: sequential).
        #[arg(long, default_value_t = 1)]
        parallel: usize,
        /// Only list the planned changes, never touch an instance.
        #[arg(long)]
        dry_run: bool,
        /// Seconds to wait for an instance to become healthy (default 180).
        #[arg(long, default_value_t = fleett::models::UPGRADE_TIMEOUT_DEFAULT_S)]
        timeout: u64,
    },
    /// Check the API, authentication, Docker, secrets and egress (client mode).
    Doctor,
    /// Send a message to another instance over the fleet bus (client mode).
    Say {
        /// Recipient instance id.
        to: String,
        /// Message body (16 KiB max).
        body: String,
        /// `ask`, `tell` or `result`.
        #[arg(long, default_value = "tell")]
        kind: String,
        /// Sending instance (defaults to FLEETT_INSTANCE).
        #[arg(long)]
        from: Option<String>,
        /// Conversation id; the bus uses it to stop request/reply loops.
        #[arg(long)]
        correlation: Option<String>,
        /// Time the message stays queued, in seconds (30..2592000).
        #[arg(long)]
        ttl: Option<u64>,
    },
    /// Read the message inbox or outbox of an instance (client mode).
    Messages {
        /// Instance id.
        id: String,
        /// `inbox`, `outbox` or `all`.
        #[arg(long, default_value = "inbox")]
        view: String,
        /// `queued`, `delivered` or `read`.
        #[arg(long)]
        status: Option<String>,
        /// Maximum rows (1..=500).
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Mark one message as read (client mode).
    Ack {
        /// Recipient instance id.
        id: String,
        /// Message id.
        message_id: String,
    },
    /// Read and write the shared fleet memory (client mode).
    Memory {
        #[command(subcommand)]
        action: MemoryCommands,
    },
    /// List the outbound webhooks of the control plane (client mode).
    Webhooks,
    /// Manage one outbound webhook (client mode).
    Webhook {
        #[command(subcommand)]
        action: WebhookCommands,
    },
    /// Show the recent deliveries of one webhook (client mode).
    Deliveries {
        /// Webhook id.
        id: String,
        /// Maximum rows (1..=200).
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}

#[derive(Subcommand)]
enum WebhookCommands {
    /// Create a webhook subscribed to one or more events.
    Add {
        /// Display name.
        #[arg(long)]
        name: String,
        /// Endpoint URL (http or https).
        #[arg(long)]
        url: String,
        /// Comma-separated events or prefix wildcards (`instance.started,upgrade.*`).
        #[arg(long)]
        events: String,
        /// HMAC-SHA256 signing secret (never returned by the API).
        #[arg(long)]
        secret: String,
    },
    /// Delete a webhook and its delivery log.
    Rm {
        /// Webhook id.
        id: String,
    },
    /// Send one signed `webhook.test` delivery.
    Test {
        /// Webhook id.
        id: String,
    },
}

#[derive(Subcommand)]
enum MemoryCommands {
    /// Create or update one entry.
    Set {
        /// `global`, `shared`, `instance:<id>` or `project:<name>`.
        namespace: String,
        key: String,
        value: String,
        /// Tag, repeatable.
        #[arg(long)]
        tag: Vec<String>,
        /// Expire after N seconds.
        #[arg(long)]
        ttl: Option<u64>,
    },
    /// Read one entry by namespace and key.
    Get { namespace: String, key: String },
    /// Browse entries.
    List {
        #[arg(long)]
        namespace: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Full text search.
    Search {
        query: String,
        #[arg(long)]
        namespace: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show every version of one entry.
    History { id: i64 },
    /// Restore an old version as a new one.
    Restore { id: i64, version: u32 },
    /// Soft-delete one entry.
    Delete { id: i64 },
    /// Copy an entry into another namespace (default global).
    Promote {
        id: i64,
        #[arg(long, default_value = "global")]
        namespace: String,
    },
}

#[derive(Subcommand, Clone)]
enum BackupCommands {
    /// List the archives of the backup directory, newest first.
    List {
        /// Directory holding the archives (default `<root>/backups`).
        #[arg(long)]
        dest: Option<PathBuf>,
    },
    /// Restore one archive into an empty data root.
    Restore {
        /// Archive produced by `fleett backup`.
        file: PathBuf,
        /// Data root to restore into (default `FLEETT_ROOT`).
        #[arg(long)]
        root: Option<PathBuf>,
        /// Accept a non-empty root and overwrite its files.
        #[arg(long)]
        force: bool,
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
    let json = cli.json;

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        // Logs never pollute the machine-readable stdout of the CLI.
        .with_writer(std::io::stderr)
        .init();

    let code = match cli.command.unwrap_or(Commands::Serve) {
        Commands::Serve => {
            serve(Config::from_env()?).await?;
            0
        }
        Commands::Migrate { from, apply } => {
            // Migration never serves HTTP: no bearer token is needed.
            let cfg = Config::from_env_for_maintenance()?;
            migrate::run(&cfg, &from, apply).await?;
            0
        }
        Commands::Egress { action } => {
            // Firewall commands never serve HTTP: no bearer token is needed.
            let cfg = Config::from_env_for_maintenance()?;
            egress::run(action.into(), &cfg)?
        }
        Commands::Secrets { action } => {
            // Key management never serves HTTP: no bearer token is needed.
            let cfg = Config::from_env_for_maintenance()?;
            match action {
                SecretsCommands::Init => secrets::run_init(&cfg),
                SecretsCommands::Status => secrets::run_status(&cfg),
                SecretsCommands::Migrate { apply } => secrets::run_migrate(&cfg, apply),
            }?;
            0
        }
        Commands::Backup { dest, keep } => {
            // Backups never serve HTTP: no bearer token is needed.
            let cfg = Config::from_env_for_maintenance()?;
            backup::run_backup(&cfg, dest, keep)?;
            0
        }
        Commands::Backups { action } => {
            let cfg = Config::from_env_for_maintenance()?;
            match action {
                BackupCommands::List { dest } => backup::run_list(&cfg, dest),
                BackupCommands::Restore { file, root, force } => {
                    backup::run_restore(&cfg, file, root, force)
                }
            }?;
            0
        }
        // Client commands read ~/.config/fleett/config.toml and never touch
        // the local data root or Docker.
        Commands::Ls => cli::run(cli::Command::Ls, json).await,
        Commands::Ps { all } => cli::run(cli::Command::Ps { all }, json).await,
        Commands::Logs {
            id,
            tail,
            since,
            grep,
            level,
        } => {
            cli::run(
                cli::Command::Logs(cli::LogsArgs {
                    id,
                    tail,
                    since,
                    grep,
                    level,
                }),
                json,
            )
            .await
        }
        Commands::Create {
            id,
            name,
            template,
            model,
            base_url,
            api_key_env,
            gate_token,
            channel,
            channel_token,
            allow_lan,
            cpu,
            mem,
            note,
        } => {
            cli::run(
                cli::Command::Create(cli::CreateArgs {
                    id,
                    name,
                    template,
                    model,
                    base_url,
                    api_key_env,
                    gate_token,
                    channel,
                    channel_token,
                    allow_lan,
                    cpu,
                    mem,
                    note,
                }),
                json,
            )
            .await
        }
        Commands::Start { id } => cli::run(cli::Command::Start(id), json).await,
        Commands::Stop { id } => cli::run(cli::Command::Stop(id), json).await,
        Commands::Restart { id } => cli::run(cli::Command::Restart(id), json).await,
        Commands::Delete { id, purge } => cli::run(cli::Command::Delete { id, purge }, json).await,
        Commands::Usage => cli::run(cli::Command::Usage, json).await,
        Commands::Images => cli::run(cli::Command::Images, json).await,
        Commands::Upgrade {
            image,
            parallel,
            dry_run,
            timeout,
        } => {
            cli::run(
                cli::Command::Upgrade(cli::UpgradeArgs {
                    image,
                    parallel,
                    dry_run,
                    timeout_s: timeout,
                }),
                json,
            )
            .await
        }
        Commands::Doctor => cli::run(cli::Command::Doctor, json).await,
        Commands::Say {
            to,
            body,
            kind,
            from,
            correlation,
            ttl,
        } => {
            cli::run(
                cli::Command::Say(cli::SayArgs {
                    to,
                    body,
                    kind,
                    from,
                    correlation,
                    ttl_s: ttl,
                }),
                json,
            )
            .await
        }
        Commands::Messages {
            id,
            view,
            status,
            limit,
        } => {
            cli::run(
                cli::Command::Messages(cli::MessagesArgs {
                    id,
                    view,
                    status,
                    limit,
                }),
                json,
            )
            .await
        }
        Commands::Ack { id, message_id } => {
            cli::run(cli::Command::Ack { id, message_id }, json).await
        }
        Commands::Memory { action } => {
            let command = match action {
                MemoryCommands::Set {
                    namespace,
                    key,
                    value,
                    tag,
                    ttl,
                } => cli::MemoryCommand::Set {
                    namespace,
                    key,
                    value,
                    tags: tag,
                    ttl_s: ttl,
                },
                MemoryCommands::Get { namespace, key } => {
                    cli::MemoryCommand::Get { namespace, key }
                }
                MemoryCommands::List { namespace, limit } => {
                    cli::MemoryCommand::List { namespace, limit }
                }
                MemoryCommands::Search {
                    query,
                    namespace,
                    limit,
                } => cli::MemoryCommand::Search {
                    query,
                    namespace,
                    limit,
                },
                MemoryCommands::History { id } => cli::MemoryCommand::History { id },
                MemoryCommands::Restore { id, version } => {
                    cli::MemoryCommand::Restore { id, version }
                }
                MemoryCommands::Delete { id } => cli::MemoryCommand::Delete { id },
                MemoryCommands::Promote { id, namespace } => {
                    cli::MemoryCommand::Promote { id, namespace }
                }
            };
            cli::run(cli::Command::Memory(command), json).await
        }
        Commands::Webhooks => cli::run(cli::Command::Webhooks, json).await,
        Commands::Webhook { action } => match action {
            WebhookCommands::Add {
                name,
                url,
                events,
                secret,
            } => {
                let events: Vec<String> = events
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect();
                cli::run(
                    cli::Command::WebhookAdd {
                        name,
                        url,
                        events,
                        secret,
                    },
                    json,
                )
                .await
            }
            WebhookCommands::Rm { id } => cli::run(cli::Command::WebhookRemove(id), json).await,
            WebhookCommands::Test { id } => cli::run(cli::Command::WebhookTest(id), json).await,
        },
        Commands::Deliveries { id, limit } => {
            cli::run(cli::Command::Deliveries { id, limit }, json).await
        }
    };

    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
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
    let app = api::router(state.clone());

    if let Some(bind) = cfg.metrics.bind {
        spawn_metrics_listener(state.clone(), bind).await?;
    }

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

/// Start the dedicated Prometheus listener (`FLEETT_METRICS_BIND`).
///
/// It is a second, independent listener: a scrape never reaches the API port and
/// never needs an API token. A bind failure is fatal — an operator who asked for
/// metrics must not discover months later that the port was never opened.
async fn spawn_metrics_listener(state: AppState, bind: std::net::SocketAddr) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(
        bind = %bind,
        path = %fleett::metrics::METRICS_PATH,
        token = state.cfg.metrics.requires_token(),
        "metrics listening"
    );
    let app = fleett::metrics::router(state);
    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            tracing::error!(%err, "the metrics listener stopped");
        }
    });
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
