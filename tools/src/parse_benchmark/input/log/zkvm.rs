//! The zkVM backend framework, holding the parsed-logs result, the backend trait and its registry,
//! and the log-parsing helpers backends share. The framework names no specific zkVM. A backend
//! reports its own name on [`ParsedLogs`] and registers itself in [`detect_backend`].

pub mod zisk;

use std::{borrow::Cow, path::Path, sync::LazyLock};

use regex::Regex;

use crate::parse_benchmark::input::log::{Log, PhaseDef, PipelineDef};

/// The result of parsing a run's cluster logs, holding the zkVM's wire name, its ordered phase
/// preset, its fine-pipeline template, and one parsed log per proving job. The name is the
/// lowercase identity the backend reports, emitted verbatim so the framework never enumerates the
/// known zkVMs itself.
pub struct ParsedLogs {
    pub name: &'static str,
    pub phases: Vec<PhaseDef>,
    pub pipeline: Vec<PipelineDef>,
    pub logs: Vec<Log>,
}

/// A zkVM backend detects its own runs and parses their cluster logs into the log model.
pub trait ZkvmParser {
    /// Reports whether the cluster logs under the directory came from this backend.
    fn detect(&self, logs_dir: &Path) -> bool;

    /// Parses the cluster logs into the zkVM name, phase preset, and per-job logs.
    fn parse(&self, logs_dir: &Path) -> crate::parse_benchmark::Result<ParsedLogs>;
}

/// Returns the first registered backend whose detector matches the cluster logs. Registering a new
/// zkVM is a single entry in this list.
pub fn detect_backend(logs_dir: &Path) -> Option<Box<dyn ZkvmParser>> {
    let backends: Vec<Box<dyn ZkvmParser>> = vec![Box::new(zisk::ZiskParser)];
    backends.into_iter().find(|b| b.detect(logs_dir))
}

/// Matches an ANSI SGR escape sequence.
static ANSI_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*m").unwrap());

/// Removes ANSI SGR escape sequences from log text, for backends that parse colorized logs.
pub(crate) fn strip_ansi(text: &str) -> Cow<'_, str> {
    ANSI_RE.replace_all(text, "")
}

/// Returns a named capture group as a string slice, or an empty slice when absent.
pub(crate) fn cap<'a>(caps: &'a regex::Captures, name: &str) -> &'a str {
    caps.name(name).map(|m| m.as_str()).unwrap_or("")
}

/// Returns a named capture parsed as f64, defaulting to zero when absent or unparseable.
pub(crate) fn capf(caps: &regex::Captures, name: &str) -> f64 {
    cap(caps, name).parse().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use crate::parse_benchmark::input::log::zkvm::strip_ansi;

    #[test]
    fn strips_ansi_escapes() {
        let dirty = "\x1b[2m2026-05-29T09:53:15.822529Z\x1b[0m \x1b[32mINFO\x1b[0m: hello";
        assert_eq!(strip_ansi(dirty), "2026-05-29T09:53:15.822529Z INFO: hello");
    }

    #[test]
    fn leaves_clean_text_untouched() {
        assert_eq!(strip_ansi("no escapes here"), "no escapes here");
    }
}
