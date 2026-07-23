//! The openvm zkVM backend, with detection and the coordinator, worker, and phase parsers that
//! turn an Axiom Edge cluster's logs into the generic log model.
//!
//! An Axiom Edge cluster is one edge-manager writing coordinator.log plus one edge-worker process
//! per GPU writing worker-{host}-gpu{gpu}.log, four workers sharing each host node. Every work
//! item logs one self-contained completion line carrying its duration, so the parsers are one
//! pattern per item kind with no bracket pairing.

pub mod coordinator;
pub mod phases;
pub mod worker;

use std::{io::Read, path::Path};

use crate::parse_benchmark::input::log::{
    PhaseDef,
    zkvm::{ParsedLogs, ZkvmParser},
};

/// The openvm backend.
pub struct OpenvmParser;

/// Returns the ordered openvm phase preset. The metered execution, segment, and recursion phases
/// overlap because the executor meters segments while they prove and recursion consumes segments as
/// they complete, so none of the three is a blocking stage.
pub fn openvm_phases() -> Vec<PhaseDef> {
    [
        ("input", "Input Transfer", false),
        ("metered_execution", "Metered Execution", true),
        ("segment", "Segment", true),
        ("recursion", "Recursion", true),
        ("wrap", "Wrap", false),
    ]
    .into_iter()
    .map(|(name, label, overlap)| PhaseDef {
        name: name.to_string(),
        label: label.to_string(),
        overlap,
    })
    .collect()
}

/// Reports whether coordinator log text was produced by the Axiom Edge manager.
pub fn detect_openvm(coordinator_text: &str) -> bool {
    coordinator_text.contains("edge_manager")
}

/// How much of the coordinator log detection reads. The manager banner sits in the first lines, so
/// a bounded prefix identifies the backend without reading the whole multi-gigabyte log.
const DETECT_PREFIX_BYTES: u64 = 64 * 1024;

impl OpenvmParser {
    /// The coordinator log path within a run's logs directory. The coordinator.log name comes from
    /// the benchmark deployment, kept here rather than in the shared run layout.
    fn coordinator_log(logs_dir: &Path) -> std::path::PathBuf {
        logs_dir.join("coordinator.log")
    }

    /// Reads the bounded detection prefix of the coordinator log, decoded lossily so a multi-byte
    /// character cut at the boundary cannot fail detection.
    fn coordinator_prefix(logs_dir: &Path) -> Option<String> {
        let file = std::fs::File::open(Self::coordinator_log(logs_dir)).ok()?;
        let mut buf = Vec::new();
        file.take(DETECT_PREFIX_BYTES).read_to_end(&mut buf).ok()?;
        Some(String::from_utf8_lossy(&buf).into_owned())
    }
}

impl ZkvmParser for OpenvmParser {
    fn detect(&self, logs_dir: &Path) -> bool {
        Self::coordinator_prefix(logs_dir)
            .map(|t| detect_openvm(&t))
            .unwrap_or(false)
    }

    fn parse(&self, logs_dir: &Path) -> crate::parse_benchmark::Result<ParsedLogs> {
        let coord_path = Self::coordinator_log(logs_dir);
        let coord_text = crate::parse_benchmark::read_to_string_at(&coord_path)?;
        let raw_jobs = coordinator::parse(&coord_text)?;
        let worker = worker::load(logs_dir)?;
        let logs = raw_jobs
            .iter()
            .map(|raw| {
                phases::build_log(
                    raw,
                    worker.items.get(&raw.id),
                    worker.announcements.get(&raw.id),
                    worker.writes.get(&raw.id),
                    worker.sends.get(&raw.id),
                )
            })
            .collect();

        Ok(ParsedLogs {
            name: "openvm",
            phases: openvm_phases(),
            pipeline: phases::openvm_pipeline(),
            logs,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::parse_benchmark::input::log::zkvm::openvm::{detect_openvm, openvm_phases};

    #[test]
    fn the_phase_preset_marks_the_overlapping_phases() {
        let named: Vec<(String, String, bool)> = openvm_phases()
            .into_iter()
            .map(|p| (p.name, p.label, p.overlap))
            .collect();
        let expected = [
            ("input", "Input Transfer", false),
            ("metered_execution", "Metered Execution", true),
            ("segment", "Segment", true),
            ("recursion", "Recursion", true),
            ("wrap", "Wrap", false),
        ]
        .map(|(name, label, overlap)| (name.to_string(), label.to_string(), overlap));
        assert_eq!(named, expected);
    }

    #[test]
    fn detects_the_edge_manager_target() {
        assert!(detect_openvm(
            "2026-07-21T13:54:44.352947Z  INFO edge_manager: Starting Edge Manager on 0.0.0.0:3000"
        ));
        assert!(!detect_openvm(
            "2026-05-29T09:51:19Z INFO: zisk-coordinator listening on 0.0.0.0:7000"
        ));
    }
}
