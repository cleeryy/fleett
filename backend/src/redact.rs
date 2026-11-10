//! Redaction shared by the assistant and the provider test (issues #56, #64).
//!
//! One rule set for everything that quotes an upstream answer: known literal
//! values (a provider secret, a header value, a secret environment value) are
//! replaced first, then the token shapes. The result is safe to log, to return
//! in an error body and to send to a model.

use regex::Regex;
use std::sync::OnceLock;

/// Replacement written wherever a secret used to be.
pub const REDACTED: &str = "[REDACTED]";

/// Literal secret values plus token shapes, applied to every outgoing string.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    /// Exact values (provider secret, header values), longest first.
    needles: Vec<String>,
}

impl Redactor {
    /// Build a redactor; empty values are ignored.
    pub fn new(values: impl IntoIterator<Item = String>) -> Self {
        let mut needles: Vec<String> = values
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect();
        needles.sort_by_key(|value| std::cmp::Reverse(value.len()));
        needles.dedup();
        Self { needles }
    }

    /// Replace every known value and token shape. Returns the text and the
    /// number of replacements.
    pub fn redact(&self, text: &str) -> (String, usize) {
        let mut output = text.to_string();
        let mut count = 0usize;
        for needle in &self.needles {
            count += output.matches(needle.as_str()).count();
            output = output.replace(needle.as_str(), REDACTED);
        }
        for regex in patterns() {
            count += regex.find_iter(&output).count();
            output = regex.replace_all(&output, REDACTED).into_owned();
        }
        (output, count)
    }

    /// Redact, then truncate on a UTF-8 boundary: the shape of an error detail.
    pub fn redact_capped(&self, text: &str, max: usize) -> String {
        let (redacted, _) = self.redact(text);
        if redacted.len() <= max {
            return redacted.trim().to_string();
        }
        let mut end = max;
        while end > 0 && !redacted.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", redacted[..end].trim())
    }
}

fn patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            // OpenAI-style key.
            Regex::new(r"sk-[A-Za-z0-9_-]{8,}").expect("valid regex"),
            // fleett token (32 hex digits).
            Regex::new(r"flt_[A-Za-z0-9_-]{8,}").expect("valid regex"),
            // Authorization header value.
            Regex::new(r"Bearer\s+\S+").expect("valid regex"),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values_and_token_shapes_are_redacted() {
        let redactor = Redactor::new(vec!["sk-super-secret-value".to_string()]);
        let (text, count) = redactor.redact(
            "secret=sk-super-secret-value other sk-abcdefgh1234 flt_0123456789abcdef0123456789abcdef \
             Authorization: Bearer abc.def",
        );
        assert!(!text.contains("sk-super-secret-value"));
        assert!(!text.contains("sk-abcdefgh1234"));
        assert!(!text.contains("flt_0123456789abcdef"));
        assert!(!text.contains("abc.def"));
        assert_eq!(count, 4, "{text}");
        assert!(text.contains(REDACTED));
    }

    #[test]
    fn capped_redaction_keeps_a_utf8_boundary() {
        let redactor = Redactor::new(vec!["hunter2".to_string()]);
        let text = format!("ééééé hunter2 {}", "x".repeat(100));
        let capped = redactor.redact_capped(&text, 8);
        assert!(!capped.contains("hunter2"), "{capped}");
        assert!(capped.len() <= 11, "{capped}");
    }

    #[test]
    fn a_header_value_is_scrubbed_like_a_secret() {
        let redactor = Redactor::new(vec!["session-value-123".to_string()]);
        let (text, count) = redactor.redact(r#"{"error":"bad header session-value-123"}"#);
        assert!(!text.contains("session-value-123"));
        assert_eq!(count, 1);
    }
}
