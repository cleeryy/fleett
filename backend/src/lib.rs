//! fleett — control plane for fleet-managed Hermes agents.

pub mod api;
pub mod archive;
pub mod auth;
pub mod backup;
pub mod bus;
pub mod cli;
pub mod config;
pub mod db;
pub mod driver;
pub mod egress;
pub mod envs;
pub mod files;
pub mod history;
pub mod instance_archive;
pub mod logs;
pub mod metrics;
pub mod migrate;
pub mod models;
pub mod providers;
pub mod render;
pub mod secrets;
pub mod store;
pub mod templates_builtin;

pub use api::{router, AppState};
pub use config::Config;
