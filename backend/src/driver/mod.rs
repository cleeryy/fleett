//! Container drivers.
//!
//! `FLEET_DRIVER=compose` talks to Docker through `docker compose -f …`; the
//! mock driver keeps every instance in memory and never touches Docker, which
//! is what the test suite uses.

pub mod compose;
pub mod mock;

use crate::models::{ContainerInfo, InstanceSpec, Status};
use std::future::Future;
use std::pin::Pin;

/// Result of `docker compose ps` for one instance.
#[derive(Debug, Clone)]
pub struct PsResult {
    pub status: Status,
    pub container: Option<ContainerInfo>,
}

/// A failed driver call, carrying the compose stderr.
#[derive(Debug, Clone, thiserror::Error)]
#[error("docker compose {operation} failed: {detail}")]
pub struct DriverError {
    pub operation: String,
    pub detail: String,
}

impl DriverError {
    pub fn new(operation: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            detail: detail.into(),
        }
    }
}

pub type DriverFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Lifecycle operations the control plane needs.
pub trait Driver: Send + Sync {
    fn name(&self) -> &'static str;

    /// Cheap liveness probe for `GET /api/health`.
    fn ping(&self) -> DriverFuture<'_, bool>;

    fn up<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>>;
    fn stop<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>>;
    fn start<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>>;
    fn restart<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>>;
    fn down<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>>;
    fn ps<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<PsResult, DriverError>>;
    fn logs<'a>(
        &'a self,
        spec: &'a InstanceSpec,
        tail: u32,
    ) -> DriverFuture<'a, Result<String, DriverError>>;
}
