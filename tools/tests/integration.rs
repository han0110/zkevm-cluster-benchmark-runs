//! Integration tests over the committed fixture runs under tests/fixture and tests/fixture-openvm.
//!
//! Each fixture is a self-contained run (logs/ plus zkevm-metrics/) checked into the repository,
//! so these tests exercise the whole pipeline on a fresh checkout with no external data. The zisk
//! fixture holds ten proofs, and the openvm fixture holds two fully logged proofs plus one block
//! whose proof the lossy coordinator capture dropped entirely.

use std::{
    io::Read,
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
};

use tools::parse_benchmark::{
    self,
    input::{
        Sources,
        log::{
            LogStatus,
            zkvm::{detect_backend, openvm, zisk::coordinator},
        },
    },
    parse_to_benchmark,
};

/// The committed fixture run directory.
fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixture")
}

/// The committed openvm fixture run directory.
fn fixture_openvm_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixture-openvm")
}

/// The numeric cells of a wire pipeline row, its optional trailing metadata object dropped.
fn row_cells(row: &[serde_json::Value]) -> Vec<i64> {
    row.iter().filter_map(serde_json::Value::as_i64).collect()
}

/// The id of a wire pipeline row's trailing metadata object, or None on a row without one.
fn row_id(row: &[serde_json::Value]) -> Option<i64> {
    row.last()
        .and_then(|v| v.get("id"))
        .and_then(serde_json::Value::as_i64)
}

/// A fresh scratch directory under the system temp root, for a test that writes a benchmark.json.
fn tempdir() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("tools-it-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The fixture holds ten successful proofs for the consecutive blocks 25192300 through 25192309.
const EXPECTED_PROOFS: usize = 10;

#[test]
fn coordinator_log_parses_without_error() {
    let dir = fixture_dir();
    let coord = std::fs::read_to_string(dir.join("logs/coordinator.log")).unwrap();
    // The committed fixture exercises every recognized line shape, so the real coordinator log
    // must parse without error.
    let logs = coordinator::parse(&coord).unwrap();
    let success = logs
        .iter()
        .filter(|l| l.status == LogStatus::Success)
        .count();
    assert_eq!(success, EXPECTED_PROOFS, "coordinator success logs");
}

#[test]
fn zisk_backend_parses_cluster_logs() {
    let logs_dir = fixture_dir().join("logs");
    let backend = detect_backend(&logs_dir).expect("zisk backend detected");
    let parsed = backend.parse(&logs_dir).unwrap();

    assert_eq!(parsed.name, "zisk");
    assert_eq!(parsed.phases.len(), 5, "phase preset");
    let success = parsed
        .logs
        .iter()
        .filter(|l| l.status == LogStatus::Success)
        .count();
    assert_eq!(success, EXPECTED_PROOFS, "success logs");
    // Every successful log carries exactly one aggregator node, whose final phase window is set.
    assert!(
        parsed
            .logs
            .iter()
            .filter(|l| l.status == LogStatus::Success)
            .all(|l| l
                .nodes
                .iter()
                .filter(|n| n.phases.last().is_some_and(Option::is_some))
                .count()
                == 1)
    );
}

#[test]
fn sources_load_metrics_hardware_and_telemetry() {
    let sources = Sources::load(&fixture_dir()).unwrap();
    assert_eq!(sources.blocks.len(), EXPECTED_PROOFS, "metric blocks");
    assert_eq!(sources.dmon.len(), 4, "dmon nodes");
    assert!(sources.dmon.values().all(|rows| !rows.is_empty()));
    assert_eq!(sources.hardware.gpu_models.len(), 4);
    assert_eq!(sources.meta.guest.as_deref(), Some("zisk-eth-client-reth"));
    assert_eq!(sources.meta.guest_version.as_deref(), Some("v0.9.0"));
    assert_eq!(sources.meta.version.as_deref(), Some("v0.18.0"));
}

#[test]
fn assembles_lean_benchmark_document() {
    let dir = fixture_dir();
    let b = parse_to_benchmark(&dir).unwrap();

    assert_eq!(b.schema_version, 1);
    assert_eq!(b.software.zkvm.name, "zisk");
    assert_eq!(b.software.zkvm.version, "v0.18.0");
    assert_eq!(b.software.guest.name, "zisk-eth-client-reth");
    assert_eq!(b.software.guest.version, "v0.9.0");
    assert_eq!(b.software.zkvm.phases.len(), 5);
    assert_eq!(b.software.zkvm.phases[0].name, "input");
    assert_eq!(b.software.zkvm.phases[0].label, "Input Transfer");
    assert_eq!(b.software.zkvm.phases[3].label, "Prove + Recurse");
    // The fine-pipeline template names the thirteen zisk kinds, whose indices the per-node
    // pipeline rows reference. The wc kind appears once per side of the prove boundary.
    assert_eq!(b.software.zkvm.pipeline.len(), 13);
    assert_eq!(b.software.zkvm.pipeline[2].name, "wc_contribution");
    assert_eq!(b.software.zkvm.pipeline[2].phase, "commit");
    assert!(b.software.zkvm.pipeline[2].paired);
    assert_eq!(b.software.zkvm.pipeline[7].name, "wc_proof");
    assert_eq!(b.software.zkvm.pipeline[7].phase, "prove");
    assert!(b.software.zkvm.pipeline[7].paired);
    // A fresh parse yields one run, and the fixture basename carries no timestamp so the benchmark
    // id and the run id are both the bare basename.
    assert_eq!(b.id, "fixture");
    // The name and description come from the run directory's input benchmark.json.
    assert_eq!(b.name, "fixture");
    assert_eq!(
        b.description,
        "Committed ten-proof zisk fixture run for the parser integration tests."
    );
    assert_eq!(b.runs.len(), 1);
    let run = &b.runs[0];
    assert_eq!(run.id, "fixture");
    assert!(run.started_at > 0);
    assert_eq!(run.block_count, EXPECTED_PROOFS);
    assert_eq!(run.success_count, EXPECTED_PROOFS);
    assert_eq!(run.failure_count, 0);
    assert_eq!(b.hardware.gpu_models.len(), 4);
    assert_eq!(b.hardware.nodes.len(), 4);

    assert_eq!(run.blocks.len(), EXPECTED_PROOFS);
    assert!(run.blocks.iter().all(|bl| bl.status == "success"));
    assert!(
        run.blocks
            .iter()
            .all(|bl| bl.proving_ms.is_some_and(|m| m > 0))
    );
    assert!(run.blocks.iter().all(|bl| bl.nodes.len() == 4));
    // A clean run leaves no node a crash marker.
    assert!(
        run.blocks
            .iter()
            .all(|bl| bl.nodes.iter().all(|n| n.crashed_ms.is_none()))
    );
    assert!(
        run.blocks
            .iter()
            .all(|bl| bl.nodes.iter().all(|n| n.phases.len() == 5))
    );
    // The aggregator is inferred from the single node carrying a non-null fifth (aggregate) window.
    assert!(
        run.blocks
            .iter()
            .all(|bl| bl.nodes.iter().filter(|n| n.phases[4].is_some()).count() == 1)
    );

    // Each block is identified by its metric file name verbatim, in completion order.
    let names: Vec<&str> = run.blocks.iter().map(|bl| bl.name.as_str()).collect();
    let expected: Vec<String> = (25192300..=25192309)
        .map(|n| format!("rpc_block_{n}"))
        .collect();
    assert_eq!(
        names,
        expected.iter().map(String::as_str).collect::<Vec<_>>()
    );
    // Block names are unique within the run, the invariant the parser asserts and the views rely
    // on.
    let mut unique = names.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), names.len(), "block names are unique");

    let total_gas: u64 = run.blocks.iter().filter_map(|bl| bl.gas_used).sum();
    assert_eq!(total_gas, 279_981_944);
    assert_eq!(run.statistics.p50_proving_ms, Some(6202));
    assert!(run.statistics.mean_proving_ms.is_some());
    assert!(run.statistics.mean_gas_per_s.is_some());
    assert_eq!(run.statistics.nodes.len(), 4);

    assert_eq!(run.telemetry.nodes.len(), 4);
    assert_eq!(run.telemetry.metrics.len(), 12);
    // The three memory metrics trail the catalog so the frontend charts them at the end. Pinning
    // their name, label, and unit catches a future name, label, or ordering regression the count
    // misses.
    let tail: Vec<(&str, &str, &str)> = run.telemetry.metrics[9..]
        .iter()
        .map(|m| (m.name.as_str(), m.label.as_str(), m.unit.as_str()))
        .collect();
    assert_eq!(
        tail,
        vec![
            ("fb", "Frame Buffer Memory", "MiB"),
            ("bar1", "BAR1 Memory", "MiB"),
            ("ccpm", "Protected Memory", "MiB"),
        ]
    );
    for node in &run.telemetry.nodes {
        assert_eq!(node.metrics.len(), 12);
        for grid in node.metrics.values() {
            assert_eq!(grid.len(), 4);
        }
    }

    // Telemetry is normalized onto one shared one-second axis anchored at the run epoch. Every node
    // grid has the same tick width regardless of how many seconds that node actually sampled, so
    // the grids align by index. The fixture spans 223 seconds from the earliest reading to the
    // latest across the four workers.
    let widths: Vec<usize> = run
        .telemetry
        .nodes
        .iter()
        .flat_map(|n| n.metrics.values().map(|g| g[0].len()))
        .collect();
    assert!(
        widths.iter().all(|&w| w == 223),
        "all node grids share the 223-second axis, got {widths:?}"
    );

    // A node that started after the earliest reading carries leading nulls, and a node with a
    // dropped second carries an interior null between two real readings. Both prove missing
    // seconds are filled rather than collapsed, which is what keeps later readings from sliding
    // earlier on the axis.
    let pwr_rows: Vec<_> = run
        .telemetry
        .nodes
        .iter()
        .filter_map(|n| n.metrics.get("pwr").map(|g| &g[0]))
        .collect();
    let leading_null = pwr_rows
        .iter()
        .any(|row| row.first().is_some_and(|v| v.is_null()));
    assert!(
        leading_null,
        "a late-starting node must carry leading null telemetry"
    );
    let interior_gap = pwr_rows.iter().any(|row| {
        (1..row.len().saturating_sub(1))
            .any(|i| row[i].is_null() && !row[i - 1].is_null() && !row[i + 1].is_null())
    });
    assert!(
        interior_gap,
        "an interior dropped second must be a null between two readings"
    );
}

#[test]
fn first_block_matches_known_values() {
    let dir = fixture_dir();
    let b = parse_to_benchmark(&dir).unwrap();
    let first = b.runs[0]
        .blocks
        .iter()
        .find(|bl| bl.name == "rpc_block_25192300")
        .expect("block rpc_block_25192300");

    assert_eq!(first.proving_ms, Some(7342), "block 25192300 proving_ms");
    assert_eq!(first.gas_used, Some(29758135), "block 25192300 gas");
    assert_eq!(
        first.meta.get("steps").and_then(|v| v.as_u64()),
        Some(304964424),
        "block 25192300 steps"
    );
    // node3 aggregated block 25192300, and nodes are emitted in sorted id order, so node3 is index
    // 2.
    assert!(
        first.nodes[2].phases[4].is_some(),
        "node3 carries the aggregate window"
    );
    assert!(
        (0..4)
            .filter(|&i| i != 2)
            .all(|i| first.nodes[i].phases[4].is_none()),
        "only the aggregator carries the aggregate window"
    );
}

#[test]
fn blocks_carry_pipeline_items() {
    let dir = fixture_dir();
    let b = parse_to_benchmark(&dir).unwrap();
    let kind_count = b.software.zkvm.pipeline.len() as i64;
    let mut cached_proof_rows = 0;
    let mut full_contribution_rows = 0;
    let mut lone_contribution_rows = 0;
    for block in &b.runs[0].blocks {
        for (ni, node) in block.nodes.iter().enumerate() {
            // Every node works dozens of brackets per block, so a thin count means lost items.
            assert!(
                node.pipeline.len() >= 10,
                "block {} node {ni} carries only {} pipeline rows",
                block.name,
                node.pipeline.len()
            );
            let aggregator = node.phases[4].is_some();
            let rows: Vec<Vec<i64>> = node.pipeline.iter().map(|r| row_cells(r)).collect();
            for row in &rows {
                assert!(
                    (0..kind_count).contains(&row[0]),
                    "kind {} out of template range",
                    row[0]
                );
                // The clean fixture leaves nothing dangling, so every row is a complete segment
                // or a complete pair.
                assert!(
                    row.len() == 3 || row.len() == 5,
                    "unexpected arity in block {} node {ni}: {row:?}",
                    block.name
                );
                // The vadcop kinds run only on the node that aggregated the block.
                assert!(
                    !(row[0] == 11 || row[0] == 12) || aggregator,
                    "vadcop row on non-aggregator node {ni} of block {}",
                    block.name
                );
                if row[0] == 7 && row.len() == 3 {
                    cached_proof_rows += 1;
                }
                if row[0] == 2 && row.len() == 5 {
                    full_contribution_rows += 1;
                }
                if row[0] == 2 && row.len() == 3 {
                    lone_contribution_rows += 1;
                }
            }
            assert!(
                rows.windows(2)
                    .all(|w| (w[0][1], w[0][0]) <= (w[1][1], w[1][0])),
                "pipeline rows out of (start, kind) order in block {}",
                block.name
            );
        }
    }
    // A proof over a cached witness is a compute-only row, and a generated witness claimed by its
    // contribution is a full pair. Nearly every contribution pairs its witness, with only the
    // table airs left compute-only, so the paired rows dominate the lone ones.
    assert!(cached_proof_rows > 0, "no compute-only wc_proof row");
    assert!(
        full_contribution_rows > lone_contribution_rows,
        "paired wc_contribution rows must outnumber lone ones, \
         got {full_contribution_rows} paired vs {lone_contribution_rows} lone"
    );
}

#[test]
fn block_logs_capture_the_proving_window() {
    let dir = fixture_dir();
    let b = parse_to_benchmark(&dir).unwrap();
    let first = b.runs[0]
        .blocks
        .iter()
        .find(|bl| bl.name == "rpc_block_25192300")
        .expect("block rpc_block_25192300");

    // The block carries the coordinator and worker log lines of its proving window.
    assert!(!first.logs.is_empty(), "block carries log lines");
    let roles: std::collections::BTreeSet<&str> =
        first.logs.iter().map(|l| l.role.as_str()).collect();
    assert!(roles.contains("coordinator"), "coordinator lines present");
    assert!(
        roles.iter().any(|r| r.starts_with("worker")),
        "worker lines present, tagged by worker number"
    );
    // Every level is kept, so the bulk DEBUG worker trace is captured rather than dropped.
    assert!(
        first.logs.iter().any(|l| l.level == "debug"),
        "debug lines are captured"
    );
    // The module path that precedes a level token is dropped, so no message begins with a
    // `module::path` token the way the raw worker lines do.
    assert!(
        first.logs.iter().all(|l| l
            .msg
            .split_whitespace()
            .next()
            .is_none_or(|w| !w.contains("::"))),
        "the leading module path is stripped from the message"
    );
    // The lines are in microsecond time order, each rebased to an offset from the block start.
    assert!(
        first.logs.windows(2).all(|w| w[0].time <= w[1].time),
        "lines are time ordered"
    );
    assert_eq!(
        first.logs.first().unwrap().time,
        0,
        "the first line sits at the block start"
    );
    // The log is bounded by the block's proving window, not the whole 223-second run. Time is in
    // microseconds, so the bound is 30 seconds expressed in microseconds.
    let last = first.logs.last().unwrap().time;
    assert!(
        last < 30_000_000,
        "lines stay within the proving window, got {last}us"
    );
}

#[test]
fn writes_lean_document_with_sidecar_per_block_log_files() {
    let dir = tempdir();
    let out = dir.join("benchmark.json");
    parse_benchmark::run(&[fixture_dir()], &out, false, false).expect("write succeeds");

    // benchmark.json stays lean, carrying no inline block logs.
    let doc_text = std::fs::read_to_string(&out).unwrap();
    assert!(
        !doc_text.contains("\"logs\""),
        "benchmark.json carries no inline logs"
    );

    // Each block's logs land in a per-block tar.json under log/{bench_id}/{run_id}/ named by the
    // block, holding the role, time, level, and message of each kept line. The file is a gzipped
    // tar carrying a .tar.json suffix. The fixture's bench id and run id are both "fixture",
    // and the block name is verbatim.
    let log_file = dir
        .join("log")
        .join("fixture")
        .join("fixture")
        .join("rpc_block_25192300.tar.json");
    assert!(log_file.is_file(), "per-block log archive written");

    // The archive gunzips and untars to a single member whose JSON is the block's log lines.
    let gz = flate2::read::GzDecoder::new(std::fs::File::open(&log_file).unwrap());
    let mut archive = tar::Archive::new(gz);
    let mut member = archive.entries().unwrap().next().unwrap().unwrap();
    let mut text = String::new();
    member.read_to_string(&mut text).unwrap();
    let entries: serde_json::Value = serde_json::from_str(&text).unwrap();
    let arr = entries.as_array().expect("an array of log lines");
    assert!(!arr.is_empty(), "the block's log archive is not empty");
    let first = &arr[0];
    assert!(first.get("role").and_then(|v| v.as_str()).is_some());
    assert!(first.get("time").and_then(|v| v.as_i64()).is_some());
    assert!(first.get("level").and_then(|v| v.as_str()).is_some());
    assert!(first.get("msg").and_then(|v| v.as_str()).is_some());
}

/// Asserts the fixture serializes byte-for-byte to the committed golden document, guarding the lean
/// schema, its field order, and its number formatting against unintended drift. Regenerate the
/// golden with `cargo run -- parse-benchmark --input tests/fixture --output
/// tests/fixture/output.json --force` only when a change to the document is intended.
#[test]
fn fixture_serializes_byte_for_byte_to_the_golden_document() {
    let generated =
        tools::parse_benchmark::output::to_json(&parse_to_benchmark(&fixture_dir()).unwrap())
            .unwrap();
    let expected = std::fs::read_to_string(fixture_dir().join("output.json")).unwrap();
    assert_eq!(
        generated, expected,
        "serialized benchmark.json drifted from tests/fixture/output.json"
    );
}

/// Writing refuses to clobber an existing output without --force, and --force replaces it.
#[test]
fn write_refuses_to_overwrite_without_force() {
    let dir = tempdir();
    let out = dir.join("benchmark.json");

    parse_benchmark::run(&[fixture_dir()], &out, false, false).expect("first write succeeds");
    let err = parse_benchmark::run(&[fixture_dir()], &out, false, false)
        .expect_err("a second write without --force is refused");
    assert!(
        matches!(err, parse_benchmark::ParseError::OutputExists(_)),
        "expected OutputExists, got {err:?}"
    );
    parse_benchmark::run(&[fixture_dir()], &out, true, false).expect("--force overwrites");

    let doc = parse_benchmark::output::read(&out).unwrap();
    assert_eq!(doc.runs.len(), 1, "a forced overwrite is still one run");
}

/// Patching the same fixture twice appends a second run, suffixing the duplicate run id, and keeps
/// the cluster identity once.
#[test]
fn patch_appends_a_second_run_and_dedupes_the_id() {
    let dir = tempdir();
    let out = dir.join("benchmark.json");

    let missing = parse_benchmark::run(&[fixture_dir()], &out, false, true)
        .expect_err("patching a missing target is refused");
    assert!(
        matches!(missing, parse_benchmark::ParseError::PatchTargetMissing(_)),
        "expected PatchTargetMissing, got {missing:?}"
    );

    parse_benchmark::run(&[fixture_dir()], &out, false, false).expect("seed the document");
    let added =
        parse_benchmark::run(&[fixture_dir()], &out, false, true).expect("patch appends a run");
    assert_eq!(
        added, EXPECTED_PROOFS,
        "the patch added the fixture's blocks"
    );

    let doc = parse_benchmark::output::read(&out).unwrap();
    assert_eq!(doc.runs.len(), 2);
    assert_eq!(doc.id, "fixture");
    assert_eq!(doc.name, "fixture");
    // The re-parsed run carries the same id, so the append suffixes the duplicate to stay
    // addressable.
    assert_eq!(doc.runs[0].id, "fixture");
    assert_eq!(doc.runs[1].id, "fixture-patch-1");
    // Both runs carry the fixture's ten blocks, since each is a full parse of the same directory.
    assert_eq!(doc.runs[1].blocks.len(), EXPECTED_PROOFS);
}

/// The openvm fixture holds two fully logged proofs, blocks 25580000 and 25580001, plus block
/// 25580606 whose proof the lossy coordinator capture dropped entirely.
const EXPECTED_OPENVM_LOGGED: usize = 2;

#[test]
fn openvm_coordinator_log_parses_without_error() {
    let dir = fixture_openvm_dir();
    let coord = std::fs::read_to_string(dir.join("logs/coordinator.log")).unwrap();
    let logs = openvm::coordinator::parse(&coord).unwrap();
    let success = logs
        .iter()
        .filter(|l| l.status == LogStatus::Success)
        .count();
    assert_eq!(success, EXPECTED_OPENVM_LOGGED, "coordinator success logs");
}

#[test]
fn openvm_backend_parses_cluster_logs() {
    let logs_dir = fixture_openvm_dir().join("logs");
    let backend = detect_backend(&logs_dir).expect("openvm backend detected");
    let parsed = backend.parse(&logs_dir).unwrap();

    assert_eq!(parsed.name, "openvm");
    assert_eq!(parsed.phases.len(), 5, "phase preset");
    // Execution, segment, and recursion are the overlap phases, since the executor meters segments
    // while they prove and recursion consumes segments as they complete.
    let preset: Vec<(&str, bool)> = parsed
        .phases
        .iter()
        .map(|p| (p.name.as_str(), p.overlap))
        .collect();
    assert_eq!(
        preset,
        vec![
            ("input", false),
            ("execution", true),
            ("segment", true),
            ("recursion", true),
            ("wrap", false),
        ]
    );
    // No openvm kind is paired, the work items each logging one self-contained completion span,
    // execution deriving one span per sent segment, and fast-forward taken from each proof line.
    assert_eq!(parsed.pipeline.len(), 6, "kind template");
    assert!(parsed.pipeline.iter().all(|k| !k.paired));
    let success = parsed
        .logs
        .iter()
        .filter(|l| l.status == LogStatus::Success)
        .count();
    assert_eq!(success, EXPECTED_OPENVM_LOGGED, "success logs");
    // Every successful log carries exactly one aggregator node, whose final phase window is set.
    assert!(
        parsed
            .logs
            .iter()
            .filter(|l| l.status == LogStatus::Success)
            .all(|l| l
                .nodes
                .iter()
                .filter(|n| n.phases.last().is_some_and(Option::is_some))
                .count()
                == 1)
    );
}

#[test]
fn openvm_assembles_document_with_an_unlogged_block() {
    let dir = fixture_openvm_dir();
    let b = parse_to_benchmark(&dir).unwrap();

    assert_eq!(b.software.zkvm.name, "openvm");
    assert_eq!(b.software.zkvm.version, "8f86342");
    assert_eq!(b.software.guest.name, "reth");
    assert_eq!(b.software.guest.version, "c5dff62");
    assert_eq!(b.software.zkvm.phases[1].name, "execution");
    assert_eq!(b.software.zkvm.phases[1].label, "Execution");
    assert_eq!(b.software.zkvm.phases[4].label, "Wrap");
    // The pipeline kinds keep their wire indices and labels while their phase ownership follows
    // the preset. Fast-forward is the prover's re-execution, so it shares the execution phase.
    let owners: Vec<(&str, &str)> = b
        .software
        .zkvm
        .pipeline
        .iter()
        .map(|k| (k.name.as_str(), k.phase.as_str()))
        .collect();
    assert_eq!(
        owners,
        vec![
            ("execution", "execution"),
            ("fastfwd", "execution"),
            ("app_segment", "segment"),
            ("leaf", "recursion"),
            ("internal", "recursion"),
            ("wrap", "wrap"),
        ]
    );
    assert_eq!(b.hardware.nodes.len(), 4);

    let run = &b.runs[0];
    // The metric archive is the source of truth for which blocks exist, so the coordinator-dropped
    // block is still a block, and every metric reports success.
    assert_eq!(run.block_count, 3);
    assert_eq!(run.success_count, 3);
    // The block id comes from the metric metadata's original test name, not the verbose archive
    // name field.
    let names: Vec<&str> = run.blocks.iter().map(|bl| bl.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["mainnet_25580000", "mainnet_25580001", "mainnet_25580606"]
    );

    let first = &run.blocks[0];
    assert_eq!(first.proving_ms, Some(4883));
    assert_eq!(first.gas_used, Some(26211834));
    assert_eq!(
        first.meta.get("segments").and_then(|v| v.as_u64()),
        Some(52)
    );
    assert_eq!(
        first.meta.get("input_size").and_then(|v| v.as_u64()),
        Some(6711680)
    );
    assert!(first.nodes.iter().all(|n| n.phases.len() == 5));
    // Every participating node carries an input transfer window ending at its last
    // input-file-written time, at or before its execution starts, the small honest gap between the
    // input write and the worker taking up the proof.
    assert!(first.nodes.iter().all(|n| n.phases[0].is_some()));
    assert!(
        first
            .nodes
            .iter()
            .all(|n| match (&n.phases[0], &n.phases[1]) {
                (Some(input), Some(execution)) =>
                    input.start_ms + input.dur_ms <= execution.start_ms,
                _ => false,
            })
    );
    // Every participating node of a logged block carries an execution window that overlaps its
    // segment envelope, starting at or before the envelope and ending after it begins.
    assert!(first.nodes.iter().all(|n| n.phases[1].is_some()));
    assert!(
        first
            .nodes
            .iter()
            .all(|n| match (&n.phases[1], &n.phases[2]) {
                (Some(execution), Some(segment)) =>
                    execution.start_ms <= segment.start_ms
                        && segment.start_ms < execution.start_ms + execution.dur_ms,
                _ => false,
            })
    );
    // Exactly one node carries the wrap window, the one whose worker wrapped the root proof.
    assert_eq!(
        first.nodes.iter().filter(|n| n.phases[4].is_some()).count(),
        1
    );

    // The dropped block binds no log, so it renders from its metric alone, every phase null and no
    // pipeline, with participation defaulting to the full cluster.
    let dropped = &run.blocks[2];
    assert_eq!(dropped.proving_ms, Some(4851));
    assert!(dropped.meta.is_empty());
    assert!(
        dropped
            .nodes
            .iter()
            .all(|n| n.phases.iter().all(Option::is_none) && n.pipeline.is_empty())
    );
    assert!(dropped.nodes.iter().all(|n| n.participated));
}

#[test]
fn openvm_blocks_carry_pipeline_items() {
    let dir = fixture_openvm_dir();
    let b = parse_to_benchmark(&dir).unwrap();
    let kind_count = b.software.zkvm.pipeline.len() as i64;
    // Kind indices of the openvm template, in wire order.
    let (execution, fastfwd, app_segment, leaf, wrap) = (0, 1, 2, 3, 5);

    for (block, segments) in b.runs[0].blocks.iter().take(2).zip([52i64, 62]) {
        let rows: Vec<&Vec<serde_json::Value>> =
            block.nodes.iter().flat_map(|n| &n.pipeline).collect();
        let cells: Vec<Vec<i64>> = rows.iter().map(|r| row_cells(r)).collect();
        assert!(
            cells.iter().all(|r| (0..kind_count).contains(&r[0])),
            "kind out of template range in {}",
            block.name
        );
        // Completion-line records leave nothing dangling and no openvm kind is paired, so every
        // row is one complete span.
        assert!(cells.iter().all(|r| r.len() == 3));
        // One app row per segment, their ids exactly the segment indices the workers named.
        let ids_of = |kind: i64| -> Vec<i64> {
            let mut ids: Vec<i64> = rows
                .iter()
                .zip(&cells)
                .filter(|(_, c)| c[0] == kind)
                .map(|(r, _)| row_id(r).expect("row without an id"))
                .collect();
            ids.sort_unstable();
            ids
        };
        assert_eq!(
            ids_of(app_segment),
            (0..segments).collect::<Vec<i64>>(),
            "app segment ids in {}",
            block.name
        );
        // Every leaf row carries its leaf index, distinct and within the arity-4 leaf count.
        let leaf_ids = ids_of(leaf);
        assert!(!leaf_ids.is_empty(), "no leaf rows in {}", block.name);
        assert!(
            leaf_ids.windows(2).all(|w| w[0] < w[1]),
            "duplicate leaf ids in {}",
            block.name
        );
        assert!(
            leaf_ids
                .iter()
                .all(|id| (0..(segments + 3) / 4).contains(id)),
            "leaf id out of range in {}",
            block.name
        );
        // One derived execution row per sent segment, their ids exactly the segment indices, one
        // execution row for every app segment across the block.
        assert_eq!(
            ids_of(execution),
            (0..segments).collect::<Vec<i64>>(),
            "execution ids in {}",
            block.name
        );
        // One fast-forward row per segment, stored even at zero width, so their ids are exactly the
        // segment indices.
        assert_eq!(
            ids_of(fastfwd),
            (0..segments).collect::<Vec<i64>>(),
            "fast-forward ids in {}",
            block.name
        );
        for node in &block.nodes {
            let node_cells: Vec<Vec<i64>> = node.pipeline.iter().map(|r| row_cells(r)).collect();
            // Each execution and fast-forward row shares its id with an app segment proved on the
            // same node, the worker having metered, fast-forwarded, and proved that segment.
            let app_ids: std::collections::BTreeSet<i64> = node
                .pipeline
                .iter()
                .filter(|r| row_cells(r)[0] == app_segment)
                .map(|r| row_id(r).expect("app segment row without an id"))
                .collect();
            assert!(
                node.pipeline
                    .iter()
                    .filter(|r| row_cells(r)[0] == execution || row_cells(r)[0] == fastfwd)
                    .all(|r| app_ids.contains(
                        &row_id(r).expect("execution or fast-forward row without an id")
                    )),
                "an execution or fast-forward row's id is not an app segment on the same node in {}",
                block.name
            );
            // The final wrap runs only on the node that carries the wrap window.
            let wraps = node_cells.iter().filter(|r| r[0] == wrap).count();
            assert_eq!(wraps, usize::from(node.phases[4].is_some()));
            assert!(
                node_cells
                    .windows(2)
                    .all(|w| (w[0][1], w[0][0]) <= (w[1][1], w[1][0])),
                "pipeline rows out of (start, kind) order in {}",
                block.name
            );
        }
    }
}

#[test]
fn openvm_sidecars_skip_the_unlogged_block() {
    let dir = tempdir();
    let out = dir.join("benchmark.json");
    parse_benchmark::run(&[fixture_openvm_dir()], &out, false, false).expect("write succeeds");

    let log_dir = dir
        .join("log")
        .join("fixture-openvm")
        .join("fixture-openvm");
    assert!(log_dir.join("mainnet_25580000.tar.json").is_file());
    assert!(log_dir.join("mainnet_25580001.tar.json").is_file());
    // A block with no captured log lines writes no archive, which the frontend reports as absent.
    assert!(!log_dir.join("mainnet_25580606.tar.json").exists());
}

/// Asserts the openvm fixture serializes byte-for-byte to the committed golden document.
/// Regenerate the golden with `cargo run -- parse-benchmark --input tests/fixture-openvm --output
/// tests/fixture-openvm/output.json --force` only when a change to the document is intended.
#[test]
fn openvm_fixture_serializes_byte_for_byte_to_the_golden_document() {
    let generated = tools::parse_benchmark::output::to_json(
        &parse_to_benchmark(&fixture_openvm_dir()).unwrap(),
    )
    .unwrap();
    let expected = std::fs::read_to_string(fixture_openvm_dir().join("output.json")).unwrap();
    assert_eq!(
        generated, expected,
        "serialized benchmark.json drifted from tests/fixture-openvm/output.json"
    );
}

/// A patch is refused when the run's benchmark name differs from the existing document, so a run is
/// never appended to a different benchmark.
#[test]
fn patch_refuses_a_different_benchmark_name() {
    let dir = tempdir();
    let out = dir.join("benchmark.json");

    parse_benchmark::run(&[fixture_dir()], &out, false, false).expect("seed the document");
    // Rename the seeded benchmark, so the next patch arrives from a run whose name no longer
    // matches.
    let mut doc = parse_benchmark::output::read(&out).unwrap();
    doc.name = "other-benchmark".to_string();
    parse_benchmark::output::write(&doc, &out).unwrap();

    let err = parse_benchmark::run(&[fixture_dir()], &out, false, true)
        .expect_err("a name mismatch is refused");
    assert!(
        matches!(err, parse_benchmark::ParseError::PatchMismatch("name")),
        "expected PatchMismatch(name), got {err:?}"
    );
}
