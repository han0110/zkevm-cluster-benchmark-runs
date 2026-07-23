//! Parses the edge-manager coordinator.log into first-seen-ordered raw jobs.
//!
//! Every fact arrives on one self-contained line carrying the full proof uuid, so each field is a
//! single pattern with no cross-line state. The capture that produced these logs drops lines under
//! load, therefore a job may miss any subset of its lifecycle and unknown lines are skipped rather
//! than fatal. The guard against silent format drift is per anchor phrase instead of zisk's tag
//! routing, because the manager has no bounded tag namespace. A line carrying a known anchor that
//! fails its full pattern is fatal, so a reworded field forces a parser update rather than quietly
//! zeroing the data.

use std::{collections::HashMap, sync::LazyLock};

use regex::Regex;

use crate::parse_benchmark::input::log::{
    LogStatus, Ts,
    zkvm::{cap, strip_ansi},
};

/// Matches the proof start line, the job's begin marker.
static RE_START: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<ts>\S+)\s+INFO edge_manager::handlers: Starting Edge proof: proof_uuid=(?P<job>[^,]+), program=\S+$",
    )
    .unwrap()
});

/// Matches the fan-out completion line whose elapsed span back-computes the fan-out start, both on
/// the manager clock.
static RE_FANOUT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<ts>\S+)\s+INFO edge_manager::handlers: Input fan-out complete for proof (?P<job>\S+): workers=\d+, bytes=(?P<bytes>\d+), elapsed=(?P<elapsed>\d+)ms$",
    )
    .unwrap()
});

/// Matches the tree-shape line fired once per proof when the segment count is learned.
static RE_TAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<ts>\S+)\s+INFO edge_manager::proof_state::result_handler: trigger_tail_proofs for proof (?P<job>\S+): num_segments=(?P<segments>\d+), leaf_arity=(?P<arity>\d+), internal_arity=\d+, num_leaf_proofs=\d+, num_internal_layers=\d+, effective_final_layer=\d+$",
    )
    .unwrap()
});

/// Matches the per-proof metrics summary whose e2e span is the authoritative duration.
static RE_METRICS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<ts>\S+)\s+INFO edge_manager::proof_state::metrics_report: Emitted metrics for proof (?P<job>\S+): e2e=(?P<e2e>\d+)ms, proving=\d+ms, app=\d+ms, leaf=\d+ms, internal=\d+ms, compress=\d+ms$",
    )
    .unwrap()
});

/// Matches the proof completion line, the job's success marker and end time.
static RE_COMPLETED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<ts>\S+)\s+INFO edge_manager::proof_state::result_handler: Proof (?P<job>\S+) is completed$",
    )
    .unwrap()
});

/// Matches the proof failure line at any level, the job's failure marker and end time.
static RE_FAILED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<ts>\S+)\s+(?:INFO|WARN|ERROR) edge_manager::\S+: Proof (?P<job>\S+) failed at step \S+",
    )
    .unwrap()
});

/// Anchor phrases of the lines the parser extracts. A line carrying one that its full pattern then
/// fails to match is fatal, so a manager format change cannot silently drop data.
const ANCHORS: [&str; 6] = [
    "Starting Edge proof",
    "Input fan-out complete",
    "trigger_tail_proofs for proof",
    "Emitted metrics for proof",
    " is completed",
    " failed at step ",
];

/// An openvm proving job as read from the coordinator log, before translation to the generic
/// model. Window fields are epoch milliseconds because they are derived by subtracting printed
/// spans from line timestamps.
#[derive(Clone)]
pub struct RawJob {
    pub id: String,
    pub status: LogStatus,
    pub t_start: Option<Ts>,
    pub t_end: Option<Ts>,
    /// When the manager began the input fan-out in epoch microseconds, the completion line's
    /// timestamp minus its elapsed span, on the manager clock. Microsecond precision keeps a
    /// sub-millisecond fan-out, whose elapsed prints as zero, from collapsing the input window.
    pub fanout_start_us: Option<i64>,
    pub input_bytes: Option<u64>,
    pub num_segments: Option<u64>,
    /// How many app segments feed one leaf proof, the divisor turning a leaf's start segment into
    /// its leaf index.
    pub leaf_arity: Option<i64>,
    pub duration_s: Option<f64>,
}

impl RawJob {
    /// Creates an empty raw job for the given id.
    fn new(id: &str) -> RawJob {
        RawJob {
            id: id.to_string(),
            status: LogStatus::Unknown,
            t_start: None,
            t_end: None,
            fanout_start_us: None,
            input_bytes: None,
            num_segments: None,
            leaf_arity: None,
            duration_s: None,
        }
    }
}

/// Builds the fatal error for an anchored line that matched no known pattern.
fn unrecognized(line: &str) -> crate::parse_benchmark::ParseError {
    crate::parse_benchmark::ParseError::UnrecognizedCoordinatorLine(line.trim().to_string())
}

/// Parses coordinator log text into first-seen-ordered raw jobs. Unknown lines are skipped because
/// the capture is lossy and the manager's line vocabulary is unbounded, while an anchored line that
/// fails its full pattern is fatal.
pub fn parse(text: &str) -> crate::parse_benchmark::Result<Vec<RawJob>> {
    let clean = strip_ansi(text);
    let mut state = Coordinator::default();
    for raw in clean.lines() {
        state.consume(raw)?;
    }
    Ok(state.finish())
}

/// Accumulates jobs in first-seen order while parsing the coordinator log line by line.
#[derive(Default)]
struct Coordinator {
    order: Vec<String>,
    map: HashMap<String, RawJob>,
}

impl Coordinator {
    /// Routes one log line through the known patterns, skipping unknown lines and failing on an
    /// anchored line no pattern covers.
    fn consume(&mut self, raw: &str) -> crate::parse_benchmark::Result<()> {
        if let Some(c) = RE_START.captures(raw) {
            self.ensure(cap(&c, "job")).t_start = Some(Ts::parse(cap(&c, "ts"))?);
        } else if let Some(c) = RE_FANOUT.captures(raw) {
            let ts = Ts::parse(cap(&c, "ts"))?;
            let elapsed: i64 = cap(&c, "elapsed").parse().unwrap_or(0);
            let j = self.ensure(cap(&c, "job"));
            j.fanout_start_us = Some(ts.epoch_us() - elapsed * 1000);
            j.input_bytes = cap(&c, "bytes").parse().ok();
        } else if let Some(c) = RE_TAIL.captures(raw) {
            let j = self.ensure(cap(&c, "job"));
            j.num_segments = cap(&c, "segments").parse().ok();
            j.leaf_arity = cap(&c, "arity").parse().ok();
        } else if let Some(c) = RE_METRICS.captures(raw) {
            let e2e: Option<f64> = cap(&c, "e2e").parse().ok();
            self.ensure(cap(&c, "job")).duration_s = e2e.map(|ms| ms / 1000.0);
        } else if let Some(c) = RE_COMPLETED.captures(raw) {
            let j = self.ensure(cap(&c, "job"));
            j.status = LogStatus::Success;
            j.t_end = Some(Ts::parse(cap(&c, "ts"))?);
        } else if let Some(c) = RE_FAILED.captures(raw) {
            let ts = Ts::parse(cap(&c, "ts"))?;
            let j = self.ensure(cap(&c, "job"));
            // A failure line never demotes a completed proof.
            if j.status != LogStatus::Success {
                j.status = LogStatus::Failed;
                j.t_end = Some(ts);
            }
        } else if ANCHORS.iter().any(|a| raw.contains(a)) {
            return Err(unrecognized(raw));
        }
        Ok(())
    }

    /// Inserts a job into the order on first sight, returning a mutable handle to it.
    fn ensure(&mut self, jid: &str) -> &mut RawJob {
        if !self.map.contains_key(jid) {
            self.order.push(jid.to_string());
            self.map.insert(jid.to_string(), RawJob::new(jid));
        }
        self.map.get_mut(jid).unwrap()
    }

    /// Returns the jobs in first-seen order.
    fn finish(mut self) -> Vec<RawJob> {
        self.order
            .into_iter()
            .filter_map(|id| self.map.remove(&id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::parse_benchmark::{
        ParseError,
        input::log::{LogStatus, Ts, zkvm::openvm::coordinator::parse},
    };

    // Verbatim lines from a real run, one complete proof lifecycle with the high-volume scheduler
    // and batching lines the parser skips.
    const NAMED: &str = "\
2026-07-21T13:55:07.562820Z  INFO edge_manager::handlers: Staged input for proof ere-6f8d9d31213b4238b586606bc0e6bb19 (main=true, deferral_states=0, deferral_inputs=0)
2026-07-21T13:55:07.563047Z  INFO edge_manager::handlers: Starting Edge proof: proof_uuid=ere-6f8d9d31213b4238b586606bc0e6bb19, program=ere-d11a3ab751015b5e@v2290884206
2026-07-21T13:56:21.631436Z  INFO edge_manager::scheduler: init_proof ere-6f8d9d31213b4238b586606bc0e6bb19: 16 workers registered (16 app-eligible / normal-set)
2026-07-21T13:56:21.675074Z  INFO edge_manager::handlers: Successfully uploaded input to worker 0 for proof ere-6f8d9d31213b4238b586606bc0e6bb19
2026-07-21T13:56:21.705734Z  INFO edge_manager::handlers: Input fan-out complete for proof ere-6f8d9d31213b4238b586606bc0e6bb19: workers=16, bytes=6711680, elapsed=73ms
2026-07-21T13:56:21.706118Z  INFO edge_manager::handlers: Work sent to worker 8 for proof ere-6f8d9d31213b4238b586606bc0e6bb19: prover_id=8, num_provers=16
2026-07-21T13:56:22.809842Z  INFO edge_manager::handlers: Received result from worker 0: proof_uuid=ere-6f8d9d31213b4238b586606bc0e6bb19, kind=app
2026-07-21T13:56:22.809826Z  INFO edge_manager::proof_state::result_handler: Batch [0-3] not yet complete, missing segment 1
2026-07-21T13:56:23.676185Z  INFO edge_manager::proof_state::result_handler: Execute e2 done, cost: 0
2026-07-21T13:56:23.676207Z  INFO edge_manager::proof_state::result_handler: trigger_tail_proofs for proof ere-6f8d9d31213b4238b586606bc0e6bb19: num_segments=52, leaf_arity=4, internal_arity=3, num_leaf_proofs=13, num_internal_layers=3, effective_final_layer=2
2026-07-21T13:56:23.272492Z  INFO edge_manager::proof_state::result_handler: Batch [0-3] complete! Creating leaf proof request with 4 app proofs
2026-07-21T14:00:04.356336Z  INFO edge_manager::handlers: Sending leaf_prove work to worker 0 for proof ere-6f8d9d31213b4238b586606bc0e6bb19: segments=[20, 23], children=4, queue_wait=2ms
2026-07-21T13:56:26.354010Z  INFO edge_manager::handlers: Sending internal_prove work to worker 0 for proof ere-6f8d9d31213b4238b586606bc0e6bb19: layer=2, segments=[0, 51], children=2, is_final=true, queue_wait=1ms
2026-07-21T13:56:26.514833Z  INFO edge_manager::proof_state::result_handler: Proof ere-6f8d9d31213b4238b586606bc0e6bb19 is completed
2026-07-21T13:56:26.520021Z  INFO edge_manager::handlers: Persisted final proof ere-6f8d9d31213b4238b586606bc0e6bb19 to /var/tmp/openvm-final-proofs/ere-6f8d9d31213b4238b586606bc0e6bb19.proof.bin
2026-07-21T13:56:26.521188Z  INFO edge_manager::proof_state::metrics_report: Emitted metrics for proof ere-6f8d9d31213b4238b586606bc0e6bb19: e2e=4883ms, proving=4809ms, app=59885ms, leaf=3053ms, internal=1095ms, compress=69ms
2026-07-21T13:56:26.521449Z  INFO edge_manager::handlers: Proof ere-6f8d9d31213b4238b586606bc0e6bb19 reached terminal state Completed, cleaning up scheduler state
";

    #[test]
    fn parses_every_extracted_field_of_a_named_job() {
        let mut jobs = parse(NAMED).expect("parse should succeed");
        assert_eq!(jobs.len(), 1);
        let j = jobs.remove(0);
        assert_eq!(j.id, "ere-6f8d9d31213b4238b586606bc0e6bb19");
        assert_eq!(j.status, LogStatus::Success);
        assert_eq!(j.input_bytes, Some(6711680));
        assert_eq!(j.num_segments, Some(52));
        assert_eq!(j.leaf_arity, Some(4));
        assert_eq!(j.duration_s, Some(4.883));
        assert_eq!(
            j.t_start.map(Ts::epoch_ms),
            Some(Ts::parse("2026-07-21T13:55:07.563047Z").unwrap().epoch_ms())
        );
        assert_eq!(
            j.t_end.map(Ts::epoch_ms),
            Some(Ts::parse("2026-07-21T13:56:26.514833Z").unwrap().epoch_ms())
        );
        // The fan-out start is the completion timestamp minus the printed elapsed span, in
        // microseconds.
        let fanout_done = Ts::parse("2026-07-21T13:56:21.705734Z").unwrap().epoch_us();
        assert_eq!(j.fanout_start_us, Some(fanout_done - 73 * 1000));
    }

    #[test]
    fn a_job_missing_its_start_line_still_parses() {
        // The lossy capture can drop any line, so a completion without a start yields a job whose
        // t_start stays unset.
        let text = "\
2026-07-21T13:56:26.514833Z  INFO edge_manager::proof_state::result_handler: Proof ere-aaaabbbbccccddddeeeeffff00001111 is completed
";
        let jobs = parse(text).expect("parse should succeed");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, LogStatus::Success);
        assert!(jobs[0].t_start.is_none() && jobs[0].t_end.is_some());
    }

    #[test]
    fn a_failure_line_marks_the_job_failed() {
        let text = "\
2026-07-21T13:55:07.563047Z  INFO edge_manager::handlers: Starting Edge proof: proof_uuid=ere-aaaabbbbccccddddeeeeffff00001111, program=ere-d11a3ab751015b5e@v2290884206
2026-07-21T13:57:00.000000Z ERROR edge_manager::proof_state::result_handler: Proof ere-aaaabbbbccccddddeeeeffff00001111 failed at step leaf_prove: worker 3 returned 500
";
        let jobs = parse(text).expect("parse should succeed");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, LogStatus::Failed);
        assert!(jobs[0].t_end.is_some());
    }

    #[test]
    fn an_anchored_line_that_fails_its_pattern_is_fatal() {
        // A reworded field on a known line must fail the parse rather than silently zero the data.
        let text = "2026-07-21T13:56:21.705734Z  INFO edge_manager::handlers: Input fan-out complete for proof ere-6f8d: workers=16, total_bytes=6711680, elapsed=73ms";
        let err = parse(text).err().expect("the reworded line must fail");
        assert!(
            matches!(&err, ParseError::UnrecognizedCoordinatorLine(line) if line.contains("total_bytes")),
            "expected UnrecognizedCoordinatorLine, got {err:?}"
        );
    }

    #[test]
    fn unknown_lines_are_skipped() {
        // Boot, registration, eviction, and scheduler lines carry no anchor and produce no job.
        let text = "\
2026-07-21T13:54:44.352947Z  INFO edge_manager: Starting Edge Manager on 0.0.0.0:3000
2026-07-21T13:55:05.923645Z  INFO edge_manager::handlers: Registering ere-d11a3ab751015b5e@v2290884206 with 16 worker(s) (3592648 ELF bytes)
2026-07-21T15:10:56.535332Z  INFO edge_manager::handlers: Evicting stale proof state: ere-3ef06174f66f4470aeba5ca75b768fdf
2026-07-21T13:56:21.631436Z  INFO edge_manager::scheduler: set_num_segments for ere-6f8d9d31213b4238b586606bc0e6bb19: num_segments=52, num_workers=16
";
        let jobs = parse(text).expect("parse should succeed");
        assert!(jobs.is_empty(), "unknown lines must not create a job");
    }
}
