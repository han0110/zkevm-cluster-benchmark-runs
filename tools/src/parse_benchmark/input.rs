//! The input stage, reading a run directory into the intermediate model. The `log` submodule parses
//! the cluster logs through a zkVM backend, and the readers here load metrics, hardware, and
//! telemetry into [`Sources`] so the measurement sources stay independent of the zkVM.

pub mod dmon;
pub mod log;
pub mod metrics;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use serde::Deserialize;

use crate::parse_benchmark::{
    input::{
        dmon::DmonRow,
        log::lines::{RawLine, load_raw_lines},
        metrics::{Hardware, MetricBlock, MetricsMeta},
    },
    io_at, json_at, read_dir_at, read_to_string_at,
};

/// Benchmark identity recorded by the deployment in the run directory's benchmark.json, the human
/// name and description shown on the overview. Both default to empty when the file or a field is
/// absent, and the assembler fills an empty name from the derived benchmark id.
#[derive(Deserialize, Default)]
pub struct BenchMeta {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
}

impl BenchMeta {
    /// Reads the run directory's benchmark.json identity, returning empty defaults when it is
    /// absent. Unknown fields are ignored, so a directory whose benchmark.json already holds a
    /// full document still yields its name and description.
    pub fn load(input: &Path) -> crate::parse_benchmark::Result<BenchMeta> {
        let path = input.join("benchmark.json");
        if !path.is_file() {
            return Ok(BenchMeta::default());
        }
        let text = read_to_string_at(&path)?;
        serde_json::from_str(&text).map_err(json_at(&path))
    }
}

/// The non-log sources of a run, holding the per-proof metrics and their metadata, the node
/// hardware spec, the GPU telemetry keyed by node, and the kept raw cluster-log lines.
pub struct Sources {
    pub blocks: Vec<MetricBlock>,
    pub meta: MetricsMeta,
    pub hardware: Hardware,
    pub dmon: BTreeMap<u32, Vec<DmonRow>>,
    /// Every kept coordinator and worker log line, sorted by timestamp, which the assembler slices
    /// into each block's proving window.
    pub raw_log: Vec<RawLine>,
}

impl Sources {
    /// Reads the metrics, hardware, telemetry, and raw cluster-log lines of a run, leaving the
    /// structured log parse to the backend.
    pub fn load(input: &Path) -> crate::parse_benchmark::Result<Sources> {
        let metrics_dir = input.join("zkevm-metrics");
        if !metrics_dir.is_dir() {
            return Err(crate::parse_benchmark::ParseError::MissingMetricsDir(
                metrics_dir,
            ));
        }
        let (blocks, meta) = metrics::load_metrics(&metrics_dir)?;
        let hardware = metrics::load_hardware(&metrics_dir)?;
        let logs_dir = input.join("logs");
        let dmon = dmon::load_dmon(&logs_dir)?;
        let raw_log = load_raw_lines(&logs_dir)?;
        Ok(Sources {
            blocks,
            meta,
            hardware,
            dmon,
            raw_log,
        })
    }
}

/// Matches a worker-N.log or per-GPU worker-N-gpuG.log file name, capturing the node digit in the
/// first group and the optional GPU index in the second, and excluding the worker-N-dmon.log
/// telemetry files. Shared by the raw-line extractor and the structured worker-log parser, the two
/// readers that enumerate worker logs, so the file selection stays identical between them.
pub(crate) static WORKER_LOG_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^worker-(\d+)(?:-gpu(\d+))?\.log$").unwrap());

/// Lists a directory's per-node files whose name `re` matches with a node digit in its first
/// capture group, each paired with that digit and sorted ascending by it.
///
/// Shared by the worker-log and telemetry readers, whose node ordering the rest of the document
/// depends on. A non-UTF-8 name or one `re` rejects is skipped.
pub(crate) fn worker_files_sorted(
    dir: &Path,
    re: &regex::Regex,
) -> crate::parse_benchmark::Result<Vec<(u32, PathBuf)>> {
    let entries = read_dir_at(dir)?;
    let mut files: Vec<(u32, PathBuf)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io_at(dir))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(digit) = re
            .captures(name)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<u32>().ok())
        {
            files.push((digit, path));
        }
    }
    files.sort_by_key(|(digit, _)| *digit);
    Ok(files)
}
