//! Reads the per-GPU edge-worker logs into fine pipeline items, per-proof coordinator
//! announcements, per-proof segment-send moments, and per-proof input-transfer end times.
//!
//! Every work item logs one completion line carrying its duration and sub-timings, so an item's
//! window is the line timestamp minus the printed span with no bracket pairing and no dangling
//! reconstruction. A dropped line is a missing item, never a corrupt one. Each worker additionally
//! derives one metered-execution item per segment it sends for proving, each spanning the prior
//! segment's send to this send and the first the parallel-coordinator announcement so its serial
//! executor tiles without overlap, carrying the segment id, and records when it wrote the received
//! input file, the node's input transfer ending at the last such
//! write among its workers. Items are grouped by proof and by host node, the
//! worker-{host}-gpu{gpu}.log file name placing each worker process on its host. The tracing span
//! fields name the proof as proof_id on some functions and proof_uuid on others, and both are
//! accepted.

use std::{collections::BTreeMap, path::Path, sync::LazyLock};

use regex::Regex;

use crate::parse_benchmark::input::{
    WORKER_LOG_RE,
    log::{
        PipelineItem, PipelineItemMeta, Ts,
        zkvm::{
            cap,
            openvm::phases::{APP_SEGMENT, INTERNAL, LEAF, METERED_EXECUTION, WRAP},
            strip_ansi,
        },
    },
    worker_files_sorted,
};

/// Everything recovered from the worker logs, the per-proof pipeline items, the per-proof
/// coordinator announcements, the per-proof segment-send moments, and the per-proof input-transfer
/// end times.
pub struct WorkerData {
    /// Per-proof fine pipeline items, keyed by the full proof uuid then by node. The manager and
    /// the workers both print the uuid untruncated, so the raw id is the join key.
    pub items: BTreeMap<String, JobItems>,
    /// Per-proof metered-execution starts, keyed by the full proof uuid then by node, each the
    /// earliest parallel-coordinator announcement among the node's workers.
    pub announcements: BTreeMap<String, JobAnnouncements>,
    /// Per-proof metered-execution ends, keyed by the full proof uuid then by node, each the latest
    /// segment-send moment among the node's workers.
    pub sends: BTreeMap<String, JobSends>,
    /// Per-proof input-transfer ends, keyed by the full proof uuid then by node, each the latest
    /// input-file-written time among the node's workers, when its input transfer completes.
    pub writes: BTreeMap<String, JobWrites>,
}

/// Pipeline items for every node on a proof, keyed by node id, the value of the per-proof map.
pub type JobItems = BTreeMap<String, Vec<PipelineItem>>;

/// The earliest parallel-coordinator epoch-ms timestamp for every node on a proof, keyed by node
/// id, the value of the per-proof announcement map.
pub type JobAnnouncements = BTreeMap<String, i64>;

/// The latest segment-send epoch-ms timestamp for every node on a proof, keyed by node id, the
/// value of the per-proof send map, closing the node's metered execution window.
pub type JobSends = BTreeMap<String, i64>;

/// The latest input-file-written epoch-microsecond timestamp for every node on a proof, keyed by
/// node id, the value of the per-proof write map. Microsecond precision keeps a sub-millisecond
/// input transfer from collapsing against the fan-out start.
pub type JobWrites = BTreeMap<String, i64>;

/// Matches the proof identifier span field under either of its two names.
static RE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"proof_(?:id|uuid)=(?P<job>[^\s}]+)").unwrap());

/// Matches the input-file-written line each worker logs right after receiving its input upload,
/// the end of that worker's input transfer, carrying the proof uuid in the written path.
static RE_WRITE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"Input file written to "[^"]*edge_(?P<job>[^/"]+)/input\.bin"\s*$"#).unwrap()
});

/// Matches an app segment completion line from any of the three consumer variants, carrying the
/// segment index and the total prove span. The fast-forward and STARK sub-spans stay in the raw
/// logs only.
static RE_APP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:Coordinator-consumer: proved|Consumer: proved|Prover: generated app proof for) segment (?P<i>\d+): queue_wait=\d+ms, fastfwd=\d+ms, stark=\d+ms, prove=(?P<p>\d+)ms\s*$",
    )
    .unwrap()
});

/// Matches a leaf aggregation completion line, carrying its start segment.
static RE_LEAF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Generated leaf proof for segments \[(?P<a>\d+), \d+\] \((?P<t>\d+)ms\)\s*$")
        .unwrap()
});

/// Matches an internal aggregation completion line. The compress span is the final wrap on the
/// root proof and zero elsewhere, and it follows the prove span within the line's total.
static RE_INTERNAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"Generated internal proof: layer=\d+, segment=\[\d+, \d+\], is_final=(?:true|false), prove=(?P<p>\d+)ms, compress=(?P<c>\d+)ms\s*$",
    )
    .unwrap()
});

/// Matches the final wrap completion line, emitted once per proof on the root worker.
static RE_WRAP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Final internal proof wrapped successfully \((?P<t>\d+)ms\)\s*$").unwrap()
});

/// Matches the parallel-coordinator announcement each worker prints when it takes up a proof, the
/// earliest worker-side moment of that worker's metered execution.
static RE_COORDINATOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Parallel coordinator: prover_id=\d+, num_provers=\d+, max_app_provers=\d+\s*$")
        .unwrap()
});

/// Matches the segment-send line each worker prints when its executor dispatches a metered segment
/// for proving, carrying the segment id, the moment closing that segment's metered-execution item.
static RE_SEND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Executor \(parallel\): sending segment (?P<i>\d+) for proving\s*$").unwrap()
});

/// Anchor phrases of the work-item completion lines. A line carrying one that its full pattern
/// then fails to match is fatal, so a worker format change cannot silently drop items.
const ANCHORS: [&str; 8] = [
    "proved segment ",
    "generated app proof for segment",
    "Generated leaf proof",
    "Generated internal proof:",
    "wrapped successfully",
    "Parallel coordinator:",
    "sending segment ",
    "Input file written to ",
];

/// Loads pipeline items, coordinator announcements, segment-send moments, and input-file-written
/// times from every worker-{host}-gpu{gpu}.log under a logs directory. The shared worker pattern
/// also admits a bare worker-{host}.log, which an openvm run never contains.
pub fn load(logs_dir: &Path) -> crate::parse_benchmark::Result<WorkerData> {
    let mut items = BTreeMap::new();
    let mut announcements = BTreeMap::new();
    let mut sends = BTreeMap::new();
    let mut writes = BTreeMap::new();
    for (digit, path) in worker_files_sorted(logs_dir, &WORKER_LOG_RE)? {
        let text = crate::parse_benchmark::read_to_string_at(&path)?;
        let clean = strip_ansi(&text);
        let node = format!("node{digit}");
        parse_worker(
            &clean,
            &node,
            &mut items,
            &mut announcements,
            &mut sends,
            &mut writes,
        )?;
    }
    Ok(WorkerData {
        items,
        announcements,
        sends,
        writes,
    })
}

/// Parses one ansi-stripped worker log into pipeline items, coordinator announcements, segment-send
/// moments, and input-file-written times for the given node.
pub(crate) fn parse_worker(
    clean: &str,
    node: &str,
    items: &mut BTreeMap<String, JobItems>,
    announcements: &mut BTreeMap<String, JobAnnouncements>,
    sends: &mut BTreeMap<String, JobSends>,
    writes: &mut BTreeMap<String, JobWrites>,
) -> crate::parse_benchmark::Result<()> {
    // Per-proof state local to this worker, one call being one worker log. The worker's first
    // announcement starts its first metered-execution item, each later item starts at the prior
    // segment's send, and each segment it sends closes one such item at that segment's id.
    let mut coordinator: BTreeMap<String, i64> = BTreeMap::new();
    let mut sent: BTreeMap<String, Vec<(i64, i64)>> = BTreeMap::new();
    for line in clean.lines() {
        // Every extracted line carries the edge_worker target, which cheaply skips the bulk GPU
        // memory telemetry emitted under other targets.
        if !line.contains("edge_worker::") {
            continue;
        }
        if let Some(c) = RE_APP.captures(line) {
            let end = line_end_ms(line)?;
            let start = end - capi(&c, "p");
            // The span is the whole prove placed backward from the completion timestamp, and the
            // id is the segment index the line names.
            push(
                items,
                line,
                node,
                span_item(APP_SEGMENT, start, end, Some(capi(&c, "i"))),
            )?;
        } else if let Some(c) = RE_LEAF.captures(line) {
            let end = line_end_ms(line)?;
            let t = capi(&c, "t");
            // The provisional id is the leaf's start segment, which the translator divides by the
            // proof's leaf arity into the leaf index.
            push(
                items,
                line,
                node,
                span_item(LEAF, end - t, end, Some(capi(&c, "a"))),
            )?;
        } else if let Some(c) = RE_INTERNAL.captures(line) {
            let end = line_end_ms(line)?;
            let (prove, compress) = (capi(&c, "p"), capi(&c, "c"));
            // The line's timestamp follows the wrap, so the prove window ends where the compress
            // span begins.
            push(
                items,
                line,
                node,
                span_item(INTERNAL, end - prove - compress, end - compress, None),
            )?;
        } else if let Some(c) = RE_WRAP.captures(line) {
            let end = line_end_ms(line)?;
            let t = capi(&c, "t");
            push(items, line, node, span_item(WRAP, end - t, end, None))?;
        } else if RE_COORDINATOR.is_match(line) {
            let ts = line_end_ms(line)?;
            let job = proof_id(line)?.to_string();
            // The earliest announcement per proof and node starts the node's metered execution,
            // while the worker's own first announcement starts every metered item it derives.
            let slot = announcements
                .entry(job.clone())
                .or_default()
                .entry(node.to_string())
                .or_insert(ts);
            *slot = (*slot).min(ts);
            coordinator.entry(job).or_insert(ts);
        } else if let Some(c) = RE_SEND.captures(line) {
            let ts = line_end_ms(line)?;
            let job = proof_id(line)?.to_string();
            let segment = capi(&c, "i");
            // The latest send per proof and node closes the node's metered execution, while the
            // worker's own sends each close one derived metered item at the segment they name.
            let slot = sends
                .entry(job.clone())
                .or_default()
                .entry(node.to_string())
                .or_insert(ts);
            *slot = (*slot).max(ts);
            sent.entry(job).or_default().push((segment, ts));
        } else if let Some(c) = RE_WRITE.captures(line) {
            let ts = line_end_us(line)?;
            // The write follows the upload receipt, so the node's last write among its workers ends
            // its input transfer. The moment is kept in microseconds so a sub-millisecond transfer
            // stays measurable against the fan-out start, and the proof uuid rides in the written
            // path, not a span field.
            let slot = writes
                .entry(cap(&c, "job").to_string())
                .or_default()
                .entry(node.to_string())
                .or_insert(ts);
            *slot = (*slot).max(ts);
        } else if ANCHORS.iter().any(|a| line.contains(a)) {
            return Err(crate::parse_benchmark::ParseError::UnrecognizedWorkerLine(
                line.trim().to_string(),
            ));
        }
    }
    // One derived metered-execution item per segment this worker sends for a proof it also
    // announces, carrying the segment id it shares with the app segment of the same index. A single
    // executor thread meters the worker's segments serially, so the items tile without overlap, each
    // starting at the prior segment's send and the first at the announcement, which keeps the VM
    // setup prologue on the first item. A worker missing its announcement derives nothing.
    for (job, ts) in coordinator {
        let Some(segments) = sent.get(&job) else {
            continue;
        };
        let node_items = items
            .entry(job)
            .or_default()
            .entry(node.to_string())
            .or_default();
        let starts = std::iter::once(ts).chain(segments.iter().map(|&(_, send)| send));
        for (&(segment, send), start) in segments.iter().zip(starts) {
            node_items.push(span_item(METERED_EXECUTION, start, send, Some(segment)));
        }
    }
    Ok(())
}

/// A single-span item of the given kind, carrying an optional id.
fn span_item(kind: usize, start: i64, end: i64, id: Option<i64>) -> PipelineItem {
    PipelineItem {
        kind,
        first: (start, Some(end)),
        second: None,
        meta: PipelineItemMeta {
            id,
            ..Default::default()
        },
    }
}

/// The proof id the line's span names. A work line without a proof span field is fatal because
/// every prover function carries one, so its absence is format drift.
fn proof_id(line: &str) -> crate::parse_benchmark::Result<&str> {
    match RE_ID.captures(line).and_then(|c| c.name("job")) {
        Some(m) => Ok(m.as_str()),
        None => Err(crate::parse_benchmark::ParseError::UnrecognizedWorkerLine(
            line.trim().to_string(),
        )),
    }
}

/// Appends an item under the line's proof id and the node.
fn push(
    items: &mut BTreeMap<String, JobItems>,
    line: &str,
    node: &str,
    item: PipelineItem,
) -> crate::parse_benchmark::Result<()> {
    items
        .entry(proof_id(line)?.to_string())
        .or_default()
        .entry(node.to_string())
        .or_default()
        .push(item);
    Ok(())
}

/// The line's timestamp in epoch milliseconds, the completion moment items are placed backward
/// from.
fn line_end_ms(line: &str) -> crate::parse_benchmark::Result<i64> {
    let ts = line.split(' ').next().unwrap_or("");
    Ok(Ts::parse(ts)?.epoch_ms())
}

/// The line's timestamp in epoch microseconds, the finer resolution the input-transfer end keeps so
/// a sub-millisecond fan-out stays measurable.
fn line_end_us(line: &str) -> crate::parse_benchmark::Result<i64> {
    let ts = line.split(' ').next().unwrap_or("");
    Ok(Ts::parse(ts)?.epoch_us())
}

/// A named capture parsed as i64 milliseconds. The patterns guarantee digits, so the parse only
/// fails on an absurd overflow, which zero keeps harmless.
fn capi(c: &regex::Captures, name: &str) -> i64 {
    cap(c, name).parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::parse_benchmark::{
        ParseError,
        input::log::{
            Ts,
            zkvm::openvm::{
                phases::{APP_SEGMENT, INTERNAL, LEAF, METERED_EXECUTION, WRAP},
                worker::parse_worker,
            },
        },
    };

    /// Parses one line for node1 and returns the items it produced for the given proof.
    fn items_of(text: &str, job: &str) -> Vec<crate::parse_benchmark::input::log::PipelineItem> {
        let mut items = BTreeMap::new();
        let mut announcements = BTreeMap::new();
        let mut sends = BTreeMap::new();
        let mut writes = BTreeMap::new();
        parse_worker(
            text,
            "node1",
            &mut items,
            &mut announcements,
            &mut sends,
            &mut writes,
        )
        .expect("parse should succeed");
        items
            .get(job)
            .and_then(|j| j.get("node1"))
            .cloned()
            .unwrap_or_default()
    }

    fn ms(value: &str) -> i64 {
        Ts::parse(value).unwrap().epoch_ms()
    }

    #[test]
    fn an_app_segment_line_yields_a_single_span_with_its_index() {
        // Verbatim consumer line from a real run. The span is the whole prove placed backward from
        // the line timestamp, and the id is the segment index.
        let line = "2026-07-21T13:56:24.477882Z  INFO app_consumer{proof_id=ere-6f8d9d31213b4238b586606bc0e6bb19 prover_id=0 consumer_idx=1}: edge_worker::provers::sharded_app_prover::real_impl: Consumer: proved segment 16: queue_wait=0ms, fastfwd=4ms, stark=1483ms, prove=1487ms";
        let items = items_of(line, "ere-6f8d9d31213b4238b586606bc0e6bb19");
        assert_eq!(items.len(), 1);
        let end = ms("2026-07-21T13:56:24.477882Z");
        assert_eq!(items[0].kind, APP_SEGMENT);
        assert_eq!(items[0].first, (end - 1487, Some(end)));
        assert_eq!(items[0].second, None);
        assert_eq!(items[0].meta.id, Some(16));
    }

    #[test]
    fn the_coordinator_consumer_variant_parses_alike() {
        let line = "2026-07-21T13:56:22.806865Z  INFO coordinate_parallel_prove{proof_id=ere-6f8d9d31213b4238b586606bc0e6bb19 prover_id=0 num_provers=16}: edge_worker::provers::sharded_app_prover::real_impl: Coordinator-consumer: proved segment 0: queue_wait=0ms, fastfwd=0ms, stark=384ms, prove=384ms";
        let items = items_of(line, "ere-6f8d9d31213b4238b586606bc0e6bb19");
        assert_eq!(items.len(), 1);
        let end = ms("2026-07-21T13:56:22.806865Z");
        assert_eq!(items[0].first, (end - 384, Some(end)));
        assert_eq!(items[0].meta.id, Some(0));
    }

    #[test]
    fn leaf_and_wrap_lines_yield_single_spans() {
        let text = "\
2026-07-21T13:56:25.046890Z  INFO prove_leaf_with_prover{proof_id=ere-aaaabbbbccccddddeeeeffff00001111 segment_start=16 segment_end=19 num_app_proofs=4}: edge_worker::provers::leaf_prover::real_impl: Generated leaf proof for segments [16, 19] (221ms)
2026-07-21T13:56:26.513681Z  INFO prove_internal_with_prover{proof_id=ere-aaaabbbbccccddddeeeeffff00001111 layer_idx=2 segment_start=0 segment_end=51 is_final=true num_child_proofs=2}: edge_worker::provers::internal_prover::real_impl: Final internal proof wrapped successfully (69ms)
";
        let items = items_of(text, "ere-aaaabbbbccccddddeeeeffff00001111");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, LEAF);
        // The leaf's provisional id is its start segment, awaiting the arity division.
        assert_eq!(items[0].meta.id, Some(16));
        assert_eq!(items[1].kind, WRAP);
        assert_eq!(items[1].meta.id, None);
        assert!(items.iter().all(|i| i.second.is_none()));
    }

    #[test]
    fn executor_finished_lines_are_skipped() {
        // The executor-finished line carries no send or completion anchor, so both prover-path
        // variants parse without error and produce nothing.
        let text = "\
2026-07-21T13:56:24.725734Z  INFO coordinate_parallel_prove{proof_id=ere-aaaabbbbccccddddeeeeffff00001111 prover_id=0 num_provers=16}: edge_worker::provers::sharded_app_prover::real_impl: Executor thread (parallel): finished, 5 segments in 2331ms
2026-07-21T13:56:24.725812Z  INFO prove_app{proof_id=ere-aaaabbbbccccddddeeeeffff00001111}: edge_worker::provers::app_prover::real_impl: Executor thread: finished, 52 segments discovered in 2331ms
";
        let mut items = BTreeMap::new();
        let mut announcements = BTreeMap::new();
        let mut sends = BTreeMap::new();
        let mut writes = BTreeMap::new();
        parse_worker(
            text,
            "node1",
            &mut items,
            &mut announcements,
            &mut sends,
            &mut writes,
        )
        .unwrap();
        assert!(
            items.is_empty() && announcements.is_empty() && sends.is_empty() && writes.is_empty()
        );
    }

    #[test]
    fn an_internal_line_spans_the_prove_and_excludes_the_compress() {
        // The line timestamp follows the wrap, so the prove window ends where the compress span
        // begins. A non-final proof's compress is zero and changes nothing.
        let line = "2026-07-21T13:56:26.513683Z  INFO prove_internal_with_prover{proof_id=ere-aaaabbbbccccddddeeeeffff00001111 layer_idx=2 segment_start=0 segment_end=51 is_final=true num_child_proofs=2}: edge_worker::provers::internal_prover::real_impl: Generated internal proof: layer=2, segment=[0, 51], is_final=true, prove=88ms, compress=69ms";
        let items = items_of(line, "ere-aaaabbbbccccddddeeeeffff00001111");
        assert_eq!(items.len(), 1);
        let end = ms("2026-07-21T13:56:26.513683Z");
        assert_eq!(items[0].kind, INTERNAL);
        assert_eq!(items[0].first, (end - 88 - 69, Some(end - 69)));
    }

    #[test]
    fn an_input_file_written_line_records_the_last_write_per_node() {
        // Verbatim write lines from a real run. The proof uuid rides in the written path, and the
        // latest write among a node's workers ends its input transfer.
        let text = "\
2026-07-22T09:24:04.001673Z  INFO edge_worker::handlers: Input file written to \"/dev/shm/edge_ere-d29eebc6d2404aedb9f86d3071deefe7/input.bin\"
2026-07-22T09:24:04.002471Z  INFO edge_worker::handlers: Input file written to \"/dev/shm/edge_ere-d29eebc6d2404aedb9f86d3071deefe7/input.bin\"
";
        let mut items = BTreeMap::new();
        let mut announcements = BTreeMap::new();
        let mut sends = BTreeMap::new();
        let mut writes = BTreeMap::new();
        parse_worker(
            text,
            "node2",
            &mut items,
            &mut announcements,
            &mut sends,
            &mut writes,
        )
        .unwrap();
        assert!(items.is_empty() && announcements.is_empty() && sends.is_empty());
        assert_eq!(
            writes
                .get("ere-d29eebc6d2404aedb9f86d3071deefe7")
                .and_then(|w| w.get("node2"))
                .copied(),
            Some(Ts::parse("2026-07-22T09:24:04.002471Z").unwrap().epoch_us())
        );
    }

    #[test]
    fn a_parallel_coordinator_line_records_the_earliest_announcement() {
        // Verbatim announcement from a real run, followed by a later duplicate for the same proof
        // on the same node. The earliest timestamp is kept and no pipeline item is produced.
        let text = "\
2026-07-21T13:56:22.192354Z  INFO coordinate_parallel_prove{proof_id=ere-6f8d9d31213b4238b586606bc0e6bb19 prover_id=0 num_provers=16}: edge_worker::provers::sharded_app_prover::real_impl: Parallel coordinator: prover_id=0, num_provers=16, max_app_provers=2
2026-07-21T13:56:22.253367Z  INFO coordinate_parallel_prove{proof_id=ere-6f8d9d31213b4238b586606bc0e6bb19 prover_id=1 num_provers=16}: edge_worker::provers::sharded_app_prover::real_impl: Parallel coordinator: prover_id=1, num_provers=16, max_app_provers=2
";
        let mut items = BTreeMap::new();
        let mut announcements = BTreeMap::new();
        let mut sends = BTreeMap::new();
        let mut writes = BTreeMap::new();
        parse_worker(
            text,
            "node1",
            &mut items,
            &mut announcements,
            &mut sends,
            &mut writes,
        )
        .unwrap();
        assert!(items.is_empty() && sends.is_empty());
        assert_eq!(
            announcements
                .get("ere-6f8d9d31213b4238b586606bc0e6bb19")
                .and_then(|s| s.get("node1"))
                .copied(),
            Some(ms("2026-07-21T13:56:22.192354Z"))
        );
    }

    #[test]
    fn a_send_line_yields_a_segment_send_moment() {
        // Verbatim send line from a real run. It carries no completion span, so it records the
        // node's latest send moment and buffers the segment for a derived metered item, producing
        // no standalone pipeline item on its own.
        let text = "2026-07-21T13:56:22.421870Z  INFO coordinate_parallel_prove{proof_id=ere-6f8d9d31213b4238b586606bc0e6bb19 prover_id=0 num_provers=16}: edge_worker::provers::sharded_app_prover::real_impl: Executor (parallel): sending segment 32 for proving";
        let mut items = BTreeMap::new();
        let mut announcements = BTreeMap::new();
        let mut sends = BTreeMap::new();
        let mut writes = BTreeMap::new();
        parse_worker(
            text,
            "node1",
            &mut items,
            &mut announcements,
            &mut sends,
            &mut writes,
        )
        .unwrap();
        // Without an announcement the send derives no item, but the node's send moment is recorded.
        assert!(items.is_empty() && announcements.is_empty() && writes.is_empty());
        assert_eq!(
            sends
                .get("ere-6f8d9d31213b4238b586606bc0e6bb19")
                .and_then(|s| s.get("node1"))
                .copied(),
            Some(ms("2026-07-21T13:56:22.421870Z"))
        );
    }

    #[test]
    fn a_worker_derives_one_metered_execution_item_per_sent_segment() {
        // The serial executor tiles its derived items with no overlap, each spanning the prior
        // segment's send to its own send and the first spanning the announcement to its send, and
        // each carries the segment id it shares with the app segment of the same index.
        let text = "\
2026-07-21T13:56:22.253367Z  INFO coordinate_parallel_prove{proof_id=ere-6f8d9d31213b4238b586606bc0e6bb19 prover_id=0 num_provers=16}: edge_worker::provers::sharded_app_prover::real_impl: Parallel coordinator: prover_id=0, num_provers=16, max_app_provers=2
2026-07-21T13:56:22.421870Z  INFO coordinate_parallel_prove{proof_id=ere-6f8d9d31213b4238b586606bc0e6bb19 prover_id=0 num_provers=16}: edge_worker::provers::sharded_app_prover::real_impl: Executor (parallel): sending segment 0 for proving
2026-07-21T13:56:22.990247Z  INFO coordinate_parallel_prove{proof_id=ere-6f8d9d31213b4238b586606bc0e6bb19 prover_id=0 num_provers=16}: edge_worker::provers::sharded_app_prover::real_impl: Executor (parallel): sending segment 16 for proving
";
        let items = items_of(text, "ere-6f8d9d31213b4238b586606bc0e6bb19");
        let derived: Vec<_> = items
            .iter()
            .filter(|i| i.kind == METERED_EXECUTION)
            .collect();
        assert_eq!(derived.len(), 2);
        let coord = ms("2026-07-21T13:56:22.253367Z");
        let send0 = ms("2026-07-21T13:56:22.421870Z");
        let send16 = ms("2026-07-21T13:56:22.990247Z");
        // The first item starts at the announcement and each later item at the prior send, so the
        // items tile [coord, send16] consecutively without overlap, and each carries its sent
        // segment id.
        assert_eq!(derived[0].first, (coord, Some(send0)));
        assert_eq!(derived[0].meta.id, Some(0));
        assert_eq!(derived[1].first, (send0, Some(send16)));
        assert_eq!(derived[1].meta.id, Some(16));
    }

    #[test]
    fn no_item_derives_without_the_announcement_or_without_a_send() {
        // A worker whose announcement line the capture dropped derives nothing from its sends, and
        // a worker that announced but sent no segment derives nothing either.
        let send_only = "2026-07-21T13:56:22.421870Z  INFO coordinate_parallel_prove{proof_id=ere-6f8d9d31213b4238b586606bc0e6bb19 prover_id=0 num_provers=16}: edge_worker::provers::sharded_app_prover::real_impl: Executor (parallel): sending segment 0 for proving";
        assert!(items_of(send_only, "ere-6f8d9d31213b4238b586606bc0e6bb19").is_empty());

        let announcement_only = "2026-07-21T13:56:22.192354Z  INFO coordinate_parallel_prove{proof_id=ere-6f8d9d31213b4238b586606bc0e6bb19 prover_id=0 num_provers=16}: edge_worker::provers::sharded_app_prover::real_impl: Parallel coordinator: prover_id=0, num_provers=16, max_app_provers=2";
        assert!(items_of(announcement_only, "ere-6f8d9d31213b4238b586606bc0e6bb19").is_empty());
    }

    #[test]
    fn a_vm_setup_line_is_skipped() {
        // Newer builds print per-step VM setup timings during metered execution. The line carries
        // no completion anchor, so it parses without error and produces nothing.
        let line = "2026-07-21T13:56:22.410000Z  INFO coordinate_parallel_prove{proof_id=ere-6f8d9d31213b4238b586606bc0e6bb19 prover_id=0 num_provers=16}: edge_worker::provers::sharded_app_prover::real_impl: VM setup: input_read=12ms, stdin=3ms, metered_ctx=1ms, vm_state=30ms, snapshot_clone=170ms, total=220ms";
        let mut items = BTreeMap::new();
        let mut announcements = BTreeMap::new();
        let mut sends = BTreeMap::new();
        let mut writes = BTreeMap::new();
        parse_worker(
            line,
            "node1",
            &mut items,
            &mut announcements,
            &mut sends,
            &mut writes,
        )
        .unwrap();
        assert!(
            items.is_empty() && announcements.is_empty() && sends.is_empty() && writes.is_empty()
        );
    }

    #[test]
    fn an_anchored_line_that_fails_its_pattern_is_fatal() {
        // A reworded timing field must fail the parse rather than silently drop the item.
        let line = "2026-07-21T13:56:24.477882Z  INFO app_consumer{proof_id=ere-6f8d prover_id=0 consumer_idx=1}: edge_worker::provers::sharded_app_prover::real_impl: Consumer: proved segment 16: wait=0ms, fastfwd=4ms, stark=1483ms, total=1487ms";
        let mut items = BTreeMap::new();
        let mut announcements = BTreeMap::new();
        let mut sends = BTreeMap::new();
        let mut writes = BTreeMap::new();
        let err = parse_worker(
            line,
            "node1",
            &mut items,
            &mut announcements,
            &mut sends,
            &mut writes,
        )
        .expect_err("the reworded line must fail");
        assert!(matches!(err, ParseError::UnrecognizedWorkerLine(_)));
    }

    #[test]
    fn progress_and_telemetry_lines_are_skipped() {
        // Segment start markers, streaming notices, and the GPU memory telemetry carry no
        // completion anchor and produce nothing.
        let text = "\
2026-07-21T13:56:21.706034Z  INFO edge_worker::prover_pool: Dispatched app job to slot=0
2026-07-21T13:56:22.421906Z  INFO coordinate_parallel_prove{proof_id=ere-6f8d prover_id=0 num_provers=16}: edge_worker::provers::sharded_app_prover::real_impl: Coordinator-consumer: proving segment 0
2026-07-21T13:56:22.806881Z  INFO edge_worker::handlers: Streaming result 1 for proof ere-6f8d (worker_id=0)
2026-07-21T13:56:24.477871Z  INFO app_consumer{proof_id=ere-6f8d prover_id=0 consumer_idx=1}:stark_prove_excluding_trace{phase=\"prover\"}: openvm_cuda_common::memory_manager: GPU mem: used=-2.6 GiB, current=3.5 GiB, peak=9.0 GiB, in pool=17.0 GiB (prover.prove_whir_opening)
";
        let mut items = BTreeMap::new();
        let mut announcements = BTreeMap::new();
        let mut sends = BTreeMap::new();
        let mut writes = BTreeMap::new();
        parse_worker(
            text,
            "node1",
            &mut items,
            &mut announcements,
            &mut sends,
            &mut writes,
        )
        .unwrap();
        assert!(
            items.is_empty() && announcements.is_empty() && sends.is_empty() && writes.is_empty()
        );
    }
}
