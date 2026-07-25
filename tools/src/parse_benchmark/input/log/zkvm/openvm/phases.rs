//! Translates parsed openvm jobs into the generic model, building each node's phase windows from
//! its worker lines and the manager's input fan-out lines.
//!
//! Execution spans the node's earliest parallel-coordinator announcement to its latest segment
//! send, the full executor span every prover runs while metering segments and dispatching them for
//! proving. It overlaps the segment and recursion phases, which are the envelopes of their kinds'
//! completed items on each node, because the executor meters segments while they prove and
//! recursion consumes segments as they complete. The wrap slot holds the final wrap envelope on the
//! one node that proved it, which preserves the aggregator convention of the final slot being
//! non-null on a single node. Input transfer runs from the manager-clock fan-out start to the
//! node's last worker-clock input-file-written time, both kept in microseconds so a sub-millisecond
//! fan-out still measures, and so it abuts the node's execution. Its span therefore crosses the two
//! clocks, as the worker-clock windows do against the manager-clock block start, and is only as
//! aligned as the cluster's NTP.

use std::collections::{BTreeMap, BTreeSet};

use crate::parse_benchmark::input::log::{
    Log, LogNode, PipelineDef, PipelineItem, PipelineItemMeta, SubPhaseDef,
    zkvm::openvm::{
        coordinator::RawJob,
        worker::{JobAnnouncements, JobItems, JobSends, JobWrites},
    },
};

/// The phase slots in the openvm preset order.
const INPUT: usize = 0;
const EXECUTION_PHASE: usize = 1;
const SEGMENT: usize = 2;
const RECURSION: usize = 3;
const WRAP_PHASE: usize = 4;
const PHASE_COUNT: usize = 5;

/// The pipeline kind indices, the single source both the wire template and the worker parser use
/// so the two cannot drift.
pub(crate) const EXECUTION: usize = 0;
pub(crate) const FASTFWD: usize = 1;
pub(crate) const APP_SEGMENT: usize = 2;
pub(crate) const LEAF: usize = 3;
pub(crate) const INTERNAL: usize = 4;
pub(crate) const WRAP: usize = 5;

/// One pipeline kind of the template, in wire order.
struct Kind {
    name: &'static str,
    label: &'static str,
    phase: &'static str,
    paired: bool,
}

/// The ordered kind template. No openvm kind is paired because every item is one self-contained
/// span, the work items from their completion lines, execution derived per sent segment, and
/// fast-forward taken from each segment's proof line. Fast-forward is the prover's re-execution of
/// a segment before its proof, so it shares the execution phase.
const KINDS: [Kind; 6] = [
    Kind {
        name: "execution",
        label: "Metered Execution",
        phase: "execution",
        paired: false,
    },
    Kind {
        name: "fastfwd",
        label: "Fast Forward",
        phase: "execution",
        paired: false,
    },
    Kind {
        name: "app_segment",
        label: "Segment",
        phase: "segment",
        paired: false,
    },
    Kind {
        name: "leaf",
        label: "Recursion",
        phase: "recursion",
        paired: false,
    },
    Kind {
        name: "internal",
        label: "Internal Aggregation",
        phase: "recursion",
        paired: false,
    },
    Kind {
        name: "wrap",
        label: "Wrap",
        phase: "wrap",
        paired: false,
    },
];

/// Returns the ordered openvm fine-pipeline template.
pub fn openvm_pipeline() -> Vec<PipelineDef> {
    KINDS
        .iter()
        .map(|kind| PipelineDef {
            name: kind.name.to_string(),
            label: kind.label.to_string(),
            phase: kind.phase.to_string(),
            paired: kind.paired,
        })
        .collect()
}

/// One STARK proving sub-step, its wire name, display label, and the drained-span key it appears
/// under, suffixed _time_ms, on a worker completion line.
struct SubStep {
    name: &'static str,
    label: &'static str,
    span_key: &'static str,
}

/// The seven sub-steps, in template and execution order. The durations partition each item's STARK
/// time up to residuals, so the same set breaks down both the segment and the recursion phase.
const SUB_STEPS: [SubStep; 7] = [
    SubStep {
        name: "execute_preflight",
        label: "Execute Preflight",
        span_key: "execute_preflight_time_ms",
    },
    SubStep {
        name: "trace_gen",
        label: "Trace Gen",
        span_key: "trace_gen_time_ms",
    },
    SubStep {
        name: "main_trace_commit",
        label: "Commit",
        span_key: "prover.main_trace_commit_time_ms",
    },
    SubStep {
        name: "logup_gkr",
        label: "LogUp GKR",
        span_key: "prover.rap_constraints.logup_gkr_time_ms",
    },
    SubStep {
        name: "round0",
        label: "Sumcheck Univariate Skip",
        span_key: "prover.rap_constraints.round0_time_ms",
    },
    SubStep {
        name: "mle_rounds",
        label: "Sumcheck Multilinear Rounds",
        span_key: "prover.rap_constraints.mle_rounds_time_ms",
    },
    SubStep {
        name: "openings",
        label: "Open",
        span_key: "prover.openings_time_ms",
    },
];

/// The coarse phases the sub-steps break down, one owner per stage. The app segments own the
/// segment phase, and the leaf and internal proofs own the recursion phase.
const OWNERS: [&str; 2] = ["segment", "recursion"];

/// The owner index of the segment breakdown, filled by each app segment item.
pub(crate) const SEGMENT_OWNER: usize = 0;

/// The owner index of the recursion breakdown, filled by each leaf and internal item, an internal
/// final proof carrying the merged wrap timings.
pub(crate) const RECURSION_OWNER: usize = 1;

/// Returns the sub-phase template in wire order, the seven sub-steps under the segment owner then
/// the seven under the recursion owner, referenced positionally by each item's spans and each
/// block's sub-phase rows.
pub fn subphase_template() -> Vec<SubPhaseDef> {
    OWNERS
        .iter()
        .flat_map(|owner| {
            SUB_STEPS.iter().map(move |step| SubPhaseDef {
                name: step.name.to_string(),
                label: step.label.to_string(),
                phase: owner.to_string(),
            })
        })
        .collect()
}

/// Returns the openvm component pipeline template, the seven STARK sub-steps under the segment
/// owner then under the recursion owner, mirroring the sub-phase template. A new-format run appends
/// these to the base template so each sub-step is a first-class pipeline kind the wire rows
/// reference in place of the monolithic segment, leaf, and internal items, its name and owner phase
/// matching a sub-phase so the frontend tints it by that sub-phase rather than the flat phase
/// color.
pub fn openvm_component_pipeline() -> Vec<PipelineDef> {
    OWNERS
        .iter()
        .flat_map(|owner| {
            SUB_STEPS.iter().map(move |step| PipelineDef {
                name: step.name.to_string(),
                label: step.label.to_string(),
                phase: owner.to_string(),
                paired: false,
            })
        })
        .collect()
}

/// Resolves a drained-span map into the sub-phase template slots of one owner, matching each of the
/// seven sub-step keys and dropping every other key, in template order so the pairs stay sorted by
/// slot. The slot is the owner times the sub-step count plus the step index.
pub(crate) fn owner_span_rows(map: &BTreeMap<&str, u64>, owner: usize) -> Vec<[u64; 2]> {
    SUB_STEPS
        .iter()
        .enumerate()
        .filter_map(|(step, s)| {
            map.get(s.span_key)
                .map(|&ms| [(owner * SUB_STEPS.len() + step) as u64, ms])
        })
        .collect()
}

/// Builds the generic log for one parsed openvm job.
pub fn build_log(
    raw: &RawJob,
    items: Option<&JobItems>,
    announcements: Option<&JobAnnouncements>,
    writes: Option<&JobWrites>,
    sends: Option<&JobSends>,
) -> Log {
    let mut meta = BTreeMap::new();
    if let Some(v) = raw.input_bytes {
        meta.insert("input_size".to_string(), v.into());
    }
    if let Some(v) = raw.num_segments {
        meta.insert("segments".to_string(), v.into());
    }

    let nodes = build_nodes(raw, items, announcements, writes, sends);
    let participants = nodes.iter().map(|n| n.id.clone()).collect();

    Log {
        id: raw.id.clone(),
        status: raw.status,
        t_start: raw.t_start,
        t_end: raw.t_end,
        duration_s: raw.duration_s,
        meta,
        nodes,
        participants,
    }
}

/// Builds per-node records from the node's items, its coordinator announcements, and its
/// input-file-written times. A node with no window and no items is dropped. The openvm records are
/// completion lines only, so there is no crash reconstruction and no node end marker.
fn build_nodes(
    raw: &RawJob,
    items: Option<&JobItems>,
    announcements: Option<&JobAnnouncements>,
    writes: Option<&JobWrites>,
    sends: Option<&JobSends>,
) -> Vec<LogNode> {
    let mut ids: BTreeSet<&str> = BTreeSet::new();
    if let Some(map) = items {
        ids.extend(map.keys().map(String::as_str));
    }
    if let Some(map) = writes {
        ids.extend(map.keys().map(String::as_str));
    }

    ids.into_iter()
        .filter_map(|id| {
            let node_items: Vec<PipelineItem> = items
                .and_then(|m| m.get(id))
                .into_iter()
                .flatten()
                .cloned()
                .map(|item| leaf_index(item, raw.leaf_arity))
                .collect();
            let mut phases = vec![None; PHASE_COUNT];
            // Input transfer runs from the manager's fan-out start to the node's last
            // input-file-written time among its workers, and stays null when either side is
            // missing.
            phases[INPUT] =
                input_window(raw.fanout_start_us, writes.and_then(|m| m.get(id)).copied());
            // Execution runs from the node's earliest parallel-coordinator announcement to its
            // latest segment send, the full executor span that overlaps the segment phase, and
            // stays null when either side is missing.
            phases[EXECUTION_PHASE] = window(
                announcements.and_then(|m| m.get(id)).copied(),
                sends.and_then(|m| m.get(id)).copied(),
            );
            phases[SEGMENT] = envelope(&node_items, &[APP_SEGMENT]);
            phases[RECURSION] = envelope(&node_items, &[LEAF, INTERNAL]);
            phases[WRAP_PHASE] = envelope(&node_items, &[WRAP]);
            if phases.iter().all(Option::is_none) && node_items.is_empty() {
                return None;
            }
            Some(LogNode {
                id: id.to_string(),
                phases,
                items: node_items,
                end: None,
            })
        })
        .collect()
}

/// Turns a leaf item's provisional id, its start segment as the worker records it, into the leaf
/// index by dividing by the proof's leaf arity. The id is omitted when the coordinator line
/// carrying the arity was dropped, and every other item passes through unchanged.
fn leaf_index(item: PipelineItem, leaf_arity: Option<i64>) -> PipelineItem {
    if item.kind != LEAF {
        return item;
    }
    let id = leaf_arity
        .filter(|arity| *arity > 0)
        .and_then(|arity| item.meta.id.map(|start| start / arity));
    PipelineItem {
        meta: PipelineItemMeta { id, ..item.meta },
        ..item
    }
}

/// Combines two optional epoch-ms bounds into an ordered window when both are present and ordered.
fn window(start: Option<i64>, end: Option<i64>) -> Option<(i64, i64)> {
    match (start, end) {
        (Some(s), Some(e)) if e > s => Some((s, e)),
        _ => None,
    }
}

/// Combines the microsecond fan-out start and the node's last worker write into an input-transfer
/// window in epoch milliseconds, null only when a side is missing. A normal transfer spans whole
/// milliseconds. A sub-millisecond one whose truncated bounds share a millisecond keeps a
/// one-millisecond floor when the microsecond bounds are ordered, so an instantaneous fan-out still
/// renders a band rather than dropping the node. A worker whose write precedes the fan-out start,
/// a node local to the manager, reads as an instantaneous window at the start.
fn input_window(start_us: Option<i64>, end_us: Option<i64>) -> Option<(i64, i64)> {
    let (start_us, end_us) = (start_us?, end_us?);
    let (start_ms, end_ms) = (start_us / 1000, end_us / 1000);
    let end_ms = if end_ms > start_ms {
        end_ms
    } else if end_us > start_us {
        start_ms + 1
    } else {
        start_ms
    };
    Some((start_ms, end_ms))
}

/// The envelope of the node's items of the given kinds, from the earliest segment start to the
/// latest segment end.
fn envelope(items: &[PipelineItem], kinds: &[usize]) -> Option<(i64, i64)> {
    let mut bounds: Option<(i64, i64)> = None;
    for item in items.iter().filter(|i| kinds.contains(&i.kind)) {
        for (start, end) in [Some(item.first), item.second].into_iter().flatten() {
            let Some(end) = end else { continue };
            bounds = Some(match bounds {
                Some((s, e)) => (s.min(start), e.max(end)),
                None => (start, end),
            });
        }
    }
    bounds
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::parse_benchmark::input::log::{
        LogStatus, PipelineItem, PipelineItemMeta, Ts,
        zkvm::openvm::{
            coordinator::RawJob,
            phases::{APP_SEGMENT, LEAF, WRAP, build_log},
            worker::{JobAnnouncements, JobItems, JobSends, JobWrites},
        },
    };

    fn raw_job() -> RawJob {
        RawJob {
            id: "ere-aaaabbbbccccddddeeeeffff00001111".to_string(),
            status: LogStatus::Success,
            t_start: Some(Ts::parse("2026-07-21T13:55:07.563047Z").unwrap()),
            t_end: Some(Ts::parse("2026-07-21T13:56:26.514833Z").unwrap()),
            fanout_start_us: Some(1_000_000_000),
            input_bytes: Some(6711680),
            num_segments: Some(52),
            leaf_arity: Some(4),
            duration_s: Some(4.883),
        }
    }

    fn span(kind: usize, start: i64, end: i64) -> PipelineItem {
        PipelineItem {
            kind,
            first: (start, Some(end)),
            second: None,
            meta: PipelineItemMeta::default(),
        }
    }

    #[test]
    fn windows_derive_from_items_and_writes() {
        // node1 proves app segments and node2 proves the final wrap, each writing its input file.
        let mut items: JobItems = BTreeMap::new();
        items.insert(
            "node1".to_string(),
            vec![span(APP_SEGMENT, 1_002_000, 1_003_000)],
        );
        items.insert(
            "node2".to_string(),
            vec![
                span(LEAF, 1_003_000, 1_003_500),
                span(WRAP, 1_004_000, 1_004_200),
            ],
        );
        let by_job = BTreeMap::from([(raw_job().id.clone(), items)]);
        let announcements: JobAnnouncements = BTreeMap::from([("node1".to_string(), 1_000_500)]);
        // The node's latest segment send closes its metered execution window.
        let sends: JobSends = BTreeMap::from([("node1".to_string(), 1_002_800)]);
        // Writes are microsecond timestamps well past the fan-out start, so each spans whole
        // milliseconds.
        let writes: JobWrites = BTreeMap::from([
            ("node1".to_string(), 1_000_060_000),
            ("node2".to_string(), 1_000_070_000),
        ]);

        let log = build_log(
            &raw_job(),
            by_job.get(&raw_job().id),
            Some(&announcements),
            Some(&writes),
            Some(&sends),
        );
        assert_eq!(log.nodes.len(), 2);

        let node1 = &log.nodes[0];
        assert_eq!(node1.id, "node1");
        // Input transfer ends at the node's last input-file-written time.
        assert_eq!(node1.phases[0], Some((1_000_000, 1_000_060)));
        // Execution runs from the announcement to the last segment send, overlapping the segment
        // envelope.
        assert_eq!(node1.phases[1], Some((1_000_500, 1_002_800)));
        assert_eq!(node1.phases[2], Some((1_002_000, 1_003_000)));
        assert!(node1.phases[3].is_none() && node1.phases[4].is_none());

        // Only the wrap node carries the final slot, the aggregator convention. Without an
        // announcement or a segment send its execution stays null.
        let node2 = &log.nodes[1];
        assert_eq!(node2.phases[0], Some((1_000_000, 1_000_070)));
        assert!(node2.phases[1].is_none());
        assert_eq!(node2.phases[3], Some((1_003_000, 1_003_500)));
        assert_eq!(node2.phases[4], Some((1_004_000, 1_004_200)));
    }

    #[test]
    fn a_sub_millisecond_input_transfer_keeps_the_node_with_a_measurable_window() {
        // A tiny proof fans out in under a millisecond, its elapsed printed as zero, so the fan-out
        // start and the worker writes share a millisecond. A remote node whose microsecond write
        // follows the start keeps a one-millisecond input band, while a node local to the manager,
        // written before the start, keeps an instantaneous window. Both stay in the proof rather
        // than dropping, the failure block 25580396 showed.
        let mut job = raw_job();
        job.fanout_start_us = Some(1_000_002_500);
        let writes: JobWrites = BTreeMap::from([
            ("node1".to_string(), 1_000_002_800), // remote, write after the fan-out start
            ("node2".to_string(), 1_000_001_800), // local, write before the fan-out start
        ]);
        let log = build_log(&job, None, None, Some(&writes), None);
        assert_eq!(log.nodes.len(), 2);
        assert_eq!(log.nodes[0].phases[0], Some((1_000_002, 1_000_003)));
        assert_eq!(log.nodes[1].phases[0], Some((1_000_002, 1_000_002)));
        assert_eq!(
            log.participants,
            vec!["node1".to_string(), "node2".to_string()]
        );
    }

    #[test]
    fn the_execution_window_runs_from_the_announcement_to_the_last_send() {
        let mut items: JobItems = BTreeMap::new();
        // node1 proves two app segments whose envelope the execution window overlaps rather than
        // abuts, since the executor keeps sending while the earlier segments prove.
        items.insert(
            "node1".to_string(),
            vec![
                span(APP_SEGMENT, 1_001_200, 1_002_100),
                span(APP_SEGMENT, 1_001_000, 1_002_000),
            ],
        );
        // node2 lost its announcement line and node3 sent no segment, so each side of the window
        // is missing once and both nodes carry a null execution.
        items.insert(
            "node2".to_string(),
            vec![span(APP_SEGMENT, 1_001_100, 1_002_000)],
        );
        items.insert("node3".to_string(), vec![span(LEAF, 1_003_000, 1_003_500)]);
        let by_job = BTreeMap::from([(raw_job().id.clone(), items)]);
        let announcements: JobAnnouncements = BTreeMap::from([
            ("node1".to_string(), 1_000_500),
            ("node3".to_string(), 1_000_600),
        ]);
        let sends: JobSends = BTreeMap::from([
            ("node1".to_string(), 1_001_900),
            ("node2".to_string(), 1_001_100),
        ]);

        let log = build_log(
            &raw_job(),
            by_job.get(&raw_job().id),
            Some(&announcements),
            None,
            Some(&sends),
        );
        // The window runs from the earliest coordinator start to the latest segment send, ending
        // inside the segment envelope so the two phases overlap.
        assert_eq!(log.nodes[0].phases[1], Some((1_000_500, 1_001_900)));
        assert!(log.nodes[0].phases[1].unwrap().1 > log.nodes[0].phases[2].unwrap().0);
        assert!(log.nodes[1].phases[1].is_none());
        assert!(log.nodes[2].phases[1].is_none());
    }

    #[test]
    fn leaf_ids_divide_by_the_arity_and_omit_when_unknown() {
        let with_id = |kind: usize, start: i64, end: i64, id: i64| PipelineItem {
            meta: PipelineItemMeta {
                id: Some(id),
                ..Default::default()
            },
            ..span(kind, start, end)
        };
        let mut items: JobItems = BTreeMap::new();
        items.insert(
            "node1".to_string(),
            vec![
                with_id(LEAF, 1_003_000, 1_003_500, 16),
                with_id(APP_SEGMENT, 1_002_000, 1_002_500, 3),
            ],
        );
        let by_job = BTreeMap::from([(raw_job().id.clone(), items)]);

        // The arity 4 turns the leaf's start segment 16 into leaf index 4, while the app segment
        // id passes through untouched.
        let log = build_log(&raw_job(), by_job.get(&raw_job().id), None, None, None);
        let ids: Vec<(usize, Option<i64>)> = log.nodes[0]
            .items
            .iter()
            .map(|i| (i.kind, i.meta.id))
            .collect();
        assert_eq!(ids, vec![(LEAF, Some(4)), (APP_SEGMENT, Some(3))]);

        // A proof whose trigger_tail_proofs line was dropped carries no arity, so the leaf id is
        // omitted rather than left as a raw segment number.
        let mut no_arity = raw_job();
        no_arity.leaf_arity = None;
        let log = build_log(&no_arity, by_job.get(&raw_job().id), None, None, None);
        let ids: Vec<Option<i64>> = log.nodes[0].items.iter().map(|i| i.meta.id).collect();
        assert_eq!(ids, vec![None, Some(3)]);
    }

    #[test]
    fn meta_and_participants_come_from_the_job_and_its_nodes() {
        let mut items: JobItems = BTreeMap::new();
        items.insert("node1".to_string(), vec![span(LEAF, 1_001_000, 1_002_000)]);
        let by_job = BTreeMap::from([(raw_job().id.clone(), items)]);

        let log = build_log(&raw_job(), by_job.get(&raw_job().id), None, None, None);
        assert_eq!(log.meta.get("input_size"), Some(&6711680u64.into()));
        assert_eq!(log.meta.get("segments"), Some(&52u64.into()));
        assert_eq!(log.participants, vec!["node1".to_string()]);
    }

    #[test]
    fn a_job_with_no_worker_evidence_yields_no_nodes() {
        // A proof whose worker lines were all dropped keeps its job record for timing but names no
        // node, since it has neither items nor input-file-written times.
        let log = build_log(&raw_job(), None, None, None, None);
        assert!(log.nodes.is_empty() && log.participants.is_empty());
        assert_eq!(log.status, LogStatus::Success);
    }

    #[test]
    fn the_sub_step_spans_do_not_shift_the_phase_envelopes() {
        // The segment and recursion envelopes read the monolithic item windows, so an item's STARK
        // sub-step spans, which the wire emits as component rows in place of the item, never move
        // its phase window. A new-format node keeps the identical phase envelopes of the same node
        // without spans.
        let phases_of = |items: JobItems| {
            let by_job = BTreeMap::from([(raw_job().id.clone(), items)]);
            build_log(&raw_job(), by_job.get(&raw_job().id), None, None, None).nodes[0]
                .phases
                .clone()
        };
        let plain: JobItems = BTreeMap::from([(
            "node1".to_string(),
            vec![
                span(APP_SEGMENT, 1_002_000, 1_003_000),
                span(LEAF, 1_003_000, 1_003_500),
            ],
        )]);
        let spanned = |kind: usize, start: i64, end: i64, spans: Vec<[u64; 2]>| PipelineItem {
            meta: PipelineItemMeta {
                spans,
                spans_window: Some((start, end)),
                ..Default::default()
            },
            ..span(kind, start, end)
        };
        let with_spans: JobItems = BTreeMap::from([(
            "node1".to_string(),
            vec![
                spanned(APP_SEGMENT, 1_002_000, 1_003_000, vec![[0, 400], [1, 500]]),
                spanned(LEAF, 1_003_000, 1_003_500, vec![[7, 200]]),
            ],
        )]);
        assert_eq!(phases_of(plain), phases_of(with_spans));
    }
}
