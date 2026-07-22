//! Extracts each node's fine pipeline items from the zisk worker marker brackets.
//!
//! The kinds table below is the single source for the wire template and the marker classifier, so
//! the template indices and the extracted items can never drift apart. The worker brackets every
//! sub-step between ">>>" and "<<<" markers at mixed log levels, so the scanner is level agnostic
//! and matches stems rather than line shapes. Pairing runs in two layers per job and node. Layer A
//! joins each close to its pending open per stem and instance or tag, using the printed duration
//! to pick among overlapping brackets. Layer B assembles a completed witness interval and the
//! compute bracket that claims it into one item, the claim taken at the compute open so a dangling
//! compute keeps its witness. The thread-interleaved logger usually prints a compute open
//! microseconds before its witness close, so a witness that closes after its compute opened
//! attaches late to the earliest unclaimed compute open of its kind. A close with no pending open
//! and a marker outside any job are dropped, and a job's leftover state flushes as dangling and
//! lone items when the next job starts, the process restarts, or the log ends.

use std::collections::{BTreeMap, VecDeque};

use crate::parse_benchmark::input::log::{
    PipelineDef, PipelineItem, Ts, job_prefix,
    zkvm::zisk::worker::{has_ts, is_restart_banner, leading_ts},
};

/// A marker stem as printed after the bracket arrows, with whether an underscore and instance
/// digits follow it in the log.
struct Stem {
    text: &'static str,
    numbered: bool,
}

/// How a paired kind's compute open claims a completed witness interval.
#[derive(Clone, Copy)]
enum Claim {
    /// The witness holding the same instance digits, a single held interval per instance, which
    /// for an unnumbered stem degenerates to the kind's single slot.
    ByX,
    /// The earliest unconsumed witness under the compute marker's bracket tag.
    TagFifo,
    /// The earliest unconsumed witness in the job regardless of tags.
    GlobalFifo,
}

/// The witness half of a paired kind, its stem and how compute opens claim its intervals.
struct Witness {
    stem: Stem,
    claim: Claim,
}

/// One fine-pipeline kind, its wire identity and the marker stems that produce it.
struct Kind {
    name: &'static str,
    label: &'static str,
    phase: &'static str,
    compute: Stem,
    witness: Option<Witness>,
}

/// Builds a stem, keeping each kinds row below to a single readable line.
const fn stem(text: &'static str, numbered: bool) -> Stem {
    Stem { text, numbered }
}

/// Builds an unpaired kind whose sole bracket is its compute stem.
const fn single(
    name: &'static str,
    label: &'static str,
    phase: &'static str,
    compute: Stem,
) -> Kind {
    Kind {
        name,
        label,
        phase,
        compute,
        witness: None,
    }
}

/// Builds a paired kind, its witness stem and claim policy joined to its compute stem.
const fn paired(
    name: &'static str,
    label: &'static str,
    phase: &'static str,
    witness: Stem,
    claim: Claim,
    compute: Stem,
) -> Kind {
    Kind {
        name,
        label,
        phase,
        compute,
        witness: Some(Witness {
            stem: witness,
            claim,
        }),
    }
}

/// The zisk fine-pipeline kinds in template order. The wc witness stem appears under both the
/// contribution and proof kinds and splits on the prove boundary at classification, where the
/// commit-phase row precedes the prove-phase row. Any stem absent from this table is ignored.
static KINDS: [Kind; 13] = [
    single(
        "minimal_trace",
        "Minimal Trace",
        "emulation",
        stem("COMPUTE_MINIMAL_TRACE", false),
    ),
    single("plan", "Plan", "commit", stem("PLAN", false)),
    paired(
        "wc_contribution",
        "Witness + Contribution",
        "commit",
        stem("GENERATING_WC", true),
        Claim::ByX,
        stem("GET_CONTRIBUTION_AIR", true),
    ),
    single(
        "mem_plan_wait",
        "Mem Plan Wait",
        "commit",
        stem("WAIT_PLAN_MEM_CPP", false),
    ),
    single(
        "mem_plan_collect",
        "Collect Mem Plans",
        "commit",
        stem("COLLECT_MEM_PLANS", false),
    ),
    single(
        "tables",
        "Table Witness",
        "commit",
        stem("CALCULATING_TABLES", false),
    ),
    single(
        "internal_contribution",
        "Internal Contribution",
        "commit",
        stem("CALCULATE_INTERNAL_CONTRIBUTION", false),
    ),
    paired(
        "wc_proof",
        "Witness + Proof",
        "prove",
        stem("GENERATING_WC", true),
        Claim::ByX,
        stem("GEN_PROOF", true),
    ),
    paired(
        "recursive1",
        "Recursive1 Witness + Proof",
        "prove",
        stem("GENERATING_RECURSIVE1_WITNESS", true),
        Claim::TagFifo,
        stem("GEN_RECURSIVE_PROOF_Recursive1", false),
    ),
    paired(
        "recursive2",
        "Aggregation + Recursive2 Proof",
        "prove",
        stem("GENERATE_WITNESS_AGGREGATION", false),
        Claim::GlobalFifo,
        stem("GEN_RECURSIVE_PROOF_Recursive2", false),
    ),
    paired(
        "compressor",
        "Compressor Witness + Proof",
        "prove",
        stem("GENERATING_COMPRESSOR_WITNESS", true),
        Claim::TagFifo,
        stem("GEN_RECURSIVE_PROOF_Compressor", false),
    ),
    paired(
        "vadcop_final",
        "VadcopFinal Witness + Proof",
        "aggregate",
        stem("GENERATE_VADCOP_FINAL_PROOF_WITNESS", false),
        Claim::ByX,
        stem("GEN_RECURSIVE_PROOF_VadcopFinal", false),
    ),
    paired(
        "vadcop_final_compressed",
        "VadcopFinalCompressed Witness + Proof",
        "aggregate",
        stem("GENERATE_VADCOP_FINAL_COMPRESSED_PROOF_WITNESS", false),
        Claim::ByX,
        stem("GEN_RECURSIVE_PROOF_VadcopFinalCompressed", false),
    ),
];

/// Returns the ordered zisk fine-pipeline template.
pub fn zisk_pipeline() -> Vec<PipelineDef> {
    KINDS
        .iter()
        .map(|k| PipelineDef {
            name: k.name.to_string(),
            label: k.label.to_string(),
            phase: k.phase.to_string(),
            paired: k.witness.is_some(),
        })
        .collect()
}

/// Which half of a kind a marker stem names.
#[derive(Clone, Copy, PartialEq)]
enum Side {
    Witness,
    Compute,
}

/// A classified marker token, the kind and side its stem names, the stem text keying layer A, and
/// the instance digits when the stem carries them.
struct Hit<'a> {
    kind: usize,
    side: Side,
    stem: &'static str,
    x: Option<&'a str>,
}

/// Classifies a marker token against the kinds table, whole-token stems first and then a stem with
/// trailing instance digits after an underscore, so a stem that itself ends in a digit is never
/// corrupted by blind suffix stripping. An unlisted token yields None.
fn classify(token: &str, proof_side: bool) -> Option<Hit<'_>> {
    if let Some(hit) = lookup(token, false, None, proof_side) {
        return Some(hit);
    }
    let (base, digits) = token.rsplit_once('_')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    lookup(base, true, Some(digits), proof_side)
}

/// Finds the kind and side a stem names. A witness stem shared by two kinds names the earlier
/// table row while a contribution job runs and the later one otherwise, which is how the wc stem
/// splits between the contribution and proof kinds.
fn lookup<'a>(text: &str, numbered: bool, x: Option<&'a str>, proof_side: bool) -> Option<Hit<'a>> {
    let mut found: Option<Hit<'a>> = None;
    for (kind, k) in KINDS.iter().enumerate() {
        if k.compute.numbered == numbered && k.compute.text == text {
            return Some(Hit {
                kind,
                side: Side::Compute,
                stem: k.compute.text,
                x,
            });
        }
        if let Some(w) = &k.witness
            && w.stem.numbered == numbered
            && w.stem.text == text
            && (found.is_none() || proof_side)
        {
            found = Some(Hit {
                kind,
                side: Side::Witness,
                stem: w.stem.text,
                x,
            });
        }
    }
    found
}

/// A pending bracket open, the kind and side its stem resolved to, its open instant, and the
/// witness interval a compute open claimed.
struct Open {
    kind: usize,
    side: Side,
    ts: i64,
    claimed: Option<(i64, i64)>,
}

/// The pairing state of one job on one node, flushed when the job ends.
struct JobState {
    job: String,
    skip: bool,
    /// Layer A pending opens per stem and instance-or-tag suffix, in open order.
    opens: BTreeMap<(&'static str, String), Vec<Open>>,
    /// Completed witness intervals a ByX compute claims, per kind and instance.
    witnesses: BTreeMap<(usize, String), (i64, i64)>,
    /// Completed witness queues the TagFifo and GlobalFifo computes pop, per kind and tag with the
    /// global queue under the empty tag.
    queues: BTreeMap<(usize, String), VecDeque<(i64, i64)>>,
    items: Vec<PipelineItem>,
}

impl JobState {
    /// Creates the empty pairing state for a job, skipping collection on a re-dispatch.
    fn new(job: String, skip: bool) -> JobState {
        JobState {
            job,
            skip,
            opens: BTreeMap::new(),
            witnesses: BTreeMap::new(),
            queues: BTreeMap::new(),
            items: Vec::new(),
        }
    }

    /// Records a bracket open. A compute open claims its completed witness here rather than at its
    /// close, so a dangling compute keeps the witness it consumed.
    fn open(&mut self, hit: Hit, tag: &str, ts: i64) {
        let claimed = match hit.side {
            Side::Compute => self.claim(hit.kind, hit.x, tag),
            Side::Witness => None,
        };
        self.opens
            .entry((hit.stem, hit.x.unwrap_or(tag).to_string()))
            .or_default()
            .push(Open {
                kind: hit.kind,
                side: hit.side,
                ts,
                claimed,
            });
    }

    /// Joins a bracket close to its pending open, returning whether this job held one so the
    /// caller can route a miss to the other job slot. A completed witness attaches late to a
    /// pending compute open of its kind or is stored for a later compute claim, under the kind its
    /// own open resolved to, and a completed compute emits its item.
    fn close(&mut self, hit: &Hit, tag: &str, ts: i64, duration: Option<i64>) -> bool {
        let Some(pending) = self
            .opens
            .get_mut(&(hit.stem, hit.x.unwrap_or(tag).to_string()))
        else {
            return false;
        };
        let Some(open) = take_open(pending, ts, duration) else {
            return false;
        };
        match open.side {
            Side::Witness => {
                let (x, interval) = (hit.x.unwrap_or(""), (open.ts, ts));
                if !self.late_attach(open.kind, x, tag, interval) {
                    self.store(open.kind, x, tag, interval);
                }
            }
            Side::Compute => self.items.push(item(open, Some(ts))),
        }
        true
    }

    /// Attaches a completed witness interval to the earliest pending compute open of its kind
    /// whose claim came up empty, keyed like the kind's claim policy, because the interleaved
    /// logger usually prints the compute open before the witness close. Returns false when no such
    /// open is pending, leaving the interval for a later compute open to claim.
    fn late_attach(&mut self, kind: usize, x: &str, tag: &str, interval: (i64, i64)) -> bool {
        let compute = &KINDS[kind].compute;
        let key = match KINDS[kind].witness.as_ref().unwrap().claim {
            Claim::ByX => compute.numbered.then_some(x),
            Claim::TagFifo => Some(tag),
            Claim::GlobalFifo => None,
        };
        let Some(open) = self
            .opens
            .iter_mut()
            .filter(|((stem, suffix), _)| {
                *stem == compute.text && key.is_none_or(|k| k == suffix.as_str())
            })
            .flat_map(|(_, pending)| pending.iter_mut())
            .filter(|open| open.claimed.is_none())
            .min_by_key(|open| open.ts)
        else {
            return false;
        };
        open.claimed = Some(interval);
        true
    }

    /// Stores a completed witness interval where its kind's claim policy looks for it.
    fn store(&mut self, kind: usize, x: &str, tag: &str, interval: (i64, i64)) {
        match KINDS[kind].witness.as_ref().unwrap().claim {
            Claim::ByX => {
                self.witnesses.insert((kind, x.to_string()), interval);
            }
            Claim::TagFifo => self
                .queues
                .entry((kind, tag.to_string()))
                .or_default()
                .push_back(interval),
            Claim::GlobalFifo => self
                .queues
                .entry((kind, String::new()))
                .or_default()
                .push_back(interval),
        }
    }

    /// Claims the completed witness a compute open consumes, per its kind's claim policy. A miss
    /// leaves the item compute-only, the normal state for a cached witness.
    fn claim(&mut self, kind: usize, x: Option<&str>, tag: &str) -> Option<(i64, i64)> {
        match KINDS[kind].witness.as_ref()?.claim {
            Claim::ByX => self.witnesses.remove(&(kind, x.unwrap_or("").to_string())),
            Claim::TagFifo => self.queues.get_mut(&(kind, tag.to_string()))?.pop_front(),
            Claim::GlobalFifo => self.queues.get_mut(&(kind, String::new()))?.pop_front(),
        }
    }

    /// The best duration distance among this job's pending opens for a close, None when none pend,
    /// zero without a printed duration so slot preference decides.
    fn close_distance(&self, hit: &Hit, tag: &str, ts: i64, duration: Option<i64>) -> Option<i64> {
        let pending = self
            .opens
            .get(&(hit.stem, hit.x.unwrap_or(tag).to_string()))?;
        let distances = pending.iter().map(|open| match duration {
            Some(d) => (ts - d - open.ts).abs(),
            None => 0,
        });
        distances.min()
    }
}

/// Pops the pending open a close pairs with. The printed duration back-computes the open instant
/// and picks the nearest pending open, earliest on a tie, which pairs overlapping brackets whose
/// completions arrive out of order. Without a duration the earliest open is taken.
fn take_open(pending: &mut Vec<Open>, close_ts: i64, duration: Option<i64>) -> Option<Open> {
    if pending.is_empty() {
        return None;
    }
    let index = match duration {
        Some(d) => pending
            .iter()
            .enumerate()
            .min_by_key(|(_, open)| ((close_ts - d - open.ts).abs(), open.ts))
            .map(|(i, _)| i)
            .unwrap(),
        None => 0,
    };
    Some(pending.remove(index))
}

/// Builds the item an open produced, the claimed witness first and the compute segment second, or
/// the open's own segment alone when nothing was claimed. A None end marks a segment the flush
/// left dangling, and a pending witness open flushes through the same unclaimed shape.
fn item(open: Open, end: Option<i64>) -> PipelineItem {
    match open.claimed {
        Some((start, witness_end)) => PipelineItem {
            kind: open.kind,
            first: (start, Some(witness_end)),
            second: Some((open.ts, end)),
        },
        None => PipelineItem {
            kind: open.kind,
            first: (open.ts, end),
            second: None,
        },
    }
}

/// Flushes a job's leftover pairing state and records the node's items, a pending open becoming a
/// dangling item and an unclaimed completed witness a lone segment. A skipped re-dispatch records
/// nothing, keeping the first run's items.
fn flush(state: JobState, node: &str, pipelines: &mut BTreeMap<String, JobPipelines>) {
    if state.skip {
        return;
    }
    let JobState {
        job,
        opens,
        witnesses,
        queues,
        mut items,
        ..
    } = state;
    for open in opens.into_values().flatten() {
        items.push(item(open, None));
    }
    let lone = |kind: usize, (start, end): (i64, i64)| PipelineItem {
        kind,
        first: (start, Some(end)),
        second: None,
    };
    items.extend(witnesses.into_iter().map(|((kind, _), w)| lone(kind, w)));
    items.extend(
        queues
            .into_iter()
            .flat_map(|((kind, _), queue)| queue.into_iter().map(move |w| lone(kind, w))),
    );
    pipelines
        .entry(job)
        .or_default()
        .entry(node.to_string())
        .or_insert(items);
}

/// Per-node pipeline items for one job, keyed by node id, the value of the per-job map.
pub type JobPipelines = BTreeMap<String, Vec<PipelineItem>>;

/// Parses one ansi-stripped worker log's pipeline markers into per-job items for the given node,
/// keyed by the eight-hex job prefix like the stage map. The contribution and prove jobs are
/// tracked in separate slots like the stage parser because zisk may dispatch the next job's
/// contribution while the current job proves. Starting Partial Contribution flushes the old
/// contribution slot, and a re-dispatched job keeps its first run's items, the module's
/// first-write-wins convention. Starting Prove flushes the old prove slot and promotes the
/// contribution slot when it holds the same job. An open routes to the slot owning its kind's
/// phase, with the wc stem resolving to the contribution kind while a contribution runs, a close
/// routes to whichever slot holds its pending open, and the prove-success line leaves the prove
/// slot open because the aggregator's vadcop brackets follow inside the same job window. A restart
/// banner mid-job flushes both slots' pending opens as dangling items, so the restart's own
/// brackets attach to nothing.
pub fn parse(
    clean: &str,
    node: &str,
    pipelines: &mut BTreeMap<String, JobPipelines>,
) -> crate::parse_benchmark::Result<()> {
    let mut p1: Option<JobState> = None;
    let mut p2: Option<JobState> = None;
    for line in clean.lines() {
        if !has_ts(line) {
            if is_restart_banner(line) {
                // The prove slot flushes first so a re-dispatched contribution of the same job
                // never outraces the first run under first-write-wins.
                for s in [p2.take(), p1.take()].into_iter().flatten() {
                    flush(s, node, pipelines);
                }
            }
            continue;
        }
        if let Some((_, uuid)) = line.split_once(" INFO: Starting Partial Contribution for ") {
            if let Some(s) = p1.take() {
                flush(s, node, pipelines);
            }
            p1 = Some(fresh(uuid, node, pipelines));
            continue;
        }
        if let Some((_, id)) = line.split_once(" INFO: Starting Prove for JobId(") {
            if let Some(s) = p2.take() {
                flush(s, node, pipelines);
            }
            p2 = p1
                .take_if(|s| s.job == job_prefix(id))
                .or_else(|| Some(fresh(id, node, pipelines)));
            continue;
        }
        if p1.is_none() && p2.is_none() {
            continue;
        }
        let (is_open, tail) = if let Some(i) = line.find(" >>> ") {
            (true, &line[i + 5..])
        } else if let Some(i) = line.find(" <<< ") {
            (false, &line[i + 5..])
        } else {
            continue;
        };
        let mut tokens = tail.split_ascii_whitespace();
        // The decorative startup banners put a bracketed rank first, which no stem matches.
        let Some(hit) = tokens.next().and_then(|t| classify(t, p1.is_none())) else {
            continue;
        };
        let mut tag = "";
        let mut duration = None;
        for token in tokens {
            if let Some(inner) = token.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
                tag = inner;
            } else if let Some(ms) = token.strip_prefix('(').and_then(|t| t.strip_suffix("ms)")) {
                duration = ms.parse::<i64>().ok();
            }
        }
        let ts = Ts::parse(leading_ts(line))?.epoch_ms();
        if is_open {
            let slot = match KINDS[hit.kind].phase {
                "emulation" | "commit" => &mut p1,
                _ => &mut p2,
            };
            if let Some(s) = slot.as_mut().filter(|s| !s.skip) {
                s.open(hit, tag, ts);
            }
        } else {
            // Both slots can hold a pending open under the close's key when the same instance runs
            // in the proving job and the next contribution, so the printed duration picks the slot
            // whose open it back-computes to, the contribution slot on a tie or with no duration.
            let d1 = p1
                .as_ref()
                .and_then(|s| s.close_distance(&hit, tag, ts, duration));
            let d2 = p2
                .as_ref()
                .and_then(|s| s.close_distance(&hit, tag, ts, duration));
            let prove_first = matches!((d1, d2), (Some(a), Some(b)) if b < a);
            let (first, second) = if prove_first {
                (&mut p2, &mut p1)
            } else {
                (&mut p1, &mut p2)
            };
            if !first
                .as_mut()
                .is_some_and(|s| s.close(&hit, tag, ts, duration))
                && let Some(s) = second.as_mut()
            {
                s.close(&hit, tag, ts, duration);
            }
        }
    }
    for s in [p2, p1].into_iter().flatten() {
        flush(s, node, pipelines);
    }
    Ok(())
}

/// Creates the pairing state for a newly announced job, skipping collection when the node already
/// recorded the job, the re-dispatch first-write-wins convention.
fn fresh(id: &str, node: &str, pipelines: &BTreeMap<String, JobPipelines>) -> JobState {
    let key = job_prefix(id);
    let skip = pipelines
        .get(&key)
        .is_some_and(|jobs| jobs.contains_key(node));
    JobState::new(key, skip)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::parse_benchmark::input::log::{
        PipelineItem, Ts,
        zkvm::zisk::{
            pipeline::{parse, zisk_pipeline},
            zisk_phases,
        },
    };

    /// The epoch milliseconds of an RFC-3339 instant, for comparing item segments.
    fn ms(value: &str) -> i64 {
        Ts::parse(value).unwrap().epoch_ms()
    }

    /// Parses a log for node1 and returns the items of the given job.
    fn parse_job(log: &str, job: &str) -> Vec<PipelineItem> {
        let mut map = BTreeMap::new();
        parse(log, "node1", &mut map).unwrap();
        map.remove(job)
            .expect("job items")
            .remove("node1")
            .expect("node1 items")
    }

    // The loader ansi-strips before calling the parser, so the tests pass already-clean text. A wc
    // bracket before Starting Prove is a contribution witness and one after it is a proof witness,
    // each claimed by its own compute stem, with each compute open printed before its witness
    // close the way the interleaved logger orders the real logs.
    const WC_SPLIT_LOG: &str = "\
2026-06-02T06:36:20.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for aaaa1111-0000-0000-0000-000000000000
2026-06-02T06:36:21.000000Z proofman::proofman DEBUG: >>> GENERATING_WC_2 [0:12]
2026-06-02T06:36:21.600000Z proofman::proofman DEBUG: >>> GET_CONTRIBUTION_AIR_2 [0:12]
2026-06-02T06:36:21.500000Z proofman::proofman DEBUG: <<< GENERATING_WC_2 [0:12] (500ms)
2026-06-02T06:36:21.800000Z proofman::proofman DEBUG: <<< GET_CONTRIBUTION_AIR_2 [0:12] (200ms)
2026-06-02T06:36:22.000000Z zisk_worker::worker_node INFO: Starting Prove for JobId(aaaa1111\u{2026})
2026-06-02T06:36:22.100000Z proofman::proofman DEBUG: >>> GENERATING_WC_2 [0:12]
2026-06-02T06:36:22.500000Z proofman::proofman DEBUG: >>> GEN_PROOF_2 [0:12]
2026-06-02T06:36:22.400000Z proofman::proofman DEBUG: <<< GENERATING_WC_2 [0:12] (300ms)
2026-06-02T06:36:22.900000Z proofman::proofman DEBUG: <<< GEN_PROOF_2 [0:12] (400ms)
";

    #[test]
    fn wc_splits_between_commit_and_prove_by_the_starting_prove_marker() {
        let items = parse_job(WC_SPLIT_LOG, "aaaa1111");
        assert_eq!(items.len(), 2);
        // The pre-prove wc pairs the contribution kind with both segments complete.
        assert_eq!(items[0].kind, 2);
        assert_eq!(
            items[0].first,
            (
                ms("2026-06-02T06:36:21.000Z"),
                Some(ms("2026-06-02T06:36:21.500Z"))
            )
        );
        assert_eq!(
            items[0].second,
            Some((
                ms("2026-06-02T06:36:21.600Z"),
                Some(ms("2026-06-02T06:36:21.800Z"))
            ))
        );
        // The post-prove wc pairs the proof kind, untouched by the earlier contribution pair.
        assert_eq!(items[1].kind, 7);
        assert_eq!(
            items[1].first,
            (
                ms("2026-06-02T06:36:22.100Z"),
                Some(ms("2026-06-02T06:36:22.400Z"))
            )
        );
        assert_eq!(
            items[1].second,
            Some((
                ms("2026-06-02T06:36:22.500Z"),
                Some(ms("2026-06-02T06:36:22.900Z"))
            ))
        );
    }

    // The logger prints the compute open a few microseconds before the witness close it consumes,
    // the usual thread interleave in the real logs, which the late attach still pairs.
    const INTERLEAVED_WC_LOG: &str = "\
2026-06-02T06:36:20.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for aaaa1111-0000-0000-0000-000000000000
2026-06-02T06:36:21.000000Z proofman::proofman DEBUG: >>> GENERATING_WC_2 [0:12]
2026-06-02T06:36:21.604000Z proofman::proofman DEBUG: >>> GET_CONTRIBUTION_AIR_2 [0:12]
2026-06-02T06:36:21.604013Z proofman::proofman DEBUG: <<< GENERATING_WC_2 [0:12] (604ms)
2026-06-02T06:36:21.804000Z proofman::proofman DEBUG: <<< GET_CONTRIBUTION_AIR_2 [0:12] (200ms)
";

    #[test]
    fn a_witness_closing_after_its_compute_opened_still_pairs() {
        let items = parse_job(INTERLEAVED_WC_LOG, "aaaa1111");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, 2);
        assert_eq!(
            items[0].first,
            (
                ms("2026-06-02T06:36:21.000000Z"),
                Some(ms("2026-06-02T06:36:21.604013Z"))
            )
        );
        assert_eq!(
            items[0].second,
            Some((
                ms("2026-06-02T06:36:21.604000Z"),
                Some(ms("2026-06-02T06:36:21.804000Z"))
            ))
        );
    }

    // A proof bracket with no post-prove wc for its instance, the shape a cached witness leaves.
    const CACHED_WITNESS_LOG: &str = "\
2026-06-02T06:36:20.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for aaaa1111-0000-0000-0000-000000000000
2026-06-02T06:36:22.000000Z zisk_worker::worker_node INFO: Starting Prove for JobId(aaaa1111\u{2026})
2026-06-02T06:36:22.500000Z proofman::proofman DEBUG: >>> GEN_PROOF_6 [0:12]
2026-06-02T06:36:22.900000Z proofman::proofman DEBUG: <<< GEN_PROOF_6 [0:12] (400ms)
";

    #[test]
    fn a_cached_witness_yields_a_compute_only_item() {
        let items = parse_job(CACHED_WITNESS_LOG, "aaaa1111");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, 7);
        assert_eq!(
            items[0].first,
            (
                ms("2026-06-02T06:36:22.500Z"),
                Some(ms("2026-06-02T06:36:22.900Z"))
            )
        );
        assert_eq!(items[0].second, None);
    }

    // A table air's contribution runs without any wc bracket for its instance.
    const TABLE_AIR_LOG: &str = "\
2026-06-02T06:36:20.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for aaaa1111-0000-0000-0000-000000000000
2026-06-02T06:36:21.000000Z proofman::proofman DEBUG: >>> GET_CONTRIBUTION_AIR_1340 [0:19]
2026-06-02T06:36:21.200000Z proofman::proofman DEBUG: <<< GET_CONTRIBUTION_AIR_1340 [0:19] (200ms)
";

    #[test]
    fn a_table_air_without_wc_is_compute_only() {
        let items = parse_job(TABLE_AIR_LOG, "aaaa1111");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, 2);
        assert_eq!(
            items[0].first,
            (
                ms("2026-06-02T06:36:21.000Z"),
                Some(ms("2026-06-02T06:36:21.200Z"))
            )
        );
        assert_eq!(items[0].second, None);
    }

    // Three recursive1 witnesses complete under two tags before any proof runs, so each proof must
    // claim the earliest unconsumed witness of its own tag.
    const RECURSIVE1_LOG: &str = "\
2026-06-02T06:36:20.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for aaaa1111-0000-0000-0000-000000000000
2026-06-02T06:36:22.000000Z zisk_worker::worker_node INFO: Starting Prove for JobId(aaaa1111\u{2026})
2026-06-02T06:36:23.000000Z proofman::recursion DEBUG: >>> GENERATING_RECURSIVE1_WITNESS_10 [0:1]
2026-06-02T06:36:23.100000Z proofman::recursion DEBUG: <<< GENERATING_RECURSIVE1_WITNESS_10 [0:1] (100ms)
2026-06-02T06:36:23.200000Z proofman::recursion DEBUG: >>> GENERATING_RECURSIVE1_WITNESS_14 [0:1]
2026-06-02T06:36:23.300000Z proofman::recursion DEBUG: <<< GENERATING_RECURSIVE1_WITNESS_14 [0:1] (100ms)
2026-06-02T06:36:23.400000Z proofman::recursion DEBUG: >>> GENERATING_RECURSIVE1_WITNESS_20 [0:2]
2026-06-02T06:36:23.500000Z proofman::recursion DEBUG: <<< GENERATING_RECURSIVE1_WITNESS_20 [0:2] (100ms)
2026-06-02T06:36:24.000000Z proofman::recursion DEBUG: >>> GEN_RECURSIVE_PROOF_Recursive1 [0:2]
2026-06-02T06:36:24.100000Z proofman::recursion DEBUG: <<< GEN_RECURSIVE_PROOF_Recursive1 [0:2] (100ms)
2026-06-02T06:36:24.200000Z proofman::recursion DEBUG: >>> GEN_RECURSIVE_PROOF_Recursive1 [0:1]
2026-06-02T06:36:24.300000Z proofman::recursion DEBUG: <<< GEN_RECURSIVE_PROOF_Recursive1 [0:1] (100ms)
2026-06-02T06:36:24.400000Z proofman::recursion DEBUG: >>> GEN_RECURSIVE_PROOF_Recursive1 [0:1]
2026-06-02T06:36:24.500000Z proofman::recursion DEBUG: <<< GEN_RECURSIVE_PROOF_Recursive1 [0:1] (100ms)
";

    #[test]
    fn recursive1_pairs_witness_to_proof_by_tag_fifo() {
        let items = parse_job(RECURSIVE1_LOG, "aaaa1111");
        assert_eq!(items.len(), 3);
        assert!(items.iter().all(|i| i.kind == 8));
        // The tag 0:2 proof takes the tag's only witness, not the earlier 0:1 ones.
        assert_eq!(items[0].first.0, ms("2026-06-02T06:36:23.400Z"));
        assert_eq!(items[0].second.unwrap().0, ms("2026-06-02T06:36:24.000Z"));
        // The two 0:1 proofs take their tag's witnesses in completion order.
        assert_eq!(items[1].first.0, ms("2026-06-02T06:36:23.000Z"));
        assert_eq!(items[1].second.unwrap().0, ms("2026-06-02T06:36:24.200Z"));
        assert_eq!(items[2].first.0, ms("2026-06-02T06:36:23.200Z"));
        assert_eq!(items[2].second.unwrap().0, ms("2026-06-02T06:36:24.400Z"));
    }

    // Two aggregation witnesses overlap and their closes arrive in the reverse of open order, so
    // only the printed durations pair them correctly where first-open-first-close would not.
    const OVERLAP_LOG: &str = "\
2026-06-02T06:36:20.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for aaaa1111-0000-0000-0000-000000000000
2026-06-02T06:36:20.500000Z zisk_worker::worker_node INFO: Starting Prove for JobId(aaaa1111\u{2026})
2026-06-02T06:36:21.000000Z proofman::recursion DEBUG: >>> GENERATE_WITNESS_AGGREGATION
2026-06-02T06:36:22.000000Z proofman::recursion DEBUG: >>> GENERATE_WITNESS_AGGREGATION
2026-06-02T06:36:22.050000Z proofman::recursion DEBUG: <<< GENERATE_WITNESS_AGGREGATION (50ms)
2026-06-02T06:36:22.100000Z proofman::recursion DEBUG: <<< GENERATE_WITNESS_AGGREGATION (1100ms)
";

    #[test]
    fn overlapping_closes_resolve_by_printed_duration() {
        let items = parse_job(OVERLAP_LOG, "aaaa1111");
        // Both witnesses flush unclaimed as lone segments, whose bounds show the pairing.
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.kind == 9 && i.second.is_none()));
        let short = (
            ms("2026-06-02T06:36:22.000Z"),
            Some(ms("2026-06-02T06:36:22.050Z")),
        );
        let long = (
            ms("2026-06-02T06:36:21.000Z"),
            Some(ms("2026-06-02T06:36:22.100Z")),
        );
        assert!(items.iter().any(|i| i.first == short));
        assert!(items.iter().any(|i| i.first == long));
    }

    // A completed aggregation witness followed by a recursive2 proof, whose 0:0 tag the global
    // queue ignores.
    const RECURSIVE2_LOG: &str = "\
2026-06-02T06:36:20.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for aaaa1111-0000-0000-0000-000000000000
2026-06-02T06:36:20.500000Z zisk_worker::worker_node INFO: Starting Prove for JobId(aaaa1111\u{2026})
2026-06-02T06:36:21.000000Z proofman::recursion DEBUG: >>> GENERATE_WITNESS_AGGREGATION
2026-06-02T06:36:21.500000Z proofman::recursion DEBUG: <<< GENERATE_WITNESS_AGGREGATION (500ms)
2026-06-02T06:36:21.600000Z proofman::recursion DEBUG: >>> GEN_RECURSIVE_PROOF_Recursive2 [0:0]
2026-06-02T06:36:21.900000Z proofman::recursion DEBUG: <<< GEN_RECURSIVE_PROOF_Recursive2 [0:0] (300ms)
";

    #[test]
    fn aggregation_witness_pairs_the_next_recursive2_proof() {
        let items = parse_job(RECURSIVE2_LOG, "aaaa1111");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, 9);
        assert_eq!(
            items[0].first,
            (
                ms("2026-06-02T06:36:21.000Z"),
                Some(ms("2026-06-02T06:36:21.500Z"))
            )
        );
        assert_eq!(
            items[0].second,
            Some((
                ms("2026-06-02T06:36:21.600Z"),
                Some(ms("2026-06-02T06:36:21.900Z"))
            ))
        );
    }

    // Two aggregation witnesses complete before either recursive2 proof opens, so the global queue
    // hands them out in completion order and the proofs' differing bracket tags carry no weight.
    const GLOBAL_FIFO_LOG: &str = "\
2026-06-02T06:36:20.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for aaaa1111-0000-0000-0000-000000000000
2026-06-02T06:36:20.500000Z zisk_worker::worker_node INFO: Starting Prove for JobId(aaaa1111\u{2026})
2026-06-02T06:36:21.000000Z proofman::recursion DEBUG: >>> GENERATE_WITNESS_AGGREGATION
2026-06-02T06:36:21.200000Z proofman::recursion DEBUG: <<< GENERATE_WITNESS_AGGREGATION (200ms)
2026-06-02T06:36:21.300000Z proofman::recursion DEBUG: >>> GENERATE_WITNESS_AGGREGATION
2026-06-02T06:36:21.500000Z proofman::recursion DEBUG: <<< GENERATE_WITNESS_AGGREGATION (200ms)
2026-06-02T06:36:22.000000Z proofman::recursion DEBUG: >>> GEN_RECURSIVE_PROOF_Recursive2 [0:1]
2026-06-02T06:36:22.300000Z proofman::recursion DEBUG: <<< GEN_RECURSIVE_PROOF_Recursive2 [0:1] (300ms)
2026-06-02T06:36:22.400000Z proofman::recursion DEBUG: >>> GEN_RECURSIVE_PROOF_Recursive2 [0:0]
2026-06-02T06:36:22.700000Z proofman::recursion DEBUG: <<< GEN_RECURSIVE_PROOF_Recursive2 [0:0] (300ms)
";

    #[test]
    fn global_fifo_pops_queued_aggregation_witnesses_in_completion_order() {
        let items = parse_job(GLOBAL_FIFO_LOG, "aaaa1111");
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.kind == 9));
        // The first proof takes the witness that completed first, the second the later one.
        assert_eq!(
            items[0].first,
            (
                ms("2026-06-02T06:36:21.000Z"),
                Some(ms("2026-06-02T06:36:21.200Z"))
            )
        );
        assert_eq!(
            items[0].second,
            Some((
                ms("2026-06-02T06:36:22.000Z"),
                Some(ms("2026-06-02T06:36:22.300Z"))
            ))
        );
        assert_eq!(
            items[1].first,
            (
                ms("2026-06-02T06:36:21.300Z"),
                Some(ms("2026-06-02T06:36:21.500Z"))
            )
        );
        assert_eq!(
            items[1].second,
            Some((
                ms("2026-06-02T06:36:22.400Z"),
                Some(ms("2026-06-02T06:36:22.700Z"))
            ))
        );
    }

    // The aggregator's vadcop brackets follow the prove-success line inside the same job window,
    // wrapped in unlisted proof stems.
    const VADCOP_LOG: &str = "\
2026-06-02T06:36:20.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for aaaa1111-0000-0000-0000-000000000000
2026-06-02T06:36:22.000000Z zisk_worker::worker_node INFO: Starting Prove for JobId(aaaa1111\u{2026})
2026-06-02T06:36:30.000000Z zisk_worker::worker INFO: Prove computation successful for JobId(aaaa1111\u{2026})
2026-06-02T06:36:31.000000Z proofman::recursion INFO: >>> GENERATE_VADCOP_FINAL_PROOF
2026-06-02T06:36:31.100000Z proofman::recursion DEBUG: >>> GENERATE_VADCOP_FINAL_PROOF_WITNESS
2026-06-02T06:36:31.200000Z proofman::recursion DEBUG: <<< GENERATE_VADCOP_FINAL_PROOF_WITNESS (100ms)
2026-06-02T06:36:31.300000Z proofman::recursion DEBUG: >>> GEN_RECURSIVE_PROOF_VadcopFinal [0:0]
2026-06-02T06:36:31.400000Z proofman::recursion DEBUG: <<< GEN_RECURSIVE_PROOF_VadcopFinal [0:0] (100ms)
2026-06-02T06:36:31.500000Z proofman::recursion INFO: <<< GENERATE_VADCOP_FINAL_PROOF (500ms)
2026-06-02T06:36:31.600000Z proofman::recursion INFO: >>> GENERATE_VADCOP_FINAL_COMPRESSED_PROOF
2026-06-02T06:36:31.700000Z proofman::recursion DEBUG: >>> GENERATE_VADCOP_FINAL_COMPRESSED_PROOF_WITNESS
2026-06-02T06:36:31.800000Z proofman::recursion DEBUG: <<< GENERATE_VADCOP_FINAL_COMPRESSED_PROOF_WITNESS (100ms)
2026-06-02T06:36:31.900000Z proofman::recursion DEBUG: >>> GEN_RECURSIVE_PROOF_VadcopFinalCompressed [0:0]
2026-06-02T06:36:32.000000Z proofman::recursion DEBUG: <<< GEN_RECURSIVE_PROOF_VadcopFinalCompressed [0:0] (100ms)
2026-06-02T06:36:32.100000Z proofman::recursion INFO: <<< GENERATE_VADCOP_FINAL_COMPRESSED_PROOF (500ms)
2026-06-02T06:36:35.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for bbbb2222-0000-0000-0000-000000000000
";

    #[test]
    fn vadcop_pairs_attach_to_the_job_after_prove_success() {
        let items = parse_job(VADCOP_LOG, "aaaa1111");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, 11);
        assert_eq!(
            items[0].first,
            (
                ms("2026-06-02T06:36:31.100Z"),
                Some(ms("2026-06-02T06:36:31.200Z"))
            )
        );
        assert_eq!(
            items[0].second,
            Some((
                ms("2026-06-02T06:36:31.300Z"),
                Some(ms("2026-06-02T06:36:31.400Z"))
            ))
        );
        assert_eq!(items[1].kind, 12);
        assert!(items[1].second.is_some());
    }

    // A node that froze mid-prove, one proof bracket still open with its late-attached witness and
    // one wc bracket never closed, followed by the untimestamped restart output and the next job.
    const CRASH_LOG: &str = "\
2026-06-02T06:36:20.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for dddd4444-0000-0000-0000-000000000000
2026-06-02T06:36:22.000000Z zisk_worker::worker_node INFO: Starting Prove for JobId(dddd4444\u{2026})
2026-06-02T06:36:23.000000Z proofman::proofman DEBUG: >>> GENERATING_WC_2 [0:12]
2026-06-02T06:36:23.600000Z proofman::proofman DEBUG: >>> GEN_PROOF_2 [0:12]
2026-06-02T06:36:23.500000Z proofman::proofman DEBUG: <<< GENERATING_WC_2 [0:12] (500ms)
2026-06-02T06:36:23.700000Z proofman::proofman DEBUG: >>> GENERATING_WC_6 [0:12]
--------------------------------------------------------------------------
Primary job  terminated normally, but 1 process returned
mpirun noticed that process rank 0 with PID 0 on node host exited on signal 9 (Killed).
ZisK Worker v0.18.0
2026-06-02T06:37:00.000000Z proofman::proofman INFO: >>> INITIALIZING_PROOFMAN
2026-06-02T06:37:05.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for eeee5555-0000-0000-0000-000000000000
2026-06-02T06:37:06.000000Z executor::executor INFO: >>> PLAN
2026-06-02T06:37:06.200000Z executor::executor INFO: <<< PLAN (200ms)
";

    #[test]
    fn a_crash_leaves_dangling_items_and_the_next_job_starts_clean() {
        let mut map = BTreeMap::new();
        parse(CRASH_LOG, "node1", &mut map).unwrap();
        let crashed = map.get("dddd4444").and_then(|j| j.get("node1")).unwrap();
        assert_eq!(crashed.len(), 2);
        // The late-attached witness stays with its dangling proof, the completed segment first.
        let dangling_pair = crashed.iter().find(|i| i.second.is_some()).unwrap();
        assert_eq!(dangling_pair.kind, 7);
        assert_eq!(
            dangling_pair.first,
            (
                ms("2026-06-02T06:36:23.000Z"),
                Some(ms("2026-06-02T06:36:23.500Z"))
            )
        );
        assert_eq!(
            dangling_pair.second,
            Some((ms("2026-06-02T06:36:23.600Z"), None))
        );
        // The never-closed wc open flushes as a dangling lone segment.
        let dangling_open = crashed.iter().find(|i| i.second.is_none()).unwrap();
        assert_eq!(dangling_open.kind, 7);
        assert_eq!(dangling_open.first, (ms("2026-06-02T06:36:23.700Z"), None));
        // The restart's own brackets attach to nothing and the next job starts clean.
        let next = map.get("eeee5555").and_then(|j| j.get("node1")).unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].kind, 1);
        assert_eq!(
            next[0].first,
            (
                ms("2026-06-02T06:37:06.000Z"),
                Some(ms("2026-06-02T06:37:06.200Z"))
            )
        );
    }

    // Brackets outside the kinds table, before any job, and the decorative rank banners.
    const UNLISTED_LOG: &str = "\
2026-06-02T06:36:10.000000Z proofman::proofman INFO: >>> INITIALIZING_PROOFMAN
2026-06-02T06:36:11.000000Z proofman::proofman INFO: <<< INITIALIZING_PROOFMAN (1000ms)
2026-06-02T06:36:20.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for aaaa1111-0000-0000-0000-000000000000
2026-06-02T06:36:21.000000Z asm_runner::asm_services::services DEBUG: >>> [0] Creating shmem for service (stdio): mo
2026-06-02T06:36:22.000000Z executor::executor DEBUG: >>> MEM_SORT
2026-06-02T06:36:22.500000Z executor::executor DEBUG: <<< MEM_SORT (500ms)
2026-06-02T06:36:23.000000Z proofman::proofman INFO: >>> GENERATING_PROOFS
2026-06-02T06:36:24.000000Z proofman::proofman INFO: <<< GENERATING_PROOFS (1000ms)
";

    #[test]
    fn unlisted_stems_produce_no_items() {
        let items = parse_job(UNLISTED_LOG, "aaaa1111");
        assert!(items.is_empty());
    }

    #[test]
    fn kinds_reference_existing_phase_names() {
        let pipeline = zisk_pipeline();
        let names: Vec<&str> = pipeline.iter().map(|k| k.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "minimal_trace",
                "plan",
                "wc_contribution",
                "mem_plan_wait",
                "mem_plan_collect",
                "tables",
                "internal_contribution",
                "wc_proof",
                "recursive1",
                "recursive2",
                "compressor",
                "vadcop_final",
                "vadcop_final_compressed",
            ]
        );
        let phases: Vec<String> = zisk_phases().into_iter().map(|p| p.name).collect();
        assert!(pipeline.iter().all(|k| phases.contains(&k.phase)));
        let paired: Vec<usize> = pipeline
            .iter()
            .enumerate()
            .filter(|(_, k)| k.paired)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(paired, vec![2, 7, 8, 9, 10, 11, 12]);
    }

    // The same job dispatched twice to the node, whose second run must not overwrite the first.
    const RETRY_LOG: &str = "\
2026-06-02T06:36:20.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for aaaa1111-0000-0000-0000-000000000000
2026-06-02T06:36:21.000000Z executor::executor INFO: >>> PLAN
2026-06-02T06:36:21.200000Z executor::executor INFO: <<< PLAN (200ms)
2026-06-02T06:36:25.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for bbbb2222-0000-0000-0000-000000000000
2026-06-02T06:36:30.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for aaaa1111-0000-0000-0000-000000000000
2026-06-02T06:36:31.000000Z executor::executor INFO: >>> COMPUTE_MINIMAL_TRACE
2026-06-02T06:36:32.000000Z executor::executor INFO: <<< COMPUTE_MINIMAL_TRACE (1000ms)
";

    #[test]
    fn a_retried_job_keeps_the_first_runs_items() {
        let mut map = BTreeMap::new();
        parse(RETRY_LOG, "node1", &mut map).unwrap();
        let items = map.get("aaaa1111").and_then(|j| j.get("node1")).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, 1);
        assert_eq!(
            items[0].first,
            (
                ms("2026-06-02T06:36:21.000Z"),
                Some(ms("2026-06-02T06:36:21.200Z"))
            )
        );
    }

    // Job B's contribution dispatch and PLAN bracket arrive while job A still proves, the overlap
    // zisk produces when it hands the next job out early, so each slot keeps its own job's
    // brackets.
    const INTERLEAVED_JOBS_LOG: &str = "\
2026-06-02T06:36:20.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for aaaa1111-0000-0000-0000-000000000000
2026-06-02T06:36:21.000000Z proofman::proofman DEBUG: >>> GENERATING_WC_2 [0:12]
2026-06-02T06:36:21.600000Z proofman::proofman DEBUG: >>> GET_CONTRIBUTION_AIR_2 [0:12]
2026-06-02T06:36:21.500000Z proofman::proofman DEBUG: <<< GENERATING_WC_2 [0:12] (500ms)
2026-06-02T06:36:21.800000Z proofman::proofman DEBUG: <<< GET_CONTRIBUTION_AIR_2 [0:12] (200ms)
2026-06-02T06:36:22.000000Z zisk_worker::worker_node INFO: Starting Prove for JobId(aaaa1111\u{2026})
2026-06-02T06:36:23.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for bbbb2222-0000-0000-0000-000000000000
2026-06-02T06:36:23.100000Z executor::executor INFO: >>> PLAN
2026-06-02T06:36:23.300000Z executor::executor INFO: <<< PLAN (200ms)
2026-06-02T06:36:24.000000Z proofman::proofman DEBUG: >>> GEN_PROOF_2 [0:12]
2026-06-02T06:36:24.400000Z proofman::proofman DEBUG: <<< GEN_PROOF_2 [0:12] (400ms)
";

    #[test]
    fn an_interleaved_next_contribution_keeps_the_proving_jobs_brackets() {
        let mut map = BTreeMap::new();
        parse(INTERLEAVED_JOBS_LOG, "node1", &mut map).unwrap();
        // Job A holds its contribution pair and its proof, closed after job B was dispatched.
        let a = map.get("aaaa1111").and_then(|j| j.get("node1")).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].kind, 2);
        assert_eq!(
            a[0].second,
            Some((
                ms("2026-06-02T06:36:21.600Z"),
                Some(ms("2026-06-02T06:36:21.800Z"))
            ))
        );
        assert_eq!(a[1].kind, 7);
        assert_eq!(
            a[1].first,
            (
                ms("2026-06-02T06:36:24.000Z"),
                Some(ms("2026-06-02T06:36:24.400Z"))
            )
        );
        assert_eq!(a[1].second, None);
        // Job B holds its own PLAN bracket, and neither job flushes anything dangling.
        let b = map.get("bbbb2222").and_then(|j| j.get("node1")).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].kind, 1);
        assert_eq!(
            b[0].first,
            (
                ms("2026-06-02T06:36:23.100Z"),
                Some(ms("2026-06-02T06:36:23.300Z"))
            )
        );
        assert!(a.iter().chain(b).all(|i| i.first.1.is_some()));
    }

    // The same wc instance pends in the proving job and the next contribution when the close
    // arrives, so only its printed duration says which job it belongs to.
    const CROSS_SLOT_LOG: &str = "\
2026-06-02T06:36:20.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for aaaa1111-0000-0000-0000-000000000000
2026-06-02T06:36:21.000000Z zisk_worker::worker_node INFO: Starting Prove for JobId(aaaa1111\u{2026})
2026-06-02T06:36:22.000000Z proofman::proofman DEBUG: >>> GENERATING_WC_2 [0:12]
2026-06-02T06:36:22.500000Z zisk_worker::worker_node INFO: Starting Partial Contribution for bbbb2222-0000-0000-0000-000000000000
2026-06-02T06:36:22.900000Z proofman::proofman DEBUG: >>> GENERATING_WC_2 [0:12]
2026-06-02T06:36:23.100000Z proofman::proofman DEBUG: <<< GENERATING_WC_2 [0:12] (1100ms)
2026-06-02T06:36:23.150000Z proofman::proofman DEBUG: <<< GENERATING_WC_2 [0:12] (250ms)
";

    #[test]
    fn a_cross_slot_close_routes_to_the_slot_its_duration_points_at() {
        let mut map = BTreeMap::new();
        parse(CROSS_SLOT_LOG, "node1", &mut map).unwrap();
        // The 1100ms close back-computes to the proving job's open even though the contribution
        // slot also holds the instance, and the 250ms close then pairs the contribution's open.
        let a = map.get("aaaa1111").and_then(|j| j.get("node1")).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].kind, 7);
        assert_eq!(
            a[0].first,
            (
                ms("2026-06-02T06:36:22.000Z"),
                Some(ms("2026-06-02T06:36:23.100Z"))
            )
        );
        let b = map.get("bbbb2222").and_then(|j| j.get("node1")).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].kind, 2);
        assert_eq!(
            b[0].first,
            (
                ms("2026-06-02T06:36:22.900Z"),
                Some(ms("2026-06-02T06:36:23.150Z"))
            )
        );
    }

    // A job re-dispatched mid-prove holds both slots at the end of the log, the retry's fresh
    // contribution in one and the first run's promoted prove state in the other.
    const REDISPATCH_EOF_LOG: &str = "\
2026-06-02T06:36:20.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for cccc3333-0000-0000-0000-000000000000
2026-06-02T06:36:21.000000Z executor::executor INFO: >>> PLAN
2026-06-02T06:36:21.200000Z executor::executor INFO: <<< PLAN (200ms)
2026-06-02T06:36:22.000000Z zisk_worker::worker_node INFO: Starting Prove for JobId(cccc3333\u{2026})
2026-06-02T06:36:22.500000Z proofman::proofman DEBUG: >>> GEN_PROOF_5 [0:12]
2026-06-02T06:36:22.900000Z proofman::proofman DEBUG: <<< GEN_PROOF_5 [0:12] (400ms)
2026-06-02T06:36:25.000000Z zisk_worker::worker_node INFO: Starting Partial Contribution for cccc3333-0000-0000-0000-000000000000
2026-06-02T06:36:26.000000Z executor::executor INFO: >>> COMPUTE_MINIMAL_TRACE
2026-06-02T06:36:27.000000Z executor::executor INFO: <<< COMPUTE_MINIMAL_TRACE (1000ms)
";

    #[test]
    fn an_end_of_log_flush_keeps_the_first_run_of_a_redispatched_job() {
        let mut map = BTreeMap::new();
        parse(REDISPATCH_EOF_LOG, "node1", &mut map).unwrap();
        // The prove slot flushes first, so the first run's plan and proof survive first-write-wins
        // and the retry's fresh contribution is dropped.
        let items = map.get("cccc3333").and_then(|j| j.get("node1")).unwrap();
        let kinds: Vec<usize> = items.iter().map(|i| i.kind).collect();
        assert_eq!(kinds, vec![1, 7]);
    }
}
