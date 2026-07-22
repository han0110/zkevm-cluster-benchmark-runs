//! Translates parsed zisk jobs into the generic model, building each node's phase windows from its
//! worker sub-phase markers.
//!
//! The preset orders the phases input, emulation, witgen, prove, aggregate. Each comes straight
//! from the worker log markers. Input transfer runs from the coordinator job start to the worker's
//! Starting Partial Contribution, when the node has the input and begins, emulation is the
//! COMPUTE_MINIMAL_TRACE bracket, witgen runs from the PLAN marker to the contribution success
//! line, and prove is the GENERATING_PROOFS bracket. The phases may leave a gap between them. The
//! aggregate window is bounded by the aggregator worker log with a coordinator phase3 fallback and
//! attached only to the aggregator. A clean job carries every marker, while an incomplete job clips
//! its unfinished phase to where the node crashed or was cancelled.

use std::collections::{BTreeMap, BTreeSet};

use crate::parse_benchmark::input::log::{
    Log, LogNode, LogStatus, NodeEnd, NodeEndKind, Ts,
    zkvm::zisk::{
        coordinator::RawJob,
        pipeline::JobPipelines,
        worker::{AggBounds, JobStages, WorkerStages},
    },
};

/// The phase slots in the zisk preset order.
const INPUT: usize = 0;
const EMULATION: usize = 1;
const WITGEN: usize = 2;
const PROVE: usize = 3;
const AGGREGATE: usize = 4;
const PHASE_COUNT: usize = 5;

/// Builds the generic log for one parsed zisk job.
pub fn build_log(
    raw: &RawJob,
    agg: Option<&AggBounds>,
    stages: Option<&JobStages>,
    items: Option<&JobPipelines>,
) -> Log {
    let mut meta = BTreeMap::new();
    if let Some(v) = raw.input_bytes {
        meta.insert("input_size".to_string(), v.into());
    }
    if let Some(v) = raw.instances {
        meta.insert("instances".to_string(), v.into());
    }
    if let Some(v) = raw.steps {
        meta.insert("steps".to_string(), v.into());
    }

    Log {
        id: raw.id.clone(),
        status: raw.status,
        t_start: raw.t_start,
        t_end: raw.t_end,
        duration_s: raw.duration_s,
        meta,
        nodes: build_nodes(raw, agg, stages, items),
        participants: participants(raw, stages),
    }
}

/// The node ids that took part in a job, the union of every source that names a worker, so a node
/// missing from the worker stages, coordinator phase events, and crash and cancel lines alike did
/// not work the job.
fn participants(raw: &RawJob, stages: Option<&JobStages>) -> Vec<String> {
    let mut set: BTreeSet<&str> = BTreeSet::new();
    if let Some(map) = stages {
        set.extend(map.keys().map(String::as_str));
    }
    set.extend(raw.p1.iter().map(String::as_str));
    set.extend(raw.p2.iter().map(String::as_str));
    if let Some(agg) = raw.aggregator.as_deref() {
        set.insert(agg);
    }
    set.extend(raw.node_ends.iter().map(|(worker, _, _)| worker.as_str()));
    set.into_iter().map(String::from).collect()
}

/// Builds per-node records from the worker sub-phase markers, the aggregator additionally carrying
/// the cluster aggregate window. A clean job does not clip its windows, while an incomplete one
/// clips its unfinished phase to the node's crash or cancel moment, the job's terminal time, or its
/// last marker. The fine pipeline items pass through unclipped, a dangling item keeping its open
/// end. A node with no window, no items, and no end marker is dropped.
fn build_nodes(
    raw: &RawJob,
    agg: Option<&AggBounds>,
    stages: Option<&JobStages>,
    items: Option<&JobPipelines>,
) -> Vec<LogNode> {
    let aggregate = aggregate_window(raw, agg);
    let clean = raw.status == LogStatus::Success;

    let mut ends: BTreeMap<&str, NodeEnd> = BTreeMap::new();
    for (worker, ts, kind) in &raw.node_ends {
        ends.entry(worker.as_str()).or_insert(NodeEnd {
            at_ms: ts.epoch_ms(),
            kind: *kind,
        });
    }

    let mut ids: BTreeSet<&str> = BTreeSet::new();
    if let Some(map) = stages {
        ids.extend(map.keys().map(String::as_str));
    }
    if let Some(map) = items {
        ids.extend(map.keys().map(String::as_str));
    }
    ids.extend(raw.p1.iter().map(String::as_str));
    ids.extend(raw.p2.iter().map(String::as_str));
    ids.extend(ends.keys().copied());
    if let Some(agg) = raw.aggregator.as_deref() {
        ids.insert(agg);
    }

    let job_end = raw.t_end.map(Ts::epoch_ms);

    ids.into_iter()
        .filter_map(|id| {
            let stage = stages.and_then(|m| m.get(id));
            // The worker log is authoritative for how this node ended, the freeze a crash left or
            // the node's own cancellation. The coordinator end, late and naming only
            // one crasher, is the fallback for a node the worker log does not cover.
            let end = stage
                .and_then(worker_node_end)
                .or_else(|| ends.get(id).copied());
            // A clean job's windows are never clipped, signalled by the i64::MAX cap. An incomplete
            // one clips to the node end, then to the node's own last marker so a
            // timeout or torn-down node ends where it stalled rather than at the job's
            // terminal time, and only then to job_end.
            let cap = if clean {
                i64::MAX
            } else {
                end.map(|e| e.at_ms)
                    .or_else(|| stage.and_then(max_stage_ms))
                    .or(job_end)?
            };
            let is_agg = raw.aggregator.as_deref() == Some(id);
            let job_start = raw.t_start.map(Ts::epoch_ms);
            let phases =
                stage_windows(stage, cap, job_start, is_agg.then_some(aggregate).flatten());
            let node_items = items.and_then(|m| m.get(id)).cloned().unwrap_or_default();
            if phases.iter().all(Option::is_none) && end.is_none() && node_items.is_empty() {
                return None;
            }
            Some(LogNode {
                id: id.to_string(),
                phases,
                items: node_items,
                end,
            })
        })
        .collect()
}

/// Builds a node's preset phase windows from its stage markers, clipped to a cap. A finite cap
/// clips an unfinished phase that has a start but no end, while the sentinel i64::MAX of a clean
/// job requires both bounds so a missing marker leaves the phase absent. The input-transfer window
/// runs from the coordinator job start to the node's Starting Partial Contribution. The aggregate
/// window, present only on the aggregator, is appended and likewise clipped.
fn stage_windows(
    stage: Option<&WorkerStages>,
    cap: i64,
    job_start: Option<i64>,
    aggregate: Option<(i64, i64)>,
) -> Vec<Option<(i64, i64)>> {
    let mut phases = vec![None; PHASE_COUNT];
    if let Some(s) = stage {
        let ms = |t: Option<Ts>| t.map(Ts::epoch_ms);
        let win = |start: Option<i64>, end: Option<i64>| -> Option<(i64, i64)> {
            let start = start?;
            let end = match end {
                Some(e) => e.min(cap),
                None if cap != i64::MAX => cap,
                None => return None,
            };
            (end > start).then_some((start, end))
        };
        phases[INPUT] = win(job_start, ms(s.contribution_start));
        phases[EMULATION] = win(ms(s.emulation_start), ms(s.emulation_end));
        phases[WITGEN] = win(ms(s.witgen_start), ms(s.witgen_end));
        phases[PROVE] = win(ms(s.prove_start), ms(s.prove_end));
    }
    if let Some((start, end)) = aggregate {
        let end = end.min(cap);
        if end > start {
            phases[AGGREGATE] = Some((start, end));
        }
    }
    phases
}

/// The node's own end of an incomplete job from its worker stages, preferred over the coordinator's
/// later, single-node view. A crash wins over a cancellation, so a node told to stop that then died
/// anyway in the same job is reported as crashed at its freeze, the last line it emitted before
/// going silent, rather than hidden behind the earlier cancellation. A node whose process did not
/// restart but that logged its own cancellation ended there. A node the worker log shows neither
/// cancelling nor crashing yields None, leaving the coordinator end or no end at all.
fn worker_node_end(s: &WorkerStages) -> Option<NodeEnd> {
    if s.crashed {
        Some(NodeEnd {
            at_ms: s
                .last_activity
                .map(Ts::epoch_ms)
                .or_else(|| max_stage_ms(s))?,
            kind: NodeEndKind::Crashed,
        })
    } else {
        s.cancelled_at.map(|c| NodeEnd {
            at_ms: c.epoch_ms(),
            kind: NodeEndKind::Cancelled,
        })
    }
}

/// The latest stage timestamp a node reported, the last-resort cap when no end time is known.
fn max_stage_ms(s: &WorkerStages) -> Option<i64> {
    [
        s.contribution_start,
        s.emulation_start,
        s.emulation_end,
        s.witgen_start,
        s.witgen_end,
        s.prove_start,
        s.prove_end,
    ]
    .into_iter()
    .flatten()
    .map(|t| t.epoch_ms())
    .max()
}

/// The cluster aggregate window, from the aggregator worker log with a coordinator phase3 fallback.
fn aggregate_window(raw: &RawJob, agg: Option<&AggBounds>) -> Option<(i64, i64)> {
    let start = agg
        .and_then(|a| a.t_start.or(a.t_first_step))
        .or(raw.t_p3_start)
        .map(Ts::epoch_ms);
    let end = agg.and_then(|a| a.t_end).or(raw.t_p3_end).map(Ts::epoch_ms);
    pair(start, end)
}

/// Combines two optional epoch bounds into an ordered window when both are present and ordered.
fn pair(start: Option<i64>, end: Option<i64>) -> Option<(i64, i64)> {
    match (start, end) {
        (Some(s), Some(e)) if e >= s => Some((s, e)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::parse_benchmark::input::log::{
        NodeEndKind, Ts,
        zkvm::zisk::{
            phases::{stage_windows, worker_node_end},
            worker::WorkerStages,
        },
    };

    fn ts(value: &str) -> Option<Ts> {
        Some(Ts::parse(value).unwrap())
    }

    #[test]
    fn clean_windows_come_straight_from_the_markers() {
        let s = WorkerStages {
            contribution_start: ts("2026-05-29T00:00:00.100Z"),
            emulation_start: ts("2026-05-29T00:00:01.000Z"),
            emulation_end: ts("2026-05-29T00:00:02.000Z"),
            witgen_start: ts("2026-05-29T00:00:02.500Z"),
            witgen_end: ts("2026-05-29T00:00:04.000Z"),
            prove_start: ts("2026-05-29T00:00:05.000Z"),
            prove_end: ts("2026-05-29T00:00:09.000Z"),
            ..Default::default()
        };
        // Input transfer runs from the coordinator job start to the contribution start.
        let job_start = Ts::parse("2026-05-29T00:00:00.000Z").unwrap().epoch_ms();
        let w = stage_windows(Some(&s), i64::MAX, Some(job_start), None);
        // Each phase spans its own markers and the phases may leave a gap between them.
        assert!(w[0].is_some() && w[1].is_some() && w[2].is_some() && w[3].is_some());
        // A non-aggregator carries no aggregate window.
        assert!(w[4].is_none());
    }

    #[test]
    fn an_unfinished_phase_clips_to_the_cap() {
        let s = WorkerStages {
            contribution_start: ts("2026-05-29T00:00:00.100Z"),
            emulation_start: ts("2026-05-29T00:00:01.000Z"),
            emulation_end: None, // crashed mid-emulation
            ..Default::default()
        };
        let job_start = Ts::parse("2026-05-29T00:00:00.000Z").unwrap().epoch_ms();
        let cap = Ts::parse("2026-05-29T00:00:03.000Z").unwrap().epoch_ms();
        let w = stage_windows(Some(&s), cap, Some(job_start), None);
        // Emulation runs from its start to the cap, and the later phases were never reached.
        assert_eq!(w[1].map(|(_, e)| e), Some(cap));
        assert!(w[2].is_none() && w[3].is_none());
    }

    #[test]
    fn worker_end_prefers_the_freeze_and_the_own_cancellation_time() {
        // A crashed node ends at its freeze, the last line it emitted, not at any later coordinator
        // time.
        let crashed = WorkerStages {
            witgen_start: ts("2026-05-29T00:00:05.000Z"),
            last_activity: ts("2026-05-29T00:00:19.700Z"),
            crashed: true,
            ..Default::default()
        };
        let end = worker_node_end(&crashed).expect("a crashed node has an end");
        assert_eq!(end.kind, NodeEndKind::Crashed);
        assert_eq!(
            end.at_ms,
            Ts::parse("2026-05-29T00:00:19.700Z").unwrap().epoch_ms()
        );

        // A cancelled sibling ends at its own cancellation line.
        let cancelled = WorkerStages {
            witgen_start: ts("2026-05-29T00:00:05.000Z"),
            cancelled_at: ts("2026-05-29T00:00:23.700Z"),
            ..Default::default()
        };
        let end = worker_node_end(&cancelled).expect("a cancelled node has an end");
        assert_eq!(end.kind, NodeEndKind::Cancelled);
        assert_eq!(
            end.at_ms,
            Ts::parse("2026-05-29T00:00:23.700Z").unwrap().epoch_ms()
        );

        // A node told to stop that then died anyway in the same job is crashed at its freeze, not
        // hidden behind the earlier cancellation.
        let cancelled_then_crashed = WorkerStages {
            cancelled_at: ts("2026-05-29T00:00:05.400Z"),
            last_activity: ts("2026-05-29T00:00:06.100Z"),
            crashed: true,
            ..Default::default()
        };
        let end = worker_node_end(&cancelled_then_crashed).expect("an end");
        assert_eq!(end.kind, NodeEndKind::Crashed);
        assert_eq!(
            end.at_ms,
            Ts::parse("2026-05-29T00:00:06.100Z").unwrap().epoch_ms()
        );

        // A node the worker log shows neither cancelling nor crashing yields no worker end.
        let clean = WorkerStages {
            prove_end: ts("2026-05-29T00:00:09.000Z"),
            ..Default::default()
        };
        assert!(worker_node_end(&clean).is_none());
    }
}
