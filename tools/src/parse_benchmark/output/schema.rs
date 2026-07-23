//! Output schema structs serialized into benchmark.json.
//!
//! The benchmark holds the cluster identity once and a list of runs, each carrying its own
//! statistics, blocks, and telemetry, so repeated runs accumulate in one document. Time is integer
//! milliseconds offset from the run epoch, and telemetry is columnar on an implicit one-second
//! axis.
//!
//! The structs round-trip through JSON so a patch can read, append a run, and write back, which is
//! why every struct derives Deserialize and the cluster-identity subtree also derives PartialEq for
//! the patch's same-cluster guard. Serde field order is wire order.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The complete benchmark document written to benchmark.json. `id` is the identity shared by every
/// run, derived from the run directory basename, while `name` and `description` are the
/// human-facing identity read from the run directory's input benchmark.json. `runs` holds one entry
/// per execution. Hardware and software are held once because a patch only appends a run when they,
/// and the name, match the existing document.
#[derive(Serialize, Deserialize)]
pub struct Benchmark {
    pub schema_version: u32,
    pub hardware: Hardware,
    pub software: Software,
    pub id: String,
    pub name: String,
    pub description: String,
    pub runs: Vec<Run>,
}

/// Node hardware assumed identical across the cluster, with the node ids in order.
#[derive(Serialize, Deserialize, PartialEq)]
pub struct Hardware {
    pub cpu_model: Option<String>,
    pub ram_gib: Option<u64>,
    pub gpu_models: Vec<String>,
    pub nodes: Vec<String>,
}

/// The proving software identity, the zkVM and the guest program.
#[derive(Serialize, Deserialize, PartialEq)]
pub struct Software {
    pub zkvm: Zkvm,
    pub guest: Guest,
}

/// The zkVM that produced the proofs, carrying its ordered phase preset.
#[derive(Serialize, Deserialize, PartialEq)]
pub struct Zkvm {
    pub name: String,
    pub version: String,
    pub phases: Vec<Phase>,
    /// The ordered fine-pipeline item kinds, referenced positionally by each block node's pipeline
    /// rows. Empty when the backend declares none, and omitted from the wire so older documents
    /// round-trip unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pipeline: Vec<PipelineStep>,
}

/// The guest program that was proven.
#[derive(Serialize, Deserialize, PartialEq)]
pub struct Guest {
    pub name: String,
    pub version: String,
}

/// One phase of the proving pipeline, rendered in array order and colored by position.
#[derive(Serialize, Deserialize, PartialEq)]
pub struct Phase {
    pub name: String,
    pub label: String,
    /// An overlap phase runs concurrently with its neighbors. Omitted from the wire when false so
    /// older documents round-trip unchanged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub overlap: bool,
}

/// One fine-pipeline item kind. A paired kind may carry a witness segment and a compute segment in
/// one row, while an unpaired kind is a single segment.
#[derive(Serialize, Deserialize, PartialEq)]
pub struct PipelineStep {
    pub name: String,
    pub label: String,
    /// The owning phase name from the zkVM phase preset, the row's color source.
    pub phase: String,
    pub paired: bool,
}

/// One execution of the benchmark with its own statistics, blocks, and telemetry. The block,
/// success, and failure counts let the consumer skip recomputation.
#[derive(Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    /// First job start as unix epoch milliseconds.
    pub started_at: i64,
    pub block_count: usize,
    pub success_count: usize,
    pub failure_count: usize,
    pub statistics: Statistics,
    pub blocks: Vec<Block>,
    pub telemetry: Telemetry,
}

/// Cluster proving statistics and the per-node GPU rollups indexed to hardware.nodes.
#[derive(Serialize, Deserialize)]
pub struct Statistics {
    pub mean_proving_ms: Option<i64>,
    pub p50_proving_ms: Option<i64>,
    pub p90_proving_ms: Option<i64>,
    pub p95_proving_ms: Option<i64>,
    pub p99_proving_ms: Option<i64>,
    pub mean_gas_per_s: Option<i64>,
    pub nodes: Vec<NodeStats>,
}

/// Proving-window GPU rollup for one node, positioned to match hardware.nodes, null where absent.
/// It aggregates only telemetry sampled inside a clean block's proving window, so idle time and
/// degraded or aborted jobs do not pull it off the normal proving load.
#[derive(Serialize, Deserialize)]
pub struct NodeStats {
    pub max_temp: Option<f64>,
    pub temp_throttle_seconds: f64,
    pub mean_sm: Option<f64>,
    pub mean_mem: Option<f64>,
    pub peak_rxpci: Option<f64>,
    pub peak_txpci: Option<f64>,
}

/// A phase window relative to its block start, expressed as offset and duration.
#[derive(Serialize, Deserialize, Clone)]
pub struct PhaseWindow {
    pub start_ms: i64,
    pub dur_ms: i64,
}

/// One node's contribution to a block, positioned to match hardware.nodes. Phase windows align to
/// the preset order, and the final aggregate window is non-null only on the aggregator, which is
/// how it is identified. On a crashed block `crashed_ms` is the block-start offset at which this
/// node was blamed, with later phases null, and it is null on every node of a clean block.
#[derive(Serialize, Deserialize)]
pub struct BlockNode {
    pub phases: Vec<Option<PhaseWindow>>,
    /// Fine pipeline items of this node as variable-arity integer rows [kind, s, d, s2, d2] with
    /// trailing fields absent, times in milliseconds from the block start. Arity 3 is one complete
    /// segment, arity 5 a witness and compute pair, and arities 2 and 4 leave the last segment
    /// dangling where a crash cut it off. A paired kind whose witness completed with no compute
    /// left to claim it, because none opened before a crash or its compute closed before the
    /// witness did, also serializes as arity 3, indistinguishable from a compute-only row. An item
    /// with metadata appends it after the integers as one trailing object of the optional keys id,
    /// the item number within its kind, and cpu_heavy and gpu_heavy, the segment index of each
    /// side of a pair, while an item without metadata stays a pure integer row. Rows sort by start
    /// then kind, and the field is omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pipeline: Vec<Vec<Value>>,
    pub crashed_ms: Option<i64>,
    /// How this node ended on a crashed block, "crashed" for the lost node or "cancelled" for a
    /// sibling-stopped one. Null when the node has no crash marker.
    pub crash_kind: Option<String>,
    /// Whether this node took part in the block. False marks a node the proof ran without, so its
    /// figures are not comparable to a full-cluster run.
    pub participated: bool,
}

/// A single proven block, emitted in completion order. `name` is its metric file name, unique
/// within the run and the identifier the views key on.
#[derive(Serialize, Deserialize)]
pub struct Block {
    pub name: String,
    pub status: String,
    /// Block start offset from the run epoch in milliseconds.
    pub start_ms: i64,
    pub gas_used: Option<u64>,
    /// Authoritative wall-clock proving time from the metric file, null unless the proof
    /// succeeded.
    pub proving_ms: Option<i64>,
    pub proof_size: Option<u64>,
    pub verification_time_ms: Option<u64>,
    /// zkVM-specific per-block scalars keyed by field name, such as input_size and steps for zisk.
    pub meta: BTreeMap<String, Value>,
    pub nodes: Vec<BlockNode>,
    /// The coordinator and worker log lines within this block's proving window, in time order,
    /// every level including DEBUG and TRACE. Skipped from benchmark.json and written to a sibling
    /// per-block log file instead.
    #[serde(skip)]
    pub logs: Vec<LogEntry>,
}

/// One cluster-log line attached to a block, the role that emitted it, its offset from the block
/// start in microseconds, its lowercase level, and the message body. The microsecond precision
/// keeps lines that share a millisecond in order, while the frontend renders the offset down to
/// milliseconds. Serialized into the per-block log file, not benchmark.json.
#[derive(Serialize, Deserialize)]
pub struct LogEntry {
    pub role: String,
    pub time: i64,
    pub level: String,
    pub msg: String,
}

/// Display metadata for one telemetry metric, driving frontend panels.
#[derive(Serialize, Deserialize)]
pub struct Metric {
    pub name: String,
    pub label: String,
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

/// One node's telemetry as per-metric [gpu][tick] arrays on the shared one-second axis. Tick i is i
/// seconds after the run epoch, the same axis for every node, so nodes align by index.
#[derive(Serialize, Deserialize)]
pub struct NodeTelemetry {
    /// Metric name to a [gpu][tick] grid. A cell is null where a gpu lacked a reading or the node
    /// did not sample that second. Empty metrics are omitted.
    pub metrics: BTreeMap<String, Vec<Vec<Value>>>,
}

/// All node telemetry plus the catalog of metrics present in the run.
#[derive(Serialize, Deserialize)]
pub struct Telemetry {
    pub metrics: Vec<Metric>,
    pub nodes: Vec<NodeTelemetry>,
}

#[cfg(test)]
mod tests {
    use crate::parse_benchmark::output::schema::{BlockNode, Phase, Zkvm};

    #[test]
    fn a_phase_serializes_overlap_only_when_true() {
        let wire = r#"{"name":"input","label":"Input Transfer"}"#;
        let phase: Phase = serde_json::from_str(wire).unwrap();
        assert!(!phase.overlap);
        assert_eq!(serde_json::to_string(&phase).unwrap(), wire);

        let overlapping = Phase {
            name: "segment".to_string(),
            label: "Segment".to_string(),
            overlap: true,
        };
        assert_eq!(
            serde_json::to_string(&overlapping).unwrap(),
            r#"{"name":"segment","label":"Segment","overlap":true}"#
        );
    }

    #[test]
    fn a_zkvm_without_a_pipeline_template_round_trips_unchanged() {
        let wire = r#"{"name":"zisk","version":"v1","phases":[]}"#;
        let zkvm: Zkvm = serde_json::from_str(wire).unwrap();
        assert!(zkvm.pipeline.is_empty());
        assert_eq!(serde_json::to_string(&zkvm).unwrap(), wire);
    }

    #[test]
    fn a_node_without_pipeline_rows_round_trips_unchanged() {
        let wire = r#"{"phases":[null],"crashed_ms":null,"crash_kind":null,"participated":true}"#;
        let node: BlockNode = serde_json::from_str(wire).unwrap();
        assert!(node.pipeline.is_empty());
        assert_eq!(serde_json::to_string(&node).unwrap(), wire);
    }

    #[test]
    fn a_pipeline_row_round_trips_its_trailing_metadata_object() {
        // A metadata-free row stays pure integers while a trailing object rides its row untouched.
        let wire = r#"{"phases":[null],"pipeline":[[0,306,1538],[0,412,1490,{"id":12,"cpu_heavy":0,"gpu_heavy":1}]],"crashed_ms":null,"crash_kind":null,"participated":true}"#;
        let node: BlockNode = serde_json::from_str(wire).unwrap();
        assert_eq!(serde_json::to_string(&node).unwrap(), wire);
    }
}
