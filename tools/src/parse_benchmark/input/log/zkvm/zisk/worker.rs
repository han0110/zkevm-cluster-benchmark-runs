//! Reads the zisk worker logs for aggregation bounds and per-node phase progress.
//!
//! The worker logs are large, so each is read and ansi-stripped once and handed to each sub-parser
//! in one pass. `parse_agg` recovers a clean job's aggregation window from the aggregator's
//! markers, `parse_stages` reconstructs the per-node sub-phase timeline a crashed job leaves
//! behind because the coordinator logs a phase only when it finishes while the worker brackets
//! every sub-step, and `pipeline::parse` extracts the fine pipeline items from those brackets.
//! Stage and aggregation fields are first-write-wins, so a later job on the same worker never
//! overwrites an earlier one.

use std::{collections::BTreeMap, path::Path, sync::LazyLock};

use crate::parse_benchmark::input::{
    WORKER_LOG_RE,
    log::{
        Ts, job_prefix,
        zkvm::{
            cap, strip_ansi,
            zisk::pipeline::{self, JobPipelines},
        },
    },
    worker_files_sorted,
};

/// Everything recovered from the worker logs, the per-job aggregation bounds, node stages, and
/// fine pipeline items.
pub struct WorkerData {
    /// Per-job aggregation bounds, keyed by the eight-hex job prefix so the key matches the stage
    /// map and the coordinator-side lookup regardless of how each log truncates the job id.
    pub agg: BTreeMap<String, AggBounds>,
    /// Per-job sub-phase stage timestamps, keyed by the eight-hex job prefix then by node.
    pub stages: BTreeMap<String, JobStages>,
    /// Per-job fine pipeline items, keyed by the eight-hex job prefix then by node.
    pub pipelines: BTreeMap<String, JobPipelines>,
}

/// Aggregation timing bounds for one job, recovered from the aggregator worker log.
#[derive(Clone, Default)]
pub struct AggBounds {
    pub t_start: Option<Ts>,
    pub t_first_step: Option<Ts>,
    pub t_end: Option<Ts>,
}

/// Boundary timestamps of one node's sub-phases for one job, any subset present. A field is set the
/// first time its marker appears within the job, so a later job on the same worker never overwrites
/// it. The unfinished phase has a start but no end, which the assembler clips to the crash moment.
///
/// The last three fields carry the worker-local truth about how an incomplete job ended on this
/// node, which the coordinator log reports late and only for one node. `last_activity` is the
/// timestamp of the last line the node emitted before its process died, the authoritative freeze
/// point a crash leaves. `cancelled_at` is when the node learned the job was cancelled after a
/// sibling crashed, from its own cancellation line, earlier than the coordinator's acknowledgement.
/// `crashed` records that the node's process restarted mid-job, the worker-log signature of a
/// crash, which the coordinator collapses to a single named node even when several froze.
#[derive(Clone, Default)]
pub struct WorkerStages {
    // The Starting Partial Contribution marker, when the node has fully received the job input and
    // begins, the end of the input-transfer window that runs from the coordinator job start.
    pub contribution_start: Option<Ts>,
    pub emulation_start: Option<Ts>,
    pub emulation_end: Option<Ts>,
    pub witgen_start: Option<Ts>,
    pub witgen_end: Option<Ts>,
    pub prove_start: Option<Ts>,
    pub prove_end: Option<Ts>,
    pub last_activity: Option<Ts>,
    pub cancelled_at: Option<Ts>,
    pub crashed: bool,
}

/// Stage timestamps for every node on a job, keyed by node id, the value of the per-job map.
pub type JobStages = BTreeMap<String, WorkerStages>;

/// Matches the first aggregation-step line of a job, carrying the job id.
static RE_AGG_START: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"^(?P<ts>\S+) \S+ INFO: Starting aggregation step for JobId\((?P<job>[^)]*)\)",
    )
    .unwrap()
});

/// Matches the last-proof-received line, which carries no job id.
static RE_AGG_LAST_PROOF: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^(?P<ts>\S+) \S+ INFO: Last proof received").unwrap());

/// Matches the aggregation-task-completed line, carrying the job id.
static RE_AGG_DONE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"^(?P<ts>\S+) \S+ INFO: Aggregation task completed for JobId\((?P<job>[^)]*)\)",
    )
    .unwrap()
});

/// Loads aggregation bounds, node phase stages, and pipeline items from every worker-N.log under a
/// logs directory.
pub fn load(logs_dir: &Path) -> crate::parse_benchmark::Result<WorkerData> {
    let mut agg = BTreeMap::new();
    let mut stages = BTreeMap::new();
    let mut pipelines = BTreeMap::new();
    for (digit, path) in worker_files_sorted(logs_dir, &WORKER_LOG_RE)? {
        let text = crate::parse_benchmark::read_to_string_at(&path)?;
        let clean = strip_ansi(&text);
        let node = format!("node{digit}");
        parse_agg(&clean, &mut agg)?;
        parse_stages(&clean, &node, &mut stages)?;
        pipeline::parse(&clean, &node, &mut pipelines)?;
    }
    Ok(WorkerData {
        agg,
        stages,
        pipelines,
    })
}

/// Parses one ansi-stripped worker log's aggregation markers into per-job bounds, keyed by the
/// eight-hex job prefix so the key matches the coordinator-side lookup and the per-node stage map
/// regardless of how either log truncates the job id.
fn parse_agg(
    clean: &str,
    agg: &mut BTreeMap<String, AggBounds>,
) -> crate::parse_benchmark::Result<()> {
    let mut current: Option<String> = None;
    for raw in clean.lines() {
        if let Some(c) = RE_AGG_START.captures(raw) {
            let key = job_prefix(cap(&c, "job"));
            let rec = agg.entry(key.clone()).or_default();
            if rec.t_first_step.is_none() {
                rec.t_first_step = Some(Ts::parse(cap(&c, "ts"))?);
            }
            current = Some(key);
            continue;
        }
        if let Some(c) = RE_AGG_LAST_PROOF.captures(raw) {
            if let Some(key) = current.as_deref()
                && let Some(rec) = agg.get_mut(key)
                && rec.t_start.is_none()
            {
                rec.t_start = Some(Ts::parse(cap(&c, "ts"))?);
            }
            continue;
        }
        if let Some(c) = RE_AGG_DONE.captures(raw) {
            let rec = agg.entry(job_prefix(cap(&c, "job"))).or_default();
            rec.t_end = Some(Ts::parse(cap(&c, "ts"))?);
            current = None;
            continue;
        }
    }
    Ok(())
}

/// Parses one ansi-stripped worker log into per-job stage timestamps for the given node. The
/// current phase1 and phase2 jobs are tracked separately because each is introduced by its own
/// marker and the sub-step lines in between carry no job id. The Starting Partial Contribution
/// marker records when the node has received the job input and begins, the end of the
/// input-transfer window the assembler runs from the coordinator job start.
///
/// The same pass recovers how an incomplete job ended on this node. `last_line` holds the latest
/// timestamped line of the running job, so when a process-restart banner appears the node has
/// crashed and that held line is its freeze point. The banner and the restart output that follows
/// carry no timestamp, so they never advance the freeze, and a new Starting Partial Contribution
/// clears the held line. The node's own cancellation line records when a surviving sibling learned
/// to stop.
fn parse_stages(
    clean: &str,
    node: &str,
    stages: &mut BTreeMap<String, JobStages>,
) -> crate::parse_benchmark::Result<()> {
    let mut cur_p1: Option<String> = None;
    let mut cur_p2: Option<String> = None;
    // The last timestamped line of the running job and whether the job's node has already crashed,
    // so the restart output after a freeze neither advances the freeze nor is mistaken for
    // fresh work.
    let mut last_line: Option<&str> = None;
    let mut frozen = false;

    for line in clean.lines() {
        // A line with no leading timestamp is restart or wrapped output, not a job sub-step. A
        // restart banner while a job runs means the node's process died, so the held last
        // line is its freeze.
        if !has_ts(line) {
            if !frozen
                && let Some(job) = cur_p1.clone()
                && is_restart_banner(line)
            {
                if let Some(ll) = last_line {
                    let ts = Ts::parse(leading_ts(ll))?;
                    set(stages, &job, node, ts, |s| &mut s.last_activity);
                }
                set_crashed(stages, &job, node);
                frozen = true;
            }
            continue;
        }
        // Hold the latest timestamped line of the running job as the freeze candidate, until a
        // crash freezes it or a new job clears it.
        if !frozen && cur_p1.is_some() {
            last_line = Some(line);
        }

        let Some(body) = info_body(line) else {
            continue;
        };

        if let Some(uuid) = body.strip_prefix("Starting Partial Contribution for ") {
            let key = job_prefix(uuid);
            let ts = Ts::parse(leading_ts(line))?;
            set(stages, &key, node, ts, |s| &mut s.contribution_start);
            cur_p1 = Some(key);
            // A new job begins, so the prior job's freeze tracking is retired.
            frozen = false;
            last_line = None;
        } else if body.starts_with(">>> COMPUTE_MINIMAL_TRACE") {
            if let Some(key) = cur_p1.clone() {
                let ts = Ts::parse(leading_ts(line))?;
                set(stages, &key, node, ts, |s| &mut s.emulation_start);
            }
        } else if body.starts_with("<<< COMPUTE_MINIMAL_TRACE") {
            if let Some(key) = cur_p1.clone() {
                let ts = Ts::parse(leading_ts(line))?;
                set(stages, &key, node, ts, |s| &mut s.emulation_end);
            }
        } else if is_plan_open(body) {
            if let Some(key) = cur_p1.clone() {
                let ts = Ts::parse(leading_ts(line))?;
                set(stages, &key, node, ts, |s| &mut s.witgen_start);
            }
        } else if body.starts_with("Contribution computation successful") {
            if let Some(key) = cur_p1.clone() {
                let ts = Ts::parse(leading_ts(line))?;
                set(stages, &key, node, ts, |s| &mut s.witgen_end);
            }
        } else if let Some(rest) = body.strip_prefix("Starting Prove for JobId(") {
            let key = job_prefix(rest);
            let ts = Ts::parse(leading_ts(line))?;
            set(stages, &key, node, ts, |s| &mut s.prove_start);
            cur_p2 = Some(key);
        } else if body.starts_with("<<< GENERATING_PROOFS")
            && let Some(key) = cur_p2.clone()
        {
            let ts = Ts::parse(leading_ts(line))?;
            set(stages, &key, node, ts, |s| &mut s.prove_end);
        } else if let Some(uuid) = cancelled_uuid(body) {
            // The node's own cancellation line, earlier than the coordinator's acknowledgement, the
            // accurate moment a surviving sibling stopped working after a peer crashed.
            let key = job_prefix(uuid);
            let ts = Ts::parse(leading_ts(line))?;
            set(stages, &key, node, ts, |s| &mut s.cancelled_at);
        }
    }
    Ok(())
}

/// Whether a line begins with a timestamp, the digit-led tracing lines. The restart banner and the
/// mpirun teardown output begin with a letter or symbol, so this cheaply separates job sub-steps
/// from the restart output a crash interleaves between two jobs.
pub(crate) fn has_ts(line: &str) -> bool {
    line.as_bytes().first().is_some_and(u8::is_ascii_digit)
}

/// Whether a line is part of a worker process (re)start, the worker binary banner or the mpirun
/// signal teardown that precedes it. Each appears once per process start, never per job, so seeing
/// one while a job runs is the worker-log signature that the node crashed and is restarting. Only
/// the non-timestamped lines are tested, so a job's own log text never trips it.
pub(crate) fn is_restart_banner(line: &str) -> bool {
    line.starts_with("ZisK Worker")
        || line.starts_with("Primary job")
        || line.contains("exited on signal")
}

/// Whether a worker INFO body opens the planning bracket that starts witness generation, which zisk
/// 1.1.0 renamed from PLAN to PLAN_SECONDARY. The match is exact so a longer stem sharing the prefix
/// is never mistaken for it.
fn is_plan_open(body: &str) -> bool {
    matches!(body.trim_end(), ">>> PLAN" | ">>> PLAN_SECONDARY")
}

/// The job uuid of a worker cancellation line, "Job <uuid> cancelled: <reason>", or None for any
/// other line. The reason names the crashed peer, but the cancellation time belongs to the node
/// being parsed.
fn cancelled_uuid(body: &str) -> Option<&str> {
    body.strip_prefix("Job ")
        .and_then(|rest| rest.split_once(" cancelled:"))
        .map(|(uuid, _)| uuid)
}

/// The message body of a worker INFO line, the part after the INFO marker, or None for a non-INFO
/// line. Skipping non-INFO lines first keeps the hot path off the voluminous DEBUG output.
fn info_body(line: &str) -> Option<&str> {
    line.find(" INFO: ").map(|i| &line[i + 7..])
}

/// The leading whitespace-delimited token of a line, its timestamp.
pub(crate) fn leading_ts(line: &str) -> &str {
    line.split(' ').next().unwrap_or("")
}

/// Sets a stage field for a job-and-node the first time it is seen, never overwriting a prior
/// value.
fn set(
    stages: &mut BTreeMap<String, JobStages>,
    key: &str,
    node: &str,
    ts: Ts,
    field: impl Fn(&mut WorkerStages) -> &mut Option<Ts>,
) {
    let slot = field(
        stages
            .entry(key.to_string())
            .or_default()
            .entry(node.to_string())
            .or_default(),
    );
    if slot.is_none() {
        *slot = Some(ts);
    }
}

/// Marks a job-and-node as crashed, the worker-log conclusion that the node's process died mid-job.
fn set_crashed(stages: &mut BTreeMap<String, JobStages>, key: &str, node: &str) {
    stages
        .entry(key.to_string())
        .or_default()
        .entry(node.to_string())
        .or_default()
        .crashed = true;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::parse_benchmark::input::log::{
        Ts,
        zkvm::zisk::worker::{AggBounds, parse_agg, parse_stages},
    };

    // The loader ansi-strips before calling the parsers, so the tests pass already-clean text. The
    // job id is the truncated JobId(8hex...) form the worker log prints, and the agg map keys on
    // its eight-hex prefix.
    const AGG_LOG: &str = "\
2026-05-29T09:53:16.000000Z zisk_worker::worker INFO: Starting aggregation step for JobId(efd42874\u{2026})
2026-05-29T09:53:16.384473Z proofman::proofman INFO: Last proof received. [4] proofs were received
2026-05-29T09:53:16.678613Z zisk_worker::worker_node INFO: Aggregation task completed for JobId(efd42874\u{2026})
";

    #[test]
    fn binds_first_step_last_proof_and_done() {
        let mut agg: BTreeMap<String, AggBounds> = BTreeMap::new();
        parse_agg(AGG_LOG, &mut agg).unwrap();
        // The bounds are keyed by the eight-hex job prefix, not the raw truncated id.
        let rec = agg.get("efd42874").expect("efd42874 bounds");
        assert_eq!(
            rec.t_first_step.map(Ts::epoch_ms),
            Some(Ts::parse("2026-05-29T09:53:16.000000Z").unwrap().epoch_ms())
        );
        assert_eq!(
            rec.t_start.map(Ts::epoch_ms),
            Some(Ts::parse("2026-05-29T09:53:16.384473Z").unwrap().epoch_ms())
        );
        assert_eq!(
            rec.t_end.map(Ts::epoch_ms),
            Some(Ts::parse("2026-05-29T09:53:16.678613Z").unwrap().epoch_ms())
        );
    }

    const STAGE_LOG: &str = "\
2026-06-02T06:36:20.100000Z h2::codec::framed_read DEBUG: received
2026-06-02T06:36:20.200000Z h2::codec::framed_read DEBUG: received
2026-06-02T06:36:20.329136Z zisk_worker::worker_node INFO: Starting Partial Contribution for efd42874-b32d-4539-9ac7-8c7d900b73d1
2026-06-02T06:36:20.341869Z executor::executor INFO: >>> COMPUTE_MINIMAL_TRACE
2026-06-02T06:36:22.000000Z h2::codec::framed_read DEBUG: received
2026-06-02T06:36:25.109399Z executor::executor INFO: <<< COMPUTE_MINIMAL_TRACE (4767ms)
2026-06-02T06:36:25.200000Z executor::executor INFO: >>> PLAN
2026-06-02T06:36:30.000000Z zisk_worker::worker INFO: Contribution computation successful for JobId(efd42874\u{2026})
2026-06-02T06:36:31.000000Z zisk_worker::worker_node INFO: Starting Prove for JobId(efd42874\u{2026})
2026-06-02T06:36:40.000000Z proofman::proofman INFO: <<< GENERATING_PROOFS (9000ms)
2026-06-02T06:36:45.000000Z h2::codec::framed_read DEBUG: received
2026-06-02T06:36:45.100000Z h2::codec::framed_read DEBUG: received
2026-06-02T06:36:45.200000Z zisk_worker::worker_node INFO: Starting Partial Contribution for abcd1234-0000-0000-0000-000000000000
";

    #[test]
    fn records_each_subphase_boundary_for_the_job_and_node() {
        let mut stages = BTreeMap::new();
        parse_stages(STAGE_LOG, "node4", &mut stages).unwrap();
        let s = stages
            .get("efd42874")
            .and_then(|j| j.get("node4"))
            .expect("node4 stages");
        // Input transfer ends at Starting Partial Contribution, when the node has the input and
        // begins, with the h2 received frames ignored.
        assert_eq!(
            s.contribution_start.map(Ts::epoch_ms),
            Some(Ts::parse("2026-06-02T06:36:20.329136Z").unwrap().epoch_ms())
        );
        assert!(s.emulation_start.is_some() && s.emulation_end.is_some());
        assert!(s.witgen_start.is_some() && s.witgen_end.is_some());
        assert!(s.prove_start.is_some() && s.prove_end.is_some());

        // The next job records its own contribution start, the proving-time received frames between
        // the two jobs carrying no weight.
        let next = stages
            .get("abcd1234")
            .and_then(|j| j.get("node4"))
            .expect("next job stages");
        assert_eq!(
            next.contribution_start.map(Ts::epoch_ms),
            Some(Ts::parse("2026-06-02T06:36:45.200000Z").unwrap().epoch_ms())
        );
    }

    // The v1.1.0 phase-1 markers, whose planning bracket is PLAN_SECONDARY and whose emulation
    // bracket now encloses the contribution work zisk streams alongside the assembly execution.
    const STAGE_LOG_V1_1: &str = "\
2026-08-18T14:26:02.174498Z zisk_worker::worker_node INFO: Starting Partial Contribution for 18689344-7792-4e26-a958-212cc6dccc84
2026-08-18T14:26:02.264677Z executor::executor INFO: >>> COMPUTE_MINIMAL_TRACE
2026-08-18T14:26:02.512151Z executor::executor INFO: First GPU contribution queued
2026-08-18T14:26:02.629278Z executor::executor INFO: <<< COMPUTE_MINIMAL_TRACE (364ms)
2026-08-18T14:26:02.629293Z executor::executor INFO: >>> PLAN_SECONDARY
2026-08-18T14:26:03.607868Z zisk_worker::worker INFO: Contribution computation successful for JobId(18689344\u{2026})
2026-08-18T14:26:03.640000Z zisk_worker::worker_node INFO: Starting Prove for JobId(18689344\u{2026})
2026-08-18T14:26:05.274000Z proofman::proofman INFO: <<< GENERATING_PROOFS (1634ms)
";

    #[test]
    fn the_renamed_planning_marker_starts_witness_generation() {
        // zisk 1.1.0 renamed the planning bracket from PLAN to PLAN_SECONDARY without moving it, so
        // witness generation still runs from that bracket to the contribution success line.
        let mut stages = BTreeMap::new();
        parse_stages(STAGE_LOG_V1_1, "node1", &mut stages).unwrap();
        let s = stages
            .get("18689344")
            .and_then(|j| j.get("node1"))
            .expect("node1 stages");
        assert_eq!(
            s.witgen_start.map(Ts::epoch_ms),
            Some(Ts::parse("2026-08-18T14:26:02.629293Z").unwrap().epoch_ms())
        );
        assert_eq!(
            s.witgen_end.map(Ts::epoch_ms),
            Some(Ts::parse("2026-08-18T14:26:03.607868Z").unwrap().epoch_ms())
        );
        // Emulation still ends where witness generation begins, so the two windows stay adjacent.
        assert_eq!(
            s.emulation_end.map(Ts::epoch_ms),
            s.witgen_start.map(Ts::epoch_ms)
        );
        assert!(s.prove_start.is_some() && s.prove_end.is_some());
    }

    // A node that froze mid-contribution, modeled on the eest run0 crash. The last timestamped line
    // is a witness-generation DEBUG marker, followed by the untimestamped mpirun teardown and
    // worker banner, then the restart's own timestamped lines and the next job.
    const CRASH_LOG: &str = "\
2026-06-03T00:28:50.947466Z zisk_worker::worker_node INFO: Starting Partial Contribution for d177ea78-4a1f-4f1d-8888-9fa228cf7514
2026-06-03T00:28:50.959238Z executor::executor INFO: >>> COMPUTE_MINIMAL_TRACE
2026-06-03T00:28:55.514618Z executor::executor INFO: <<< COMPUTE_MINIMAL_TRACE (4555ms)
2026-06-03T00:28:55.514631Z executor::executor INFO: >>> PLAN
2026-06-03T00:29:10.643541Z proofman::proofman DEBUG: >>> GET_CONTRIBUTION_AIR_1340 [0:19]
--------------------------------------------------------------------------
Primary job  terminated normally, but 1 process returned
a non-zero exit code. Per user-direction, the job has been aborted.
mpirun noticed that process rank 0 with PID 0 on node host exited on signal 9 (Killed).
ZisK Worker v0.18.0
2026-06-03T00:29:20.467168Z proofman_common::proof_ctx INFO: Creating proof context
2026-06-03T00:29:20.472783Z proofman::proofman INFO: >>> INITIALIZING_PROOFMAN
2026-06-03T00:29:25.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for 04bd25d2-65bf-4e60-965f-ff1c688a022b
";

    #[test]
    fn a_crash_freezes_at_the_last_line_before_the_restart_banner() {
        let mut stages = BTreeMap::new();
        parse_stages(CRASH_LOG, "node4", &mut stages).unwrap();
        let s = stages
            .get("d177ea78")
            .and_then(|j| j.get("node4"))
            .expect("crashed job stages");
        assert!(s.crashed, "the restart banner marks the node crashed");
        // The freeze is the last work line, not the restart's later timestamped lines.
        assert_eq!(
            s.last_activity.map(Ts::epoch_ms),
            Some(Ts::parse("2026-06-03T00:29:10.643541Z").unwrap().epoch_ms())
        );
        // The completed sub-phases up to the freeze are still recorded.
        assert!(s.emulation_start.is_some() && s.emulation_end.is_some());
        assert!(s.witgen_start.is_some() && s.witgen_end.is_none());
        // The restart belongs to the next job, which is not itself crashed.
        let next = stages.get("04bd25d2").and_then(|j| j.get("node4"));
        assert!(next.is_none_or(|n| !n.crashed && n.last_activity.is_none()));
    }

    // A surviving sibling cancelled after a peer crashed, with its own cancellation and recovery
    // lines.
    const CANCEL_LOG: &str = "\
2026-06-03T00:28:50.947466Z zisk_worker::worker_node INFO: Starting Partial Contribution for d177ea78-4a1f-4f1d-8888-9fa228cf7514
2026-06-03T00:28:50.958847Z executor::executor INFO: >>> COMPUTE_MINIMAL_TRACE
2026-06-03T00:28:55.570272Z executor::executor INFO: <<< COMPUTE_MINIMAL_TRACE (4611ms)
2026-06-03T00:28:55.570294Z executor::executor INFO: >>> PLAN
2026-06-03T00:29:14.647634Z zisk_worker::worker_node INFO: Job d177ea78-4a1f-4f1d-8888-9fa228cf7514 cancelled: Worker WorkerId(node4) connection dropped
2026-06-03T00:29:14.679326Z zisk_worker::worker_node WARN: [Recovery] node1: running cluster cancellation handshake
2026-06-03T00:29:15.532165Z zisk_worker::worker_node INFO: Starting Partial Contribution for 04bd25d2-65bf-4e60-965f-ff1c688a022b
";

    #[test]
    fn a_cancelled_sibling_records_its_own_cancellation_time() {
        let mut stages = BTreeMap::new();
        parse_stages(CANCEL_LOG, "node1", &mut stages).unwrap();
        let s = stages
            .get("d177ea78")
            .and_then(|j| j.get("node1"))
            .expect("cancelled job stages");
        // The cancellation time is the worker's own line, not a crash and not the coordinator's
        // ack.
        assert!(!s.crashed);
        assert_eq!(
            s.cancelled_at.map(Ts::epoch_ms),
            Some(Ts::parse("2026-06-03T00:29:14.647634Z").unwrap().epoch_ms())
        );
        // It reached witgen but never finished its contribution before being cancelled.
        assert!(s.witgen_start.is_some() && s.witgen_end.is_none());
    }

    // Two nodes parsed from their own logs in the same job, only one of which carries a restart
    // banner, so the crash classification is independent per node rather than a single
    // coordinator-named one.
    #[test]
    fn multiple_nodes_each_classify_from_their_own_log() {
        let mut stages = BTreeMap::new();
        parse_stages(CRASH_LOG, "node4", &mut stages).unwrap();
        parse_stages(CANCEL_LOG, "node1", &mut stages).unwrap();
        let job = stages.get("d177ea78").expect("job stages");
        assert!(job.get("node4").is_some_and(|n| n.crashed));
        assert!(
            job.get("node1")
                .is_some_and(|n| !n.crashed && n.cancelled_at.is_some())
        );
    }
}
