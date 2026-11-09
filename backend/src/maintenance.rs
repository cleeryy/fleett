//! Maintenance windows (issue #39).
//!
//! A window is a recurring UTC day/time range during which destructive
//! operations are allowed. Outside every enabled window, `POST /api/upgrade`
//! and `DELETE /api/instances/{id}` answer `403 maintenance_window` with the
//! next opening, before anything is pulled or written.
//!
//! Enforcement is off when no enabled window exists, so a fresh install is
//! never locked out. `FLEETT_MAINTENANCE_ENFORCE=0` disables it entirely.
//!
//! Overnight windows (`start_minute >= end_minute`) belong to the day they
//! start: `sat 22:00 → sun 02:00` is declared with `days: ["sat"]`. A window
//! whose start and end are equal covers the whole day.

use crate::api::error::ApiError;
use crate::api::AppState;
use crate::db::{Db, DbError};
use crate::models::now_iso;
use chrono::{DateTime, Datelike, Duration as ChronoDuration, NaiveDate, TimeZone, Timelike, Utc};
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Interval of the background loop that fires scheduled deployments.
pub const TICK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Day codes accepted in `maintenance_windows.days`, in display order.
pub const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// RFC 3339 UTC timestamp (`2026-11-09T04:00:00Z`).
fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn day_code(day: chrono::Weekday) -> &'static str {
    match day {
        chrono::Weekday::Mon => "mon",
        chrono::Weekday::Tue => "tue",
        chrono::Weekday::Wed => "wed",
        chrono::Weekday::Thu => "thu",
        chrono::Weekday::Fri => "fri",
        chrono::Weekday::Sat => "sat",
        chrono::Weekday::Sun => "sun",
    }
}

fn parse_day(value: &str) -> Option<chrono::Weekday> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "mon" | "monday" => chrono::Weekday::Mon,
        "tue" | "tues" | "tuesday" => chrono::Weekday::Tue,
        "wed" | "wednesday" => chrono::Weekday::Wed,
        "thu" | "thur" | "thurs" | "thursday" => chrono::Weekday::Thu,
        "fri" | "friday" => chrono::Weekday::Fri,
        "sat" | "saturday" => chrono::Weekday::Sat,
        "sun" | "sunday" => chrono::Weekday::Sun,
        _ => return None,
    })
}

/// Parse an `HH:MM` (or bare `H`) UTC time of day into minutes.
pub fn parse_hhmm(value: &str) -> Result<u32, String> {
    let value = value.trim();
    let (hour, minute) = match value.split_once(':') {
        Some((hour, minute)) => (hour, minute),
        None => (value, "0"),
    };
    let hour: u32 = hour
        .parse()
        .map_err(|_| format!("invalid time \"{value}\": expected HH:MM"))?;
    let minute: u32 = minute
        .parse()
        .map_err(|_| format!("invalid time \"{value}\": expected HH:MM"))?;
    if hour > 23 || minute > 59 {
        return Err(format!(
            "invalid time \"{value}\": expected HH:MM (00:00..23:59)"
        ));
    }
    Ok(hour * 60 + minute)
}

/// `600` → `10:00`, for error messages and the CLI.
pub fn format_minute(minute: u32) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

/// Normalise a list of day codes: validated, deduplicated, canonical order.
pub fn parse_days(days: &[String]) -> Result<Vec<&'static str>, String> {
    let mut parsed: Vec<chrono::Weekday> = Vec::new();
    for value in days.iter().flat_map(|value| value.split(',')) {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let day = parse_day(value)
            .ok_or_else(|| format!("invalid day \"{value}\": expected mon..sun"))?;
        if !parsed.contains(&day) {
            parsed.push(day);
        }
    }
    if parsed.is_empty() {
        return Err("days must list at least one day (mon..sun)".to_string());
    }
    Ok(DAYS
        .iter()
        .copied()
        .filter(|code| parsed.iter().any(|day| day_code(*day) == *code))
        .collect())
}

/// One maintenance window, as stored and returned by the API.
#[derive(Debug, Clone, Serialize)]
pub struct MaintenanceWindow {
    pub id: String,
    pub name: String,
    /// Canonical day codes (`mon`..`sun`).
    pub days: Vec<String>,
    /// UTC minute since midnight, inclusive (`0..=1439`).
    pub start_minute: u32,
    /// UTC minute since midnight, exclusive (`0..=1439`).
    pub end_minute: u32,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl MaintenanceWindow {
    /// `true` when `now` falls inside the window (minute granularity, UTC).
    pub fn is_open(&self, now: DateTime<Utc>) -> bool {
        self.is_open_on(now.date_naive(), now.hour() * 60 + now.minute())
    }

    fn is_open_on(&self, date: NaiveDate, minute: u32) -> bool {
        if !self.enabled {
            return false;
        }
        let today = day_code(date.weekday());
        let qualifies = |code: &str| self.days.iter().any(|day| day == code);
        if self.start_minute == self.end_minute {
            // A zero-length range means "all day".
            return qualifies(today);
        }
        if self.start_minute < self.end_minute {
            qualifies(today) && minute >= self.start_minute && minute < self.end_minute
        } else {
            // Overnight: the window opened on `today` or is the tail of the
            // window that opened the day before.
            let yesterday = day_code((date - ChronoDuration::days(1)).weekday());
            (qualifies(today) && minute >= self.start_minute)
                || (qualifies(yesterday) && minute < self.end_minute)
        }
    }

    /// Earliest opening strictly after `now`, scanning the next eight days.
    pub fn next_open(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        if !self.enabled {
            return None;
        }
        for offset in 0..=8 {
            let date = now.date_naive() + ChronoDuration::days(offset);
            if !self.days.iter().any(|day| day == day_code(date.weekday())) {
                continue;
            }
            let naive = date.and_hms_opt(self.start_minute / 60, self.start_minute % 60, 0)?;
            let candidate = Utc.from_utc_datetime(&naive);
            if candidate > now {
                return Some(candidate);
            }
        }
        None
    }
}

/// `GET /api/maintenance-windows/status`.
#[derive(Debug, Clone, Serialize)]
pub struct WindowsStatus {
    pub open: bool,
    pub next_open: Option<String>,
    /// `false` when `FLEETT_MAINTENANCE_ENFORCE` disabled the gate.
    pub enforce: bool,
}

fn window_from_row(row: &Row<'_>) -> rusqlite::Result<MaintenanceWindow> {
    let days: String = row.get(2)?;
    Ok(MaintenanceWindow {
        id: row.get(0)?,
        name: row.get(1)?,
        days: days
            .split(',')
            .map(str::trim)
            .filter(|day| !day.is_empty())
            .map(str::to_string)
            .collect(),
        start_minute: row.get::<_, i64>(3)?.clamp(0, 1439) as u32,
        end_minute: row.get::<_, i64>(4)?.clamp(0, 1439) as u32,
        enabled: row.get::<_, i64>(5)? != 0,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

const COLUMNS: &str = "id, name, days, start_minute, end_minute, enabled, created_at, updated_at";

pub fn list(db: &Db) -> Result<Vec<MaintenanceWindow>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM maintenance_windows ORDER BY name, id"
        ))?;
        let rows = statement
            .query_map([], window_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

pub fn get(db: &Db, id: &str) -> Result<Option<MaintenanceWindow>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM maintenance_windows WHERE id = ?1"),
            params![id],
            window_from_row,
        )
        .optional()
    })
}

/// Find a window by id first, then by name (case-insensitive).
pub fn find(db: &Db, id_or_name: &str) -> Result<Option<MaintenanceWindow>, DbError> {
    if let Some(window) = get(db, id_or_name)? {
        return Ok(Some(window));
    }
    let needle = id_or_name.trim().to_ascii_lowercase();
    Ok(list(db)?
        .into_iter()
        .find(|window| window.name.to_ascii_lowercase() == needle))
}

/// Body of `POST /api/maintenance-windows`.
#[derive(Debug, Clone, Deserialize)]
pub struct NewWindow {
    pub name: Option<String>,
    #[serde(default)]
    pub days: Option<Days>,
    pub start_minute: Option<u32>,
    pub end_minute: Option<u32>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `["mon","tue"]` or `"mon,tue"` are both accepted.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Days {
    List(Vec<String>),
    Csv(String),
}

impl Days {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::List(days) => days,
            Self::Csv(text) => text
                .split(',')
                .map(str::trim)
                .filter(|day| !day.is_empty())
                .map(str::to_string)
                .collect(),
        }
    }
}

/// Body of `PATCH /api/maintenance-windows/{id}`: every field optional.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WindowPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub days: Option<Days>,
    #[serde(default)]
    pub start_minute: Option<u32>,
    #[serde(default)]
    pub end_minute: Option<u32>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

fn validate_name(name: &str) -> Result<String, ApiError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_window",
            "name must not be empty",
        ));
    }
    if trimmed.chars().count() > 64 {
        return Err(ApiError::bad_request(
            "invalid_window",
            "name must be at most 64 characters",
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "invalid_window",
            "name must not contain control characters",
        ));
    }
    Ok(trimmed.to_string())
}

fn validate_minutes(start: u32, end: u32) -> Result<(), ApiError> {
    if start > 1439 || end > 1439 {
        return Err(ApiError::bad_request(
            "invalid_window",
            "start_minute and end_minute must be between 0 and 1439",
        ));
    }
    Ok(())
}

fn invalid_days(message: String) -> ApiError {
    ApiError::bad_request("invalid_window", message)
}

pub fn insert(db: &Db, new: &NewWindow) -> Result<MaintenanceWindow, ApiError> {
    let name = validate_name(new.name.as_deref().unwrap_or("maintenance"))?;
    let days = parse_days(&new.days.clone().map(Days::into_vec).unwrap_or_default())
        .map_err(invalid_days)?;
    let start = new.start_minute.unwrap_or(0);
    let end = new.end_minute.unwrap_or(0);
    validate_minutes(start, end)?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso();
    db.with_transaction(|conn| {
        conn.execute(
            "INSERT INTO maintenance_windows (id, name, days, start_minute, end_minute, enabled, \
             created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                id,
                name,
                days.join(","),
                start as i64,
                end as i64,
                i64::from(new.enabled.unwrap_or(true)),
                now,
            ],
        )?;
        Ok(())
    })
    .map_err(ApiError::from)?;
    get(db, &id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::internal("the maintenance window disappeared after its insert"))
}

pub fn update(db: &Db, id: &str, patch: &WindowPatch) -> Result<MaintenanceWindow, ApiError> {
    let Some(mut window) = get(db, id).map_err(ApiError::from)? else {
        return Err(ApiError::not_found(format!(
            "maintenance window \"{id}\" not found"
        )));
    };
    if let Some(name) = &patch.name {
        window.name = validate_name(name)?;
    }
    if let Some(days) = &patch.days {
        window.days = parse_days(&days.clone().into_vec())
            .map_err(invalid_days)?
            .into_iter()
            .map(str::to_string)
            .collect();
    }
    if let Some(start) = patch.start_minute {
        window.start_minute = start;
    }
    if let Some(end) = patch.end_minute {
        window.end_minute = end;
    }
    if let Some(enabled) = patch.enabled {
        window.enabled = enabled;
    }
    validate_minutes(window.start_minute, window.end_minute)?;
    window.updated_at = now_iso();
    db.with_transaction(|conn| {
        conn.execute(
            "UPDATE maintenance_windows SET name = ?2, days = ?3, start_minute = ?4, \
             end_minute = ?5, enabled = ?6, updated_at = ?7 WHERE id = ?1",
            params![
                window.id,
                window.name,
                window.days.join(","),
                window.start_minute as i64,
                window.end_minute as i64,
                i64::from(window.enabled),
                window.updated_at,
            ],
        )?;
        Ok(())
    })
    .map_err(ApiError::from)?;
    Ok(window)
}

pub fn delete(db: &Db, id: &str) -> Result<bool, DbError> {
    db.with_transaction(|conn| {
        let deleted = conn.execute("DELETE FROM maintenance_windows WHERE id = ?1", params![id])?;
        Ok(deleted > 0)
    })
}

/// Current gate state. `open` is `true` when any enabled window is open (or
/// when nothing is enforced).
pub fn status(db: &Db, enforce: bool, now: DateTime<Utc>) -> Result<WindowsStatus, DbError> {
    let windows = list(db)?;
    let enabled: Vec<&MaintenanceWindow> = windows.iter().filter(|window| window.enabled).collect();
    let open = enable_now(enforce, &enabled, now);
    let next_open = if open || !enforce {
        None
    } else {
        enabled
            .iter()
            .filter_map(|window| window.next_open(now))
            .min()
    };
    Ok(WindowsStatus {
        open,
        next_open: next_open.map(stamp),
        enforce,
    })
}

fn enable_now(enforce: bool, enabled: &[&MaintenanceWindow], now: DateTime<Utc>) -> bool {
    !enforce || enabled.is_empty() || enabled.iter().any(|window| window.is_open(now))
}

/// Refuse a destructive operation when the gate is closed.
///
/// Returns `Ok(())` when enforcement is disabled, when no enabled window
/// exists, or when a window is currently open. `window_id` restricts the run
/// to one named window (the `window` field of an upgrade request).
pub fn enforce_window_at(
    state: &AppState,
    window_id: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    if !state.cfg.maintenance_enforce {
        return Ok(());
    }
    let windows = list(&state.db).map_err(ApiError::from)?;
    let enabled: Vec<&MaintenanceWindow> = match window_id {
        Some(id) => windows
            .iter()
            .filter(|window| window.id == id && window.enabled)
            .collect(),
        None => windows.iter().filter(|window| window.enabled).collect(),
    };
    if window_id.is_none() && enabled.is_empty() {
        return Ok(());
    }
    if enabled.iter().any(|window| window.is_open(now)) {
        return Ok(());
    }
    let next_open = enabled
        .iter()
        .filter_map(|window| window.next_open(now))
        .min();
    let message = match &next_open {
        Some(at) => format!(
            "no maintenance window is open; the next opening is {}",
            stamp(*at)
        ),
        None => "no maintenance window is open".to_string(),
    };
    Err(
        ApiError::forbidden("maintenance_window", message).with_details(json!({
            "next_open": next_open.as_ref().map(|at| stamp(*at)),
        })),
    )
}

/// Gate with the wall clock of the server.
pub fn enforce(state: &AppState) -> Result<(), ApiError> {
    enforce_window_at(state, None, Utc::now())
}

/// Parse the `at` field of an upgrade request.
///
/// Accepted: an RFC 3339 timestamp with any offset (`2026-11-08T04:00:00+01:00`)
/// or a `sun 04:00` day+time expression, both interpreted in UTC. The result
/// is always strictly in the future.
pub fn parse_at(value: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("at must not be empty".to_string());
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(trimmed) {
        let parsed = parsed.with_timezone(&Utc);
        if parsed <= now {
            return Err(format!(
                "at must be in the future ({} is in the past or now)",
                stamp(parsed)
            ));
        }
        return Ok(parsed);
    }

    let mut parts = trimmed.split_whitespace();
    let (Some(day), Some(time), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(
            "invalid at: expected an RFC 3339 timestamp or \"<day> HH:MM\", e.g. \"sun 04:00\""
                .to_string(),
        );
    };
    let weekday =
        parse_day(day).ok_or_else(|| format!("invalid day \"{day}\": expected mon..sun"))?;
    let minute = parse_hhmm(time)?;
    for offset in 0..=8 {
        let date = now.date_naive() + ChronoDuration::days(offset);
        if date.weekday() != weekday {
            continue;
        }
        let naive = date
            .and_hms_opt(minute / 60, minute % 60, 0)
            .ok_or_else(|| format!("invalid time \"{time}\""))?;
        let candidate = Utc.from_utc_datetime(&naive);
        if candidate > now {
            return Ok(candidate);
        }
    }
    Err(format!("cannot resolve the next \"{trimmed}\""))
}

/// Fire every scheduled deployment whose time has come.
///
/// Called every [`TICK_INTERVAL`] by the server. One running upgrade blocks the
/// loop: a due row stays `scheduled` and is retried at the next tick.
pub async fn tick_scheduled(state: &AppState) {
    if state.upgrade.is_running() {
        return;
    }
    let due = match crate::deployments::due(&state.db, &now_iso()) {
        Ok(due) => due,
        Err(err) => {
            tracing::warn!(%err, "cannot read the scheduled deployments");
            return;
        }
    };
    for record in due {
        if state.upgrade.is_running() {
            return;
        }
        let Some(request) = record.request() else {
            let _ = crate::deployments::fail(
                &state.db,
                &record.id,
                "scheduled deployment has no request payload",
            );
            continue;
        };
        let request = match serde_json::from_value::<crate::models::UpgradeRequest>(request) {
            Ok(request) => request,
            Err(err) => {
                let _ = crate::deployments::fail(
                    &state.db,
                    &record.id,
                    &format!("invalid scheduled request: {err}"),
                );
                continue;
            }
        };
        crate::api::upgrade::run_scheduled(state.clone(), request, record.id.clone()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(days: &[&str], start: u32, end: u32) -> MaintenanceWindow {
        MaintenanceWindow {
            id: "w1".to_string(),
            name: "test".to_string(),
            days: days.iter().map(|day| day.to_string()).collect(),
            start_minute: start,
            end_minute: end,
            enabled: true,
            created_at: now_iso(),
            updated_at: now_iso(),
        }
    }

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("timestamp")
            .with_timezone(&Utc)
    }

    #[test]
    fn a_plain_window_opens_and_closes() {
        let window = window(&["mon"], 120, 180);
        let monday = at("2026-11-09T02:00:00Z"); // Monday
        assert!(window.is_open(monday));
        assert!(window.is_open(at("2026-11-09T02:59:00Z")));
        assert!(
            !window.is_open(at("2026-11-09T03:00:00Z")),
            "end is exclusive"
        );
        assert!(!window.is_open(at("2026-11-09T01:59:00Z")));
        assert!(!window.is_open(at("2026-11-10T02:30:00Z")), "another day");
    }

    #[test]
    fn an_overnight_window_belongs_to_its_start_day() {
        // Saturday 22:00 → Sunday 02:00. Monday 2026-11-09 is the reference.
        let window = window(&["sat"], 22 * 60, 2 * 60);
        assert!(window.is_open(at("2026-11-07T23:00:00Z"))); // Saturday
        assert!(window.is_open(at("2026-11-08T01:30:00Z"))); // Sunday, tail
        assert!(!window.is_open(at("2026-11-08T02:00:00Z"))); // Sunday, closed
        assert!(!window.is_open(at("2026-11-08T21:00:00Z"))); // Sunday evening
        assert!(!window.is_open(at("2026-11-07T21:00:00Z"))); // Saturday, early

        // The next opening from Sunday noon is the next Saturday.
        let next = window
            .next_open(at("2026-11-08T12:00:00Z"))
            .expect("next opening");
        assert_eq!(stamp(next), "2026-11-14T22:00:00Z");
    }

    #[test]
    fn a_zero_length_window_covers_the_whole_day() {
        let window = window(&["sun"], 0, 0);
        assert!(window.is_open(at("2026-11-08T00:00:00Z")));
        assert!(window.is_open(at("2026-11-08T23:59:00Z")));
        assert!(!window.is_open(at("2026-11-09T00:00:00Z")));
    }

    #[test]
    fn a_disabled_window_never_opens() {
        let mut window = window(&["mon", "tue", "wed", "thu", "fri", "sat", "sun"], 0, 0);
        window.enabled = false;
        assert!(!window.is_open(at("2026-11-09T12:00:00Z")));
        assert!(window.next_open(at("2026-11-09T12:00:00Z")).is_none());
    }

    #[test]
    fn the_next_opening_is_strictly_in_the_future() {
        let window = window(&["mon"], 600, 660);
        let now = at("2026-11-09T10:00:00Z"); // inside the window
        assert_eq!(
            stamp(window.next_open(now).expect("next")),
            "2026-11-16T10:00:00Z",
            "the current opening is not the next one"
        );
        let now = at("2026-11-09T09:59:59Z");
        assert_eq!(
            stamp(window.next_open(now).expect("next")),
            "2026-11-09T10:00:00Z"
        );
    }

    #[test]
    fn days_are_validated_and_canonicalised() {
        assert_eq!(
            parse_days(&["sun,mon".to_string(), "MON".to_string()]).expect("days"),
            ["mon", "sun"]
        );
        assert!(parse_days(&["funday".to_string()]).is_err());
        assert!(parse_days(&[]).is_err());
        assert!(parse_days(&["mon".to_string()]).is_ok());
    }

    #[test]
    fn times_round_trip() {
        assert_eq!(parse_hhmm("04:00").expect("time"), 240);
        assert_eq!(parse_hhmm("4").expect("time"), 240);
        assert_eq!(parse_hhmm("00:00").expect("time"), 0);
        assert!(parse_hhmm("24:00").is_err());
        assert!(parse_hhmm("04:60").is_err());
        assert!(parse_hhmm("nope").is_err());
        assert_eq!(format_minute(240), "04:00");
        assert_eq!(format_minute(1439), "23:59");
    }

    #[test]
    fn at_accepts_rfc3339_and_day_time() {
        let now = at("2026-11-09T12:00:00Z"); // Monday
        assert_eq!(
            stamp(parse_at("2026-11-10T04:00:00+01:00", now).expect("rfc3339")),
            "2026-11-10T03:00:00Z"
        );
        assert_eq!(
            stamp(parse_at("sun 04:00", now).expect("day time")),
            "2026-11-15T04:00:00Z"
        );
        assert_eq!(
            stamp(parse_at("Monday 18:00", now).expect("today")),
            "2026-11-09T18:00:00Z"
        );
        assert_eq!(
            stamp(parse_at("mon 11:00", now).expect("next week")),
            "2026-11-16T11:00:00Z",
            "a time already past today resolves to the next week"
        );
        assert!(parse_at("2026-11-01T00:00:00Z", now).is_err());
        assert!(parse_at("someday 04:00", now).is_err());
        assert!(parse_at("sun", now).is_err());
    }
}
