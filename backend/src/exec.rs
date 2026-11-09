//! Command allowlist of the instance console (`POST /api/instances/{id}/exec`).
//!
//! Execution is argv-based: the driver runs `docker compose exec -T agent …`
//! directly, without a shell. Shell metacharacters are therefore inert, and the
//! only gate between an operator and arbitrary code inside a container is this
//! policy:
//!
//! - an allowlist of command *basenames*, configurable with
//!   `FLEETT_EXEC_ALLOWLIST` (comma-separated), with a conservative default;
//! - a hard denylist that is always refused, even when the allowlist is `*`;
//! - a basename-only rule: a path (`/bin/ls`, `./ls`) is refused, so an
//!   operator cannot point at an arbitrary binary of the image;
//! - an argument-based guard against the `env` escalation (`env rm -rf /`),
//!   since `env` itself is allowlisted.
//!
//! Every call — accepted or refused — is audited by the API layer
//! (`instance.exec`). See `docs/EXEC.md`.

use std::collections::BTreeSet;

/// Commands accepted when `FLEETT_EXEC_ALLOWLIST` is not set.
pub const DEFAULT_ALLOWLIST: [&str; 17] = [
    "ls", "cat", "tail", "head", "grep", "env", "ps", "df", "du", "date", "which", "pwd", "id",
    "uname", "uptime", "free", "echo",
];

/// Commands refused even when `FLEETT_EXEC_ALLOWLIST=*`.
///
/// These either destroy data, spawn arbitrary code, change the host/container
/// state or reach the network. Shells are listed explicitly even though argv
/// execution never interprets a command line: an interactive `sh -c …` would
/// turn the allowlist into a formality.
pub const DENYLIST: [&str; 34] = [
    "rm",
    "rmdir",
    "mv",
    "dd",
    "mkfs",
    "sh",
    "bash",
    "zsh",
    "fish",
    "python",
    "python3",
    "perl",
    "ruby",
    "node",
    "nc",
    "ncat",
    "curl",
    "wget",
    "socat",
    "chmod",
    "chown",
    "mount",
    "umount",
    "kill",
    "killall",
    "reboot",
    "shutdown",
    "docker",
    "systemctl",
    "su",
    "sudo",
    "apt",
    "apk",
    "yum",
];

/// Default timeout of one command, in seconds.
pub const DEFAULT_TIMEOUT_S: u64 = 30;
/// Shortest accepted timeout.
pub const MIN_TIMEOUT_S: u64 = 1;
/// Longest accepted timeout: one command must not pin an API connection for
/// minutes, and a hung container is diagnosed with `logs`, not with `exec`.
pub const MAX_TIMEOUT_S: u64 = 300;
/// Largest number of arguments (including the command name).
pub const MAX_ARGS: usize = 32;
/// Longest accepted argument, in bytes.
pub const MAX_ARG_BYTES: usize = 4096;
/// Cap of each captured stream (stdout and stderr are capped separately).
pub const MAX_OUTPUT_BYTES: usize = 256 * 1024;
/// Appended to a stream that reached [`MAX_OUTPUT_BYTES`].
pub const TRUNCATION_MARKER: &str = "\n[fleett: output truncated at 262144 bytes]\n";
/// Value of `FLEETT_EXEC_ALLOWLIST` that disables the allowlist.
pub const WILDCARD: &str = "*";

/// Why one invocation was refused. `code` is the machine-readable reason
/// returned as `error.details.reason`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecRefusal {
    pub code: &'static str,
    pub reason: String,
}

impl ExecRefusal {
    fn new(code: &'static str, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
        }
    }
}

/// Effective command policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecPolicy {
    allowlist: BTreeSet<String>,
    wildcard: bool,
    /// Where the allowlist comes from (`default` or `FLEETT_EXEC_ALLOWLIST`).
    source: &'static str,
}

impl Default for ExecPolicy {
    fn default() -> Self {
        Self::default_policy()
    }
}

impl ExecPolicy {
    /// Policy used when no environment value is configured.
    pub fn default_policy() -> Self {
        Self::from_allowlist(DEFAULT_ALLOWLIST.iter().copied(), false, "default")
    }

    /// Build the policy from `FLEETT_EXEC_ALLOWLIST`.
    ///
    /// An absent or blank value keeps the default allowlist. `*` (alone or
    /// among other entries) disables the allowlist; the hard denylist still
    /// applies. Entries that are not plain basenames are ignored with a
    /// warning: a typo must fail closed, never widen the policy.
    pub fn from_config(value: Option<&str>) -> Self {
        let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
            return Self::default_policy();
        };
        let mut names = Vec::new();
        let mut wildcard = false;
        for entry in value.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            if entry == WILDCARD {
                wildcard = true;
                continue;
            }
            if !is_command_name(entry) {
                tracing::warn!(
                    entry,
                    "ignoring an invalid FLEETT_EXEC_ALLOWLIST entry (expected a command basename)"
                );
                continue;
            }
            names.push(entry);
        }
        Self::from_allowlist(names, wildcard, "FLEETT_EXEC_ALLOWLIST")
    }

    fn from_allowlist(
        entries: impl IntoIterator<Item = impl Into<String>>,
        wildcard: bool,
        source: &'static str,
    ) -> Self {
        Self {
            allowlist: entries.into_iter().map(Into::into).collect(),
            wildcard,
            source,
        }
    }

    /// Sorted allowlist, as returned by `GET /api/exec/policy`.
    pub fn allowlist(&self) -> Vec<String> {
        self.allowlist.iter().cloned().collect()
    }

    /// Sorted hard denylist.
    pub fn denylist() -> Vec<&'static str> {
        let mut names: Vec<&'static str> = DENYLIST.to_vec();
        names.sort_unstable();
        names
    }

    /// `true` when `FLEETT_EXEC_ALLOWLIST=*` was assumed explicitly.
    pub fn wildcard(&self) -> bool {
        self.wildcard
    }

    /// `default` or `FLEETT_EXEC_ALLOWLIST`.
    pub fn source(&self) -> &'static str {
        self.source
    }

    /// Accept or refuse one argv vector.
    pub fn check(&self, argv: &[String]) -> Result<(), ExecRefusal> {
        if argv.is_empty() {
            return Err(ExecRefusal::new(
                "empty_argv",
                "argv must contain at least a command name",
            ));
        }
        if argv.len() > MAX_ARGS {
            return Err(ExecRefusal::new(
                "too_many_arguments",
                format!("at most {MAX_ARGS} arguments are accepted"),
            ));
        }
        for argument in argv {
            if argument.len() > MAX_ARG_BYTES {
                return Err(ExecRefusal::new(
                    "argument_too_long",
                    format!("each argument must be at most {MAX_ARG_BYTES} bytes"),
                ));
            }
            if argument.contains('\0') {
                return Err(ExecRefusal::new(
                    "invalid_argument",
                    "an argument contains a NUL byte",
                ));
            }
        }

        let program = argv[0].as_str();
        if program.is_empty() {
            return Err(ExecRefusal::new(
                "empty_command",
                "the command name must not be empty",
            ));
        }
        if program.starts_with('-') {
            return Err(ExecRefusal::new(
                "invalid_command",
                "the command name must not start with \"-\"",
            ));
        }
        if program.contains('/') {
            return Err(ExecRefusal::new(
                "path_not_allowed",
                "the command must be a bare basename resolved through the container PATH, \
                 not a path",
            ));
        }
        if !is_command_name(program) {
            return Err(ExecRefusal::new(
                "invalid_command",
                "the command must be a plain basename ([A-Za-z0-9._+-])",
            ));
        }

        if DENYLIST.contains(&program) {
            return Err(ExecRefusal::new(
                "denied_command",
                format!("\"{program}\" is permanently refused by the exec denylist"),
            ));
        }

        if !self.wildcard {
            if !self.allowlist.contains(program) {
                return Err(ExecRefusal::new(
                    "not_allowlisted",
                    format!("\"{program}\" is not in the exec allowlist"),
                ));
            }
            // `env` is allowlisted because dumping the container environment is
            // a routine diagnostic, but `env <cmd>` executes `<cmd>` and would
            // bypass the whole allowlist.
            if program == "env" && argv.len() > 1 {
                return Err(ExecRefusal::new(
                    "unsafe_arguments",
                    "\"env\" only accepts no argument in allowlist mode: `env <command>` would \
                     run an unvetted command",
                ));
            }
        }

        Ok(())
    }
}

/// Command basenames are intentionally narrow: no `/`, no whitespace, no
/// shell metacharacter.
fn is_command_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '+' | '-'))
}

/// Truncate one captured stream, appending [`TRUNCATION_MARKER`] when it was
/// cut. The cut happens on a UTF-8 boundary; the marker is not counted in the
/// 256 KiB budget.
pub fn truncate_output(text: &str) -> (String, bool) {
    if text.len() <= MAX_OUTPUT_BYTES {
        return (text.to_string(), false);
    }
    let mut end = MAX_OUTPUT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + TRUNCATION_MARKER.len());
    out.push_str(&text[..end]);
    out.push_str(TRUNCATION_MARKER);
    (out, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn the_default_allowlist_accepts_a_listing_inside_the_data_directory() {
        let policy = ExecPolicy::default_policy();
        assert_eq!(policy.source(), "default");
        assert!(!policy.wildcard());
        policy
            .check(&argv(&["ls", "/opt/data"]))
            .expect("ls is allowed");
        policy.check(&argv(&["df", "-h"])).expect("df is allowed");
        assert!(policy.allowlist().contains(&"ls".to_string()));
    }

    #[test]
    fn the_denylist_refuses_destructive_binaries_and_shells() {
        let policy = ExecPolicy::default_policy();
        for program in ["rm", "sh", "bash", "dd", "curl", "docker", "sudo"] {
            let refusal = policy
                .check(&argv(&[program, "-rf", "/"]))
                .expect_err(program);
            assert_eq!(refusal.code, "denied_command", "{program}");
            assert!(refusal.reason.contains(program), "{}", refusal.reason);
        }
    }

    #[test]
    fn a_command_outside_the_allowlist_is_refused() {
        let policy = ExecPolicy::from_config(Some("ls,cat"));
        assert_eq!(policy.source(), "FLEETT_EXEC_ALLOWLIST");
        policy.check(&argv(&["ls"])).expect("ls allowed");
        policy
            .check(&argv(&["cat", "/etc/hostname"]))
            .expect("cat allowed");
        let refusal = policy
            .check(&argv(&["df", "-h"]))
            .expect_err("df not listed");
        assert_eq!(refusal.code, "not_allowlisted");
    }

    #[test]
    fn a_path_never_matches_the_basename_policy() {
        let policy = ExecPolicy::default_policy();
        for program in ["/bin/ls", "./ls", "bin/cat"] {
            let refusal = policy.check(&argv(&[program])).expect_err(program);
            assert_eq!(refusal.code, "path_not_allowed", "{program}");
        }
    }

    #[test]
    fn the_denylist_survives_the_wildcard() {
        let policy = ExecPolicy::from_config(Some("*"));
        assert!(policy.wildcard());
        policy
            .check(&argv(&["htop"]))
            .expect("wildcard accepts htop");
        let refusal = policy
            .check(&argv(&["rm", "-rf", "/"]))
            .expect_err("rm stays refused");
        assert_eq!(refusal.code, "denied_command");
        // With the wildcard assumed explicitly, `env cmd` is the operator's
        // choice: the allowlist guard does not apply.
        policy.check(&argv(&["env", "ls"])).expect("wildcard env");
    }

    #[test]
    fn env_may_not_execute_another_command_in_allowlist_mode() {
        let policy = ExecPolicy::default_policy();
        policy.check(&argv(&["env"])).expect("bare env is a dump");
        let refusal = policy.check(&argv(&["env", "ls"])).expect_err("env ls");
        assert_eq!(refusal.code, "unsafe_arguments");
    }

    #[test]
    fn invalid_allowlist_entries_fail_closed() {
        let policy = ExecPolicy::from_config(Some("ls, /usr/bin/cat, bad name,, "));
        assert_eq!(policy.allowlist(), vec!["ls".to_string()]);
        let refusal = policy.check(&argv(&["cat"])).expect_err("cat ignored");
        assert_eq!(refusal.code, "not_allowlisted");
    }

    #[test]
    fn a_blank_environment_value_keeps_the_default() {
        assert_eq!(ExecPolicy::from_config(None), ExecPolicy::default_policy());
        assert_eq!(
            ExecPolicy::from_config(Some("   ")),
            ExecPolicy::default_policy()
        );
    }

    #[test]
    fn argv_is_bounded() {
        let policy = ExecPolicy::default_policy();

        let empty: Vec<String> = Vec::new();
        assert_eq!(policy.check(&empty).expect_err("empty").code, "empty_argv");

        let mut many = vec!["ls".to_string()];
        many.extend(std::iter::repeat_n("x".to_string(), MAX_ARGS));
        assert_eq!(
            policy.check(&many).expect_err("too many").code,
            "too_many_arguments"
        );

        let long = vec!["ls".to_string(), "a".repeat(MAX_ARG_BYTES + 1)];
        assert_eq!(
            policy.check(&long).expect_err("too long").code,
            "argument_too_long"
        );

        let nul = vec!["ls".to_string(), "a\0b".to_string()];
        assert_eq!(
            policy.check(&nul).expect_err("nul").code,
            "invalid_argument"
        );

        let dash = vec!["--help".to_string()];
        assert_eq!(
            policy.check(&dash).expect_err("dash").code,
            "invalid_command"
        );
    }

    #[test]
    fn output_is_truncated_at_the_cap_with_a_marker() {
        let short = "hello";
        let (text, truncated) = truncate_output(short);
        assert_eq!(text, short);
        assert!(!truncated);

        let long = "a".repeat(MAX_OUTPUT_BYTES + 10);
        let (text, truncated) = truncate_output(&long);
        assert!(truncated);
        assert_eq!(text.len(), MAX_OUTPUT_BYTES + TRUNCATION_MARKER.len());
        assert!(text.ends_with(TRUNCATION_MARKER));

        // A multi-byte character ending exactly on the cap is kept whole.
        let mut edge = "a".repeat(MAX_OUTPUT_BYTES - 2);
        edge.push('é');
        edge.push_str("tail");
        let (text, truncated) = truncate_output(&edge);
        assert!(truncated);
        assert!(text.contains('é'));
        assert_eq!(text.len(), MAX_OUTPUT_BYTES + TRUNCATION_MARKER.len());

        // A character straddling the cap is dropped, never split.
        let mut straddling = "a".repeat(MAX_OUTPUT_BYTES - 1);
        straddling.push('é');
        straddling.push_str("tail");
        let (text, truncated) = truncate_output(&straddling);
        assert!(truncated);
        assert!(!text.contains('é'));
        assert_eq!(text.len(), MAX_OUTPUT_BYTES - 1 + TRUNCATION_MARKER.len());
    }

    #[test]
    fn the_timeout_bounds_are_documented_ones() {
        assert_eq!(MIN_TIMEOUT_S, 1);
        assert_eq!(MAX_TIMEOUT_S, 300);
        assert!((MIN_TIMEOUT_S..=MAX_TIMEOUT_S).contains(&DEFAULT_TIMEOUT_S));
        assert_eq!(MAX_OUTPUT_BYTES, 256 * 1024);
    }
}
