//! DAG pipelines (issue #31).
//!
//! A pipeline is a small, declarative graph of agent prompts: each step names
//! an instance, a prompt and the steps it depends on. The engine runs every
//! ready step (all dependencies finished `ok`) in parallel, up to
//! [`MAX_PARALLEL_STEPS`] at a time, and substitutes the output of a dependency
//! into the prompt of its dependents (`{{steps.<id>.output}}`).
//!
//! Every run is a new UUID with its own `pipeline_steps` rows: relaunching a
//! pipeline never overwrites the history of a previous run. A run that loses a
//! step stops the rest when the failed step declares `on_failure: stop` (the
//! default); `on_failure: continue` keeps the independent branches running and
//! still marks the run `failed`.
//!
//! Scheduled (`trigger: cron`) pipelines are not launched here: issue #30 owns
//! the scheduler loop. [`run_due`] is the helper it calls when it exists.

use crate::db::{Db, DbError};
use crate::driver::{Driver, ExecOutput};
use crate::models::{now_iso, InstanceSpec};
use chrono::{DateTime, Datelike, Timelike, Utc};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::task::JoinSet;

/// Maximum number of steps in one pipeline.
pub const MAX_STEPS: usize = 50;
/// Maximum number of steps the engine launches at the same time.
pub const MAX_PARALLEL_STEPS: usize = 4;
/// Maximum size of one step prompt.
pub const MAX_PROMPT_BYTES: usize = 16 * 1024;
/// Maximum size of the output persisted for one step (64 KiB).
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// Default timeout of one step, in seconds.
pub const DEFAULT_TIMEOUT_S: u64 = 300;
/// Longest accepted step timeout (one hour).
pub const MAX_TIMEOUT_S: u64 = 3600;
/// Default number of retries of one step.
pub const DEFAULT_RETRIES: u32 = 0;
/// Longest accepted retry count.
pub const MAX_RETRIES: u32 = 5;
/// Minimum length of a pipeline name.
pub const NAME_MIN: usize = 2;
/// Maximum length of a pipeline name.
pub const NAME_MAX: usize = 63;
/// Default number of runs returned by a history query.
pub const DEFAULT_RUN_LIMIT: usize = 50;
/// Largest accepted history page.
pub const MAX_RUN_LIMIT: usize = 500;
/// Environment variable holding the command prefix run inside the container.
pub const RUN_COMMAND_ENV: &str = "FLEETT_RUN_COMMAND";
/// Command prefix used when [`RUN_COMMAND_ENV`] is unset: the prompt becomes
/// the last argument (`gateway ask "<prompt>"`).
pub const DEFAULT_RUN_COMMAND: &str = "gateway ask";

/// What launches a pipeline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    #[default]
    Manual,
    Cron,
    Webhook,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Cron => "cron",
            Self::Webhook => "webhook",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "manual" => Some(Self::Manual),
            "cron" => Some(Self::Cron),
            "webhook" => Some(Self::Webhook),
            _ => None,
        }
    }
}

impl std::fmt::Display for Trigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a failed step does to the rest of the run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnFailure {
    /// Stop launching steps (the default): pending steps become `skipped`.
    #[default]
    Stop,
    /// Keep the independent branches running.
    Continue,
}

impl OnFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Continue => "continue",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stop" => Some(Self::Stop),
            "continue" => Some(Self::Continue),
            _ => None,
        }
    }
}

/// State of one run (`pending` until the engine picks it up).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Pending,
    Running,
    Ok,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "ok" => Some(Self::Ok),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// No further transition happens once a run is in a terminal state.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Ok | Self::Failed | Self::Cancelled)
    }
}

/// State of one step of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Pending,
    Running,
    Ok,
    Failed,
    Skipped,
}

impl StepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "ok" => Some(Self::Ok),
            "failed" => Some(Self::Failed),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_S
}

/// One step of a pipeline definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineStep {
    /// Unique id inside the pipeline, used by `depends_on` and templating.
    pub id: String,
    /// Instance that runs the prompt.
    pub instance: String,
    /// Prompt sent to the agent; supports `{{steps.<id>.output}}`, `{{context}}`,
    /// `{{run.id}}`, `{{pipeline.name}}` and `{{step.id}}`.
    pub prompt: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub retries: u32,
    #[serde(default = "default_timeout", alias = "timeout")]
    pub timeout_s: u64,
    #[serde(default)]
    pub on_failure: OnFailure,
}

/// The document a pipeline is created from (JSON over the API, YAML through
/// `fleett pipeline apply`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineDefinition {
    pub name: String,
    #[serde(default)]
    pub trigger: Trigger,
    /// Standard five-field cron expression, required by `trigger: cron`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    pub steps: Vec<PipelineStep>,
}

/// One pipeline row, with its parsed definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineRecord {
    pub id: String,
    pub definition: PipelineDefinition,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl PipelineRecord {
    pub fn name(&self) -> &str {
        &self.definition.name
    }

    pub fn trigger(&self) -> Trigger {
        self.definition.trigger
    }

    pub fn view(&self) -> PipelineView {
        PipelineView {
            id: self.id.clone(),
            name: self.definition.name.clone(),
            trigger: self.definition.trigger,
            cron: self.definition.cron.clone(),
            enabled: self.enabled,
            steps: self.definition.steps.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

/// API representation of one pipeline (definition flattened for the client).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PipelineView {
    pub id: String,
    pub name: String,
    pub trigger: Trigger,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    pub enabled: bool,
    pub steps: Vec<PipelineStep>,
    pub created_at: String,
    pub updated_at: String,
}

/// One run row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PipelineRun {
    pub id: String,
    pub pipeline_id: String,
    pub status: RunStatus,
    pub trigger: Trigger,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One step row of a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PipelineStepRun {
    pub id: i64,
    pub run_id: String,
    pub step_id: String,
    pub instance_id: String,
    pub status: StepStatus,
    pub attempt: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

/// A run with its step rows (the detail endpoint and the `run --wait` output).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PipelineRunDetail {
    pub id: String,
    pub pipeline_id: String,
    pub status: RunStatus,
    pub trigger: Trigger,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub steps: Vec<PipelineStepRun>,
}

impl PipelineRunDetail {
    pub fn from_run(run: PipelineRun, steps: Vec<PipelineStepRun>) -> Self {
        Self {
            id: run.id,
            pipeline_id: run.pipeline_id,
            status: run.status,
            trigger: run.trigger,
            context: run.context,
            started_at: run.started_at,
            finished_at: run.finished_at,
            error: run.error,
            steps,
        }
    }
}

/// Every way a pipeline workflow can be refused.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("invalid pipeline name \"{0}\": use 2-63 lowercase letters, digits, '-' or '_'")]
    InvalidName(String),
    #[error("a pipeline needs at least one step")]
    NoSteps,
    #[error("a pipeline has at most {MAX_STEPS} steps (got {0})")]
    TooManySteps(usize),
    #[error("duplicate step id \"{0}\"")]
    DuplicateStep(String),
    #[error("invalid step id \"{0}\": use 1-64 letters, digits, '-' or '_'")]
    InvalidStepId(String),
    #[error("step \"{step}\" depends on unknown step \"{dependency}\"")]
    UnknownDependency { step: String, dependency: String },
    #[error("dependency cycle: {0}")]
    Cycle(String),
    #[error("step \"{step}\" has no prompt")]
    EmptyPrompt { step: String },
    #[error("step \"{step}\" prompt exceeds {MAX_PROMPT_BYTES} bytes")]
    PromptTooLarge { step: String },
    #[error("step \"{step}\" has no instance")]
    EmptyInstance { step: String },
    #[error("step \"{step}\" retries must be between 0 and {MAX_RETRIES}")]
    InvalidRetries { step: String },
    #[error("step \"{step}\" timeout_s must be between 1 and {MAX_TIMEOUT_S}")]
    InvalidTimeout { step: String },
    #[error("invalid trigger \"{0}\": use manual, cron or webhook")]
    InvalidTrigger(String),
    #[error("invalid cron expression \"{expr}\": {detail}")]
    InvalidCron { expr: String, detail: String },
    #[error("unknown instance(s): {0}")]
    UnknownInstance(String),
    #[error("unknown pipeline \"{0}\"")]
    NotFound(String),
    #[error("unknown pipeline run \"{0}\"")]
    RunNotFound(String),
    #[error("pipeline run \"{0}\" already finished")]
    RunFinished(String),
    #[error("pipeline \"{0}\" already exists")]
    NameTaken(String),
    #[error("stored pipeline definition is corrupted: {0}")]
    Corrupt(String),
    #[error("database error: {0}")]
    Db(#[from] DbError),
}

impl PipelineError {
    /// Stable machine-readable code returned by the API.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidName(_) => "invalid_name",
            Self::NoSteps => "no_steps",
            Self::TooManySteps(_) => "too_many_steps",
            Self::DuplicateStep(_) => "duplicate_step_id",
            Self::InvalidStepId(_) => "invalid_step_id",
            Self::UnknownDependency { .. } => "unknown_dependency",
            Self::Cycle(_) => "cycle_detected",
            Self::EmptyPrompt { .. } => "empty_prompt",
            Self::PromptTooLarge { .. } => "prompt_too_large",
            Self::EmptyInstance { .. } => "empty_instance",
            Self::InvalidRetries { .. } => "invalid_retries",
            Self::InvalidTimeout { .. } => "invalid_timeout",
            Self::InvalidTrigger(_) => "invalid_trigger",
            Self::InvalidCron { .. } => "invalid_cron",
            Self::UnknownInstance(_) => "unknown_instance",
            Self::NotFound(_) => "unknown_pipeline",
            Self::RunNotFound(_) => "run_not_found",
            Self::RunFinished(_) => "run_finished",
            Self::NameTaken(_) => "pipeline_exists",
            Self::Corrupt(_) => "corrupt_definition",
            Self::Db(_) => "internal",
        }
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn valid_name(name: &str) -> bool {
    let length = name.chars().count();
    if !(NAME_MIN..=NAME_MAX).contains(&length) {
        return false;
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'))
}

fn valid_step_id(id: &str) -> bool {
    let length = id.chars().count();
    if !(1..=64).contains(&length) {
        return false;
    }
    id.chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

/// Validate a definition before it is stored or executed.
pub fn validate_definition(definition: &PipelineDefinition) -> Result<(), PipelineError> {
    if !valid_name(&definition.name) {
        return Err(PipelineError::InvalidName(definition.name.clone()));
    }
    if definition.steps.is_empty() {
        return Err(PipelineError::NoSteps);
    }
    if definition.steps.len() > MAX_STEPS {
        return Err(PipelineError::TooManySteps(definition.steps.len()));
    }
    if definition.trigger == Trigger::Cron {
        let cron = definition
            .cron
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| PipelineError::InvalidCron {
                expr: String::new(),
                detail: "trigger cron requires a cron expression".to_string(),
            })?;
        validate_cron(cron)?;
    } else if let Some(cron) = definition
        .cron
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        // Keeping an expression on a manual pipeline is allowed (switching
        // `trigger` later must not force a copy-paste), but it still has to be
        // a valid expression.
        validate_cron(cron)?;
    }

    let mut ids = HashSet::new();
    for step in &definition.steps {
        if !valid_step_id(&step.id) {
            return Err(PipelineError::InvalidStepId(step.id.clone()));
        }
        if !ids.insert(step.id.as_str()) {
            return Err(PipelineError::DuplicateStep(step.id.clone()));
        }
        if step.instance.trim().is_empty() {
            return Err(PipelineError::EmptyInstance {
                step: step.id.clone(),
            });
        }
        if step.prompt.trim().is_empty() {
            return Err(PipelineError::EmptyPrompt {
                step: step.id.clone(),
            });
        }
        if step.prompt.len() > MAX_PROMPT_BYTES {
            return Err(PipelineError::PromptTooLarge {
                step: step.id.clone(),
            });
        }
        if step.retries > MAX_RETRIES {
            return Err(PipelineError::InvalidRetries {
                step: step.id.clone(),
            });
        }
        if step.timeout_s == 0 || step.timeout_s > MAX_TIMEOUT_S {
            return Err(PipelineError::InvalidTimeout {
                step: step.id.clone(),
            });
        }
    }

    for step in &definition.steps {
        for dependency in &step.depends_on {
            if !ids.contains(dependency.as_str()) {
                return Err(PipelineError::UnknownDependency {
                    step: step.id.clone(),
                    dependency: dependency.clone(),
                });
            }
        }
    }

    if let Some(cycle) = find_cycle(definition) {
        return Err(PipelineError::Cycle(cycle.join(" -> ")));
    }
    Ok(())
}

/// Depth-first cycle search; returns the ids of one cycle, in order.
fn find_cycle(definition: &PipelineDefinition) -> Option<Vec<String>> {
    fn visit<'a>(
        id: &'a str,
        steps: &HashMap<&'a str, &'a PipelineStep>,
        marks: &mut HashMap<&'a str, u8>,
        stack: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        match marks.get(id) {
            Some(2) => return None,
            Some(_) => {
                let start = stack.iter().position(|entry| *entry == id).unwrap_or(0);
                let mut cycle: Vec<String> = stack[start..]
                    .iter()
                    .map(|entry| entry.to_string())
                    .collect();
                cycle.push(id.to_string());
                return Some(cycle);
            }
            None => {}
        }
        marks.insert(id, 1);
        stack.push(id);
        if let Some(step) = steps.get(id) {
            for dependency in &step.depends_on {
                if let Some(cycle) = visit(dependency.as_str(), steps, marks, stack) {
                    return Some(cycle);
                }
            }
        }
        stack.pop();
        marks.insert(id, 2);
        None
    }

    let steps: HashMap<&str, &PipelineStep> = definition
        .steps
        .iter()
        .map(|step| (step.id.as_str(), step))
        .collect();
    // 1 = on the current path, 2 = fully explored.
    let mut marks: HashMap<&str, u8> = HashMap::new();
    let mut stack: Vec<&str> = Vec::new();
    for step in &definition.steps {
        if let Some(cycle) = visit(step.id.as_str(), &steps, &mut marks, &mut stack) {
            return Some(cycle);
        }
    }
    None
}

/// Group the steps of a validated definition into dependency waves.
///
/// Steps in the same wave are independent and may run in parallel; every wave
/// only depends on the previous ones. Served for tests and tooling: the engine
/// handles failures dynamically, this helper describes the nominal order.
pub fn waves(definition: &PipelineDefinition) -> Result<Vec<Vec<String>>, PipelineError> {
    validate_definition(definition)?;
    let mut depths: HashMap<&str, usize> = HashMap::new();
    let mut waves: Vec<Vec<String>> = Vec::new();
    // Repeated relaxation is enough for the 50-step ceiling and keeps the
    // order stable (definition order inside one wave).
    for _ in 0..definition.steps.len() {
        let mut changed = false;
        for step in &definition.steps {
            if depths.contains_key(step.id.as_str()) {
                continue;
            }
            let depth = step
                .depends_on
                .iter()
                .filter_map(|dependency| depths.get(dependency.as_str()).copied())
                .max()
                .map(|depth| depth + 1)
                .unwrap_or(0);
            if step
                .depends_on
                .iter()
                .all(|dependency| depths.contains_key(dependency.as_str()))
            {
                depths.insert(step.id.as_str(), depth);
                if waves.len() <= depth {
                    waves.resize(depth + 1, Vec::new());
                }
                waves[depth].push(step.id.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Ok(waves)
}

// ---------------------------------------------------------------------------
// Cron (parsing only; the scheduler loop belongs to issue #30)
// ---------------------------------------------------------------------------

/// Validate a standard five-field cron expression.
pub fn validate_cron(expression: &str) -> Result<(), PipelineError> {
    parse_cron(expression).map(|_| ())
}

struct CronField {
    allowed: Vec<bool>,
    /// `true` when the field was a bare `*` (needed for the DOM/DOW rule).
    any: bool,
}

impl CronField {
    fn matches(&self, value: u32) -> bool {
        self.allowed.get(value as usize).copied().unwrap_or(false)
    }
}

fn cron_error(expression: &str, detail: impl Into<String>) -> PipelineError {
    PipelineError::InvalidCron {
        expr: expression.to_string(),
        detail: detail.into(),
    }
}

struct CronSchedule {
    minute: CronField,
    hour: CronField,
    day_of_month: CronField,
    month: CronField,
    day_of_week: CronField,
}

fn parse_cron(expression: &str) -> Result<CronSchedule, PipelineError> {
    let fields: Vec<&str> = expression.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(cron_error(
            expression,
            "expected five fields: minute hour day-of-month month day-of-week",
        ));
    }
    Ok(CronSchedule {
        minute: parse_cron_field(expression, fields[0], "minute", 0, 59)?,
        hour: parse_cron_field(expression, fields[1], "hour", 0, 23)?,
        day_of_month: parse_cron_field(expression, fields[2], "day-of-month", 1, 31)?,
        month: parse_cron_field(expression, fields[3], "month", 1, 12)?,
        day_of_week: parse_cron_field(expression, fields[4], "day-of-week", 0, 7)?,
    })
}

fn parse_cron_field(
    expression: &str,
    field: &str,
    name: &str,
    min: u32,
    max: u32,
) -> Result<CronField, PipelineError> {
    let mut allowed = vec![false; (max + 1) as usize];
    let any = field == "*";
    for part in field.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(cron_error(expression, format!("{name}: empty list item")));
        }
        let (spec, step) = match part.split_once('/') {
            Some((spec, step)) => {
                let step: u32 = step.parse().map_err(|_| {
                    cron_error(expression, format!("{name}: invalid step in \"{part}\""))
                })?;
                if step == 0 {
                    return Err(cron_error(
                        expression,
                        format!("{name}: step must be positive"),
                    ));
                }
                (spec, Some(step))
            }
            None => (part, None),
        };
        let (start, end, step) = if spec == "*" {
            (min, max, step.unwrap_or(1))
        } else if let Some((start, end)) = spec.split_once('-') {
            let start = parse_cron_number(expression, name, start, min, max)?;
            let end = parse_cron_number(expression, name, end, min, max)?;
            if start > end {
                return Err(cron_error(
                    expression,
                    format!("{name}: range {start}-{end} is reversed"),
                ));
            }
            (start, end, step.unwrap_or(1))
        } else {
            let value = parse_cron_number(expression, name, spec, min, max)?;
            // `5/15` means "from 5 to the end, every 15".
            if let Some(step) = step {
                (value, max, step)
            } else {
                (value, value, 1)
            }
        };
        let mut value = start;
        while value <= end {
            allowed[value as usize] = true;
            match value.checked_add(step) {
                Some(next) => value = next,
                None => break,
            }
        }
    }
    // 0 and 7 are both Sunday.
    if name == "day-of-week" && allowed.get(7).copied().unwrap_or(false) {
        allowed[0] = true;
    }
    Ok(CronField { allowed, any })
}

fn parse_cron_number(
    expression: &str,
    name: &str,
    value: &str,
    min: u32,
    max: u32,
) -> Result<u32, PipelineError> {
    let parsed: u32 = value
        .trim()
        .parse()
        .map_err(|_| cron_error(expression, format!("{name}: invalid number \"{value}\"")))?;
    if parsed < min || parsed > max {
        return Err(cron_error(
            expression,
            format!("{name}: {parsed} is outside {min}..={max}"),
        ));
    }
    Ok(parsed)
}

/// Does `expression` fire during the minute of `at`? (UTC.)
pub fn cron_matches(expression: &str, at: &DateTime<Utc>) -> bool {
    let Ok(schedule) = parse_cron(expression) else {
        return false;
    };
    if !schedule.minute.matches(at.minute())
        || !schedule.hour.matches(at.hour())
        || !schedule.month.matches(at.month())
    {
        return false;
    }
    let day_match = schedule.day_of_month.matches(at.day());
    let dow = at.weekday().num_days_from_sunday();
    let dow_match = schedule.day_of_week.matches(dow);
    // Standard cron: when both day fields are restricted, either one matches.
    if schedule.day_of_month.any && schedule.day_of_week.any {
        true
    } else if schedule.day_of_month.any {
        dow_match
    } else if schedule.day_of_week.any {
        day_match
    } else {
        day_match || dow_match
    }
}

/// Enabled `cron` pipelines due at `at`, for the scheduler of issue #30.
///
/// The helper only *reports* what is due: it never executes a run and never
/// writes. A pipeline that already started a run during the current minute is
/// excluded, so a scheduler that polls more than once a minute fires it once.
pub fn run_due(db: &Db, at: DateTime<Utc>) -> Result<Vec<PipelineRecord>, PipelineError> {
    let records = list(db)?;
    let minute_start = at
        .with_second(0)
        .and_then(|value| value.with_nanosecond(0))
        .unwrap_or(at)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut due = Vec::new();
    for record in records {
        if !record.enabled || record.trigger() != Trigger::Cron {
            continue;
        }
        let Some(cron) = record.definition.cron.as_deref() else {
            continue;
        };
        if !cron_matches(cron, &at) {
            continue;
        }
        let started = db.with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM pipeline_runs WHERE pipeline_id = ?1 AND started_at >= ?2",
                params![&record.id, minute_start],
                |row| row.get::<_, i64>(0),
            )
        })?;
        if started == 0 {
            due.push(record);
        }
    }
    Ok(due)
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

const PIPELINE_COLUMNS: &str = "id, name, definition, trigger, enabled, created_at, updated_at";
const RUN_COLUMNS: &str =
    "id, pipeline_id, status, trigger, context, started_at, finished_at, error";
const STEP_COLUMNS: &str = "id, run_id, step_id, instance_id, status, attempt, output, error, \
                            started_at, finished_at";

fn parse_definition(text: &str, id: &str) -> Result<PipelineDefinition, PipelineError> {
    serde_json::from_str(text).map_err(|err| {
        PipelineError::Corrupt(format!(
            "stored definition of pipeline \"{id}\" is not valid JSON: {err}"
        ))
    })
}

fn canonical_json(definition: &PipelineDefinition) -> Result<String, PipelineError> {
    serde_json::to_string(definition)
        .map_err(|err| PipelineError::Corrupt(format!("cannot serialise the definition: {err}")))
}

struct StoredPipeline {
    id: String,
    name: String,
    definition: String,
    trigger: String,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

impl StoredPipeline {
    fn into_record(self) -> Result<PipelineRecord, PipelineError> {
        let parsed = parse_definition(&self.definition, &self.id)?;
        // A row written by a newer build may carry an unknown trigger; keep
        // the stored definition authoritative instead of failing the listing.
        let _ = Trigger::parse(&self.trigger);
        if parsed.name != self.name {
            // The `name` column is a copy used by the UNIQUE constraint; the
            // definition stays the source of truth.
            tracing::warn!(pipeline = %self.id, "pipeline name column is stale");
        }
        Ok(PipelineRecord {
            id: self.id,
            definition: parsed,
            enabled: self.enabled,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

fn pipeline_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredPipeline> {
    Ok(StoredPipeline {
        id: row.get(0)?,
        name: row.get(1)?,
        definition: row.get(2)?,
        trigger: row.get(3)?,
        enabled: row.get::<_, i64>(4)? != 0,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

/// Every pipeline, sorted by name.
pub fn list(db: &Db) -> Result<Vec<PipelineRecord>, PipelineError> {
    let rows = db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {PIPELINE_COLUMNS} FROM pipelines ORDER BY name"
        ))?;
        let rows = statement
            .query_map([], pipeline_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })?;
    rows.into_iter().map(StoredPipeline::into_record).collect()
}

/// One pipeline by id.
pub fn get(db: &Db, id: &str) -> Result<Option<PipelineRecord>, PipelineError> {
    let row = db.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {PIPELINE_COLUMNS} FROM pipelines WHERE id = ?1"),
            params![id],
            pipeline_row,
        )
        .optional()
    })?;
    row.map(StoredPipeline::into_record).transpose()
}

/// One pipeline by its unique name.
pub fn get_by_name(db: &Db, name: &str) -> Result<Option<PipelineRecord>, PipelineError> {
    let row = db.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {PIPELINE_COLUMNS} FROM pipelines WHERE name = ?1"),
            params![name],
            pipeline_row,
        )
        .optional()
    })?;
    row.map(StoredPipeline::into_record).transpose()
}

/// Create one pipeline from a validated definition.
///
/// The name check and the insert share one transaction: two concurrent
/// creates of the same name cannot both succeed.
pub fn create(
    db: &Db,
    definition: PipelineDefinition,
    enabled: bool,
) -> Result<PipelineRecord, PipelineError> {
    validate_definition(&definition)?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso();
    let serialized = canonical_json(&definition)?;
    let trigger = definition.trigger.as_str().to_string();
    let name = definition.name.clone();
    let taken = db.with_transaction(|conn| {
        let taken = conn
            .query_row(
                "SELECT 1 FROM pipelines WHERE name = ?1",
                params![name],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if taken {
            return Ok(true);
        }
        conn.execute(
            "INSERT INTO pipelines (id, name, definition, trigger, enabled, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, name, serialized, trigger, i64::from(enabled), now, now],
        )?;
        Ok(false)
    })?;
    if taken {
        return Err(PipelineError::NameTaken(definition.name));
    }
    Ok(PipelineRecord {
        id,
        definition,
        enabled,
        created_at: now.clone(),
        updated_at: now,
    })
}

/// Replace the definition (and optionally the enabled flag) of one pipeline.
pub fn update(
    db: &Db,
    id: &str,
    definition: PipelineDefinition,
    enabled: Option<bool>,
) -> Result<PipelineRecord, PipelineError> {
    validate_definition(&definition)?;
    let Some(existing) = get(db, id)? else {
        return Err(PipelineError::NotFound(id.to_string()));
    };
    let enabled = enabled.unwrap_or(existing.enabled);
    let now = now_iso();
    let serialized = canonical_json(&definition)?;
    let trigger = definition.trigger.as_str().to_string();
    let name = definition.name.clone();
    let outcome = db.with_transaction(|conn| {
        let taken = conn
            .query_row(
                "SELECT 1 FROM pipelines WHERE name = ?1 AND id != ?2",
                params![name, id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if taken {
            return Ok(1);
        }
        let updated = conn.execute(
            "UPDATE pipelines SET name = ?2, definition = ?3, trigger = ?4, enabled = ?5, \
             updated_at = ?6 WHERE id = ?1",
            params![id, name, serialized, trigger, i64::from(enabled), now],
        )?;
        Ok(if updated == 0 { 2 } else { 0 })
    })?;
    match outcome {
        1 => Err(PipelineError::NameTaken(definition.name)),
        2 => Err(PipelineError::NotFound(id.to_string())),
        _ => Ok(PipelineRecord {
            id: id.to_string(),
            definition,
            enabled,
            created_at: existing.created_at,
            updated_at: now,
        }),
    }
}

/// Delete one pipeline and its whole run history. `false` when the id is
/// unknown.
pub fn delete(db: &Db, id: &str) -> Result<bool, PipelineError> {
    let removed = db.with_transaction(|conn| {
        let known = conn
            .query_row("SELECT 1 FROM pipelines WHERE id = ?1", params![id], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
        if !known {
            return Ok(false);
        }
        conn.execute(
            "DELETE FROM pipeline_steps WHERE run_id IN \
             (SELECT id FROM pipeline_runs WHERE pipeline_id = ?1)",
            params![id],
        )?;
        conn.execute(
            "DELETE FROM pipeline_runs WHERE pipeline_id = ?1",
            params![id],
        )?;
        conn.execute("DELETE FROM pipelines WHERE id = ?1", params![id])?;
        Ok(true)
    })?;
    Ok(removed)
}

/// Every step instance must exist when the run starts (issue #31: instances
/// are resolved at run time, not at creation time).
pub fn validate_instances(db: &Db, definition: &PipelineDefinition) -> Result<(), PipelineError> {
    let mut missing: Vec<String> = Vec::new();
    for step in &definition.steps {
        if !missing.iter().any(|id| id == &step.instance) && !db.exists(&step.instance)? {
            missing.push(step.instance.clone());
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    missing.sort();
    Err(PipelineError::UnknownInstance(missing.join(", ")))
}

fn run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PipelineRun> {
    let status: String = row.get(2)?;
    let trigger: String = row.get(3)?;
    Ok(PipelineRun {
        id: row.get(0)?,
        pipeline_id: row.get(1)?,
        status: RunStatus::parse(&status).unwrap_or(RunStatus::Failed),
        trigger: Trigger::parse(&trigger).unwrap_or(Trigger::Manual),
        context: row.get(4)?,
        started_at: row.get(5)?,
        finished_at: row.get(6)?,
        error: row.get(7)?,
    })
}

fn step_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PipelineStepRun> {
    let status: String = row.get(4)?;
    Ok(PipelineStepRun {
        id: row.get(0)?,
        run_id: row.get(1)?,
        step_id: row.get(2)?,
        instance_id: row.get(3)?,
        status: StepStatus::parse(&status).unwrap_or(StepStatus::Failed),
        attempt: row.get::<_, i64>(5)?.max(0) as u32,
        output: row.get(6)?,
        error: row.get(7)?,
        started_at: row.get(8)?,
        finished_at: row.get(9)?,
    })
}

/// Create a new run (a fresh UUID, never reusing a previous one) and its
/// pending step rows.
pub fn start_run(
    db: &Db,
    pipeline: &PipelineRecord,
    trigger: Trigger,
    context: Option<&str>,
) -> Result<PipelineRunDetail, PipelineError> {
    let id = uuid::Uuid::new_v4().to_string();
    let pipeline_id = pipeline.id.clone();
    let definition = pipeline.definition.clone();
    let started_at = now_iso();
    db.with_transaction(|conn| {
        conn.execute(
            "INSERT INTO pipeline_runs (id, pipeline_id, status, trigger, context, started_at) \
             VALUES (?1, ?2, 'pending', ?3, ?4, ?5)",
            params![id, pipeline_id, trigger.as_str(), context, started_at],
        )?;
        for step in &definition.steps {
            conn.execute(
                "INSERT INTO pipeline_steps (run_id, step_id, instance_id, status, attempt) \
                 VALUES (?1, ?2, ?3, 'pending', 0)",
                params![id, step.id, step.instance],
            )?;
        }
        Ok(())
    })?;
    get_run(db, &pipeline.id, &id)
}

/// Runs of one pipeline, newest first.
pub fn list_runs(
    db: &Db,
    pipeline_id: &str,
    limit: usize,
) -> Result<Vec<PipelineRun>, PipelineError> {
    let limit = limit.clamp(1, MAX_RUN_LIMIT);
    let rows = db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM pipeline_runs WHERE pipeline_id = ?1 \
             ORDER BY rowid DESC LIMIT ?2"
        ))?;
        let rows = statement
            .query_map(params![pipeline_id, limit as i64], run_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })?;
    Ok(rows)
}

fn one_run(db: &Db, run_id: &str) -> Result<Option<PipelineRun>, PipelineError> {
    let row = db.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {RUN_COLUMNS} FROM pipeline_runs WHERE id = ?1"),
            params![run_id],
            run_row,
        )
        .optional()
    })?;
    Ok(row)
}

fn run_steps(db: &Db, run_id: &str) -> Result<Vec<PipelineStepRun>, PipelineError> {
    let rows = db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {STEP_COLUMNS} FROM pipeline_steps WHERE run_id = ?1 ORDER BY id"
        ))?;
        let rows = statement
            .query_map(params![run_id], step_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })?;
    Ok(rows)
}

/// One run with its step rows, scoped to its pipeline.
pub fn get_run(
    db: &Db,
    pipeline_id: &str,
    run_id: &str,
) -> Result<PipelineRunDetail, PipelineError> {
    let run = one_run(db, run_id)?.ok_or_else(|| PipelineError::RunNotFound(run_id.to_string()))?;
    if run.pipeline_id != pipeline_id {
        return Err(PipelineError::RunNotFound(run_id.to_string()));
    }
    let steps = run_steps(db, run_id)?;
    Ok(PipelineRunDetail::from_run(run, steps))
}

/// Ask a run to stop. Pending steps are marked `skipped` immediately; a step
/// that is already running finishes and its output is kept.
pub fn cancel_run(
    db: &Db,
    pipeline_id: &str,
    run_id: &str,
) -> Result<PipelineRunDetail, PipelineError> {
    let run = one_run(db, run_id)?.ok_or_else(|| PipelineError::RunNotFound(run_id.to_string()))?;
    if run.pipeline_id != pipeline_id {
        return Err(PipelineError::RunNotFound(run_id.to_string()));
    }
    if run.status.is_terminal() {
        return Err(PipelineError::RunFinished(run_id.to_string()));
    }
    let now = now_iso();
    db.with_transaction(|conn| {
        conn.execute(
            "UPDATE pipeline_runs SET status = 'cancelled' WHERE id = ?1 \
             AND status IN ('pending', 'running')",
            params![run_id],
        )?;
        conn.execute(
            "UPDATE pipeline_steps SET status = 'skipped', error = 'cancelled by an operator', \
             finished_at = ?2 WHERE run_id = ?1 AND status = 'pending'",
            params![run_id, now],
        )?;
        Ok(())
    })?;
    get_run(db, pipeline_id, run_id)
}

// ---------------------------------------------------------------------------
// Prompt execution
// ---------------------------------------------------------------------------

/// Split the command prefix and append the prompt as the last argument.
///
/// The prefix comes from [`RUN_COMMAND_ENV`] (`gateway ask` by default). It is
/// split on whitespace: there is deliberately no shell, so a prompt can never
/// be interpreted as a command.
pub fn command_argv_with(prefix: &str, prompt: &str) -> Vec<String> {
    let mut argv: Vec<String> = prefix.split_whitespace().map(str::to_string).collect();
    if argv.is_empty() {
        argv = DEFAULT_RUN_COMMAND
            .split_whitespace()
            .map(str::to_string)
            .collect();
    }
    argv.push(prompt.to_string());
    argv
}

/// Same as [`command_argv_with`] with the configured prefix.
pub fn command_argv(prompt: &str) -> Vec<String> {
    let prefix = std::env::var(RUN_COMMAND_ENV).unwrap_or_else(|_| DEFAULT_RUN_COMMAND.to_string());
    command_argv_with(&prefix, prompt)
}

/// Run one prompt inside the instance container through the driver.
pub async fn exec_prompt(
    driver: &dyn Driver,
    spec: &InstanceSpec,
    prompt: &str,
    timeout_s: u64,
) -> Result<ExecOutput, crate::driver::DriverError> {
    let argv = command_argv(prompt);
    driver.exec(spec, &argv, timeout_s).await
}

/// Render the placeholders of one step prompt.
///
/// Known placeholders:
/// - `{{steps.<id>.output}}`: stdout of a dependency;
/// - `{{context}}`: the run context (canonical JSON), empty when absent;
/// - `{{run.id}}`, `{{pipeline.name}}`, `{{step.id}}`.
///
/// An unknown placeholder is left verbatim: a typo must be visible in the
/// prompt, not silently replaced by an empty string.
pub fn render_prompt(
    template: &str,
    outputs: &HashMap<String, String>,
    context: Option<&str>,
    run_id: &str,
    pipeline_name: &str,
    step_id: &str,
) -> String {
    let mut rendered = template.to_string();
    for (id, output) in outputs {
        rendered = rendered.replace(&format!("{{{{steps.{id}.output}}}}"), output);
    }
    rendered = rendered.replace("{{context}}", context.unwrap_or_default());
    rendered = rendered.replace("{{run.id}}", run_id);
    rendered = rendered.replace("{{pipeline.name}}", pipeline_name);
    rendered = rendered.replace("{{step.id}}", step_id);
    rendered
}

/// Truncate a step output to [`MAX_OUTPUT_BYTES`] on a character boundary.
pub fn truncate_output(text: &str) -> String {
    if text.len() <= MAX_OUTPUT_BYTES {
        return text.to_string();
    }
    let mut end = MAX_OUTPUT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = text[..end].to_string();
    truncated.push_str("\n…[truncated]");
    truncated
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

struct StepOutcome {
    step_id: String,
    status: StepStatus,
    output: Option<String>,
    error: Option<String>,
}

struct Engine {
    db: Db,
    driver: Arc<dyn Driver>,
    pipeline: PipelineRecord,
    run_id: String,
    context: Option<String>,
}

/// Run a pipeline that was already recorded by [`start_run`].
///
/// Failures are persisted, never returned: the run row is the report.
pub async fn execute(db: Db, driver: Arc<dyn Driver>, pipeline: PipelineRecord, run_id: String) {
    let context = one_run(&db, &run_id)
        .ok()
        .flatten()
        .and_then(|run| run.context);
    Engine {
        db,
        driver,
        pipeline,
        run_id,
        context,
    }
    .run()
    .await;
}

impl Engine {
    async fn run(self) {
        let definition = self.pipeline.definition.clone();
        let _ = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE pipeline_runs SET status = 'running', started_at = COALESCE(started_at, ?2), \
                 error = NULL WHERE id = ?1 AND status = 'pending'",
                params![self.run_id, now_iso()],
            )
        });

        let mut statuses: HashMap<String, StepStatus> = HashMap::new();
        let mut outputs: HashMap<String, String> = HashMap::new();

        loop {
            if self.is_cancelled() {
                self.skip_pending(&definition, &mut statuses, "cancelled by an operator");
                self.finish(RunStatus::Cancelled, Some("cancelled by an operator"));
                return;
            }

            // A step whose dependency did not succeed can never run: mark it
            // skipped before looking for runnable steps, so its own dependents
            // become skippable in turn (repeat until a fixed point).
            loop {
                let mut changed = false;
                for step in &definition.steps {
                    if statuses.contains_key(&step.id) {
                        continue;
                    }
                    if !step
                        .depends_on
                        .iter()
                        .all(|dependency| statuses.contains_key(dependency))
                    {
                        continue;
                    }
                    if let Some(dependency) = step
                        .depends_on
                        .iter()
                        .find(|dependency| statuses.get(*dependency) != Some(&StepStatus::Ok))
                    {
                        let reason = format!("dependency \"{dependency}\" did not succeed");
                        statuses.insert(step.id.clone(), StepStatus::Skipped);
                        let _ =
                            self.finish_step(&step.id, StepStatus::Skipped, None, Some(&reason));
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }

            let ready: Vec<PipelineStep> = definition
                .steps
                .iter()
                .filter(|step| !statuses.contains_key(&step.id))
                .filter(|step| {
                    step.depends_on
                        .iter()
                        .all(|dependency| statuses.contains_key(dependency))
                })
                .cloned()
                .collect();
            if ready.is_empty() {
                break;
            }

            let mut cancelled = false;
            let mut stop = false;
            for chunk in ready.chunks(MAX_PARALLEL_STEPS) {
                if self.is_cancelled() {
                    cancelled = true;
                    break;
                }
                let mut tasks = JoinSet::new();
                for step in chunk {
                    let prompt = render_prompt(
                        &step.prompt,
                        &outputs,
                        self.context.as_deref(),
                        &self.run_id,
                        &self.pipeline.definition.name,
                        &step.id,
                    );
                    let db = self.db.clone();
                    let driver = Arc::clone(&self.driver);
                    let run_id = self.run_id.clone();
                    let step = step.clone();
                    tasks.spawn(async move {
                        execute_step(&db, driver.as_ref(), &run_id, &step, &prompt).await
                    });
                }
                while let Some(joined) = tasks.join_next().await {
                    let outcome = match joined {
                        Ok(outcome) => outcome,
                        Err(err) => StepOutcome {
                            step_id: String::new(),
                            status: StepStatus::Failed,
                            output: None,
                            error: Some(format!("the pipeline task failed: {err}")),
                        },
                    };
                    if outcome.step_id.is_empty() {
                        // A join error cannot be attributed to one step; fail
                        // the run rather than pretend a step succeeded.
                        let _ = self.db.with_conn(|conn| {
                            conn.execute(
                                "UPDATE pipeline_runs SET error = ?2 WHERE id = ?1",
                                params![self.run_id, outcome.error],
                            )
                        });
                        stop = true;
                        continue;
                    }
                    let on_failure = definition
                        .steps
                        .iter()
                        .find(|step| step.id == outcome.step_id)
                        .map(|step| step.on_failure)
                        .unwrap_or_default();
                    if outcome.status == StepStatus::Failed && on_failure == OnFailure::Stop {
                        stop = true;
                    }
                    if let Some(output) = &outcome.output {
                        outputs.insert(outcome.step_id.clone(), output.clone());
                    }
                    statuses.insert(outcome.step_id.clone(), outcome.status);
                }
                if stop {
                    break;
                }
            }

            if stop {
                self.skip_pending(&definition, &mut statuses, "a previous step failed");
                self.finish(RunStatus::Failed, Some("a previous step failed"));
                return;
            }
            if cancelled {
                self.skip_pending(&definition, &mut statuses, "cancelled by an operator");
                self.finish(RunStatus::Cancelled, Some("cancelled by an operator"));
                return;
            }
        }

        let failed = statuses
            .values()
            .any(|status| *status == StepStatus::Failed);
        if failed {
            self.finish(RunStatus::Failed, Some("one or more steps failed"));
        } else {
            self.finish(RunStatus::Ok, None);
        }
    }

    fn is_cancelled(&self) -> bool {
        one_run(&self.db, &self.run_id)
            .ok()
            .flatten()
            .is_some_and(|run| run.status == RunStatus::Cancelled)
    }

    fn finish_step(
        &self,
        step_id: &str,
        status: StepStatus,
        output: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), PipelineError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE pipeline_steps SET status = ?3, output = ?4, error = ?5, finished_at = ?6 \
                 WHERE run_id = ?1 AND step_id = ?2",
                params![
                    self.run_id,
                    step_id,
                    status.as_str(),
                    output,
                    error,
                    now_iso()
                ],
            )
        })?;
        Ok(())
    }

    fn skip_pending(
        &self,
        definition: &PipelineDefinition,
        statuses: &mut HashMap<String, StepStatus>,
        reason: &str,
    ) {
        for step in &definition.steps {
            if statuses.contains_key(&step.id) {
                continue;
            }
            statuses.insert(step.id.clone(), StepStatus::Skipped);
            let _ = self.finish_step(&step.id, StepStatus::Skipped, None, Some(reason));
        }
    }

    fn finish(&self, status: RunStatus, error: Option<&str>) {
        if let Err(err) = self.db.with_conn(|conn| {
            // A cancellation that lands while the last steps are finishing
            // wins: the operator asked for `cancelled`, not `ok`.
            conn.execute(
                "UPDATE pipeline_runs SET \
                 status = CASE WHEN status = 'cancelled' THEN 'cancelled' ELSE ?2 END, \
                 finished_at = ?3, \
                 error = CASE WHEN status = 'cancelled' THEN error ELSE ?4 END \
                 WHERE id = ?1",
                params![self.run_id, status.as_str(), now_iso(), error],
            )
        }) {
            tracing::error!(run = %self.run_id, %err, "could not persist the pipeline run result");
        }
    }
}

async fn execute_step(
    db: &Db,
    driver: &dyn Driver,
    run_id: &str,
    step: &PipelineStep,
    prompt: &str,
) -> StepOutcome {
    let max_attempts = step.retries.saturating_add(1);
    let mut last_error = String::new();
    for attempt in 1..=max_attempts {
        if run_is_cancelled(db, run_id) {
            let message = "cancelled by an operator";
            let _ = finish_step(
                db,
                run_id,
                &step.id,
                StepStatus::Skipped,
                None,
                Some(message),
            );
            return StepOutcome {
                step_id: step.id.clone(),
                status: StepStatus::Skipped,
                output: None,
                error: Some(message.to_string()),
            };
        }
        let _ = db.with_conn(|conn| {
            conn.execute(
                "UPDATE pipeline_steps SET status = 'running', attempt = ?3, started_at = ?4, \
                 error = NULL, output = NULL WHERE run_id = ?1 AND step_id = ?2",
                params![run_id, step.id, attempt as i64, now_iso()],
            )
        });
        let spec = match db.get(&step.instance) {
            Ok(Some(spec)) => spec,
            Ok(None) => {
                last_error = format!("unknown instance \"{}\"", step.instance);
                break;
            }
            Err(err) => {
                last_error = format!("cannot read instance \"{}\": {err}", step.instance);
                break;
            }
        };
        match exec_prompt(driver, &spec, prompt, step.timeout_s).await {
            Ok(output) if output.succeeded() => {
                let output = truncate_output(&output.stdout);
                let _ = finish_step(db, run_id, &step.id, StepStatus::Ok, Some(&output), None);
                return StepOutcome {
                    step_id: step.id.clone(),
                    status: StepStatus::Ok,
                    output: Some(output),
                    error: None,
                };
            }
            Ok(output) => {
                last_error = output
                    .failure_detail()
                    .unwrap_or_else(|| "the command failed".to_string());
            }
            Err(err) => {
                last_error = format!("{}: {}", err.operation, err.detail);
            }
        }
    }
    let _ = finish_step(
        db,
        run_id,
        &step.id,
        StepStatus::Failed,
        None,
        Some(&last_error),
    );
    StepOutcome {
        step_id: step.id.clone(),
        status: StepStatus::Failed,
        output: None,
        error: Some(last_error),
    }
}

fn run_is_cancelled(db: &Db, run_id: &str) -> bool {
    one_run(db, run_id)
        .ok()
        .flatten()
        .is_some_and(|run| run.status == RunStatus::Cancelled)
}

fn finish_step(
    db: &Db,
    run_id: &str,
    step_id: &str,
    status: StepStatus,
    output: Option<&str>,
    error: Option<&str>,
) -> Result<(), PipelineError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE pipeline_steps SET status = ?3, output = ?4, error = ?5, finished_at = ?6 \
             WHERE run_id = ?1 AND step_id = ?2",
            params![run_id, step_id, status.as_str(), output, error, now_iso()],
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::mock::MockDriver;
    use crate::models::{ChannelKind, Status, DEFAULT_CPU_LIMIT, DEFAULT_MEM_LIMIT_MB};

    fn step(id: &str, depends_on: &[&str]) -> PipelineStep {
        PipelineStep {
            id: id.to_string(),
            instance: "alice".to_string(),
            prompt: format!("prompt {id}"),
            depends_on: depends_on.iter().map(|value| value.to_string()).collect(),
            retries: 0,
            timeout_s: DEFAULT_TIMEOUT_S,
            on_failure: OnFailure::Stop,
        }
    }

    fn definition(steps: Vec<PipelineStep>) -> PipelineDefinition {
        PipelineDefinition {
            name: "release".to_string(),
            trigger: Trigger::Manual,
            cron: None,
            steps,
        }
    }

    fn db() -> Db {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.keep().join("fleet.db");
        Db::open(&path).expect("open")
    }

    fn spec(id: &str) -> InstanceSpec {
        InstanceSpec {
            id: id.to_string(),
            name: id.to_string(),
            template: "guest".to_string(),
            model: "m".to_string(),
            base_url: "https://example.test/v1".to_string(),
            api_key_env: crate::models::DEFAULT_API_KEY_ENV.to_string(),
            provider: None,
            channel: ChannelKind::None,
            shares: Vec::new(),
            mem_limit_mb: DEFAULT_MEM_LIMIT_MB,
            cpu_limit: DEFAULT_CPU_LIMIT,
            image: "agent:1".to_string(),
            previous_image: None,
            allow_lan: false,
            notes: String::new(),
            heal: crate::models::HealPolicy::Off,
            unhealthy_after: crate::models::DEFAULT_UNHEALTHY_AFTER,
            max_restarts: crate::models::DEFAULT_MAX_RESTARTS,
            heal_backoff_s: crate::models::DEFAULT_HEAL_BACKOFF_S,
            status: Status::Running,
            last_error: None,
            created_at: now_iso(),
            updated_at: now_iso(),
        }
    }

    #[test]
    fn a_dag_is_validated_and_waved() {
        let def = definition(vec![
            step("produce", &[]),
            step("verify", &["produce"]),
            step("publish", &["verify"]),
            step("report", &["produce"]),
        ]);
        validate_definition(&def).expect("valid");
        let waves = waves(&def).expect("waves");
        assert_eq!(waves.len(), 3);
        assert_eq!(waves[0], vec!["produce".to_string()]);
        assert_eq!(waves[1], vec!["verify".to_string(), "report".to_string()]);
        assert_eq!(waves[2], vec!["publish".to_string()]);
    }

    #[test]
    fn cycles_and_unknown_dependencies_are_refused() {
        let cyclic = definition(vec![step("a", &["b"]), step("b", &["a"])]);
        let err = validate_definition(&cyclic).expect_err("cycle");
        assert!(matches!(err, PipelineError::Cycle(_)), "{err}");
        assert!(err.to_string().contains("a -> b -> a"), "{err}");

        let dangling = definition(vec![step("a", &["ghost"])]);
        assert!(matches!(
            validate_definition(&dangling),
            Err(PipelineError::UnknownDependency { .. })
        ));

        let duplicate = definition(vec![step("a", &[]), step("a", &[])]);
        assert!(matches!(
            validate_definition(&duplicate),
            Err(PipelineError::DuplicateStep(_))
        ));
    }

    #[test]
    fn prompt_placeholders_are_substituted_and_unknown_ones_kept() {
        let mut outputs = HashMap::new();
        outputs.insert("produce".to_string(), "draft-42".to_string());
        let rendered = render_prompt(
            "review {{steps.produce.output}} for {{pipeline.name}} ({{step.id}}, {{run.id}}, {{context}}) {{steps.ghost.output}}",
            &outputs,
            Some("{\"ticket\":7}"),
            "run-1",
            "release",
            "verify",
        );
        assert!(
            rendered.contains("review draft-42 for release"),
            "{rendered}"
        );
        assert!(
            rendered.contains("(verify, run-1, {\"ticket\":7})"),
            "{rendered}"
        );
        assert!(rendered.contains("{{steps.ghost.output}}"), "{rendered}");
    }

    #[test]
    fn the_command_prefix_never_goes_through_a_shell() {
        let argv = command_argv_with("gateway ask --profile review", "hello world");
        assert_eq!(
            argv,
            vec![
                "gateway".to_string(),
                "ask".to_string(),
                "--profile".to_string(),
                "review".to_string(),
                "hello world".to_string()
            ]
        );
        // An empty prefix falls back to the documented default.
        let argv = command_argv_with("   ", "x");
        assert_eq!(argv[0], "gateway");
        assert_eq!(argv[1], "ask");
        assert_eq!(argv.last().map(String::as_str), Some("x"));
    }

    #[test]
    fn cron_expressions_are_parsed_and_matched() {
        for valid in ["* * * * *", "*/15 * * * *", "0 3 1-7 * 1", "5/10 * * * *"] {
            validate_cron(valid).unwrap_or_else(|err| panic!("{valid}: {err}"));
        }
        for invalid in ["", "* * * *", "60 * * * *", "*/0 * * * *", "a * * * *"] {
            assert!(validate_cron(invalid).is_err(), "{invalid} accepted");
        }

        let at = chrono::DateTime::parse_from_rfc3339("2026-11-09T03:30:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        assert!(cron_matches("30 3 * * *", &at));
        assert!(cron_matches("*/10 * * * *", &at));
        assert!(!cron_matches("31 3 * * *", &at));
        // 2026-11-09 is a Monday (day-of-week 1).
        assert!(cron_matches("30 3 * * 1", &at));
        assert!(!cron_matches("30 3 * * 2", &at));
    }

    #[test]
    fn output_is_truncated_on_a_character_boundary() {
        let text = "é".repeat(MAX_OUTPUT_BYTES);
        let truncated = truncate_output(&text);
        assert!(truncated.len() <= MAX_OUTPUT_BYTES + 32);
        assert!(truncated.ends_with("[truncated]"));
        assert!(truncated.is_char_boundary(truncated.len()));
        assert_eq!(truncate_output("short"), "short");
    }

    #[test]
    fn pipelines_round_trip_and_runs_keep_their_history() {
        let db = db();
        let def = definition(vec![step("one", &[])]);
        let first = create(&db, def.clone(), true).expect("create");
        assert_eq!(first.name(), "release");
        assert!(matches!(
            create(&db, def, true),
            Err(PipelineError::NameTaken(_))
        ));
        assert_eq!(list(&db).expect("list").len(), 1);
        assert_eq!(
            get_by_name(&db, "release").expect("by name").map(|r| r.id),
            Some(first.id.clone())
        );

        let run = start_run(&db, &first, Trigger::Manual, Some("{\"x\":1}")).expect("run");
        assert_eq!(run.status, RunStatus::Pending);
        assert_eq!(run.steps.len(), 1);
        let other = start_run(&db, &first, Trigger::Manual, None).expect("run 2");
        assert_ne!(run.id, other.id);
        assert_eq!(list_runs(&db, &first.id, 10).expect("runs").len(), 2);
        assert_eq!(
            get_run(&db, &first.id, &run.id).expect("history").steps[0].status,
            StepStatus::Pending
        );

        let cancelled = cancel_run(&db, &first.id, &run.id).expect("cancel");
        assert_eq!(cancelled.status, RunStatus::Cancelled);
        assert_eq!(cancelled.steps[0].status, StepStatus::Skipped);
        assert!(matches!(
            cancel_run(&db, &first.id, &run.id),
            Err(PipelineError::RunFinished(_))
        ));

        let mut renamed = first.definition.clone();
        renamed.name = "renamed".to_string();
        let updated = update(&db, &first.id, renamed, Some(false)).expect("update");
        assert!(!updated.enabled);
        assert!(delete(&db, &first.id).expect("delete"));
        assert!(get(&db, &first.id).expect("get").is_none());
        assert!(list_runs(&db, &first.id, 10).expect("runs").is_empty());
    }

    #[test]
    fn run_due_only_reports_matching_cron_pipelines_once_a_minute() {
        let db = db();
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-09T03:30:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        let mut scheduled = definition(vec![step("one", &[])]);
        scheduled.name = "nightly".to_string();
        scheduled.trigger = Trigger::Cron;
        scheduled.cron = Some("30 3 * * *".to_string());
        let record = create(&db, scheduled, true).expect("create");

        assert_eq!(
            run_due(&db, at)
                .expect("due")
                .iter()
                .map(|r| r.id.clone())
                .collect::<Vec<_>>(),
            vec![record.id.clone()]
        );
        // A run already started this minute means the scheduler must not
        // fire the pipeline twice.
        start_run(&db, &record, Trigger::Cron, None).expect("run");
        assert!(run_due(&db, at).expect("due").is_empty());
    }

    #[tokio::test]
    async fn the_engine_runs_a_dag_and_substitutes_outputs() {
        let db = db();
        for id in ["alice", "bob", "carol"] {
            db.insert_instance(&spec(id)).expect("instance");
        }
        let mock = Arc::new(MockDriver::new());
        mock.push_exec_result(
            "alice",
            ExecOutput {
                exit_code: Some(0),
                stdout: "draft".to_string(),
                stderr: String::new(),
                timed_out: false,
                duration_ms: 0,
                truncated: false,
            },
        );
        mock.push_exec_result(
            "bob",
            ExecOutput {
                exit_code: Some(0),
                stdout: "reviewed".to_string(),
                stderr: String::new(),
                timed_out: false,
                duration_ms: 0,
                truncated: false,
            },
        );
        mock.push_exec_result(
            "carol",
            ExecOutput {
                exit_code: Some(0),
                stdout: "published".to_string(),
                stderr: String::new(),
                timed_out: false,
                duration_ms: 0,
                truncated: false,
            },
        );
        let mut produce = step("produce", &[]);
        produce.instance = "alice".to_string();
        let mut verify = step("verify", &["produce"]);
        verify.instance = "bob".to_string();
        verify.prompt = "review {{steps.produce.output}}".to_string();
        let mut publish = step("publish", &["verify"]);
        publish.instance = "carol".to_string();
        publish.prompt = "publish {{steps.verify.output}}".to_string();
        let record = create(&db, definition(vec![produce, verify, publish]), true).expect("create");

        let run = start_run(&db, &record, Trigger::Manual, None).expect("run");
        execute(db.clone(), mock.clone(), record.clone(), run.id.clone()).await;

        let detail = get_run(&db, &record.id, &run.id).expect("detail");
        assert_eq!(detail.status, RunStatus::Ok);
        assert!(detail
            .steps
            .iter()
            .all(|step| step.status == StepStatus::Ok));
        assert_eq!(detail.steps[0].output.as_deref(), Some("draft"));
        let prompts: Vec<String> = mock
            .exec_calls()
            .iter()
            .map(|call| call.argv.last().cloned().unwrap_or_default())
            .collect();
        assert!(
            prompts.iter().any(|prompt| prompt == "review draft"),
            "{prompts:?}"
        );
        assert!(
            prompts.iter().any(|prompt| prompt == "publish reviewed"),
            "{prompts:?}"
        );
    }

    #[tokio::test]
    async fn a_failure_stops_the_run_and_continue_keeps_the_other_branch() {
        let db = db();
        for id in ["alice", "bob"] {
            db.insert_instance(&spec(id)).expect("instance");
        }
        let mock = Arc::new(MockDriver::new());
        let mut broken = step("broken", &[]);
        broken.instance = "alice".to_string();
        broken.on_failure = OnFailure::Continue;
        let mut dependent = step("dependent", &["broken"]);
        dependent.instance = "bob".to_string();
        let mut independent = step("independent", &[]);
        independent.instance = "bob".to_string();
        let record =
            create(&db, definition(vec![broken, dependent, independent]), true).expect("create");
        mock.push_exec_result(
            "alice",
            ExecOutput {
                exit_code: Some(2),
                stdout: String::new(),
                stderr: "boom".to_string(),
                timed_out: false,
                duration_ms: 0,
                truncated: false,
            },
        );

        let run = start_run(&db, &record, Trigger::Manual, None).expect("run");
        execute(db.clone(), mock.clone(), record.clone(), run.id.clone()).await;
        let detail = get_run(&db, &record.id, &run.id).expect("detail");
        assert_eq!(detail.status, RunStatus::Failed);
        let status = |id: &str| {
            detail
                .steps
                .iter()
                .find(|step| step.step_id == id)
                .map(|step| step.status)
        };
        assert_eq!(status("broken"), Some(StepStatus::Failed));
        assert_eq!(status("dependent"), Some(StepStatus::Skipped));
        assert_eq!(status("independent"), Some(StepStatus::Ok));
        assert!(detail.steps[0]
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("boom"));

        // `stop` (the default) skips every step that has not started yet.
        // `setup` succeeds in the same first wave as `broken`; `independent`
        // depends on it and is therefore still pending when the failure is
        // known, so it is skipped.
        let mock = Arc::new(MockDriver::new());
        mock.push_exec_result(
            "alice",
            ExecOutput {
                exit_code: Some(2),
                stdout: String::new(),
                stderr: "boom".to_string(),
                timed_out: false,
                duration_ms: 0,
                truncated: false,
            },
        );
        mock.push_exec_result(
            "bob",
            ExecOutput {
                exit_code: Some(0),
                stdout: "ready".to_string(),
                stderr: String::new(),
                timed_out: false,
                duration_ms: 0,
                truncated: false,
            },
        );
        let mut broken = step("broken", &[]);
        broken.instance = "alice".to_string();
        let mut setup = step("setup", &[]);
        setup.instance = "bob".to_string();
        let mut independent = step("independent", &["setup"]);
        independent.instance = "bob".to_string();
        let record = create(
            &db,
            PipelineDefinition {
                name: "stopping".to_string(),
                ..definition(vec![broken, setup, independent])
            },
            true,
        )
        .expect("create");
        let run = start_run(&db, &record, Trigger::Manual, None).expect("run");
        execute(db.clone(), mock.clone(), record.clone(), run.id.clone()).await;
        let detail = get_run(&db, &record.id, &run.id).expect("detail");
        assert_eq!(detail.status, RunStatus::Failed);
        let status = |id: &str| {
            detail
                .steps
                .iter()
                .find(|step| step.step_id == id)
                .map(|step| step.status)
        };
        assert_eq!(status("broken"), Some(StepStatus::Failed));
        assert_eq!(status("setup"), Some(StepStatus::Ok));
        assert_eq!(
            status("independent"),
            Some(StepStatus::Skipped),
            "a step that had not started is skipped when the run stops"
        );
    }

    #[tokio::test]
    async fn a_step_is_retried_up_to_its_retry_count() {
        let db = db();
        db.insert_instance(&spec("alice")).expect("instance");
        let mock = Arc::new(MockDriver::new());
        for _ in 0..2 {
            mock.push_exec_result(
                "alice",
                ExecOutput {
                    exit_code: Some(1),
                    stdout: String::new(),
                    stderr: "not yet".to_string(),
                    timed_out: false,
                    duration_ms: 0,
                    truncated: false,
                },
            );
        }
        mock.push_exec_result(
            "alice",
            ExecOutput {
                exit_code: Some(0),
                stdout: "finally".to_string(),
                stderr: String::new(),
                timed_out: false,
                duration_ms: 0,
                truncated: false,
            },
        );
        let mut step = step("flaky", &[]);
        step.instance = "alice".to_string();
        step.retries = 2;
        let record = create(&db, definition(vec![step]), true).expect("create");
        let run = start_run(&db, &record, Trigger::Manual, None).expect("run");
        execute(db.clone(), mock.clone(), record.clone(), run.id.clone()).await;

        let detail = get_run(&db, &record.id, &run.id).expect("detail");
        assert_eq!(detail.status, RunStatus::Ok);
        assert_eq!(detail.steps[0].attempt, 3);
        assert_eq!(detail.steps[0].output.as_deref(), Some("finally"));
        assert_eq!(mock.exec_calls().len(), 3);
    }
}
