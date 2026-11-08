//! hermes-fleet — control plane for fleet-managed Hermes agents.

pub mod api;
pub mod config;
pub mod db;
pub mod driver;
pub mod models;
pub mod render;
pub mod store;
pub mod templates_builtin;

pub use api::{router, AppState};
pub use config::Config;
