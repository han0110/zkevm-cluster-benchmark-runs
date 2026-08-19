//! Parses the zisk coordinator.log into first-seen-ordered raw jobs.
//!
//! Each INFO event line is routed by its leading bracket tag and matched against only that tag's
//! patterns. A line carrying a data tag ([Phase1], [Phase2], [Phase3], [Job]) or a brand-new tag
//! that matches no pattern is fatal, so a zisk log-format change forces a parser update rather than
//! dropping data in silence. Missing lines are tolerated because absent data leaves fields unset.
//! Recognized non-data tags and untagged informational lines are skipped.

use std::{collections::HashMap, sync::LazyLock};

use regex::Regex;

use crate::parse_benchmark::input::log::{
    LogStatus, NodeEndKind, Ts,
    zkvm::{cap, capf, strip_ansi},
};

/// Matches the leading bracket tag of an INFO-level coordinator event line.
static TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\S+ INFO: \[(?P<tag>[A-Za-z0-9_]+)\]").unwrap());

/// The leading bracket tag of an INFO-level coordinator event line, the first bracketed token after
/// the INFO level. Lines at other levels, untagged INFO lines, and the container preamble return
/// None, so only data-bearing INFO lines are routed by tag and checked for being unparsed.
fn leading_tag(line: &str) -> Option<&str> {
    TAG_RE
        .captures(line)
        .and_then(|c| c.name("tag"))
        .map(|m| m.as_str())
}

/// Matches the coordinator job-started line, capturing the timestamp and job id.
static RE_JOB_STARTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<ts>\S+) INFO: \[Job\] Started JobId\((?P<job>[^)]*)\) successfully Capacity: \d+CU Workers: \d+",
    )
    .unwrap()
});

/// Matches the inline-input-data line carrying the input byte count.
static RE_INPUT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<ts>\S+) INFO: Job JobId\((?P<job>[^)]*)\) using inline input data \((?P<bytes>\d+) bytes\)",
    )
    .unwrap()
});

/// Matches the phase1 start line.
static RE_P1_START: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<ts>\S+) INFO: \[Phase1\] Started with \d+ workers for JobId\((?P<job>[^)]*)\)",
    )
    .unwrap()
});

/// Matches a phase1 per-worker finish line, read for the finishing worker.
static RE_P1_DONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\S+ INFO: \[Phase1\] WorkerId\((?P<worker>[^)]*)\) finished phase 1 for JobId\((?P<job>[^)]*)\) \(\d+\s*/\s*\d+ workers done, Phase: [\d.]+s, Delay: [\d.]+s, Witness: [\d.]+s, Asm Execution: [\d.]+s at [\d.]+ MHz\)",
    )
    .unwrap()
});

/// Matches the phase2 start line.
static RE_P2_START: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<ts>\S+) INFO: \[Phase2\] Started with \d+ workers for JobId\((?P<job>[^)]*)\)",
    )
    .unwrap()
});

/// Matches a phase2 per-worker finish line, read for the finishing worker.
static RE_P2_DONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\S+ INFO: \[Phase2\] WorkerId\((?P<worker>[^)]*)\) finished phase 2 for JobId\((?P<job>[^)]*)\) \(\d+\s*/\s*\d+ workers done, [\d.]+s\)",
    )
    .unwrap()
});

/// Matches the phase3 aggregator assignment line. zisk 1.1.0 renamed the assigned role from
/// aggregator to recurser, so both spellings name the same node.
static RE_P3_ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<ts>\S+) INFO: \[Phase3\] Assigned worker WorkerId\((?P<worker>[^)]*)\) as (?:aggregator|recurser) for job JobId\((?P<job>[^)]*)\)",
    )
    .unwrap()
});

/// Matches the phase3 completion line.
static RE_P3_DONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<ts>\S+) INFO: \[Phase3\] WorkerId WorkerId\([^)]*\) done, phase 3 completed for JobId\((?P<job>[^)]*)\) \([\d.]+s\)",
    )
    .unwrap()
});

/// Matches the job-finished line carrying the total duration, step count, and instance count.
static RE_JOB_DONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<ts>\S+) INFO: \[Job\] Finished JobId\((?P<job>[^)]*)\) successfully.*?Duration: (?P<dur>[\d.]+)s \([\d.]+s\+[\d.]+s\+[\d.]+s\) Steps: (?P<steps>[\d.]+) Instances: (?P<inst>[\d.]+) Capacity: \d+CU",
    )
    .unwrap()
});

/// Matches an ERROR or WARN job failure line carrying a timeout or failure keyword. The caller
/// reads the keyword from the line itself to tell the two apart.
static RE_JOB_FAILED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<ts>\S+) (?:ERROR|WARN): .*?JobId\((?P<job>[^)]*)\).*?(?:timed out|Failed)")
        .unwrap()
});

/// Matches the coordinator's terminal `ERROR: Failed job JobId(<job>)` line, the canonical crash
/// time. The keyword precedes the id here, so [`RE_JOB_FAILED`], which expects it after, misses it.
static RE_JOB_FAILED_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<ts>\S+) ERROR: Failed job JobId\((?P<job>[^)]*)\)").unwrap()
});

/// Matches a worker's connection drop mid-job, the crashed node and the moment the cluster lost it,
/// the end point its in-progress phase is clipped to.
static RE_CONN_DROP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<ts>\S+) ERROR: Worker WorkerId\((?P<worker>[^)]+)\) connection dropped while computing for job JobId\((?P<job>[^)]*)\)",
    )
    .unwrap()
});

/// Matches a worker acknowledging cancellation after a sibling crashed. The acknowledgement time is
/// where its in-progress phase is clipped, marked as a cancellation rather than a crash.
static RE_CANCEL_ACK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<ts>\S+) INFO: Worker WorkerId\((?P<worker>[^)]+)\) acknowledged cancellation of job JobId\((?P<job>[^)]*)\)",
    )
    .unwrap()
});

/// Matches the [Job] contribution and prove-performance summary lines the benchmark extracts
/// nothing from, so these recognized non-data lines do not fail the parse as uncovered [Job] lines.
static RE_JOB_INFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\S+ INFO: \[Job\] (?:Contributions|Prove Performance)\b").unwrap()
});

/// A zisk proving job as read from the coordinator log, before translation to the generic model.
#[derive(Clone)]
pub struct RawJob {
    pub id: String,
    pub status: LogStatus,
    pub t_start: Option<Ts>,
    pub t_end: Option<Ts>,
    pub t_p3_start: Option<Ts>,
    pub t_p3_end: Option<Ts>,
    pub input_bytes: Option<u64>,
    pub instances: Option<u64>,
    pub steps: Option<u64>,
    pub duration_s: Option<f64>,
    pub aggregator: Option<String>,
    /// The workers that finished phase 1, each a node that took part in the job.
    pub p1: Vec<String>,
    /// The workers that finished phase 2, each a node that took part in the job.
    pub p2: Vec<String>,
    /// Per-worker crash or cancel moments on an incomplete job, the end point each node's
    /// in-progress phase is clipped to. Empty on a clean job.
    pub node_ends: Vec<(String, Ts, NodeEndKind)>,
}

impl RawJob {
    /// Creates an empty raw job for the given id.
    fn new(id: &str) -> RawJob {
        RawJob {
            id: id.to_string(),
            status: LogStatus::Unknown,
            t_start: None,
            t_end: None,
            t_p3_start: None,
            t_p3_end: None,
            input_bytes: None,
            instances: None,
            steps: None,
            duration_s: None,
            aggregator: None,
            p1: Vec::new(),
            p2: Vec::new(),
            node_ends: Vec::new(),
        }
    }
}

/// Tags whose lines carry no data the benchmark extracts, so they are skipped. Recovery lines trace
/// a worker leaving and re-registering after a crash, already captured by the per-node crash
/// marker.
const IGNORED_TAGS: [&str; 3] = ["Coordinator", "Setup", "Recovery"];

/// Builds the fatal error for a tagged event line that matched no known pattern.
fn unrecognized(line: &str) -> crate::parse_benchmark::ParseError {
    crate::parse_benchmark::ParseError::UnrecognizedCoordinatorLine(line.trim().to_string())
}

/// Parses coordinator log text into first-seen-ordered raw jobs. An INFO line with a data tag or
/// brand-new tag matching no pattern is fatal, so a zisk log-format change is a fixable error
/// rather than dropped data.
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
    /// Routes one log line to the precise patterns for its tag, failing on an uncovered line.
    fn consume(&mut self, raw: &str) -> crate::parse_benchmark::Result<()> {
        match leading_tag(raw) {
            Some("Phase1") => self.phase1(raw)?,
            Some("Phase2") => self.phase2(raw)?,
            Some("Phase3") => self.phase3(raw)?,
            Some("Job") => self.job(raw)?,
            Some(tag) if IGNORED_TAGS.contains(&tag) => {}
            Some(_) => return Err(unrecognized(raw)),
            None => self.untagged(raw)?,
        }
        Ok(())
    }

    /// Handles the [Phase1] start and per-worker finish lines, failing on any other [Phase1] line.
    fn phase1(&mut self, raw: &str) -> crate::parse_benchmark::Result<()> {
        if let Some(c) = RE_P1_START.captures(raw) {
            // Started marks the job seen for ordering but yields no field.
            self.ensure(cap(&c, "job"));
        } else if let Some(c) = RE_P1_DONE.captures(raw) {
            let worker = cap(&c, "worker").to_string();
            self.ensure(cap(&c, "job")).p1.push(worker);
        } else {
            return Err(unrecognized(raw));
        }
        Ok(())
    }

    /// Handles the [Phase2] start and per-worker finish lines, failing on any other [Phase2] line.
    fn phase2(&mut self, raw: &str) -> crate::parse_benchmark::Result<()> {
        if let Some(c) = RE_P2_START.captures(raw) {
            // Started marks the job seen for ordering but yields no field.
            self.ensure(cap(&c, "job"));
        } else if let Some(c) = RE_P2_DONE.captures(raw) {
            let worker = cap(&c, "worker").to_string();
            self.ensure(cap(&c, "job")).p2.push(worker);
        } else {
            return Err(unrecognized(raw));
        }
        Ok(())
    }

    /// Handles the [Phase3] aggregator assignment and completion lines, failing on any other one.
    fn phase3(&mut self, raw: &str) -> crate::parse_benchmark::Result<()> {
        if let Some(c) = RE_P3_ASSIGN.captures(raw) {
            let j = self.ensure(cap(&c, "job"));
            j.aggregator = Some(cap(&c, "worker").to_string());
            j.t_p3_start = Some(Ts::parse(cap(&c, "ts"))?);
        } else if let Some(c) = RE_P3_DONE.captures(raw) {
            self.ensure(cap(&c, "job")).t_p3_end = Some(Ts::parse(cap(&c, "ts"))?);
        } else {
            return Err(unrecognized(raw));
        }
        Ok(())
    }

    /// Handles the [Job] start and finish lines, skips the recognized informational ones, and fails
    /// on any other [Job] line.
    fn job(&mut self, raw: &str) -> crate::parse_benchmark::Result<()> {
        if let Some(c) = RE_JOB_STARTED.captures(raw) {
            self.ensure(cap(&c, "job")).t_start = Some(Ts::parse(cap(&c, "ts"))?);
        } else if let Some(c) = RE_JOB_DONE.captures(raw) {
            let j = self.ensure(cap(&c, "job"));
            j.status = LogStatus::Success;
            j.t_end = Some(Ts::parse(cap(&c, "ts"))?);
            j.duration_s = Some(capf(&c, "dur"));
            j.steps = cap(&c, "steps").replace('.', "").parse().ok();
            j.instances = cap(&c, "inst").replace('.', "").parse().ok();
        } else if RE_JOB_INFO.is_match(raw) {
            // Recognized informational [Job] line, no data extracted.
        } else {
            return Err(unrecognized(raw));
        }
        Ok(())
    }

    /// Handles untagged lines, the inline-input and failure lines. Because leading_tag matches only
    /// INFO lines, non-INFO failure lines route here too, and an unrecognized one is skipped rather
    /// than failing the parse because these lines are too varied and carry no data.
    fn untagged(&mut self, raw: &str) -> crate::parse_benchmark::Result<()> {
        if let Some(c) = RE_INPUT.captures(raw) {
            self.ensure(cap(&c, "job")).input_bytes = cap(&c, "bytes").parse().ok();
        } else if let Some(c) = RE_CONN_DROP.captures(raw) {
            let (worker, ts) = (cap(&c, "worker").to_string(), Ts::parse(cap(&c, "ts"))?);
            self.ensure(cap(&c, "job"))
                .node_ends
                .push((worker, ts, NodeEndKind::Crashed));
        } else if let Some(c) = RE_CANCEL_ACK.captures(raw) {
            let (worker, ts) = (cap(&c, "worker").to_string(), Ts::parse(cap(&c, "ts"))?);
            self.ensure(cap(&c, "job"))
                .node_ends
                .push((worker, ts, NodeEndKind::Cancelled));
        } else if let Some(c) = RE_JOB_FAILED.captures(raw) {
            // A timeout keyword anywhere wins, so "Failed ... timed out" stays a timeout.
            let status = if raw.contains("timed out") {
                LogStatus::Timeout
            } else {
                LogStatus::Failed
            };
            self.record_failure(cap(&c, "job"), cap(&c, "ts"), status)?;
        } else if let Some(c) = RE_JOB_FAILED_LINE.captures(raw) {
            // The cluster's terminal failure line with verb before id, never emitted by a finished
            // job.
            self.record_failure(cap(&c, "job"), cap(&c, "ts"), LogStatus::Failed)?;
        }
        Ok(())
    }

    /// Records a job's terminal failure status and end time unless it already succeeded. The end
    /// time is parsed only when the status is recorded.
    fn record_failure(
        &mut self,
        jid: &str,
        ts: &str,
        status: LogStatus,
    ) -> crate::parse_benchmark::Result<()> {
        let j = self.ensure(jid);
        if j.status != LogStatus::Success {
            j.status = status;
            j.t_end = Some(Ts::parse(ts)?);
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
        input::log::{
            LogStatus,
            zkvm::zisk::coordinator::{RawJob, leading_tag, parse},
        },
    };

    #[test]
    fn reads_the_info_event_tag() {
        assert_eq!(
            leading_tag("2026-05-29T09:53:10Z INFO: [Phase1] Started with 4 workers"),
            Some("Phase1")
        );
        assert_eq!(
            leading_tag("2026-05-29T09:53:16Z INFO: [Job] Finished JobId(JOBA) successfully"),
            Some("Job")
        );
    }

    #[test]
    fn ignores_untagged_other_level_and_preamble_lines() {
        // Untagged INFO line (the inline-input line carries no bracket tag).
        assert_eq!(
            leading_tag("2026-05-29T09:53:09Z INFO: Job JobId(JOBA) using inline input data"),
            None
        );
        // A tag at DEBUG or any non-INFO level is not a data event line.
        assert_eq!(
            leading_tag("2026-05-29T09:53:18Z DEBUG: [Phase1] internal detail"),
            None
        );
        // Container preamble lines have no timestamp and no tag.
        assert_eq!(leading_tag("== CUDA =="), None);
        assert_eq!(
            leading_tag("WARNING: The NVIDIA Driver was not detected."),
            None
        );
    }

    const NAMED: &str = "\
2026-05-29T09:53:09.335819Z INFO: [Job] Started JobId(JOBA) successfully Capacity: 40CU Workers: 4
2026-05-29T09:53:09.380450Z INFO: Job JobId(JOBA) using inline input data (8549632 bytes)
2026-05-29T09:53:10.000000Z INFO: [Phase1] Started with 4 workers for JobId(JOBA)
2026-05-29T09:53:12.283595Z INFO: [Phase1] WorkerId(node4) finished phase 1 for JobId(JOBA) (1/4 workers done, Phase: 2.947s, Delay: 0.057s, Witness: 2.225s, Asm Execution: 0.636s at 479.19495 MHz)
2026-05-29T09:53:13.000000Z INFO: [Phase2] Started with 4 workers for JobId(JOBA)
2026-05-29T09:53:15.821558Z INFO: [Phase2] WorkerId(node3) finished phase 2 for JobId(JOBA) (1 / 4 workers done, 3.269s)
2026-05-29T09:53:15.821539Z INFO: [Phase3] Assigned worker WorkerId(node3) as aggregator for job JobId(JOBA)
2026-05-29T09:53:16.678668Z INFO: [Phase3] WorkerId WorkerId(node3) done, phase 3 completed for JobId(JOBA) (0.305s)
2026-05-29T09:53:16.678680Z INFO: [Job] Finished JobId(JOBA) successfully \u{2714} Duration: 7.342s (3.216s+3.822s+0.305s) Steps: 304.964.424 Instances: 174 Capacity: 40CU
";

    /// Parses a single line and returns the one job it produced.
    fn one_job(line: &str) -> RawJob {
        let mut jobs = parse(line).expect("parse should succeed");
        assert_eq!(jobs.len(), 1, "expected exactly one job from: {line}");
        jobs.remove(0)
    }

    #[test]
    fn parses_every_extracted_field_of_a_named_job() {
        let mut jobs = parse(NAMED).expect("parse should succeed");
        assert_eq!(jobs.len(), 1);
        let j = jobs.remove(0);
        assert_eq!(j.id, "JOBA");
        assert_eq!(j.status, LogStatus::Success);
        assert_eq!(j.input_bytes, Some(8549632));
        assert_eq!(j.instances, Some(174));
        assert_eq!(j.steps, Some(304964424));
        assert_eq!(j.duration_s, Some(7.342));
        assert_eq!(j.aggregator.as_deref(), Some("node3"));
        assert!(j.t_start.is_some() && j.t_end.is_some());
        assert!(j.t_p3_start.is_some() && j.t_p3_end.is_some());

        assert_eq!(j.p1.len(), 1);
        assert_eq!(j.p1[0], "node4");

        assert_eq!(j.p2.len(), 1);
        assert_eq!(j.p2[0], "node3");
    }

    #[test]
    fn phase2_finish_accepts_a_spaced_worker_count() {
        let j = one_job(
            "2026-05-29T09:53:15.821558Z INFO: [Phase2] WorkerId(node3) finished phase 2 for JobId(JOBA) (1 / 4 workers done, 3.269s)",
        );
        assert_eq!(j.p2.len(), 1);
        assert_eq!(j.p2[0], "node3");
    }

    #[test]
    fn phase3_assignment_accepts_the_recurser_spelling() {
        // zisk 1.1.0 renamed the assigned phase-3 role from aggregator to recurser, so the newer
        // spelling must still name the node that carries the job's aggregate window.
        let j = one_job(
            "2026-08-18T14:26:01.618211Z INFO: [Phase3] Assigned worker WorkerId(node1) as recurser for job JobId(JOBA)",
        );
        assert_eq!(j.aggregator.as_deref(), Some("node1"));
        assert!(j.t_p3_start.is_some());
    }

    #[test]
    fn failure_line_routes_through_untagged_and_is_captured() {
        // A failure line is ERROR level, so it routes as untagged even with a [Job] tag, and is
        // captured as a failure rather than failing the parse.
        let j = one_job(
            "2026-05-29T09:53:20.000000Z ERROR: [Job] JobId(JOBA) processing Failed after timeout",
        );
        assert_eq!(j.status, LogStatus::Failed);
        assert!(j.t_end.is_some());
    }

    #[test]
    fn timed_out_line_is_captured_as_a_timeout() {
        // A timeout failure line is distinguished from a plain failure, so the timeout status
        // reaches the output rather than collapsing into a generic failure.
        let j = one_job("2026-05-29T09:53:20.000000Z ERROR: [Job] JobId(JOBA) timed out after 60s");
        assert_eq!(j.status, LogStatus::Timeout);
        assert!(j.t_end.is_some());
    }

    #[test]
    fn failure_keyword_before_timeout_still_reads_as_a_timeout() {
        // The timeout is recognized wherever the word sits, so a failure-first line is not
        // downgraded to a generic failure.
        let j = one_job(
            "2026-05-29T09:53:20.000000Z ERROR: [Job] JobId(JOBA) Failed: worker timed out",
        );
        assert_eq!(j.status, LogStatus::Timeout);
    }

    // Uncovered tagged lines are fatal so a zisk format change cannot silently drop data, while
    // missing lines remain tolerated.

    #[test]
    fn uncovered_data_tag_line_is_fatal() {
        // A renamed [Phase1] verb breaks the precise pattern, so the parse must fail loudly.
        let text = "2026-05-29T09:53:12.000000Z INFO: [Phase1] WorkerId(node4) COMPLETED phase 1 for JobId(JOBA)";
        let err = parse(text)
            .err()
            .expect("the uncovered line must fail the parse");
        let ParseError::UnrecognizedCoordinatorLine(line) = err else {
            panic!("expected UnrecognizedCoordinatorLine, got {err:?}");
        };
        assert!(
            line.contains("COMPLETED phase 1"),
            "offending line in error: {line}"
        );
    }

    #[test]
    fn brand_new_tag_is_fatal() {
        let text = "2026-05-29T09:53:12.000000Z INFO: [Phase4] new stage emitted for JobId(JOBA)";
        let err = parse(text).err().expect("the new tag must fail the parse");
        assert!(
            matches!(&err, ParseError::UnrecognizedCoordinatorLine(line) if line.contains("[Phase4]")),
            "expected UnrecognizedCoordinatorLine naming the new tag, got {err:?}"
        );
    }

    #[test]
    fn missing_lines_are_tolerated() {
        // A job that reports only a start and finish, with no per-worker phase lines, still parses.
        let text = "\
2026-05-29T09:53:09.335819Z INFO: [Job] Started JobId(JOBA) successfully Capacity: 40CU Workers: 4
2026-05-29T09:53:16.678680Z INFO: [Job] Finished JobId(JOBA) successfully \u{2714} Duration: 7.342s (3.216s+3.822s+0.305s) Steps: 304.964.424 Instances: 174 Capacity: 40CU
";
        let jobs = parse(text).expect("parse should succeed");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, LogStatus::Success);
        assert!(jobs[0].p1.is_empty() && jobs[0].p2.is_empty());
    }

    #[test]
    fn recognized_non_data_lines_parse_without_error() {
        // Informational [Job] lines, the [Coordinator] and [Setup] tags, untagged lines, DEBUG
        // lines, and the container preamble are recognized as non-data, so the parse yields no job.
        let text = "\
==========
== CUDA ==
WARNING: The NVIDIA Driver was not detected.

2026-05-29T09:53:09.000000Z INFO: [Coordinator] Registrations: 4 Reconnections: 0
2026-05-29T09:53:09.000000Z INFO: [Setup] All workers acknowledged setup for job_id abc
2026-05-29T09:53:11.000000Z INFO: [Job] Contributions ASM for JobId(JOBA) - Avg: 0.5s, Best: node4
2026-05-29T09:53:11.000000Z INFO: [Job] Prove Performance for JobId(JOBA) - Avg: 3.0s, Best: node3
2026-05-29T09:53:11.000000Z INFO: Worker WorkerId(node4) Ready (total: 4 CC: 40CU ACC: 40CU)
2026-05-29T09:53:18.000000Z DEBUG: [Phase1] internal scheduler detail for JobId(JOBA)
";
        let jobs = parse(text).expect("parse should succeed");
        assert!(
            jobs.is_empty(),
            "recognized non-data lines must not create a job"
        );
    }
}
