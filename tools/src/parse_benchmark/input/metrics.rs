//! Reads the zkevm-metrics archive of per-proof result files.
//!
//! Each file records one proof keyed solely by its name, which the archive guarantees unique, so no
//! block number is parsed from the file name and an arbitrary fixture name is carried verbatim. A
//! file reports a success or a crash section, and the crash reason is mined here for the terminal
//! status and blamed nodes.

use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
};

use crate::parse_benchmark::{input::log::Ts, io_at, json_at, read_dir_at, read_to_string_at};

/// Terminal outcome a metric file reports for one proof.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MetricStatus {
    Success,
    Crashed,
    Timeout,
}

impl MetricStatus {
    /// The wire status string for benchmark.json.
    pub fn as_str(self) -> &'static str {
        match self {
            MetricStatus::Success => "success",
            MetricStatus::Crashed => "crashed",
            MetricStatus::Timeout => "timeout",
        }
    }
}

/// One proof result read from a zkevm-metrics file.
///
/// The proof is identified solely by `name`. The proving figures are present only on success, and
/// the crash fields name the blamed nodes and the cluster job id that binds the crash to its log.
#[derive(Clone)]
pub struct MetricBlock {
    pub name: String,
    pub status: MetricStatus,
    pub block_used_gas: Option<u64>,
    pub proving_time_ms: Option<u64>,
    pub proof_size: Option<u64>,
    pub verification_time_ms: Option<u64>,
    pub timestamp_completed: Option<Ts>,
    /// Node ids the crash reason blamed, in first-seen order, empty on success or an unattributed
    /// crash.
    pub crashed_nodes: Vec<String>,
    /// The cluster job id from the crash reason, the key that binds a crash to its log.
    pub crashed_job: Option<String>,
}

/// Substrings that mark a crash reason as a timeout rather than a hard crash. The archive never
/// flags a timeout itself, so the distinction is recovered from the crash reason text.
const TIMEOUT_MARKERS: [&str; 2] = ["timed out", "timeout"];

/// Matches a WorkerId(node...) token in a crash reason, naming a blamed node.
static WORKER_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"WorkerId\((?P<node>[^)]+)\)").unwrap());

/// Matches a hyphenated uuid, the cluster job id a crash reason carries.
static UUID_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
    )
    .unwrap()
});

/// Raw on-disk shape of one metric file.
#[derive(serde::Deserialize)]
struct RawMetric {
    name: Option<String>,
    timestamp_completed: Option<String>,
    metadata: Option<RawMetadata>,
    proving: Option<RawProving>,
}

#[derive(serde::Deserialize)]
struct RawMetadata {
    block_used_gas: Option<u64>,
    original_test_name: Option<String>,
}

/// The proving result, exactly one of success or crashed in a well-formed file.
#[derive(serde::Deserialize)]
struct RawProving {
    success: Option<RawSuccess>,
    crashed: Option<RawCrashed>,
}

#[derive(serde::Deserialize)]
struct RawSuccess {
    proof_size: Option<u64>,
    proving_time_ms: Option<u64>,
    verification_time_ms: Option<u64>,
}

#[derive(serde::Deserialize)]
struct RawCrashed {
    reason: Option<String>,
}

/// Classifies a crash reason as a timeout when it carries a timeout marker, else a hard crash.
fn classify_crash(reason: &str) -> MetricStatus {
    let lower = reason.to_lowercase();
    if TIMEOUT_MARKERS.iter().any(|m| lower.contains(m)) {
        MetricStatus::Timeout
    } else {
        MetricStatus::Crashed
    }
}

/// Collects the unique node ids a crash reason blames, in first-seen order.
fn blamed_nodes(reason: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for caps in WORKER_RE.captures_iter(reason) {
        if let Some(node) = caps.name("node").map(|m| m.as_str().to_string())
            && !out.contains(&node)
        {
            out.push(node);
        }
    }
    out
}

/// Parses one metric JSON document. The block id prefers the metadata's original test name, falls
/// back to the file's name field, and finally to the file stem.
pub fn parse_metric_json(
    file_stem: &str,
    text: &str,
) -> crate::parse_benchmark::Result<MetricBlock> {
    let raw: RawMetric = serde_json::from_str(text).map_err(json_at(PathBuf::from(file_stem)))?;
    let metadata = raw.metadata;
    let name = metadata
        .as_ref()
        .and_then(|m| m.original_test_name.clone())
        .or(raw.name)
        .unwrap_or_else(|| file_stem.to_string());
    let timestamp_completed = match raw.timestamp_completed {
        Some(s) => Some(Ts::parse(&s)?),
        None => None,
    };

    let proving = raw.proving;
    let success = proving.as_ref().and_then(|p| p.success.as_ref());
    let crash_reason = proving
        .as_ref()
        .and_then(|p| p.crashed.as_ref())
        .and_then(|c| c.reason.as_deref());

    let (status, crashed_nodes, crashed_job) = match (success.is_some(), crash_reason) {
        (true, _) => (MetricStatus::Success, Vec::new(), None),
        (false, Some(reason)) => (
            classify_crash(reason),
            blamed_nodes(reason),
            UUID_RE.find(reason).map(|m| m.as_str().to_string()),
        ),
        // A file carrying neither section is treated as a crash with no detail so it never poses as
        // a success.
        (false, None) => (MetricStatus::Crashed, Vec::new(), None),
    };

    let (proof_size, proving_time_ms, verification_time_ms) = match success {
        Some(s) => (s.proof_size, s.proving_time_ms, s.verification_time_ms),
        None => (None, None, None),
    };

    Ok(MetricBlock {
        name,
        status,
        block_used_gas: metadata.and_then(|m| m.block_used_gas),
        proving_time_ms,
        proof_size,
        verification_time_ms,
        timestamp_completed,
        crashed_nodes,
        crashed_job,
    })
}

/// The guest, guest version, and zkVM version recovered from the metrics directory.
pub struct MetricsMeta {
    pub guest: Option<String>,
    pub guest_version: Option<String>,
    pub version: Option<String>,
}

impl MetricsMeta {
    /// Splits the guest and version directory tokens into their name and version parts, each at its
    /// trailing version suffix. The guest dir is like zisk-eth-client-reth-v0.9.0 and the version
    /// dir like zisk-v0.18.0.
    fn from_dirs(guest_dir: Option<String>, version_dir: Option<String>) -> MetricsMeta {
        let (guest, guest_version) = match guest_dir {
            Some(dir) => {
                let (name, version) = split_name_version(&dir);
                (Some(name), version)
            }
            None => (None, None),
        };
        let version = version_dir.map(|dir| {
            let (_, version) = split_name_version(&dir);
            version.unwrap_or(dir)
        });
        MetricsMeta {
            guest,
            guest_version,
            version,
        }
    }
}

/// Splits a name-vX.Y.Z token into its name and version parts at the last "-v" immediately followed
/// by a digit. Absent such a suffix, it falls back to a trailing "-<short sha>", a hyphen followed by
/// seven or more lowercase hex characters running to the token end, so a git-sha-suffixed token like
/// reth-c5dff62 yields name reth and version c5dff62. A token matching neither rule yields the whole
/// token as the name and no version.
fn split_name_version(token: &str) -> (String, Option<String>) {
    let at = token
        .match_indices("-v")
        .filter(|(at, _)| token[at + 2..].starts_with(|c: char| c.is_ascii_digit()))
        .last()
        .map(|(at, _)| at)
        .or_else(|| short_sha_split(token));
    match at {
        Some(at) => (token[..at].to_string(), Some(token[at + 1..].to_string())),
        None => (token.to_string(), None),
    }
}

/// The byte index of the hyphen before a trailing short git sha, a suffix of seven or more lowercase
/// hex characters running to the token end. None when the last segment is not such a sha.
fn short_sha_split(token: &str) -> Option<usize> {
    let at = token.rfind('-')?;
    let suffix = &token[at + 1..];
    (suffix.len() >= 7
        && suffix
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(at)
}

/// Loads every metric block under a metrics directory plus the guest and version metadata.
pub fn load_metrics(
    metrics_dir: &Path,
) -> crate::parse_benchmark::Result<(Vec<MetricBlock>, MetricsMeta)> {
    let mut files = Vec::new();
    collect_json(metrics_dir, &mut files)?;
    files.sort();

    let mut blocks = Vec::new();
    let mut guest_dir = None;
    let mut version_dir = None;
    for path in &files {
        // Metric files live at <guest>/<version>/<file>.json relative to the metrics dir.
        let rel = path.strip_prefix(metrics_dir).unwrap_or(path);
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if parts.len() < 3 {
            continue;
        }
        if guest_dir.is_none() {
            guest_dir = Some(parts[0].clone());
        }
        if version_dir.is_none() {
            version_dir = Some(parts[1].clone());
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let text = read_to_string_at(path)?;
        blocks.push(parse_metric_json(stem, &text)?);
    }
    blocks.sort_by_key(|b| b.timestamp_completed.map(Ts::epoch_ms).unwrap_or(i64::MIN));
    Ok((blocks, MetricsMeta::from_dirs(guest_dir, version_dir)))
}

/// Collects every *.json path beneath a directory, recursively.
fn collect_json(dir: &Path, out: &mut Vec<PathBuf>) -> crate::parse_benchmark::Result<()> {
    let entries = read_dir_at(dir)?;
    for entry in entries {
        let entry = entry.map_err(io_at(dir))?;
        let path = entry.path();
        if path.is_dir() {
            collect_json(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            out.push(path);
        }
    }
    Ok(())
}

/// Node hardware specification assumed identical across nodes.
#[derive(Clone)]
pub struct Hardware {
    pub cpu_model: Option<String>,
    pub total_ram_gib: Option<u64>,
    pub gpu_models: Vec<String>,
}

/// Raw on-disk shape of hardware.json.
#[derive(serde::Deserialize)]
struct RawHardware {
    cpu_model: Option<String>,
    total_ram_gib: Option<u64>,
    #[serde(default)]
    gpus: Vec<RawGpu>,
}

/// One GPU entry inside hardware.json.
#[derive(serde::Deserialize)]
struct RawGpu {
    model: String,
}

/// Parses a hardware.json document into the node hardware spec.
pub fn parse_hardware_json(text: &str) -> crate::parse_benchmark::Result<Hardware> {
    let raw: RawHardware =
        serde_json::from_str(text).map_err(json_at(PathBuf::from("hardware.json")))?;
    Ok(Hardware {
        cpu_model: raw.cpu_model,
        total_ram_gib: raw.total_ram_gib,
        gpu_models: raw.gpus.into_iter().map(|g| g.model).collect(),
    })
}

/// Loads hardware.json from a metrics directory, returning an empty spec when absent.
pub fn load_hardware(metrics_dir: &Path) -> crate::parse_benchmark::Result<Hardware> {
    let path = metrics_dir.join("hardware.json");
    if !path.is_file() {
        return Ok(Hardware {
            cpu_model: None,
            total_ram_gib: None,
            gpu_models: Vec::new(),
        });
    }
    let text = read_to_string_at(&path)?;
    parse_hardware_json(&text)
}

#[cfg(test)]
mod tests {
    use crate::parse_benchmark::input::metrics::{
        MetricStatus, MetricsMeta, parse_hardware_json, parse_metric_json, split_name_version,
    };

    const HARDWARE_SAMPLE: &str = r#"{
      "cpu_model": "AMD Ryzen Threadripper PRO 9975WX 32-Cores",
      "total_ram_gib": 125,
      "gpus": [
        { "model": "NVIDIA GeForce RTX 5090" },
        { "model": "NVIDIA GeForce RTX 5090" },
        { "model": "NVIDIA GeForce RTX 5090" },
        { "model": "NVIDIA GeForce RTX 5090" }
      ]
    }"#;

    #[test]
    fn parses_cpu_ram_and_gpu_models() {
        let hw = parse_hardware_json(HARDWARE_SAMPLE).unwrap();
        assert_eq!(
            hw.cpu_model.as_deref(),
            Some("AMD Ryzen Threadripper PRO 9975WX 32-Cores")
        );
        assert_eq!(hw.total_ram_gib, Some(125));
        assert_eq!(hw.gpu_models.len(), 4);
        assert_eq!(hw.gpu_models[0], "NVIDIA GeForce RTX 5090");
    }

    #[test]
    fn tolerates_missing_fields() {
        let hw = parse_hardware_json("{}").unwrap();
        assert_eq!(hw.cpu_model, None);
        assert_eq!(hw.total_ram_gib, None);
        assert!(hw.gpu_models.is_empty());
    }

    #[test]
    fn splits_name_from_trailing_version() {
        assert_eq!(
            split_name_version("zisk-eth-client-reth-v0.9.0"),
            (
                "zisk-eth-client-reth".to_string(),
                Some("v0.9.0".to_string())
            )
        );
        assert_eq!(
            split_name_version("zisk-v0.18.0"),
            ("zisk".to_string(), Some("v0.18.0".to_string()))
        );
    }

    #[test]
    fn split_keeps_whole_token_without_version_suffix() {
        assert_eq!(
            split_name_version("plain-guest"),
            ("plain-guest".to_string(), None)
        );
    }

    #[test]
    fn splits_name_from_trailing_short_sha_when_no_version() {
        assert_eq!(
            split_name_version("reth-c5dff62"),
            ("reth".to_string(), Some("c5dff62".to_string()))
        );
        assert_eq!(
            split_name_version("ethrex-e8860d2"),
            ("ethrex".to_string(), Some("e8860d2".to_string()))
        );
        assert_eq!(
            split_name_version("openvm-8f86342"),
            ("openvm".to_string(), Some("8f86342".to_string()))
        );
        // A token with neither a version nor a short-sha suffix keeps the whole token.
        assert_eq!(split_name_version("fixture"), ("fixture".to_string(), None));
        // A trailing segment shorter than seven hex characters is not a short sha, so no split.
        assert_eq!(
            split_name_version("reth-abc"),
            ("reth-abc".to_string(), None)
        );
    }

    #[test]
    fn meta_from_dirs_splits_guest_and_version() {
        let meta = MetricsMeta::from_dirs(
            Some("zisk-eth-client-reth-v0.9.0".to_string()),
            Some("zisk-v0.18.0".to_string()),
        );
        assert_eq!(meta.guest.as_deref(), Some("zisk-eth-client-reth"));
        assert_eq!(meta.guest_version.as_deref(), Some("v0.9.0"));
        assert_eq!(meta.version.as_deref(), Some("v0.18.0"));
    }

    const SAMPLE: &str = r#"{
      "name": "rpc_block_25192300",
      "timestamp_completed": "2026-05-29T09:53:16.702288335Z",
      "metadata": { "block_used_gas": 29758135 },
      "proving": { "success": {
        "output_matched": true,
        "proof_size": 261313,
        "proving_time_ms": 7342,
        "verification_time_ms": 5
      } }
    }"#;

    #[test]
    fn parses_success_fields() {
        let m = parse_metric_json("rpc_block_25192300", SAMPLE).unwrap();
        assert_eq!(m.name, "rpc_block_25192300");
        assert_eq!(m.status, MetricStatus::Success);
        assert_eq!(m.block_used_gas, Some(29758135));
        assert_eq!(m.proving_time_ms, Some(7342));
        assert_eq!(m.proof_size, Some(261313));
        assert_eq!(m.verification_time_ms, Some(5));
        assert!(m.timestamp_completed.is_some());
        assert!(m.crashed_nodes.is_empty());
        assert_eq!(m.crashed_job, None);
    }

    #[test]
    fn prefers_the_original_test_name_from_metadata() {
        // The metadata's original test name is the block id when present, so the verbose archive
        // name field is overridden.
        let text = r#"{
          "name": "eest__mainnet_25580396__block0",
          "timestamp_completed": "2026-07-22T09:24:05.022795163Z",
          "metadata": { "block_used_gas": 12345, "original_test_name": "mainnet_25580396" },
          "proving": { "success": { "proof_size": 268071, "proving_time_ms": 903, "verification_time_ms": 13 } }
        }"#;
        let m = parse_metric_json("eest__mainnet_25580396__block0", text).unwrap();
        assert_eq!(m.name, "mainnet_25580396");
        assert_eq!(m.block_used_gas, Some(12345));
    }

    #[test]
    fn keeps_a_long_fixture_name_verbatim() {
        let text = r#"{
          "name": "test_account_query.py::test_codecopy_benchmark[fork_Osaka-blockchain_test-code_size_0-mem_size_0-benchmark-gas-value_60M]",
          "timestamp_completed": "2026-06-02T06:33:26.997238738Z",
          "metadata": { "block_used_gas": 60000000 },
          "proving": { "success": { "proof_size": 261313, "proving_time_ms": 11669, "verification_time_ms": 5 } }
        }"#;
        let m = parse_metric_json("ignored-stem", text).unwrap();
        assert!(
            m.name
                .starts_with("test_account_query.py::test_codecopy_benchmark")
        );
        assert_eq!(m.status, MetricStatus::Success);
    }

    #[test]
    fn classifies_a_connection_drop_crash_and_blames_its_node() {
        let text = r#"{
          "name": "test_arithmetic.py::test_mod[fork_Osaka]",
          "timestamp_completed": "2026-06-02T07:02:15.554527895Z",
          "proving": { "crashed": { "reason": "Cluster job b051e0b6-fc1b-4bf5-8b54-71f5111886a9 failed: JobStatusFailed { failure: Some(JobFailure { kind: Some(Execution(JobFailureExecution { reason: \"Worker WorkerId(node1) connection dropped\" })) }) }" } }
        }"#;
        let m = parse_metric_json("stem", text).unwrap();
        assert_eq!(m.status, MetricStatus::Crashed);
        assert_eq!(m.crashed_nodes, vec!["node1".to_string()]);
        assert_eq!(
            m.crashed_job.as_deref(),
            Some("b051e0b6-fc1b-4bf5-8b54-71f5111886a9")
        );
        assert_eq!(m.proving_time_ms, None);
        assert_eq!(m.proof_size, None);
    }

    #[test]
    fn classifies_a_timeout_crash_from_the_reason_text() {
        let text = r#"{
          "name": "test_alt_bn128.py::test_alt_bn128[fork_Osaka]",
          "timestamp_completed": "2026-06-02T06:39:52.356076780Z",
          "proving": { "crashed": { "reason": "Cluster job ed343d4c-ded8-40e7-9bb0-385eb5ad0b03 failed: [Monitor] Phase Aggregate timed out for job JobId(ed343d4c) (100s > 100s)" } }
        }"#;
        let m = parse_metric_json("stem", text).unwrap();
        assert_eq!(m.status, MetricStatus::Timeout);
        // The aggregate timeout names no worker, so no node is blamed though the job id is known.
        assert!(m.crashed_nodes.is_empty());
        assert_eq!(
            m.crashed_job.as_deref(),
            Some("ed343d4c-ded8-40e7-9bb0-385eb5ad0b03")
        );
    }

    #[test]
    fn unattributed_grpc_crash_blames_no_node_and_has_no_job() {
        let text = r#"{
          "name": "test_identity.py::test_identity_fixed_size[fork_Osaka]",
          "timestamp_completed": "2026-06-02T08:08:43.153770574Z",
          "proving": { "crashed": { "reason": "Cluster gRPC error: code: 'Internal error', message: \"An internal error occurred\"" } }
        }"#;
        let m = parse_metric_json("stem", text).unwrap();
        assert_eq!(m.status, MetricStatus::Crashed);
        assert!(m.crashed_nodes.is_empty());
        assert_eq!(m.crashed_job, None);
    }

    #[test]
    fn falls_back_to_stem_when_name_absent() {
        let m =
            parse_metric_json("test", r#"{"timestamp_completed":"2026-05-29T09:53:16Z"}"#).unwrap();
        assert_eq!(m.name, "test");
        // A file with no proving section at all is treated as a crash, never a silent success.
        assert_eq!(m.status, MetricStatus::Crashed);
        assert_eq!(m.proving_time_ms, None);
    }
}
