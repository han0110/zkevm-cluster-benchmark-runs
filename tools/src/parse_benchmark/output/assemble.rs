//! Assembles the parsed logs and the run's measured sources into the lean output document.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::Value;

use crate::parse_benchmark::{
    input::{
        Sources,
        dmon::DmonRow,
        log::{
            Log, LogStatus, PipelineItem, PipelineItemMeta, Ts, job_prefix, lines::RawLine,
            zkvm::ParsedLogs,
        },
        metrics::{MetricBlock, MetricStatus},
    },
    output::schema::{
        Benchmark, Block, BlockNode, Guest, Hardware, LogEntry, Metric, NodeStats, NodeTelemetry,
        Phase, PhaseWindow, PipelineStep, Run, Software, Statistics, SubPhase, Telemetry, Zkvm,
    },
};

/// Rounds a value to one decimal place.
fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// The benchmark.json schema version this assembler emits.
const SCHEMA_VERSION: u32 = 1;

/// One GPU telemetry metric, its wire key and display metadata, how to read it off a dmon row, and
/// whether it serializes with one decimal place. The catalog, per-cell read, and rounding all
/// derive from this, so adding a metric is one row here.
struct MetricDef {
    key: &'static str,
    label: &'static str,
    unit: &'static str,
    max: Option<f64>,
    get: fn(&DmonRow) -> Option<f64>,
    one_decimal: bool,
}

/// Builds a metric definition, keeping each catalog row below to a single readable line.
const fn metric(
    key: &'static str,
    label: &'static str,
    unit: &'static str,
    max: Option<f64>,
    get: fn(&DmonRow) -> Option<f64>,
    one_decimal: bool,
) -> MetricDef {
    MetricDef {
        key,
        label,
        unit,
        max,
        get,
        one_decimal,
    }
}

/// Telemetry metric catalog in display order. The memory metrics fb, bar1, and ccpm trail so they
/// render at the end, and only the PCIe metrics keep a decimal.
const METRICS: [MetricDef; 12] = [
    metric("pwr", "Power Usage", "W", None, |r| r.pwr, false),
    metric(
        "sm",
        "Streaming Multiprocessor Utilization",
        "%",
        Some(100.0),
        |r| r.sm,
        false,
    ),
    metric(
        "mem",
        "Memory Utilization",
        "%",
        Some(100.0),
        |r| r.mem,
        false,
    ),
    metric("rxpci", "PCIe RX", "MiB/s", None, |r| r.rxpci, true),
    metric("txpci", "PCIe TX", "MiB/s", None, |r| r.txpci, true),
    metric("gtemp", "Temperature", "C", None, |r| r.gtemp, false),
    metric("pclk", "Processor Clock", "MHz", None, |r| r.pclk, false),
    metric("pviol", "Power Violation", "%", None, |r| r.pviol, false),
    metric("tviol", "Thermal Violation", "%", None, |r| r.tviol, false),
    metric("fb", "Frame Buffer Memory", "MiB", None, |r| r.fb, false),
    metric("bar1", "BAR1 Memory", "MiB", None, |r| r.bar1, false),
    metric("ccpm", "Protected Memory", "MiB", None, |r| r.ccpm, false),
];

/// Converts a metric reading to a JSON number, an integer or one decimal per the flag, or null when
/// absent.
fn metric_cell(one_decimal: bool, value: Option<f64>) -> Value {
    match value {
        None => Value::Null,
        Some(v) if one_decimal => serde_json::Number::from_f64(round1(v))
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Some(v) => Value::Number((v.round() as i64).into()),
    }
}

/// The benchmark name derived from a run id, the run id with a trailing -YYYYMMDD-HHMMSS timestamp
/// dropped so the several timestamped runs of one benchmark share a name. A run id without that
/// suffix is returned unchanged.
fn benchmark_name(run_id: &str) -> String {
    run_id
        .rsplit_once('-')
        .and_then(|(rest, time)| {
            let (date_rest, date) = rest.rsplit_once('-')?;
            let is_time = time.len() == 6 && time.bytes().all(|b| b.is_ascii_digit());
            let is_date = date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit());
            (is_time && is_date).then(|| date_rest.to_string())
        })
        .unwrap_or_else(|| run_id.to_string())
}

/// Assembles the parsed logs and measured sources into the final benchmark document. The benchmark
/// name and description come from the run directory's input benchmark.json, and an empty name falls
/// back to the benchmark name derived from the run id so the several timestamped runs of one
/// benchmark still share a name and merge together.
pub fn assemble(
    parsed: ParsedLogs,
    sources: Sources,
    run_id: &str,
    name: String,
    description: String,
) -> Benchmark {
    let ParsedLogs {
        name: zkvm_name,
        phases,
        pipeline,
        pipeline_components,
        subphases,
        logs,
    } = parsed;
    let Sources {
        blocks,
        meta,
        hardware,
        dmon,
        raw_log,
    } = sources;

    // A new-format run carries per-item log spans, gating both the extra breakdown templates and
    // the component pipeline rows the spans emit in place of their monolithic items. The
    // component kinds append after the base kinds, so a component of sub-phase slot i lands at
    // base plus i.
    let spans_present = any_log_spans(&logs);
    let component_base = pipeline.len();

    let t0 = run_epoch(&logs, &dmon);
    // The node axis the whole document shares, the telemetry's node set falling back to the nodes
    // the logs name when a run captured no telemetry, so a block's nodes position to hardware.nodes
    // by index even when a node took no part in that block.
    let node_ids: Vec<String> = if dmon.is_empty() {
        let set: BTreeSet<&str> = logs
            .iter()
            .flat_map(|l| l.nodes.iter().map(|n| n.id.as_str()))
            .collect();
        set.into_iter().map(str::to_string).collect()
    } else {
        dmon.keys().map(|d| format!("node{d}")).collect()
    };

    let out_blocks = build_blocks(
        &logs,
        &blocks,
        &node_ids,
        phases.len(),
        t0,
        &raw_log,
        component_base,
    );
    let telemetry = build_telemetry(&dmon, t0);
    let statistics = build_statistics(&out_blocks, &logs, &dmon, &node_ids);

    let run = Run {
        id: run_id.to_string(),
        started_at: logs
            .iter()
            .find_map(|l| l.t_start)
            .map(Ts::epoch_ms)
            .unwrap_or(t0),
        block_count: out_blocks.len(),
        success_count: out_blocks.iter().filter(|b| b.status == "success").count(),
        failure_count: out_blocks.iter().filter(|b| b.status != "success").count(),
        statistics,
        blocks: out_blocks,
        telemetry,
    };

    // The document id is the run id, the identity later runs of the same benchmark append against.
    let id = run_id.to_string();
    // An absent input name falls back to the benchmark name derived from the run id, the run id
    // with its trailing timestamp dropped, so two timestamped runs of one benchmark share a
    // name and merge into one document while the id stays the per-run identity.
    let name = if name.is_empty() {
        benchmark_name(run_id)
    } else {
        name
    };

    Benchmark {
        schema_version: SCHEMA_VERSION,
        hardware: Hardware {
            cpu_model: hardware.cpu_model,
            ram_gib: hardware.total_ram_gib,
            gpu_models: hardware.gpu_models,
            nodes: node_ids,
        },
        software: Software {
            zkvm: Zkvm {
                name: zkvm_name.to_string(),
                version: meta.version.unwrap_or_default(),
                phases: phases
                    .iter()
                    .map(|p| Phase {
                        name: p.name.clone(),
                        label: p.label.clone(),
                        overlap: p.overlap,
                    })
                    .collect(),
                // A new-format run appends the component kinds after the base kinds so their
                // sub-step rows reference them, while an unpatched-image run keeps
                // the base template alone.
                pipeline: pipeline
                    .iter()
                    .chain(
                        spans_present
                            .then_some(&pipeline_components)
                            .into_iter()
                            .flatten(),
                    )
                    .map(|p| PipelineStep {
                        name: p.name.clone(),
                        label: p.label.clone(),
                        phase: p.phase.clone(),
                        paired: p.paired,
                    })
                    .collect(),
                // The sub-phase template rides the document only when some block carries
                // log-derived spans, so a run of unpatched-image logs, including every zisk run,
                // keeps the prior schema.
                subphases: if spans_present {
                    subphases
                        .iter()
                        .map(|s| SubPhase {
                            name: s.name.clone(),
                            label: s.label.clone(),
                            phase: s.phase.clone(),
                        })
                        .collect()
                } else {
                    Vec::new()
                },
            },
            guest: Guest {
                name: meta.guest.unwrap_or_default(),
                version: meta.guest_version.unwrap_or_default(),
            },
        },
        id,
        name,
        description,
        runs: vec![run],
    }
}

/// The earliest phase-window start across a log's nodes, anchoring a job with no start time.
fn earliest_window(log: &Log) -> Option<i64> {
    log.nodes
        .iter()
        .flat_map(|n| n.phases.iter().flatten())
        .map(|w| w.0)
        .min()
}

/// Picks the run epoch as the earliest job signal or telemetry tick, so no offset is negative.
fn run_epoch(logs: &[Log], dmon: &BTreeMap<u32, Vec<DmonRow>>) -> i64 {
    let mut t0 = i64::MAX;
    for log in logs {
        if let Some(ts) = log.t_start {
            t0 = t0.min(ts.epoch_ms());
        }
        if let Some(start) = earliest_window(log) {
            t0 = t0.min(start);
        }
    }
    for rows in dmon.values() {
        for row in rows {
            t0 = t0.min(row.t.epoch_ms());
        }
    }
    if t0 == i64::MAX { 0 } else { t0 }
}

/// Builds one block per metric in completion order, enriched with the log that timed it.
///
/// The metric archive is the source of truth for which proofs exist, so a log with no metric is not
/// emitted. Each block's nodes match the shared node axis, so a node that took no part still holds
/// a slot of null phases at its index.
fn build_blocks(
    logs: &[Log],
    blocks: &[MetricBlock],
    node_ids: &[String],
    phase_count: usize,
    t0: i64,
    raw_log: &[RawLine],
    component_base: usize,
) -> Vec<Block> {
    let matched = match_blocks_to_logs(logs, blocks);
    let starts = job_starts(raw_log);
    let log_source = LogWindowSource {
        raw_log,
        starts: &starts,
    };
    blocks
        .iter()
        .enumerate()
        .map(|(bi, metric)| {
            build_block(
                metric,
                matched[bi].map(|li| &logs[li]),
                node_ids,
                phase_count,
                t0,
                &log_source,
                component_base,
            )
        })
        .collect()
}

/// The cluster-log slices a block's log window is cut from, the full timestamp-sorted lines and the
/// per-job start instants derived from them, bundled since they travel together.
struct LogWindowSource<'a> {
    raw_log: &'a [RawLine],
    starts: &'a [(i64, String)],
}

/// The job-start instants of the cluster log, each coordinator "[Job] Started" timestamp in
/// microseconds paired with its job prefix, kept in time order since the raw log is sorted. The
/// coordinator start precedes the workers' own contribution markers and is present even for a job
/// no worker ever began, so a failed block's log window capped at the next different job's start
/// never absorbs the job that follows it, neither its coordinator dispatch nor its worker restart
/// lines.
fn job_starts(raw_log: &[RawLine]) -> Vec<(i64, String)> {
    raw_log
        .iter()
        .filter_map(|l| {
            l.msg
                .strip_prefix("[Job] Started JobId(")
                .map(|rest| (l.ts.epoch_us(), job_prefix(rest)))
        })
        .collect()
}

/// Builds one block from a metric and its optional matched log.
///
/// Timing comes from the log when present, the block start, per-node phase windows, and crash time.
/// Without a log the block still carries its metric outcome and figures but has no timeline.
fn build_block(
    metric: &MetricBlock,
    log: Option<&Log>,
    node_ids: &[String],
    phase_count: usize,
    t0: i64,
    log_source: &LogWindowSource,
    component_base: usize,
) -> Block {
    let start_abs = log
        .and_then(|l| l.t_start.map(Ts::epoch_ms).or_else(|| earliest_window(l)))
        .unwrap_or(t0);
    // The job's terminal timestamp is the moment the cluster declared the crash, placing each
    // blamed node's crash marker as an offset from the block start.
    let crash_abs = log.and_then(|l| l.t_end.map(Ts::epoch_ms));

    // Each preset phase window is rebased from absolute epoch ms to a block-start offset.
    let rel = |w: Option<(i64, i64)>| {
        w.map(|(s, e)| PhaseWindow {
            start_ms: s - start_abs,
            dur_ms: e - s,
        })
    };

    let nodes = node_ids
        .iter()
        .map(|node_id| {
            let log_node = log.and_then(|l| l.nodes.iter().find(|n| &n.id == node_id));
            let phases = match log_node {
                Some(n) => n.phases.iter().map(|w| rel(*w)).collect(),
                None => vec![None; phase_count],
            };
            // The log's per-node crash or cancel marker is preferred since it knows when and how
            // the node ended. The metric reason's blamed node is the fallback when the
            // log holds no per-node end, placed at the job's terminal time.
            let (crashed_ms, crash_kind) = match log_node.and_then(|n| n.end) {
                // The offset is floored to zero so a worker crash or cancel logged before the
                // coordinator start, whether from cross-host clock skew or a cancel that preceded
                // the job Started line, never renders left of the time origin.
                Some(end) => (
                    Some((end.at_ms - start_abs).max(0)),
                    Some(end.kind.as_str().to_string()),
                ),
                None if metric.crashed_nodes.iter().any(|n| n == node_id) => (
                    crash_abs.map(|c| c - start_abs),
                    Some("crashed".to_string()),
                ),
                None => (None, None),
            };
            // A node took part when the log lists it among the participants. With no log there is
            // no participation evidence, so the node is assumed present rather than
            // flagged absent.
            let participated = match log {
                Some(l) => l.participants.iter().any(|p| p == node_id),
                None => true,
            };
            BlockNode {
                phases,
                pipeline: log_node.map_or_else(Vec::new, |n| {
                    pipeline_rows(&n.items, start_abs, component_base)
                }),
                crashed_ms,
                crash_kind,
                participated,
            }
        })
        .collect();

    Block {
        name: metric.name.clone(),
        status: metric.status.as_str().to_string(),
        start_ms: start_abs - t0,
        gas_used: metric.block_used_gas,
        proving_ms: metric.proving_time_ms.map(|m| m as i64),
        proof_size: metric.proof_size,
        verification_time_ms: metric.verification_time_ms,
        meta: log.map(|l| l.meta.clone()).unwrap_or_default(),
        nodes,
        subphases: log.map_or_else(Vec::new, block_subphase_rows),
        logs: block_logs(
            log,
            log_source.raw_log,
            start_abs,
            metric,
            log_source.starts,
        ),
    }
}

/// Whether any item across the run's logs carries a log-derived sub-phase breakdown, the condition
/// under which the sub-phase template rides the document.
fn any_log_spans(logs: &[Log]) -> bool {
    logs.iter()
        .flat_map(|l| &l.nodes)
        .flat_map(|n| &n.items)
        .any(|item| !item.meta.spans.is_empty())
}

/// Aggregates a block's per-item sub-phase spans into compact [slot, ms] rows referencing the
/// sub-phase template, summing each template slot across every item that carries it, in slot order.
/// The app segment items fill the segment slots and the leaf and internal items the recursion
/// slots, their owners already resolved into the slot each pair names. A block whose items carry no
/// spans yields no rows.
fn block_subphase_rows(log: &Log) -> Vec<[u64; 2]> {
    let mut sums: BTreeMap<u64, u64> = BTreeMap::new();
    for item in log.nodes.iter().flat_map(|n| &n.items) {
        for &[slot, ms] in &item.meta.spans {
            *sums.entry(slot).or_insert(0) += ms;
        }
    }
    sums.into_iter().map(|(slot, ms)| [slot, ms]).collect()
}

/// Encodes a node's pipeline items as wire rows rebased to the block start. A monolithic item is
/// one [kind, s, d, s2, d2] row truncated where a segment dangles, so a dangling end is carried by
/// arity rather than a sentinel, its metadata following the integers as one trailing object, absent
/// entirely on a metadata-free item. An item carrying a STARK sub-step breakdown instead emits one
/// component row per sub-step in place of its own row, so the monolithic segment, leaf, and
/// internal bars never render, the components sharing a per-node group and the item's id so the
/// frontend packs them onto one row. Rows sort by start then kind.
fn pipeline_rows(items: &[PipelineItem], start_abs: i64, component_base: usize) -> Vec<Vec<Value>> {
    let mut sorted: Vec<&PipelineItem> = items.iter().collect();
    sorted.sort_by_key(|item| (item.first.0, item.kind));
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let mut group: u64 = 0;
    for item in &sorted {
        if item.meta.spans.is_empty() {
            rows.push(monolithic_row(item, start_abs));
        } else {
            rows.extend(component_rows(item, start_abs, component_base, group));
            group += 1;
        }
    }
    rows.sort_by_key(|row| {
        (
            row[1].as_i64().unwrap_or(0),
            row.first().and_then(Value::as_i64).unwrap_or(0),
        )
    });
    rows
}

/// One monolithic item as a [kind, s, d, s2, d2] wire row truncated where a segment dangles, its
/// metadata riding as one trailing object.
fn monolithic_row(item: &PipelineItem, start_abs: i64) -> Vec<Value> {
    let mut cells = vec![item.kind as i64, item.first.0 - start_abs];
    if let Some(end) = item.first.1 {
        cells.push(end - item.first.0);
        if let Some((start2, end2)) = item.second {
            cells.push(start2 - start_abs);
            if let Some(end2) = end2 {
                cells.push(end2 - start2);
            }
        }
    }
    let mut row: Vec<Value> = cells.into_iter().map(Value::from).collect();
    row.extend(metadata_object(&item.meta));
    row
}

/// The component rows of a sub-step-carrying item, one [kind, s, d, {id, g}] row per sub-step
/// packed contiguously across the item's spans window in slot order. Each component kind is the
/// component base plus the sub-step's template slot. A merged final-internal breakdown can overrun
/// its window, so the sub-steps scale to fit and never draw past it, while a normal breakdown packs
/// at its exact durations leaving the untimed residual as a trailing gap. Cumulative rounding keeps
/// the components touching and inside the window.
fn component_rows(
    item: &PipelineItem,
    start_abs: i64,
    component_base: usize,
    group: u64,
) -> Vec<Vec<Value>> {
    let (w0, w1) = item
        .meta
        .spans_window
        .expect("with_spans sets spans_window whenever spans is non-empty, and component_rows runs only then");
    let window = (w1 - w0).max(0);
    let sum: u64 = item.meta.spans.iter().map(|&[_, ms]| ms).sum();
    let scale = if sum > window as u64 && sum > 0 {
        window as f64 / sum as f64
    } else {
        1.0
    };
    let meta = component_meta(item.meta.id, group);
    let base_off = w0 - start_abs;
    let mut cumulative: u64 = 0;
    let mut prev_off: i64 = 0;
    item.meta
        .spans
        .iter()
        .map(|&[slot, ms]| {
            cumulative += ms;
            let end_off = (cumulative as f64 * scale).round() as i64;
            let row = vec![
                Value::from((component_base + slot as usize) as i64),
                Value::from(base_off + prev_off),
                Value::from(end_off - prev_off),
                meta.clone(),
            ];
            prev_off = end_off;
            row
        })
        .collect()
}

/// The trailing metadata object of an item, holding the present fields among id, cpu_heavy, and
/// gpu_heavy, or None when every field is absent.
fn metadata_object(meta: &PipelineItemMeta) -> Option<Value> {
    let mut map = serde_json::Map::new();
    if let Some(id) = meta.id {
        map.insert("id".to_string(), id.into());
    }
    if let Some(index) = meta.cpu_heavy {
        map.insert("cpu_heavy".to_string(), index.into());
    }
    if let Some(index) = meta.gpu_heavy {
        map.insert("gpu_heavy".to_string(), index.into());
    }
    (!map.is_empty()).then(|| Value::Object(map))
}

/// The trailing metadata object of a component row, the parent item's id when present and the
/// per-node group its sibling components share.
fn component_meta(id: Option<i64>, group: u64) -> Value {
    let mut map = serde_json::Map::new();
    if let Some(id) = id {
        map.insert("id".to_string(), id.into());
    }
    map.insert("group".to_string(), group.into());
    Value::Object(map)
}

/// The kept cluster-log lines within a block's proving window, each rebased to a microsecond offset
/// from the block start. A clean block runs to its terminal time, falling back to the proving
/// duration when the log carries no end. A failed block instead runs to just before the next
/// different job's start so the following job is not pulled in, neither its coordinator dispatch
/// nor its worker restart lines, since a crashed node begins the next proof within about a second.
/// Cleanup that interleaves in time with the next job is bounded out, because the shared
/// role-tagged log axis cannot separate the failed job's lagging cleanup from the next job's lines.
/// When no following job bounds a failed block, the window reaches the latest node end. Time is at
/// microsecond precision so lines that share a millisecond keep their order, while the frontend
/// renders the offset down to milliseconds. A block with no matched log carries no lines, since it
/// has no window to slice. The lines are sliced from the microsecond-sorted set by binary search.
fn block_logs(
    log: Option<&Log>,
    raw_log: &[RawLine],
    start_abs: i64,
    metric: &MetricBlock,
    starts: &[(i64, String)],
) -> Vec<LogEntry> {
    let Some(log) = log else {
        return Vec::new();
    };
    // The block start in microseconds, the job start when known and the millisecond window start
    // otherwise, so every offset is non-negative and the first window line sits at zero.
    let start_us = log.t_start.map(Ts::epoch_us).unwrap_or(start_abs * 1000);
    // The next different job's first contribution, the upper bound a block's window never crosses.
    // The starts are sorted ascending by microsecond, so a binary search locates the first start
    // strictly after the block start and a short forward scan finds the first different job.
    let this = job_prefix(&log.id);
    let from = starts.partition_point(|(us, _)| *us <= start_us);
    let next = starts[from..]
        .iter()
        .find(|(_, prefix)| *prefix != this)
        .map(|(us, _)| *us);
    // A terminal time bounds the window directly. The dispatch-relative proving-time fallback can
    // overshoot the true finish, so it is also capped at just before the next different job's start
    // so it can never absorb the following job's lines.
    let base_end = log
        .t_end
        .map(Ts::epoch_us)
        .or_else(|| {
            metric.proving_time_ms.map(|m| {
                let estimate = start_us + m as i64 * 1000;
                match next {
                    Some(n) => estimate.min(n - 1),
                    None => estimate,
                }
            })
        })
        .unwrap_or(start_us);
    // A clean job ends at its terminal time. A failed job runs to just before the next different
    // job so the following job is not pulled in, with the latest node end plus a millisecond as the
    // fallback when no following job bounds the window.
    let end_us = match log.status {
        LogStatus::Success => base_end,
        _ => match next {
            Some(n) => n - 1,
            None => {
                let cleanup = log
                    .nodes
                    .iter()
                    .filter_map(|n| n.end)
                    .map(|e| e.at_ms * 1000 + 1000)
                    .max();
                base_end.max(cleanup.unwrap_or(base_end))
            }
        },
    };
    let lo = raw_log.partition_point(|l| l.ts.epoch_us() < start_us);
    let hi = raw_log.partition_point(|l| l.ts.epoch_us() <= end_us);
    // A coordinator log whose end precedes its start would invert the bounds, so the upper bound is
    // floored to the lower one, making an inverted window slice empty rather than panic.
    let hi = hi.max(lo);
    raw_log[lo..hi]
        .iter()
        .map(|l| LogEntry {
            role: l.role.clone(),
            time: l.ts.epoch_us() - start_us,
            level: l.level.clone(),
            msg: l.msg.clone(),
        })
        .collect()
}

/// Builds columnar telemetry, per-metric [gpu][tick] grids on one shared one-second axis.
///
/// Every node aligns to the same axis anchored at the run epoch, where tick i is exactly i seconds
/// after the epoch, so grids line up by index across nodes and against the block windows. A second
/// a node did not sample stays null, a late start or interior gap, so the frontend renders a break
/// there rather than pulling later readings onto collapsed seconds.
fn build_telemetry(dmon: &BTreeMap<u32, Vec<DmonRow>>, t0: i64) -> Telemetry {
    let mut present: BTreeSet<&str> = BTreeSet::new();

    // The tick column of a timestamp, one column per second from the run epoch.
    let tick_of = |t: i64| (((t - t0) as f64) / 1000.0).round().max(0.0) as usize;
    // The axis spans the epoch through the latest tick any node reported, so every node shares it.
    let width = dmon
        .values()
        .flatten()
        .map(|r| tick_of(r.t.epoch_ms()))
        .max()
        .map_or(0, |m| m + 1);

    let mut nodes = Vec::new();
    for rows in dmon.values() {
        let mut gpus: Vec<u32> = rows.iter().map(|r| r.gpu).collect();
        gpus.sort_unstable();
        gpus.dedup();
        let gpu_index: HashMap<u32, usize> =
            gpus.iter().enumerate().map(|(i, &g)| (g, i)).collect();
        let gpu_count = gpus.len();

        let mut metrics: BTreeMap<String, Vec<Vec<Value>>> = BTreeMap::new();
        for def in &METRICS {
            let mut grid = vec![vec![Value::Null; width]; gpu_count];
            let mut any = false;
            for row in rows {
                let gi = gpu_index[&row.gpu];
                let ti = tick_of(row.t.epoch_ms());
                let cell = metric_cell(def.one_decimal, (def.get)(row));
                if !cell.is_null() {
                    any = true;
                }
                grid[gi][ti] = cell;
            }
            if any {
                present.insert(def.key);
                metrics.insert(def.key.to_string(), grid);
            }
        }

        nodes.push(NodeTelemetry { metrics });
    }

    let metrics = METRICS
        .iter()
        .filter(|d| present.contains(&d.key))
        .map(|d| Metric {
            name: d.key.to_string(),
            label: d.label.to_string(),
            unit: d.unit.to_string(),
            max: d.max,
        })
        .collect();

    Telemetry { metrics, nodes }
}

/// Builds the run statistics from the assembled blocks and telemetry. The per-node GPU rollup
/// counts only clean blocks, so its averages read as normal proving load.
fn build_statistics(
    blocks: &[Block],
    logs: &[Log],
    dmon: &BTreeMap<u32, Vec<DmonRow>>,
    node_ids: &[String],
) -> Statistics {
    let ok: Vec<&Block> = blocks.iter().filter(|b| b.status == "success").collect();
    let mut proving: Vec<i64> = ok.iter().filter_map(|b| b.proving_ms).collect();
    let total_proving_ms: i64 = proving.iter().sum();
    let mean_proving_ms = (!proving.is_empty())
        .then(|| (total_proving_ms as f64 / proving.len() as f64).round() as i64);
    proving.sort_unstable();

    let total_gas: u64 = ok.iter().filter_map(|b| b.gas_used).sum();
    let total_proving_s = total_proving_ms as f64 / 1000.0;
    let mean_gas_per_s =
        (total_proving_s > 0.0).then(|| (total_gas as f64 / total_proving_s).round() as i64);

    let windows = node_proving_windows(logs, node_ids);
    let nodes = dmon
        .iter()
        .map(|(key, rows)| {
            let id = format!("node{key}");
            node_stats(rows, windows.get(&id).map_or(&[][..], Vec::as_slice))
        })
        .collect();

    Statistics {
        mean_proving_ms,
        p50_proving_ms: percentile(&proving, 50.0),
        p90_proving_ms: percentile(&proving, 90.0),
        p95_proving_ms: percentile(&proving, 95.0),
        p99_proving_ms: percentile(&proving, 99.0),
        mean_gas_per_s,
        nodes,
    }
}

/// Returns the linear-interpolation percentile of a sorted slice, rounded to the nearest integer.
fn percentile(sorted: &[i64], p: f64) -> Option<i64> {
    match sorted.len() {
        0 => None,
        1 => Some(sorted[0]),
        n => {
            let rank = p / 100.0 * (n - 1) as f64;
            let lo = rank.floor() as usize;
            let hi = rank.ceil() as usize;
            let frac = rank - lo as f64;
            let value = sorted[lo] as f64 + (sorted[hi] as f64 - sorted[lo] as f64) * frac;
            Some(value.round() as i64)
        }
    }
}

/// Collects each node's absolute proving windows from its clean blocks only, the union of its phase
/// windows. The map is keyed by the node{key} id hardware.nodes and the telemetry share, so a dmon
/// stream joins to its windows, against which its GPU samples are later filtered.
fn node_proving_windows(logs: &[Log], node_ids: &[String]) -> BTreeMap<String, Vec<(i64, i64)>> {
    let mut windows: BTreeMap<String, Vec<(i64, i64)>> = BTreeMap::new();
    for log in logs.iter().filter(|l| is_clean_block(l, node_ids)) {
        for node in &log.nodes {
            windows
                .entry(node.id.clone())
                .or_default()
                .extend(node.phases.iter().flatten().copied());
        }
    }
    windows
}

/// Whether a job is a clean block, a success that ran on the full cluster with every node taking
/// part. Its telemetry is the normal proving load the per-node rollup averages, so a failed,
/// timed-out, or node-missing job is excluded.
fn is_clean_block(log: &Log, node_ids: &[String]) -> bool {
    log.status == LogStatus::Success
        && node_ids
            .iter()
            .all(|id| log.participants.iter().any(|p| p == id))
}

/// Computes a GPU rollup for one node from only the telemetry samples inside its proving windows,
/// so the rollup reflects the GPU under proving load rather than the whole capture, where idle,
/// warmup, and tail seconds would otherwise pull every average toward zero.
fn node_stats(rows: &[DmonRow], windows: &[(i64, i64)]) -> NodeStats {
    let proving: Vec<&DmonRow> = rows
        .iter()
        .filter(|r| within_windows(r.t.epoch_ms(), windows))
        .collect();
    let collect = |f: fn(&DmonRow) -> Option<f64>| {
        proving.iter().copied().filter_map(f).collect::<Vec<f64>>()
    };
    let mean = |v: &[f64]| (!v.is_empty()).then(|| round1(v.iter().sum::<f64>() / v.len() as f64));
    let max = |v: &[f64]| {
        v.iter()
            .copied()
            .fold(None, |acc, x| Some(acc.map_or(x, |a: f64| a.max(x))))
    };

    let gtemp = collect(|r| r.gtemp);
    let sm = collect(|r| r.sm);
    let mem = collect(|r| r.mem);
    let rxpci = collect(|r| r.rxpci);
    let txpci = collect(|r| r.txpci);

    let n_gpus = proving
        .iter()
        .map(|r| r.gpu)
        .collect::<BTreeSet<_>>()
        .len()
        .max(1);
    let throttled = proving
        .iter()
        .filter(|r| r.tviol.is_some_and(|v| v != 0.0))
        .count();
    let temp_throttle_seconds = round1(throttled as f64 / n_gpus as f64);

    NodeStats {
        max_temp: max(&gtemp),
        temp_throttle_seconds,
        mean_sm: mean(&sm),
        mean_mem: mean(&mem),
        peak_rxpci: max(&rxpci).map(round1),
        peak_txpci: max(&txpci).map(round1),
    }
}

/// Reports whether an epoch-ms timestamp falls within any inclusive proving window.
fn within_windows(t: i64, windows: &[(i64, i64)]) -> bool {
    windows.iter().any(|&(start, end)| t >= start && t <= end)
}

/// Whether a log id keys the same cluster job as a crash reason's id. The zisk coordinator prints a
/// truncated id against the reason's full uuid, so the leading hex run is compared; an openvm ere-
/// id is logged in full on both sides, so job_prefix keeps it whole and the compare is exact.
fn job_prefix_matches(log_id: &str, job: &str) -> bool {
    let prefix = job_prefix(log_id);
    !prefix.is_empty() && job.starts_with(&prefix)
}

/// Binds each metric block to the log that timed it, by index. Crashes bind first by the cluster
/// job id in their reason, then successes bind to the latest success log finishing at or before
/// their completion in a forward-only one-to-one scan. A block with no log is left unbound.
fn match_blocks_to_logs(logs: &[Log], blocks: &[MetricBlock]) -> Vec<Option<usize>> {
    let mut by_metric: Vec<Option<usize>> = vec![None; blocks.len()];
    let mut used = vec![false; logs.len()];

    // First pass. A crash binds to the log with the same cluster job id, the key its reason shares
    // with the coordinator's truncated id.
    for (bi, block) in blocks.iter().enumerate() {
        let Some(job) = block.crashed_job.as_deref() else {
            continue;
        };
        if let Some(li) =
            (0..logs.len()).find(|&i| !used[i] && job_prefix_matches(&logs[i].id, job))
        {
            used[li] = true;
            by_metric[bi] = Some(li);
        }
    }

    // Second pass. A success binds to the latest success log finishing at or before its completion,
    // a forward-only one-to-one scan. Only successes take this path, so a crash that found no job
    // id stays unbound rather than stealing a success log by a near timestamp.
    let successes: Vec<usize> = logs
        .iter()
        .enumerate()
        .filter(|(i, l)| !used[*i] && l.status == LogStatus::Success)
        .map(|(i, _)| i)
        .collect();
    let t_end_ms = |log_idx: usize| logs[log_idx].t_end.map(Ts::epoch_ms);

    let mut sp = 0; // forward pointer into successes
    for (bi, block) in blocks.iter().enumerate() {
        if by_metric[bi].is_some() || block.status != MetricStatus::Success {
            continue;
        }
        let Some(ts) = block.timestamp_completed.map(Ts::epoch_ms) else {
            continue;
        };
        let mut best: Option<usize> = None;
        let mut k = sp;
        while k < successes.len() && t_end_ms(successes[k]).is_some_and(|te| te <= ts) {
            best = Some(k);
            k += 1;
        }
        if let Some(b) = best {
            let log_idx = successes[b];
            used[log_idx] = true;
            by_metric[bi] = Some(log_idx);
            sp = b + 1;
        }
    }

    by_metric
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::parse_benchmark::{
        input::{
            dmon::DmonRow,
            log::{
                Log, LogNode, LogStatus, NodeEnd, NodeEndKind, PipelineItem, PipelineItemMeta, Ts,
                lines::RawLine,
            },
            metrics::{MetricBlock, MetricStatus},
        },
        output::assemble::{
            LogWindowSource, any_log_spans, benchmark_name, block_logs, block_subphase_rows,
            build_block, match_blocks_to_logs, metric_cell, node_proving_windows, node_stats,
            pipeline_rows,
        },
    };

    /// Builds a telemetry row carrying a single SM and thermal-violation reading at one timestamp.
    fn row(time: &str, sm: f64, tviol: f64) -> DmonRow {
        DmonRow {
            t: Ts::parse(time).unwrap(),
            gpu: 0,
            pwr: None,
            gtemp: None,
            sm: Some(sm),
            mem: None,
            pclk: None,
            pviol: None,
            tviol: Some(tviol),
            rxpci: None,
            txpci: None,
            fb: None,
            bar1: None,
            ccpm: None,
        }
    }

    #[test]
    fn node_stats_average_only_samples_inside_proving_windows() {
        let rows = vec![
            row("2026-05-29T10:00:00Z", 10.0, 0.0), // warmup before the window, excluded
            row("2026-05-29T10:00:05Z", 80.0, 1.0), // inside the window, counted and throttled
            row("2026-05-29T10:00:06Z", 90.0, 0.0), // inside the window, counted
            row("2026-05-29T10:00:30Z", 0.0, 0.0),  // idle tail after the window, excluded
        ];
        let start = Ts::parse("2026-05-29T10:00:04Z").unwrap().epoch_ms();
        let end = Ts::parse("2026-05-29T10:00:10Z").unwrap().epoch_ms();

        let stats = node_stats(&rows, &[(start, end)]);

        // Only the two in-window readings average, so the idle 10 and 0 do not drag the mean down.
        assert_eq!(stats.mean_sm, Some(85.0));
        // The single in-window throttled second counts, the out-of-window ones do not.
        assert_eq!(stats.temp_throttle_seconds, 1.0);
    }

    #[test]
    fn node_stats_is_empty_without_any_proving_sample() {
        let rows = vec![row("2026-05-29T10:00:00Z", 50.0, 0.0)];
        let stats = node_stats(&rows, &[]);
        assert_eq!(stats.mean_sm, None);
        assert_eq!(stats.temp_throttle_seconds, 0.0);
    }

    #[test]
    fn integer_metrics_round_to_whole_numbers() {
        assert_eq!(metric_cell(false, Some(15.6)), Value::from(16));
        assert_eq!(metric_cell(false, Some(0.0)), Value::from(0));
    }

    #[test]
    fn pcie_metrics_keep_one_decimal() {
        assert_eq!(metric_cell(true, Some(15.21)), Value::from(15.2));
    }

    #[test]
    fn missing_values_become_null() {
        assert_eq!(metric_cell(false, None), Value::Null);
    }

    /// Builds a two-node job log with one phase window per node, for the clean-block window test.
    fn job(status: LogStatus, participants: &[&str], window: (i64, i64)) -> Log {
        let mut log = Log::new("job");
        log.status = status;
        log.participants = participants.iter().map(|p| p.to_string()).collect();
        log.nodes = ["node1", "node2"]
            .iter()
            .map(|id| LogNode {
                id: id.to_string(),
                phases: vec![Some(window)],
                items: Vec::new(),
                end: None,
            })
            .collect();
        log
    }

    #[test]
    fn node_windows_count_only_clean_blocks() {
        let node_ids = vec!["node1".to_string(), "node2".to_string()];
        let logs = vec![
            job(LogStatus::Success, &["node1", "node2"], (100, 200)), // clean, counted
            job(LogStatus::Success, &["node1"], (300, 400)),          // node2 missing, skipped
            job(LogStatus::Timeout, &["node1", "node2"], (500, 600)), // not a success, skipped
        ];
        let windows = node_proving_windows(&logs, &node_ids);
        // Only the clean job's window survives for each node, so the rollup that filters telemetry
        // to these windows averages normal proving rather than the degraded or aborted
        // jobs.
        assert_eq!(windows.get("node1"), Some(&vec![(100, 200)]));
        assert_eq!(windows.get("node2"), Some(&vec![(100, 200)]));
    }

    fn ts(sec: &str) -> Ts {
        Ts::parse(&format!("2026-05-29T00:00:{sec}Z")).unwrap()
    }

    fn log(id: &str, status: LogStatus, end: &str) -> Log {
        let mut l = Log::new(id);
        l.status = status;
        l.t_end = Some(ts(end));
        l
    }

    fn success_block(name: &str, completed: &str) -> MetricBlock {
        MetricBlock {
            name: name.to_string(),
            status: MetricStatus::Success,
            block_used_gas: None,
            proving_time_ms: None,
            proof_size: None,
            verification_time_ms: None,
            timestamp_completed: Some(ts(completed)),
            crashed_nodes: Vec::new(),
            crashed_job: None,
        }
    }

    fn crash_block(name: &str, job: &str) -> MetricBlock {
        MetricBlock {
            name: name.to_string(),
            status: MetricStatus::Crashed,
            block_used_gas: None,
            proving_time_ms: None,
            proof_size: None,
            verification_time_ms: None,
            timestamp_completed: Some(ts("30")),
            crashed_nodes: vec!["node1".to_string()],
            crashed_job: Some(job.to_string()),
        }
    }

    #[test]
    fn clean_runs_match_one_to_one() {
        let logs = vec![
            log("A", LogStatus::Success, "10"),
            log("B", LogStatus::Success, "20"),
        ];
        let blocks = vec![success_block("m0", "10.5"), success_block("m1", "20.5")];
        let matched = match_blocks_to_logs(&logs, &blocks);
        assert_eq!(matched, vec![Some(0), Some(1)]);
    }

    #[test]
    fn success_skips_a_failed_log() {
        let logs = vec![
            log("A", LogStatus::Success, "10"),
            log("B", LogStatus::Failed, "15"),
            log("C", LogStatus::Success, "20"),
        ];
        let blocks = vec![success_block("m0", "11"), success_block("m1", "21")];
        let matched = match_blocks_to_logs(&logs, &blocks);
        assert_eq!(matched, vec![Some(0), Some(2)]);
    }

    #[test]
    fn crash_binds_to_its_log_by_truncated_job_id() {
        // The log carries the truncated job id while the crash reason yielded the full uuid, so the
        // crash binds by prefix and the neighbouring success still binds by timestamp.
        let logs = vec![
            log("b051e0b6\u{2026}", LogStatus::Failed, "30"),
            log("c0ffee00\u{2026}", LogStatus::Success, "20"),
        ];
        let blocks = vec![
            success_block("ok", "20.5"),
            crash_block("boom", "b051e0b6-fc1b-4bf5-8b54-71f5111886a9"),
        ];
        let matched = match_blocks_to_logs(&logs, &blocks);
        assert_eq!(matched, vec![Some(1), Some(0)]);
    }

    #[test]
    fn an_openvm_crash_binds_to_its_full_ere_log_not_the_first() {
        // Every openvm log id shares the leading hex "e" of "ere-", so a leading-hex-run match
        // would bind the first ere- log. Keeping the ere- id whole binds the exact one.
        let logs = vec![
            log(
                "ere-a275aee5c2364484bb429e48dabf6738",
                LogStatus::Failed,
                "10",
            ),
            log(
                "ere-2b70e62f1f704f2ab54adffc7871155e",
                LogStatus::Failed,
                "30",
            ),
        ];
        let blocks = vec![crash_block("boom", "ere-2b70e62f1f704f2ab54adffc7871155e")];
        let matched = match_blocks_to_logs(&logs, &blocks);
        assert_eq!(matched, vec![Some(1)]);
    }

    #[test]
    fn crash_without_a_job_id_stays_unbound() {
        // An unattributed crash carries no job id and must not poach a success log by a near time.
        let logs = vec![log("aaaaaaaa\u{2026}", LogStatus::Success, "20")];
        let mut orphan = crash_block("grpc", "");
        orphan.crashed_job = None;
        let blocks = vec![orphan];
        let matched = match_blocks_to_logs(&logs, &blocks);
        assert_eq!(matched, vec![None]);
    }

    #[test]
    fn block_logs_stop_before_the_next_job() {
        let at = |s: &str| Ts::parse(&format!("2026-01-01T00:00:{s}Z")).unwrap();
        let line = |s: &str, msg: &str| RawLine {
            role: "coordinator".to_string(),
            ts: at(s),
            level: "info".to_string(),
            msg: msg.to_string(),
        };
        // Job A starts at :00. A node end that overshoots to :26 would otherwise pull in job B's
        // :25.5 line, but the next job's :25 start caps the window short of it.
        let mut a = Log::new("aaaaaaaa");
        a.t_start = Some(at("00"));
        a.t_end = Some(at("24"));
        a.nodes = vec![LogNode {
            id: "node1".to_string(),
            phases: Vec::new(),
            items: Vec::new(),
            end: Some(NodeEnd {
                at_ms: at("26").epoch_ms(),
                kind: NodeEndKind::Crashed,
            }),
        }];
        let raw = vec![
            line("23", "A cleanup line"),
            line("25.500000", "B job line"),
        ];
        let starts = vec![(at("25").epoch_us(), "bbbbbbbb".to_string())];
        let out = block_logs(
            Some(&a),
            &raw,
            at("00").epoch_ms(),
            &crash_block("a", "x"),
            &starts,
        );
        let msgs: Vec<&str> = out.iter().map(|e| e.msg.as_str()).collect();
        assert_eq!(msgs, vec!["A cleanup line"]);
    }

    #[test]
    fn block_logs_success_window_reaches_its_terminal_time() {
        let at = |s: &str| Ts::parse(&format!("2026-01-01T00:00:{s}Z")).unwrap();
        let line = |s: &str, msg: &str| RawLine {
            role: "coordinator".to_string(),
            ts: at(s),
            level: "info".to_string(),
            msg: msg.to_string(),
        };
        // A clean job whose terminal time at :24 exceeds a following job's :20 start is not
        // truncated to that start, so its terminal line at :23 stays in the window.
        let mut a = Log::new("aaaaaaaa");
        a.status = LogStatus::Success;
        a.t_start = Some(at("00"));
        a.t_end = Some(at("24"));
        let raw = vec![line("23", "A terminal line")];
        let starts = vec![(at("20").epoch_us(), "bbbbbbbb".to_string())];
        let out = block_logs(
            Some(&a),
            &raw,
            at("00").epoch_ms(),
            &success_block("a", "24"),
            &starts,
        );
        let msgs: Vec<&str> = out.iter().map(|e| e.msg.as_str()).collect();
        assert_eq!(msgs, vec!["A terminal line"]);
    }

    #[test]
    fn block_logs_does_not_panic_on_inverted_window() {
        let at = |s: &str| Ts::parse(&format!("2026-01-01T00:00:{s}Z")).unwrap();
        let line = |s: &str, msg: &str| RawLine {
            role: "coordinator".to_string(),
            ts: at(s),
            level: "info".to_string(),
            msg: msg.to_string(),
        };
        // A coordinator log whose end precedes its start inverts the window bounds. The slice must
        // be floored to empty rather than panic on a backwards range.
        let mut a = Log::new("aaaaaaaa");
        a.status = LogStatus::Success;
        a.t_start = Some(at("10"));
        a.t_end = Some(at("05"));
        let raw = vec![line("07", "A line between the inverted bounds")];
        let out = block_logs(
            Some(&a),
            &raw,
            at("10").epoch_ms(),
            &success_block("a", "05"),
            &[],
        );
        assert!(out.is_empty(), "an inverted window yields no lines");
    }

    #[test]
    fn crashed_offset_is_floored_to_zero_for_a_sub_start_end() {
        let at = |s: &str| Ts::parse(&format!("2026-01-01T00:00:{s}Z")).unwrap();
        // The coordinator started the job at :10 while node1's crash marker reads :05, earlier than
        // the start from cross-host clock skew or a cancel logged before the Started line. The
        // resulting offset must clamp to zero rather than render left of the time origin.
        let mut log = Log::new("job");
        log.status = LogStatus::Failed;
        log.t_start = Some(at("10"));
        log.participants = vec!["node1".to_string()];
        log.nodes = vec![LogNode {
            id: "node1".to_string(),
            phases: vec![None],
            items: Vec::new(),
            end: Some(NodeEnd {
                at_ms: at("05").epoch_ms(),
                kind: NodeEndKind::Crashed,
            }),
        }];
        let node_ids = vec!["node1".to_string()];
        let block = build_block(
            &crash_block("boom", "x"),
            Some(&log),
            &node_ids,
            1,
            at("10").epoch_ms(),
            &LogWindowSource {
                raw_log: &[],
                starts: &[],
            },
            0,
        );
        assert_eq!(block.nodes[0].crashed_ms, Some(0));
        assert_eq!(block.nodes[0].crash_kind.as_deref(), Some("crashed"));
    }

    /// A metadata-free pipeline item of the given kind and segments.
    fn item(
        kind: usize,
        first: (i64, Option<i64>),
        second: Option<(i64, Option<i64>)>,
    ) -> PipelineItem {
        PipelineItem {
            kind,
            first,
            second,
            meta: PipelineItemMeta::default(),
        }
    }

    #[test]
    fn pipeline_items_rebase_and_encode_by_arity() {
        let at = |s: &str| Ts::parse(&format!("2026-01-01T00:00:{s}Z")).unwrap();
        let start = at("10").epoch_ms();
        // One node holding the four item states, a lone segment, a full witness and compute pair,
        // a witness whose compute dangles, and a bare dangling open.
        let mut log = Log::new("job");
        log.status = LogStatus::Success;
        log.t_start = Some(at("10"));
        log.participants = vec!["node1".to_string()];
        log.nodes = vec![LogNode {
            id: "node1".to_string(),
            phases: vec![None],
            items: vec![
                item(0, (start + 100, Some(start + 300)), None),
                item(
                    7,
                    (start + 400, Some(start + 600)),
                    Some((start + 700, Some(start + 900))),
                ),
                item(
                    2,
                    (start + 1000, Some(start + 1100)),
                    Some((start + 1200, None)),
                ),
                item(8, (start + 1300, None), None),
            ],
            end: None,
        }];
        let node_ids = vec!["node1".to_string()];
        let block = build_block(
            &success_block("a", "20"),
            Some(&log),
            &node_ids,
            1,
            start,
            &LogWindowSource {
                raw_log: &[],
                starts: &[],
            },
            0,
        );
        // Each state encodes by arity, the dangling ends carried by truncation.
        assert_eq!(
            serde_json::to_value(&block.nodes[0].pipeline).unwrap(),
            json!([
                [0, 100, 200],
                [7, 400, 200, 700, 200],
                [2, 1000, 100, 1200],
                [8, 1300],
            ])
        );
    }

    #[test]
    fn pipeline_rows_sort_by_start_then_kind() {
        // Items arrive in close order rather than start order, and two rows share a start.
        let items = vec![
            item(5, (100, Some(150)), None),
            item(3, (100, Some(120)), None),
            item(0, (50, Some(90)), None),
        ];
        let rows = pipeline_rows(&items, 0, 6);
        assert_eq!(
            serde_json::to_value(&rows).unwrap(),
            json!([[0, 50, 40], [3, 100, 20], [5, 100, 50]])
        );
    }

    #[test]
    fn pipeline_metadata_encodes_as_a_trailing_object() {
        // The id and the heavy side markers ride their row as one trailing object, while the
        // metadata-free sibling stays a pure integer row.
        let items = vec![
            PipelineItem {
                meta: PipelineItemMeta {
                    id: Some(12),
                    ..Default::default()
                },
                ..item(0, (100, Some(300)), None)
            },
            PipelineItem {
                meta: PipelineItemMeta {
                    id: Some(3),
                    cpu_heavy: Some(0),
                    gpu_heavy: Some(1),
                    ..Default::default()
                },
                ..item(1, (400, Some(600)), Some((700, Some(900))))
            },
            item(2, (1000, Some(1100)), None),
        ];
        let rows = pipeline_rows(&items, 0, 6);
        assert_eq!(
            serde_json::to_value(&rows).unwrap(),
            json!([
                [0, 100, 200, { "id": 12 }],
                [1, 400, 200, 700, 200, { "id": 3, "cpu_heavy": 0, "gpu_heavy": 1 }],
                [2, 1000, 100],
            ])
        );
    }

    #[test]
    fn pipeline_rows_emit_the_sub_step_components_in_place_of_the_item() {
        // A segment item and an internal item each carry a STARK sub-step breakdown, so each emits
        // one component row per sub-step in place of its own row, the component kind the base plus
        // the sub-step slot, packed contiguously across the item's spans window from its start. The
        // segment breakdown fits with a trailing gap, while the internal's merged window is
        // narrower than its sum so its components scale to fill it. Each component carries
        // the parent group, and the segment components its id.
        let items = vec![
            PipelineItem {
                meta: PipelineItemMeta {
                    id: Some(4),
                    spans: vec![[0, 5], [1, 620], [2, 240]],
                    spans_window: Some((100, 1000)),
                    ..Default::default()
                },
                ..item(2, (100, Some(1000)), None)
            },
            PipelineItem {
                meta: PipelineItemMeta {
                    spans: vec![[7, 10], [8, 300]],
                    spans_window: Some((2000, 2100)),
                    ..Default::default()
                },
                ..item(4, (2000, Some(2050)), None)
            },
        ];
        let rows = pipeline_rows(&items, 0, 6);
        assert_eq!(
            serde_json::to_value(&rows).unwrap(),
            json!([
                [6, 100, 5, { "id": 4, "group": 0 }],
                [7, 105, 620, { "id": 4, "group": 0 }],
                [8, 725, 240, { "id": 4, "group": 0 }],
                [13, 2000, 3, { "group": 1 }],
                [14, 2003, 97, { "group": 1 }],
            ])
        );
        // The scaled internal components fill their 100ms window exactly and never overrun it.
        assert_eq!((2003 - 2000) + 97, 100);
    }

    #[test]
    fn block_subphase_rows_sum_the_item_spans_by_slot() {
        // The block breakdown sums each template slot across every item that carries it, the app
        // segment items filling the segment slots and the leaf and internal items the recursion
        // slots, so a slot present on two items reads their sum and an absent slot yields no row.
        let mut log = Log::new("job");
        log.nodes = vec![
            LogNode {
                id: "node1".to_string(),
                phases: Vec::new(),
                items: vec![
                    PipelineItem {
                        meta: PipelineItemMeta {
                            spans: vec![[0, 100], [1, 620]],
                            ..Default::default()
                        },
                        ..item(2, (0, Some(1)), None)
                    },
                    PipelineItem {
                        meta: PipelineItemMeta {
                            spans: vec![[8, 350]],
                            ..Default::default()
                        },
                        ..item(4, (0, Some(1)), None)
                    },
                ],
                end: None,
            },
            LogNode {
                id: "node2".to_string(),
                phases: Vec::new(),
                items: vec![PipelineItem {
                    meta: PipelineItemMeta {
                        spans: vec![[1, 380], [8, 116]],
                        ..Default::default()
                    },
                    ..item(2, (0, Some(1)), None)
                }],
                end: None,
            },
        ];
        assert_eq!(
            block_subphase_rows(&log),
            vec![[0, 100], [1, 1000], [8, 466]]
        );
        assert!(any_log_spans(std::slice::from_ref(&log)));

        // A log whose items carry no spans yields no rows and does not trigger the template.
        let bare = Log::new("bare");
        assert!(block_subphase_rows(&bare).is_empty());
        assert!(!any_log_spans(std::slice::from_ref(&bare)));
    }

    #[test]
    fn a_block_without_pipeline_omits_the_field() {
        let at = |s: &str| Ts::parse(&format!("2026-01-01T00:00:{s}Z")).unwrap();
        let mut log = Log::new("job");
        log.status = LogStatus::Success;
        log.t_start = Some(at("10"));
        log.participants = vec!["node1".to_string()];
        log.nodes = vec![LogNode {
            id: "node1".to_string(),
            phases: vec![None],
            items: Vec::new(),
            end: None,
        }];
        let node_ids = vec!["node1".to_string()];
        let block = build_block(
            &success_block("a", "20"),
            Some(&log),
            &node_ids,
            1,
            at("10").epoch_ms(),
            &LogWindowSource {
                raw_log: &[],
                starts: &[],
            },
            0,
        );
        let json = serde_json::to_string(&block).unwrap();
        assert!(
            !json.contains("\"pipeline\""),
            "an empty pipeline serializes no key"
        );
    }

    #[test]
    fn benchmark_name_drops_a_trailing_timestamp() {
        // The several timestamped runs of one benchmark share the name with the timestamp dropped,
        // so a run directory without a benchmark.json still merges with its siblings.
        assert_eq!(benchmark_name("eest-60m-20260602-000001"), "eest-60m");
        assert_eq!(benchmark_name("eest-60m-20260602-000002"), "eest-60m");
    }

    #[test]
    fn benchmark_name_keeps_a_run_id_without_a_timestamp() {
        // A run id lacking the -YYYYMMDD-HHMMSS suffix is returned unchanged, so the fixture
        // basename and a hand-named run keep their full id as the name.
        assert_eq!(benchmark_name("fixture"), "fixture");
        assert_eq!(benchmark_name("eest-60m-20260602"), "eest-60m-20260602");
        assert_eq!(
            benchmark_name("eest-60m-2026060-000001"),
            "eest-60m-2026060-000001"
        );
    }
}
