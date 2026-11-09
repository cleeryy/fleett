//! In-memory driver used by the tests and by `FLEETT_DRIVER=mock`.
//!
//! It records every call, keeps a per-instance state, and can be told to fail
//! the next call of a given operation so error paths are testable without
//! Docker.

use super::{
    Driver, DriverError, DriverFuture, ExecOutput, ExecResult, ImageRecord, InspectInfo,
    LogsOptions, PsResult, UsageSample,
};
use crate::config::DEFAULT_IMAGE;
use crate::hosts::HostCapacity;
use crate::models::{ContainerInfo, Health, InstanceSpec, Status};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime};
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
    /// Host recorded through `InstanceSpec::host` at call time.
    pub host: String,
}

/// One recorded `exec` call (pipelines, issue #31).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockExecCall {
    pub instance: String,
    pub argv: Vec<String>,
    pub timeout_s: u64,
}

#[derive(Debug)]
struct MockInstance {
    status: Status,
    health: Health,
    container_id: String,
    image: String,
    started_at: Option<SystemTime>,
    ups: u32,
    restart_count: u32,
}

/// Deterministic, Docker-free driver.
#[derive(Debug)]
pub struct MockDriver {
    image: String,
    state: Mutex<HashMap<String, MockInstance>>,
    calls: Mutex<Vec<MockCall>>,
    failures: Mutex<HashMap<String, String>>,
    /// Reported by `network_subnet`; `None` means "unknown".
    network_subnet: Mutex<Option<String>>,
    /// Canned log output, per instance (`set_logs`).
    logs: Mutex<HashMap<String, String>>,
    /// Every `logs` call with the options the API passed down.
    logs_options: Mutex<Vec<(String, LogsOptions)>>,
    /// Canned `docker inspect` payloads, per instance (`force_inspect`).
    inspect: Mutex<HashMap<String, InspectInfo>>,
    /// Canned usage samples, per instance (`force_usage`).
    usage: Mutex<HashMap<String, UsageSample>>,
    /// Health reported once an instance runs the given image (`set_image_health`).
    image_health: Mutex<HashMap<String, Health>>,
    /// Images whose pull must fail (`fail_image_pull`).
    pull_failures: Mutex<HashMap<String, String>>,
    /// Images present in the simulated local store.
    known_images: Mutex<Vec<String>>,
    /// Every `ensure_image` target, oldest first (without its host).
    image_calls: Mutex<Vec<String>>,
    /// Every `(host, image)` pair passed to `ensure_image`.
    image_hosts: Mutex<Vec<(String, String)>>,
    /// Canned capacity of each host (`probe_host`).
    host_capacity: Mutex<HashMap<String, HostCapacity>>,
    /// Host ids whose probe must fail.
    host_failures: Mutex<HashMap<String, String>>,
    /// Every host id probed, oldest first.
    host_probes: Mutex<Vec<String>>,
    /// Host ids whose `usage` call must fail (unreachable daemon).
    usage_failures: Mutex<HashMap<String, String>>,
    /// `up` sleeps this long, so the upgrade tests can observe concurrency.
    up_delay: Mutex<Option<Duration>>,
    in_flight_ups: AtomicUsize,
    max_in_flight_ups: AtomicUsize,
    /// Every `exec` call, oldest first.
    exec_calls: Mutex<Vec<MockExecCall>>,
    /// Canned results consumed by the next `exec` calls, per instance.
    exec_results: Mutex<HashMap<String, VecDeque<ExecOutput>>>,
    /// Persistent canned result (`force_exec`), consumed by every call.
    exec_sticky: Mutex<HashMap<String, ExecOutput>>,
    /// `exec` sleeps this long, so pipeline tests can observe concurrency.
    exec_delay: Mutex<Option<Duration>>,
    in_flight_execs: AtomicUsize,
    max_in_flight_execs: AtomicUsize,
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
        let image = image.into();
        let mut known = vec![DEFAULT_IMAGE.to_string()];
        if !known.contains(&image) {
            known.push(image.clone());
        }
        Self {
            image,
            state: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
            failures: Mutex::new(HashMap::new()),
            network_subnet: Mutex::new(None),
            logs: Mutex::new(HashMap::new()),
            logs_options: Mutex::new(Vec::new()),
            inspect: Mutex::new(HashMap::new()),
            usage: Mutex::new(HashMap::new()),
            image_health: Mutex::new(HashMap::new()),
            pull_failures: Mutex::new(HashMap::new()),
            known_images: Mutex::new(known),
            image_calls: Mutex::new(Vec::new()),
            image_hosts: Mutex::new(Vec::new()),
            host_capacity: Mutex::new(HashMap::new()),
            host_failures: Mutex::new(HashMap::new()),
            host_probes: Mutex::new(Vec::new()),
            usage_failures: Mutex::new(HashMap::new()),
            up_delay: Mutex::new(None),
            in_flight_ups: AtomicUsize::new(0),
            max_in_flight_ups: AtomicUsize::new(0),
            exec_calls: Mutex::new(Vec::new()),
            exec_results: Mutex::new(HashMap::new()),
            exec_sticky: Mutex::new(HashMap::new()),
            exec_delay: Mutex::new(None),
            in_flight_execs: AtomicUsize::new(0),
            max_in_flight_execs: AtomicUsize::new(0),
        }
    }

    /// Report an unhealthy (or starting) container as soon as it runs `image`.
    ///
    /// `up` sets the image of the instance to `spec.image`, then looks the
    /// health up here instead of the usual `healthy`. This is how the tests
    /// simulate a broken image without Docker.
    pub fn set_image_health(&self, image: &str, health: Health) {
        lock(&self.image_health).insert(image.to_string(), health);
    }

    /// Change the health of one *existing* simulated container.
    ///
    /// Unlike [`MockDriver::set_image_health`], which applies at the next
    /// `up`, this models a container that was healthy and degrades later (the
    /// canary soak of issue #39).
    pub fn set_instance_health(&self, id: &str, health: Health) {
        if let Some(entry) = lock(&self.state).get_mut(id) {
            entry.health = health;
        }
    }

    /// Make `ensure_image` fail for one tag, as a registry would.
    pub fn fail_image_pull(&self, image: &str, detail: impl Into<String>) {
        lock(&self.pull_failures).insert(image.to_string(), detail.into());
    }

    /// Pretend `image` is already present locally.
    pub fn register_image(&self, image: &str) {
        let mut known = lock(&self.known_images);
        if !known.iter().any(|tag| tag == image) {
            known.push(image.to_string());
        }
        known.sort();
    }

    /// Make every `up` sleep, so a test can observe how many run at once.
    pub fn set_up_delay(&self, delay: Duration) {
        *lock(&self.up_delay) = Some(delay);
    }

    /// Highest number of `up` calls that ran at the same time.
    pub fn max_concurrent_ups(&self) -> usize {
        self.max_in_flight_ups.load(Ordering::SeqCst)
    }

    /// Every image passed to `ensure_image`, oldest first.
    pub fn image_calls(&self) -> Vec<String> {
        lock(&self.image_calls).clone()
    }

    /// Report a canned result for the next `exec` call of one instance.
    ///
    /// Compatibility helper from the scheduler branch: it queues exactly one
    /// result, like [`MockDriver::push_exec_result`].
    pub fn force_exec(&self, id: &str, result: ExecOutput) {
        lock(&self.exec_sticky).insert(id.to_string(), result);
    }

    /// Compatibility helper of the exec API tests: queue one result.
    pub fn set_exec_result(&self, id: &str, result: ExecResult) {
        self.force_exec(id, result);
    }

    /// Drop every queued `exec` result of one instance.
    pub fn clear_exec(&self, id: &str) {
        lock(&self.exec_results).remove(id);
        lock(&self.exec_sticky).remove(id);
    }

    /// Queue one simulated `exec` result for the next call on `id`.
    ///
    /// Results are consumed in order, so a retry test can queue a failure and
    /// then a success. Without a queued result, `exec` echoes the last argument.
    pub fn push_exec_result(&self, id: &str, output: ExecOutput) {
        lock(&self.exec_results)
            .entry(id.to_string())
            .or_default()
            .push_back(output);
    }

    /// Drop every queued `exec` result.
    pub fn clear_exec_results(&self) {
        lock(&self.exec_results).clear();
    }

    /// Every `exec` call recorded so far, oldest first.
    pub fn exec_calls(&self) -> Vec<MockExecCall> {
        lock(&self.exec_calls).clone()
    }

    /// Make every `exec` sleep, so a test can observe how many run at once.
    pub fn set_exec_delay(&self, delay: Duration) {
        *lock(&self.exec_delay) = Some(delay);
    }

    /// Highest number of `exec` calls that ran at the same time.
    pub fn max_concurrent_execs(&self) -> usize {
        self.max_in_flight_execs.load(Ordering::SeqCst)
    }

    /// Every `(host, image)` pair passed to `ensure_image`.
    pub fn image_hosts(&self) -> Vec<(String, String)> {
        lock(&self.image_hosts).clone()
    }

    /// Canned capacity reported by `probe_host` for one host.
    pub fn set_host_capacity(&self, host: &str, capacity: HostCapacity) {
        lock(&self.host_capacity).insert(host.to_string(), capacity);
    }

    /// Make `probe_host` fail for one host, as an unreachable daemon would.
    pub fn fail_host_test(&self, host: &str, detail: impl Into<String>) {
        lock(&self.host_failures).insert(host.to_string(), detail.into());
    }

    /// Make `usage` fail for one host, as an unreachable daemon would.
    pub fn fail_usage_host(&self, host: &str, detail: impl Into<String>) {
        lock(&self.usage_failures).insert(host.to_string(), detail.into());
    }

    /// Every host id probed, oldest first.
    pub fn host_probes(&self) -> Vec<String> {
        lock(&self.host_probes).clone()
    }

    /// Host recorded for the latest call of one operation on one instance.
    pub fn host_of(&self, operation: &str, id: &str) -> Option<String> {
        lock(&self.calls)
            .iter()
            .rev()
            .find(|call| call.operation == operation && call.instance == id)
            .map(|call| call.host.clone())
    }

    /// Image the simulated container of `id` currently runs.
    pub fn image_of(&self, id: &str) -> Option<String> {
        lock(&self.state).get(id).map(|entry| entry.image.clone())
    }

    /// Pretend the `fleett` network has this IPv4 subnet.
    ///
    /// The API compares the egress marker against it; `None` (the default)
    /// keeps the marker authoritative, like a driver that cannot report one.
    pub fn force_network_subnet(&self, subnet: Option<&str>) {
        *lock(&self.network_subnet) = subnet.map(str::to_string);
    }

    /// Every call recorded so far, oldest first.
    pub fn calls(&self) -> Vec<MockCall> {
        lock(&self.calls).clone()
    }

    pub fn clear_calls(&self) {
        lock(&self.calls).clear();
    }

    /// Every `logs` call, with the options the API passed to the driver.
    pub fn logs_calls(&self) -> Vec<(String, LogsOptions)> {
        lock(&self.logs_options).clone()
    }

    /// Replace the log output of one instance (used by the filter tests).
    pub fn set_logs(&self, id: &str, text: impl Into<String>) {
        lock(&self.logs).insert(id.to_string(), text.into());
    }

    /// Report a canned `docker inspect` payload for one instance.
    pub fn force_inspect(&self, id: &str, info: InspectInfo) {
        lock(&self.inspect).insert(id.to_string(), info);
    }

    /// Drop the canned inspect payload of one instance.
    pub fn clear_inspect(&self, id: &str) {
        lock(&self.inspect).remove(id);
    }

    /// Report a canned usage sample for one instance (used by the usage tests).
    pub fn force_usage(&self, id: &str, sample: UsageSample) {
        lock(&self.usage).insert(id.to_string(), sample);
    }

    /// Drop the canned usage sample of one instance.
    pub fn clear_usage(&self, id: &str) {
        lock(&self.usage).remove(id);
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
            restart_count: 0,
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
            restart_count: 0,
        });
        entry.health = health;
    }

    /// Force the `docker inspect` restart count of one instance.
    pub fn force_restart_count(&self, id: &str, count: u32) {
        let mut state = self.ensure_instance(id);
        if let Some(entry) = state.get_mut(id) {
            entry.restart_count = count;
        }
    }

    /// Current simulated status of one instance.
    pub fn status_of(&self, id: &str) -> Option<Status> {
        lock(&self.state).get(id).map(|entry| entry.status)
    }

    fn record_for(&self, operation: &'static str, id: &str, host: &str) {
        lock(&self.calls).push(MockCall {
            operation,
            instance: id.to_string(),
            host: host.to_string(),
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
            restart_count: 0,
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

    /// Last `tail` lines of `text`, with a trailing newline like Docker.
    fn tail_lines(text: &str, tail: u32) -> String {
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(tail as usize);
        let mut selected = lines[start..].join("\n");
        if !selected.is_empty() {
            selected.push('\n');
        }
        selected
    }

    fn simulate_inspect(&self, id: &str) -> InspectInfo {
        if let Some(info) = lock(&self.inspect).get(id) {
            return info.clone();
        }
        let state = lock(&self.state);
        match state.get(id) {
            Some(entry) => InspectInfo {
                state: entry.status.as_str().to_string(),
                exit_code: Some(0),
                oom_killed: false,
                restart_count: entry.restart_count,
                error: None,
                started_at: entry.started_at.map(|started| {
                    chrono::DateTime::<chrono::Utc>::from(started)
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                }),
            },
            None => InspectInfo {
                state: "missing".to_string(),
                ..InspectInfo::default()
            },
        }
    }
}

impl Driver for MockDriver {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn ping(&self) -> DriverFuture<'_, bool> {
        Box::pin(async move { true })
    }

    fn ensure_network<'a>(
        &'a self,
        _host: &'a str,
        _name: &'a str,
    ) -> DriverFuture<'a, Result<bool, DriverError>> {
        Box::pin(async move { Ok(false) })
    }

    fn network_subnet<'a>(
        &'a self,
        _host: &'a str,
        _name: &'a str,
    ) -> DriverFuture<'a, Result<Option<String>, DriverError>> {
        Box::pin(async move { Ok(lock(&self.network_subnet).clone()) })
    }

    fn probe_host<'a>(
        &'a self,
        host: &'a crate::hosts::HostSpec,
    ) -> DriverFuture<'a, Result<HostCapacity, DriverError>> {
        Box::pin(async move {
            lock(&self.host_probes).push(host.id.clone());
            if let Some(detail) = lock(&self.host_failures).get(&host.id).cloned() {
                return Err(DriverError::new("info", detail));
            }
            Ok(lock(&self.host_capacity)
                .get(&host.id)
                .cloned()
                .unwrap_or_else(|| HostCapacity {
                    ncpu: 8,
                    mem_total_bytes: 16 * 1024 * 1024 * 1024,
                    docker_version: Some("mock".to_string()),
                }))
        })
    }

    fn up<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.record_for("up", &spec.id, &spec.host);
            self.fail_if_requested("up")?;
            let delay = *lock(&self.up_delay);
            if let Some(delay) = delay {
                let running = self.in_flight_ups.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_in_flight_ups.fetch_max(running, Ordering::SeqCst);
                tokio::time::sleep(delay).await;
                self.in_flight_ups.fetch_sub(1, Ordering::SeqCst);
            }
            let mut state = self.ensure_instance(&spec.id);
            if let Some(entry) = state.get_mut(&spec.id) {
                entry.status = Status::Running;
                entry.started_at = Some(SystemTime::now());
                entry.ups += 1;
                if !spec.image.trim().is_empty() {
                    entry.image = spec.image.clone();
                }
                // A tag whose container never becomes healthy can be declared
                // per image, which is what the rolling-upgrade tests use.
                entry.health = lock(&self.image_health)
                    .get(&entry.image)
                    .copied()
                    .unwrap_or(Health::Healthy);
            }
            Ok(())
        })
    }

    fn stop<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.record_for("stop", &spec.id, &spec.host);
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
            self.record_for("start", &spec.id, &spec.host);
            self.fail_if_requested("start")?;
            self.set_running(&spec.id);
            Ok(())
        })
    }

    fn restart<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.record_for("restart", &spec.id, &spec.host);
            self.fail_if_requested("restart")?;
            self.set_running(&spec.id);
            Ok(())
        })
    }

    fn down<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.record_for("down", &spec.id, &spec.host);
            self.fail_if_requested("down")?;
            lock(&self.state).remove(&spec.id);
            Ok(())
        })
    }

    fn ps<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<PsResult, DriverError>> {
        Box::pin(async move {
            self.record_for("ps", &spec.id, &spec.host);
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
        options: &'a LogsOptions,
    ) -> DriverFuture<'a, Result<String, DriverError>> {
        Box::pin(async move {
            self.record_for("logs", &spec.id, &spec.host);
            lock(&self.logs_options).push((spec.id.clone(), options.clone()));
            self.fail_if_requested("logs")?;
            let text = match lock(&self.logs).get(&spec.id) {
                Some(text) => text.clone(),
                None => self.logs_for(&spec.id, options.tail),
            };
            // The mock has no timestamps: `since` only bounds what Docker
            // would return, so it is recorded but not simulated.
            Ok(Self::tail_lines(&text, options.tail))
        })
    }

    fn inspect<'a>(
        &'a self,
        spec: &'a InstanceSpec,
    ) -> DriverFuture<'a, Result<InspectInfo, DriverError>> {
        Box::pin(async move {
            self.record_for("inspect", &spec.id, &spec.host);
            self.fail_if_requested("inspect")?;
            Ok(self.simulate_inspect(&spec.id))
        })
    }

    fn log_path<'a>(
        &'a self,
        spec: &'a InstanceSpec,
    ) -> DriverFuture<'a, Result<Option<std::path::PathBuf>, DriverError>> {
        Box::pin(async move {
            self.record_for("log_path", &spec.id, &spec.host);
            self.fail_if_requested("log_path")?;
            Ok(None)
        })
    }

    fn image_size<'a>(
        &'a self,
        image: &'a str,
    ) -> DriverFuture<'a, Result<Option<u64>, DriverError>> {
        Box::pin(async move {
            self.fail_if_requested("image_size")?;
            Ok(if image.trim().is_empty() {
                None
            } else {
                Some(128 * 1024 * 1024)
            })
        })
    }

    fn usage<'a>(
        &'a self,
        specs: &'a [InstanceSpec],
    ) -> DriverFuture<'a, Result<Vec<UsageSample>, DriverError>> {
        Box::pin(async move {
            for spec in specs {
                self.record_for("usage", &spec.id, &spec.host);
                if let Some(detail) = lock(&self.usage_failures).get(&spec.host).cloned() {
                    return Err(DriverError::new("usage", detail));
                }
            }
            self.fail_if_requested("usage")?;
            let forced = lock(&self.usage);
            let state = lock(&self.state);
            Ok(specs
                .iter()
                .map(|spec| match forced.get(&spec.id) {
                    Some(sample) => sample.clone(),
                    None => {
                        let running = state
                            .get(&spec.id)
                            .map(|entry| entry.status == Status::Running)
                            .unwrap_or(false);
                        simulate_usage(spec, running)
                    }
                })
                .collect())
        })
    }

    fn ensure_image<'a>(
        &'a self,
        host: &'a str,
        image: &'a str,
    ) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            lock(&self.image_calls).push(image.to_string());
            lock(&self.image_hosts).push((host.to_string(), image.to_string()));
            if let Some(detail) = lock(&self.pull_failures).get(image).cloned() {
                return Err(DriverError::new("image pull", detail));
            }
            self.register_image(image);
            Ok(())
        })
    }

    fn list_images<'a>(
        &'a self,
        _host: &'a str,
    ) -> DriverFuture<'a, Result<Vec<ImageRecord>, DriverError>> {
        Box::pin(async move {
            self.fail_if_requested("images")?;
            let known = lock(&self.known_images);
            Ok(known
                .iter()
                .map(|tag| ImageRecord {
                    tag: tag.clone(),
                    id: format!("sha256:{:016x}", stable_hash(tag)),
                    size: "1.2GB".to_string(),
                    created: "2026-11-08T00:00:00Z".to_string(),
                })
                .collect())
        })
    }

    fn exec<'a>(
        &'a self,
        spec: &'a InstanceSpec,
        argv: &'a [String],
        timeout_s: u64,
    ) -> DriverFuture<'a, Result<ExecOutput, DriverError>> {
        Box::pin(async move {
            self.record_for("exec", &spec.id, &spec.host);
            lock(&self.exec_calls).push(MockExecCall {
                instance: spec.id.clone(),
                argv: argv.to_vec(),
                timeout_s,
            });
            self.fail_if_requested("exec")?;
            let delay = *lock(&self.exec_delay);
            if let Some(delay) = delay {
                let running = self.in_flight_execs.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_in_flight_execs
                    .fetch_max(running, Ordering::SeqCst);
                tokio::time::sleep(delay).await;
                self.in_flight_execs.fetch_sub(1, Ordering::SeqCst);
            }
            if let Some(result) = lock(&self.exec_sticky).get(&spec.id) {
                return Ok(result.clone());
            }
            let queued = lock(&self.exec_results)
                .get_mut(&spec.id)
                .and_then(|results| results.pop_front());
            if let Some(output) = queued {
                return Ok(output);
            }
            // A deterministic answer that keeps the prompt visible: pipeline
            // tests that do not queue a result can still assert substitution.
            Ok(ExecOutput {
                exit_code: Some(0),
                stdout: format!("[mock] exec in {}: {}", spec.id, argv.join(" ")),
                stderr: String::new(),
                timed_out: false,
                duration_ms: 0,
                truncated: false,
            })
        })
    }
}

/// Stable simulated usage, derived from the instance id only.
fn simulate_usage(spec: &InstanceSpec, running: bool) -> UsageSample {
    let configured = u64::from(spec.mem_limit_mb) * 1024 * 1024;
    if !running {
        return UsageSample {
            running: false,
            mem_limit_bytes: configured,
            ..UsageSample::default()
        };
    }
    let seed = stable_hash(&spec.id);
    UsageSample {
        running: true,
        cpu_pct: 0.25 + (seed % 400) as f64 / 100.0,
        mem_used_bytes: configured * (30 + seed % 40) / 100,
        mem_limit_bytes: configured,
        disk_bytes: 8 * 1024 * 1024 + seed % (48 * 1024 * 1024),
    }
}

/// FNV-1a: a stable hash, unlike `DefaultHasher` whose output may change.
fn stable_hash(value: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str) -> InstanceSpec {
        InstanceSpec {
            id: id.to_string(),
            name: id.to_string(),
            template: "guest".to_string(),
            model: "m".to_string(),
            base_url: "https://example.test/v1".to_string(),
            api_key_env: crate::models::DEFAULT_API_KEY_ENV.to_string(),
            provider: None,
            channel: crate::models::ChannelKind::None,
            shares: Vec::new(),
            mem_limit_mb: crate::models::DEFAULT_MEM_LIMIT_MB,
            cpu_limit: crate::models::DEFAULT_CPU_LIMIT,
            image: "hermes-agent:0.22.2-local".to_string(),
            previous_image: None,
            allow_lan: false,
            heal: crate::models::HealPolicy::Off,
            unhealthy_after: crate::models::DEFAULT_UNHEALTHY_AFTER,
            max_restarts: crate::models::DEFAULT_MAX_RESTARTS,
            heal_backoff_s: crate::models::DEFAULT_HEAL_BACKOFF_S,
            disk_quota_mb: 0,
            disk_policy: crate::models::DiskPolicy::Warn,
            notes: String::new(),
            host: crate::hosts::LOCAL_HOST.to_string(),
            status: Status::Created,
            last_error: None,
            created_at: crate::models::now_iso(),
            updated_at: crate::models::now_iso(),
        }
    }

    #[tokio::test]
    async fn logs_are_tailed_and_the_options_are_recorded() {
        let driver = MockDriver::new();
        let instance = spec("alice");
        driver.set_logs("alice", "one\ntwo\nthree\n");

        let options = LogsOptions {
            tail: 2,
            since: Some("10m".to_string()),
        };
        let text = driver.logs(&instance, &options).await.expect("logs");
        assert_eq!(text, "two\nthree\n");
        assert_eq!(driver.logs_calls(), vec![("alice".to_string(), options)]);
    }

    #[tokio::test]
    async fn inspect_simulates_the_container_state() {
        let driver = MockDriver::new();
        let instance = spec("alice");
        driver.up(&instance).await.expect("up");

        let clean = driver.inspect(&instance).await.expect("inspect");
        assert_eq!(clean.state, "running");
        assert_eq!(clean.exit_code, Some(0));
        assert!(clean.error_message().is_none());

        driver.force_inspect(
            "alice",
            InspectInfo {
                state: "exited".to_string(),
                exit_code: Some(137),
                oom_killed: true,
                restart_count: 0,
                error: None,
                started_at: None,
            },
        );
        let broken = driver.inspect(&instance).await.expect("inspect");
        assert!(broken.oom_killed);
        assert!(broken.error_message().is_some());
    }

    #[tokio::test]
    async fn usage_is_stable_and_zero_when_stopped() {
        let driver = MockDriver::new();
        let mut instance = spec("alice");
        driver.up(&instance).await.expect("up");

        let first = driver
            .usage(std::slice::from_ref(&instance))
            .await
            .expect("usage");
        let second = driver
            .usage(std::slice::from_ref(&instance))
            .await
            .expect("usage");
        assert_eq!(first, second, "simulated numbers are stable");
        let sample = &first[0];
        assert!(sample.running);
        assert!(sample.cpu_pct > 0.0);
        assert!(sample.mem_used_bytes > 0);
        assert_eq!(
            sample.mem_limit_bytes,
            u64::from(instance.mem_limit_mb) * 1024 * 1024
        );
        assert!(sample.disk_bytes > 0);

        instance.status = Status::Stopped;
        driver.stop(&instance).await.expect("stop");
        let stopped = driver
            .usage(std::slice::from_ref(&instance))
            .await
            .expect("usage");
        assert!(!stopped[0].running);
        assert_eq!(stopped[0].cpu_pct, 0.0);
        assert_eq!(stopped[0].mem_used_bytes, 0);
        assert_eq!(stopped[0].disk_bytes, 0);
    }

    #[tokio::test]
    async fn usage_can_be_forced_and_failures_are_injectable() {
        let driver = MockDriver::new();
        let instance = spec("alice");
        driver.force_usage(
            "alice",
            UsageSample {
                running: true,
                cpu_pct: 2.5,
                mem_used_bytes: 1024,
                mem_limit_bytes: 2048,
                disk_bytes: 4096,
            },
        );
        let samples = driver
            .usage(std::slice::from_ref(&instance))
            .await
            .expect("usage");
        assert_eq!(samples[0].cpu_pct, 2.5);
        assert_eq!(samples[0].disk_bytes, 4096);

        driver.fail_next("usage", "Cannot connect to the Docker daemon");
        let err = driver
            .usage(std::slice::from_ref(&instance))
            .await
            .expect_err("injected failure");
        assert!(err.detail.contains("Docker daemon"), "{err}");
    }

    #[tokio::test]
    async fn usage_of_an_empty_fleet_touches_nothing() {
        let driver = MockDriver::new();
        let samples = driver.usage(&[]).await.expect("usage");
        assert!(samples.is_empty());
        assert!(driver.calls().is_empty());
    }

    #[tokio::test]
    async fn up_tracks_the_image_and_its_forced_health() {
        let driver = MockDriver::new();
        let mut instance = spec("alice");
        driver.set_image_health("broken:1", Health::Unhealthy);

        instance.image = "good:1".to_string();
        driver.up(&instance).await.expect("up");
        assert_eq!(driver.image_of("alice").as_deref(), Some("good:1"));
        let healthy = driver.ps(&instance).await.expect("ps");
        assert_eq!(healthy.health, Health::Healthy);

        instance.image = "broken:1".to_string();
        driver.up(&instance).await.expect("up");
        assert_eq!(driver.image_of("alice").as_deref(), Some("broken:1"));
        let broken = driver.ps(&instance).await.expect("ps");
        assert_eq!(broken.health, Health::Unhealthy);
    }

    #[tokio::test]
    async fn ensure_image_records_calls_and_can_fail() {
        let driver = MockDriver::new();
        driver
            .ensure_image(crate::hosts::LOCAL_HOST, "agent:1")
            .await
            .expect("ensure");
        assert_eq!(driver.image_calls(), vec!["agent:1".to_string()]);

        driver.fail_image_pull("agent:9", "manifest unknown");
        let err = driver
            .ensure_image(crate::hosts::LOCAL_HOST, "agent:9")
            .await
            .expect_err("pull failure");
        assert!(err.detail.contains("manifest unknown"), "{err}");

        let images = driver
            .list_images(crate::hosts::LOCAL_HOST)
            .await
            .expect("images");
        let tags: Vec<&str> = images.iter().map(|image| image.tag.as_str()).collect();
        assert!(tags.contains(&"agent:1"), "{tags:?}");
        assert!(!tags.contains(&"agent:9"), "a failed pull is not local");
    }

    #[tokio::test]
    async fn exec_records_calls_and_consumes_queued_results() {
        let driver = MockDriver::new();
        let instance = spec("alice");
        driver.push_exec_result(
            "alice",
            ExecOutput {
                exit_code: Some(0),
                stdout: "queued".to_string(),
                stderr: String::new(),
                timed_out: false,
                duration_ms: 0,
                truncated: false,
            },
        );
        let argv = vec![
            "gateway".to_string(),
            "ask".to_string(),
            "hello".to_string(),
        ];
        let output = driver.exec(&instance, &argv, 30).await.expect("exec");
        assert_eq!(output.stdout, "queued");
        let second = driver.exec(&instance, &argv, 30).await.expect("exec");
        assert!(second.stdout.contains("hello"), "{}", second.stdout);
        assert!(second.succeeded());

        let calls = driver.exec_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].instance, "alice");
        assert_eq!(calls[0].argv, argv);
        assert_eq!(calls[0].timeout_s, 30);

        driver.fail_next("exec", "container is not running");
        let err = driver
            .exec(&instance, &argv, 30)
            .await
            .expect_err("injected exec failure");
        assert!(err.detail.contains("not running"), "{err}");
    }

    #[tokio::test]
    async fn exec_delay_observes_parallelism() {
        let driver = MockDriver::new();
        let first = spec("alice");
        let second = spec("bob");
        let argv = vec!["gateway".to_string(), "ask".to_string(), "x".to_string()];
        driver.set_exec_delay(Duration::from_millis(50));
        let (left, right) = tokio::join!(
            driver.exec(&first, &argv, 30),
            driver.exec(&second, &argv, 30)
        );
        left.expect("first exec");
        right.expect("second exec");
        assert_eq!(driver.max_concurrent_execs(), 2);
    }
}
