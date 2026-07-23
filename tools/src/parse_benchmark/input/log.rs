//! The cluster-log side of the input stage. A zkVM backend under `zkvm` translates its cluster logs
//! into the [`Log`] model, one parsed log per proving job carrying only fields read from the logs.
//! [`Ts`] wraps jiff as the crate's single timestamp parse and arithmetic point.

pub mod lines;
pub mod zkvm;

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde_json::Value;

/// Wraps a jiff Timestamp so the rest of the crate never depends on the jiff API directly.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ts(Timestamp);

impl Ts {
    /// Parses an RFC-3339 timestamp with a trailing Z and any sub-second precision.
    pub fn parse(value: &str) -> crate::parse_benchmark::Result<Ts> {
        value.parse::<Timestamp>().map(Ts).map_err(|e| {
            crate::parse_benchmark::ParseError::Timestamp {
                value: value.to_string(),
                reason: e.to_string(),
            }
        })
    }

    /// Builds a timestamp from a dmon columnar Date YYYYMMDD and Time HH:MM:SS, treated as UTC.
    pub fn from_dmon(date: &str, time: &str) -> crate::parse_benchmark::Result<Ts> {
        if date.len() < 8 {
            return Err(crate::parse_benchmark::ParseError::Timestamp {
                value: format!("{date} {time}"),
                reason: "dmon date is shorter than YYYYMMDD".to_string(),
            });
        }
        let rfc = format!("{}-{}-{}T{}Z", &date[0..4], &date[4..6], &date[6..8], time);
        Ts::parse(&rfc)
    }

    /// Returns the integer milliseconds since the unix epoch, truncating sub-millisecond digits.
    pub fn epoch_ms(self) -> i64 {
        self.0.as_millisecond()
    }

    /// Returns the integer microseconds since the unix epoch, truncating sub-microsecond digits.
    /// The log offsets are stored at this precision so lines within the same millisecond keep
    /// their order.
    pub fn epoch_us(self) -> i64 {
        self.0.as_microsecond()
    }
}

/// Terminal state a backend reported for a proving job, read from its log.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogStatus {
    Success,
    Failed,
    Timeout,
    Unknown,
}

/// One proving job's parsed log, holding only fields read from the cluster logs. The meta map
/// carries backend-specific per-block scalars keyed by their output field name.
#[derive(Clone)]
pub struct Log {
    pub id: String,
    pub status: LogStatus,
    pub t_start: Option<Ts>,
    pub t_end: Option<Ts>,
    pub duration_s: Option<f64>,
    pub meta: BTreeMap<String, Value>,
    pub nodes: Vec<LogNode>,
    /// Node ids that took part in this job, the union of every source that names a worker. A
    /// hardware node absent from this list did not work the job, so the proof ran on fewer than
    /// the full cluster.
    pub participants: Vec<String>,
}

impl Log {
    /// Creates an empty log record for the given job id.
    pub fn new(id: impl Into<String>) -> Log {
        Log {
            id: id.into(),
            status: LogStatus::Unknown,
            t_start: None,
            t_end: None,
            duration_s: None,
            meta: BTreeMap::new(),
            nodes: Vec::new(),
            participants: Vec::new(),
        }
    }
}

/// How a node's work on a job ended before completion. A crashed node is the one the cluster lost,
/// and a cancelled node is one the cluster stopped after a sibling crashed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeEndKind {
    Crashed,
    Cancelled,
}

impl NodeEndKind {
    /// The wire string emitted for the node's crash marker.
    pub fn as_str(self) -> &'static str {
        match self {
            NodeEndKind::Crashed => "crashed",
            NodeEndKind::Cancelled => "cancelled",
        }
    }
}

/// The moment and manner a node stopped working on a job, in absolute epoch milliseconds.
#[derive(Clone, Copy)]
pub struct NodeEnd {
    pub at_ms: i64,
    pub kind: NodeEndKind,
}

/// One worker's contribution to a job, its phase windows aligned to the preset order.
///
/// Entry i is the absolute (start, end) epoch-ms window for preset phase i, or None where the
/// worker skipped it. The aggregator carries the cluster aggregate window in its final slot, which
/// is how the assembler identifies it. On an incomplete job `end` marks where this node crashed or
/// was cancelled, against which its in-progress phase is clipped.
#[derive(Clone)]
pub struct LogNode {
    pub id: String,
    pub phases: Vec<Option<(i64, i64)>>,
    /// The node's fine pipeline items in template kind terms, empty when the backend extracts none.
    pub items: Vec<PipelineItem>,
    pub end: Option<NodeEnd>,
}

/// One phase in a zkVM proving pipeline, used as an ordered preset.
#[derive(Clone)]
pub struct PhaseDef {
    pub name: String,
    pub label: String,
    /// An overlap phase runs concurrently with its neighbors.
    pub overlap: bool,
}

/// One fine-pipeline item kind in a zkVM's ordered template, owned by the named preset phase. A
/// paired kind carries a witness segment and a compute segment in one item.
#[derive(Clone)]
pub struct PipelineDef {
    pub name: String,
    pub label: String,
    pub phase: String,
    pub paired: bool,
}

/// One fine pipeline item on a node, in absolute epoch milliseconds. The first segment is the
/// witness of a paired kind or the sole segment of an unpaired one, and the second is the compute
/// half of a pair. A None end marks a segment a crash left open.
#[derive(Clone, Copy)]
pub struct PipelineItem {
    pub kind: usize,
    pub first: (i64, Option<i64>),
    pub second: Option<(i64, Option<i64>)>,
    pub meta: PipelineItemMeta,
}

/// Optional per-item metadata a backend reads from its logs, serialized as the wire row's trailing
/// object and wholly omitted when every field is absent. The id numbers the item within its kind,
/// and the heavy markers name the segment index of each side of a paired item, first segment zero.
#[derive(Clone, Copy, Default, PartialEq)]
pub struct PipelineItemMeta {
    pub id: Option<i64>,
    pub cpu_heavy: Option<usize>,
    pub gpu_heavy: Option<usize>,
}

/// The leading hex run of a token, the canonical key the coordinator's truncated job id, a crash
/// reason's full uuid, and the worker logs all share.
pub fn job_prefix(token: &str) -> String {
    token
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::parse_benchmark::input::log::{Ts, job_prefix};

    #[test]
    fn prefix_is_the_leading_hex_run() {
        assert_eq!(job_prefix("efd42874-b32d-4539"), "efd42874");
        assert_eq!(job_prefix("efd42874\u{2026}"), "efd42874");
    }

    #[test]
    fn epoch_ms_difference_is_exact() {
        let base = Ts::parse("2026-05-29T10:00:00.000Z").unwrap();
        let later = Ts::parse("2026-05-29T10:00:05.500Z").unwrap();
        assert_eq!(later.epoch_ms() - base.epoch_ms(), 5500);
    }

    #[test]
    fn microsecond_and_nanosecond_precision_truncate_to_ms() {
        let a = Ts::parse("2026-05-29T09:53:16.678680Z").unwrap();
        let b = Ts::parse("2026-05-29T09:53:16.702288335Z").unwrap();
        assert_eq!(b.epoch_ms() - a.epoch_ms(), 24);
    }

    #[test]
    fn from_dmon_matches_equivalent_rfc3339() {
        let from_dmon = Ts::from_dmon("20260529", "09:51:25").unwrap();
        let reference = Ts::parse("2026-05-29T09:51:25Z").unwrap();
        assert_eq!(from_dmon.epoch_ms(), reference.epoch_ms());
    }

    #[test]
    fn rejects_garbage() {
        assert!(Ts::parse("not a timestamp").is_err());
    }
}
