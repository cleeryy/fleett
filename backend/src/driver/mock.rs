//! In-memory driver used by the tests and by `FLEETT_DRIVER=mock`.
//!
//! It records every call, keeps a per-instance state, and can be told to fail
//! the next call of a given operation so error paths are testable without
//! Docker.

use super::{Driver, DriverError, DriverFuture, PsResult};
use crate::config::DEFAULT_IMAGE;
use crate::models::{ContainerInfo, Health, InstanceSpec, Status};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;
use uuid::Uuid;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One recorded lifecycle call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockCall {
    pub operation: &'static str,
    pub instance: String,
}

#[derive(Debug)]
struct MockInstance {
    status: Status,
    health: Health,
    container_id: String,
    image: String,
    started_at: Option<SystemTime>,
    ups: u32,
}

/// Deterministic, Docker-free driver.
#[derive(Debug)]
pub struct MockDriver {
    image: String,
    state: Mutex<HashMap<String, MockInstance>>,
    calls: Mutex<Vec<MockCall>>,
    failures: Mutex<HashMap<String, String>>,
}

impl Default for MockDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl MockDriver {
    pub fn new() -> Self {
        Self::with_image(DEFAULT_IMAGE)
    }

    pub fn with_image(image: impl Into<String>) -> Self {
        Self {
            image: image.into(),
            state: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
            failures: Mutex::new(HashMap::new()),
        }
    }

    /// Every call recorded so far, oldest first.
    pub fn calls(&self) -> Vec<MockCall> {
        lock(&self.calls).clone()
    }

    pub fn clear_calls(&self) {
        lock(&self.calls).clear();
    }

    /// Make the next call of `operation` fail with `stderr`.
    pub fn fail_next(&self, operation: &str, stderr: impl Into<String>) {
        lock(&self.failures).insert(operation.to_string(), stderr.into());
    }

    pub fn clear_failures(&self) {
        lock(&self.failures).clear();
    }

    /// Force a status without going through an operation (test helper).
    pub fn force_status(&self, id: &str, status: Status) {
        let mut state = lock(&self.state);
        let entry = state.entry(id.to_string()).or_insert_with(|| MockInstance {
            status,
            health: Health::Unknown,
            container_id: Uuid::new_v4().to_string(),
            image: self.image.clone(),
            started_at: Some(SystemTime::now()),
            ups: 0,
        });
        entry.status = status;
        if status != Status::Running {
            entry.started_at = None;
        }
    }

    /// Force the container health without going through an operation.
    pub fn force_health(&self, id: &str, health: Health) {
        let mut state = lock(&self.state);
        let entry = state.entry(id.to_string()).or_insert_with(|| MockInstance {
            status: Status::Running,
            health,
            container_id: Uuid::new_v4().to_string(),
            image: self.image.clone(),
            started_at: Some(SystemTime::now()),
            ups: 0,
        });
        entry.health = health;
    }

    fn record(&self, operation: &'static str, id: &str) {
        lock(&self.calls).push(MockCall {
            operation,
            instance: id.to_string(),
        });
    }

    fn fail_if_requested(&self, operation: &str) -> Result<(), DriverError> {
        let mut failures = lock(&self.failures);
        match failures.remove(operation) {
            Some(detail) => Err(DriverError::new(operation, detail)),
            None => Ok(()),
        }
    }

    fn ensure_instance(&self, id: &str) -> MutexGuard<'_, HashMap<String, MockInstance>> {
        let mut state = lock(&self.state);
        state.entry(id.to_string()).or_insert_with(|| MockInstance {
            status: Status::Created,
            health: Health::Unknown,
            container_id: Uuid::new_v4().to_string(),
            image: self.image.clone(),
            started_at: None,
            ups: 0,
        });
        state
    }

    fn set_running(&self, id: &str) {
        let mut state = self.ensure_instance(id);
        if let Some(entry) = state.get_mut(id) {
            entry.status = Status::Running;
            entry.health = Health::Healthy;
            entry.started_at = Some(SystemTime::now());
        }
    }

    fn logs_for(&self, id: &str, tail: u32) -> String {
        let state = lock(&self.state);
        let (status, ups, image) = match state.get(id) {
            Some(entry) => (entry.status, entry.ups, entry.image.clone()),
            None => (Status::Unknown, 0, self.image.clone()),
        };
        let lines = [
            format!("[mock] fleett-{id}: image={image} status={status}"),
            format!("[mock] lifecycle calls: up={ups}"),
            format!("[mock] instance {id} is simulated by FLEETT_DRIVER=mock"),
            "[mock] no container is running; start the control plane with FLEETT_DRIVER=compose"
                .to_string(),
        ];
        let start = lines.len().saturating_sub(tail as usize);
        let mut selected = lines[start..].join("\n");
        selected.push('\n');
        selected
    }
}

impl Driver for MockDriver {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn ping(&self) -> DriverFuture<'_, bool> {
        Box::pin(async move { true })
    }

    fn up<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.record("up", &spec.id);
            self.fail_if_requested("up")?;
            let mut state = self.ensure_instance(&spec.id);
            if let Some(entry) = state.get_mut(&spec.id) {
                entry.status = Status::Running;
                entry.health = Health::Healthy;
                entry.started_at = Some(SystemTime::now());
                entry.ups += 1;
            }
            Ok(())
        })
    }

    fn stop<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.record("stop", &spec.id);
            self.fail_if_requested("stop")?;
            let mut state = self.ensure_instance(&spec.id);
            if let Some(entry) = state.get_mut(&spec.id) {
                entry.status = Status::Stopped;
                entry.health = Health::Unknown;
                entry.started_at = None;
            }
            Ok(())
        })
    }

    fn start<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.record("start", &spec.id);
            self.fail_if_requested("start")?;
            self.set_running(&spec.id);
            Ok(())
        })
    }

    fn restart<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.record("restart", &spec.id);
            self.fail_if_requested("restart")?;
            self.set_running(&spec.id);
            Ok(())
        })
    }

    fn down<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.record("down", &spec.id);
            self.fail_if_requested("down")?;
            lock(&self.state).remove(&spec.id);
            Ok(())
        })
    }

    fn ps<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<PsResult, DriverError>> {
        Box::pin(async move {
            self.record("ps", &spec.id);
            self.fail_if_requested("ps")?;
            let state = lock(&self.state);
            let Some(entry) = state.get(&spec.id) else {
                return Ok(PsResult {
                    status: Status::Unknown,
                    health: Health::Unknown,
                    container: None,
                });
            };
            let uptime_s = match (entry.status, entry.started_at) {
                (Status::Running, Some(started)) => started
                    .elapsed()
                    .ok()
                    .map(|elapsed| elapsed.as_secs() as i64),
                _ => None,
            };
            Ok(PsResult {
                status: entry.status,
                health: entry.health,
                container: Some(ContainerInfo {
                    id: Some(entry.container_id.clone()),
                    image: Some(entry.image.clone()),
                    uptime_s,
                }),
            })
        })
    }

    fn logs<'a>(
        &'a self,
        spec: &'a InstanceSpec,
        tail: u32,
    ) -> DriverFuture<'a, Result<String, DriverError>> {
        Box::pin(async move {
            self.record("logs", &spec.id);
            self.fail_if_requested("logs")?;
            Ok(self.logs_for(&spec.id, tail))
        })
    }
}
