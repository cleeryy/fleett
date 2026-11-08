//! Log filtering, parsing and bounding for the logs endpoints.
//!
//! The driver only ever passes `tail` and `since` to Docker; finding a needle
//! in a 2000-line crash loop happens here, where the raw text can be filtered
//! with a regex, classified by level, and bounded before it reaches the client.

use chrono::DateTime;
use regex::Regex;
use serde::Serialize;

/// Largest response body the logs endpoint returns (1 MiB).
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Largest raw log text read before filtering (4 MiB).
///
/// `docker compose logs --tail 2000` can still return a few megabytes when the
/// agent prints stack traces; scanning is bounded so one request cannot pin the
/// control plane.
pub const MAX_SCAN_BYTES: usize = 4 * MAX_RESPONSE_BYTES;
/// Longest accepted `since` value.
pub const MAX_SINCE_LEN: usize = 64;
/// Longest accepted `grep` pattern.
pub const MAX_GREP_LEN: usize = 512;

/// Severity selected by `?level=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
}

impl Level {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "error" | "errors" => Some(Self::Error),
            "warn" | "warning" | "warnings" => Some(Self::Warn),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FilterError {
    #[error("grep must be at most {MAX_GREP_LEN} characters")]
    GrepTooLong,
    #[error("invalid grep pattern: {0}")]
    InvalidGrep(String),
    #[error("level must be \"error\" or \"warn\"")]
    InvalidLevel,
}

/// `grep` + `level` filtering of one log stream.
#[derive(Debug, Clone, Default)]
pub struct Filters {
    grep: Option<Regex>,
    level: Option<Level>,
}

impl Filters {
    /// Build the filters, refusing a broken regex or an unknown level.
    pub fn new(grep: Option<&str>, level: Option<&str>) -> Result<Self, FilterError> {
        let grep = match grep.map(str::trim).filter(|value| !value.is_empty()) {
            Some(pattern) => {
                if pattern.chars().count() > MAX_GREP_LEN {
                    return Err(FilterError::GrepTooLong);
                }
                Some(Regex::new(pattern).map_err(|err| FilterError::InvalidGrep(err.to_string()))?)
            }
            None => None,
        };
        let level = match level.map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) => Some(Level::parse(value).ok_or(FilterError::InvalidLevel)?),
            None => None,
        };
        Ok(Self { grep, level })
    }

    /// True when a line passes both filters.
    ///
    /// `level=warn` means "warnings and above": it keeps warning *and* error
    /// lines, which is what an operator asking for warnings expects.
    pub fn matches(&self, line: &str) -> bool {
        match self.level {
            Some(Level::Error) if !is_error(line) => return false,
            Some(Level::Warn) if !(is_warn(line) || is_error(line)) => return false,
            _ => {}
        }
        match &self.grep {
            Some(regex) => regex.is_match(line),
            None => true,
        }
    }

    /// Keep the lines that pass the filters.
    pub fn apply<'a>(&self, text: &'a str) -> Vec<&'a str> {
        text.lines().filter(|line| self.matches(line)).collect()
    }
}

/// One line of a `?format=json` response.
///
/// A line that starts with an RFC 3339 timestamp (optionally behind the
/// `docker compose logs` container prefix) is split into `{time, message}`;
/// everything else is returned as `{line}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum LogEntry {
    Timed { time: String, message: String },
    Plain { line: String },
}

/// Build the JSON entries of one page, stopping at the byte budget.
pub fn entries(lines: &[&str], max_bytes: usize) -> Vec<LogEntry> {
    let mut out = Vec::new();
    let mut used = 2usize; // the enclosing `[` and `]`
    for line in lines {
        let entry = entry(line);
        let size = serde_json::to_string(&entry)
            .map(|json| json.len() + 1)
            .unwrap_or(0);
        if used + size > max_bytes {
            break;
        }
        used += size;
        out.push(entry);
    }
    out
}

/// Parse one line into its JSON shape.
pub fn entry(line: &str) -> LogEntry {
    match split_timestamp(line) {
        Some((time, message)) => LogEntry::Timed { time, message },
        None => LogEntry::Plain {
            line: line.to_string(),
        },
    }
}

/// Render the kept lines as text, stopping at the byte budget.
pub fn render_text(lines: &[&str], max_bytes: usize) -> String {
    let mut out = String::new();
    for line in lines {
        if out.len() + line.len() + 1 > max_bytes {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Count and locate the error/warn lines of one stream (bounded upstream).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ErrorSummary {
    pub count: usize,
    pub last: Option<String>,
    pub last_at: Option<String>,
}

pub fn error_summary(text: &str) -> ErrorSummary {
    let mut summary = ErrorSummary::default();
    for line in text.lines() {
        if !(is_error(line) || is_warn(line)) {
            continue;
        }
        summary.count += 1;
        summary.last = Some(line.to_string());
        summary.last_at = split_timestamp(line).map(|(time, _)| time);
    }
    summary
}

/// Truncate text to at most `max_bytes`, on a character boundary.
pub fn truncate(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Normalise a `?since=` value: a Docker duration or an RFC 3339 timestamp.
pub fn normalize_since(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("since must not be empty".to_string());
    }
    if value.len() > MAX_SINCE_LEN {
        return Err(format!("since must be at most {MAX_SINCE_LEN} characters"));
    }
    if DateTime::parse_from_rfc3339(value).is_ok() || is_docker_duration(value) {
        return Ok(value.to_string());
    }
    Err(format!(
        "since must be a Docker duration (30s, 10m, 2h, 1h30m) or an RFC 3339 timestamp, got \"{value}\""
    ))
}

/// Go duration grammar used by Docker: one or more `<number><unit>` groups.
fn is_docker_duration(value: &str) -> bool {
    // Two-character units first, then the single-character ones; `µs` is the
    // microsecond spelling Go accepts next to `us`.
    const UNITS: [&str; 7] = ["ns", "us", "µs", "ms", "s", "m", "h"];
    let mut rest = value;
    if rest.is_empty() {
        return false;
    }
    while !rest.is_empty() {
        let digits = rest
            .chars()
            .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
            .count();
        if digits == 0 {
            return false;
        }
        let (number, tail) = rest.split_at(digits);
        if number.matches('.').count() > 1 || number.starts_with('.') || number.ends_with('.') {
            return false;
        }
        let Some(unit) = UNITS.iter().find(|unit| tail.starts_with(**unit)) else {
            return false;
        };
        rest = &tail[unit.len()..];
    }
    true
}

/// Error-level heuristics on one log line.
pub fn is_error(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    ["error", "traceback", "panic", "fatal", "critical"]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// Warning-level heuristics on one log line.
pub fn is_warn(line: &str) -> bool {
    line.to_ascii_lowercase().contains("warn")
}

/// Split a leading RFC 3339 timestamp off a log line.
///
/// `docker compose logs` prefixes lines with `<container>  | `; the prefix is
/// skipped so a timestamped line is still recognised as such.
pub fn split_timestamp(line: &str) -> Option<(String, String)> {
    let body = strip_compose_prefix(line);
    let candidate = body.split_whitespace().next()?;
    if DateTime::parse_from_rfc3339(candidate).is_err() {
        return None;
    }
    let message = body[candidate.len()..].trim_start().to_string();
    Some((candidate.to_string(), message))
}

fn strip_compose_prefix(line: &str) -> &str {
    match line.find("| ") {
        Some(index) if index <= 64 => line[index + 2..].trim_start(),
        _ => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_durations_are_accepted() {
        for value in ["30s", "10m", "2h", "90s", "1h30m", "1.5h", "100ms", "500us"] {
            assert_eq!(normalize_since(value).as_deref(), Ok(value), "{value}");
        }
        for value in [
            "2026-11-08T01:00:00Z",
            "2026-11-08T01:00:00+01:00",
            "2026-11-08T01:00:00.123Z",
        ] {
            assert_eq!(normalize_since(value).as_deref(), Ok(value), "{value}");
        }
    }

    #[test]
    fn invalid_since_values_are_refused() {
        for value in [
            "",
            "  ",
            "soon",
            "10",
            "m10",
            "-5m",
            "1.2.3s",
            ".5h",
            "5x",
            "10m30",
            &"9".repeat(MAX_SINCE_LEN + 1),
        ] {
            assert!(normalize_since(value).is_err(), "{value:?} accepted");
        }
    }

    #[test]
    fn grep_is_a_regex() {
        let filters = Filters::new(Some("gateway\\.sock"), None).expect("valid regex");
        assert!(filters.matches("connect to /opt/data/gateway.sock failed"));
        assert!(!filters.matches("connect to /opt/data/gatewayXsock failed"));

        assert!(matches!(
            Filters::new(Some("(unclosed"), None),
            Err(FilterError::InvalidGrep(_))
        ));
        assert!(matches!(
            Filters::new(Some(&"a".repeat(MAX_GREP_LEN + 1)), None),
            Err(FilterError::GrepTooLong)
        ));
    }

    #[test]
    fn level_filters_keep_the_expected_lines() {
        let text = "starting\nERROR: boom\nwarning: slow\nall good";
        let errors = Filters::new(None, Some("error")).expect("level");
        assert_eq!(errors.apply(text), ["ERROR: boom"]);

        let warnings = Filters::new(None, Some("warn")).expect("level");
        assert_eq!(
            warnings.apply(text),
            ["ERROR: boom", "warning: slow"],
            "warn keeps warnings and above"
        );

        assert!(matches!(
            Filters::new(None, Some("debug")),
            Err(FilterError::InvalidLevel)
        ));
    }

    #[test]
    fn level_is_case_insensitive_and_matches_crash_signatures() {
        for line in [
            "Traceback (most recent call last):",
            "FATAL: cannot start",
            "thread panicked at src/main.rs",
            "ERROR connecting",
            "critical failure",
        ] {
            assert!(is_error(line), "{line}");
        }
        for line in ["WARNING: disk", "warn: retry", "Warnings: 1"] {
            assert!(is_warn(line), "{line}");
        }
        assert!(!is_error("all good"));
        assert!(!is_warn("all good"));
    }

    #[test]
    fn timestamps_are_split_out_when_present() {
        let (time, message) = split_timestamp("2026-11-08T01:02:03Z boom").expect("timestamp");
        assert_eq!(time, "2026-11-08T01:02:03Z");
        assert_eq!(message, "boom");

        let (time, message) =
            split_timestamp("fleett-alice  | 2026-11-08T01:02:03.5Z gateway died").expect("prefix");
        assert_eq!(time, "2026-11-08T01:02:03.5Z");
        assert_eq!(message, "gateway died");

        assert_eq!(split_timestamp("no timestamp here"), None);
        assert_eq!(split_timestamp(""), None);
    }

    #[test]
    fn json_entries_have_the_documented_shape() {
        let lines = ["2026-11-08T01:02:03Z boom", "plain line"];
        let entries = entries(&lines, MAX_RESPONSE_BYTES);
        assert_eq!(
            serde_json::to_value(&entries).expect("serialises"),
            serde_json::json!([
                {"time": "2026-11-08T01:02:03Z", "message": "boom"},
                {"line": "plain line"}
            ])
        );
    }

    #[test]
    fn responses_are_bounded() {
        let long = "x".repeat(64);
        let lines: Vec<&str> = vec![long.as_str(); 100];
        let text = render_text(&lines, 100);
        assert!(text.len() <= 100, "{}", text.len());
        assert_eq!(text.lines().count(), 1);

        let json = entries(&lines, 200);
        let encoded = serde_json::to_string(&json).expect("serialises");
        assert!(encoded.len() <= 200, "{}", encoded.len());
        assert!(!json.is_empty());

        let multibyte = "é".repeat(50);
        let truncated = truncate(&multibyte, 51);
        assert!(truncated.len() <= 51);
        assert!(truncated.chars().all(|ch| ch == 'é'));
    }

    #[test]
    fn the_error_summary_counts_and_locates_the_last_line() {
        let text = "starting\nERROR: one\nwarning: two\n2026-11-08T01:02:03Z ERROR: three\nok\n";
        let summary = error_summary(text);
        assert_eq!(summary.count, 3);
        assert_eq!(
            summary.last.as_deref(),
            Some("2026-11-08T01:02:03Z ERROR: three")
        );
        assert_eq!(summary.last_at.as_deref(), Some("2026-11-08T01:02:03Z"));

        // Without a timestamp the position is unknown, and a clean log is empty.
        let summary = error_summary("ERROR: one");
        assert_eq!(summary.count, 1);
        assert_eq!(summary.last_at, None);
        assert_eq!(error_summary("all good").count, 0);
    }
}
