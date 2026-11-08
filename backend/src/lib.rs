//! fleett — control plane for fleet-managed Hermes agents.

pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod driver;
pub mod egress;
pub mod logs;
pub mod migrate;
pub mod models;
pub mod render;
pub mod secrets;
pub mod store;
pub mod templates_builtin;

pub use api::{router, AppState};
pub use config::Config;
