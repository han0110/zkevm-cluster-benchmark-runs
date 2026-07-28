//! The parse-benchmark subcommand. The input stage reads a run's cluster logs and measurement
//! sources into an intermediate model, and the output stage assembles them into benchmark.json.

pub mod error;
pub mod input;
pub mod output;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

pub use error::{ParseError, Result};
pub(crate) use error::{io_at, json_at, read_dir_at, read_to_string_at};

use crate::parse_benchmark::{
    input::{BenchMeta, Sources, log::zkvm::detect_backend},
    output::{Benchmark, assemble::assemble},
};

/// Arguments for the parse-benchmark subcommand.
#[derive(clap::Args)]
pub struct ParseBenchmarkArgs {
    /// Run directory containing zkevm-metrics/ and logs/. This flag is repeatable, every --input
    /// being a run of the one benchmark, the runs sorted by id and the earliest creating the
    /// document the rest are patched into.
    #[arg(long, required = true)]
    pub input: Vec<PathBuf>,

    /// Output path for the generated benchmark document. It must differ from the run directory's
    /// input benchmark.json, which holds the benchmark name and description.
    #[arg(long)]
    pub output: PathBuf,

    /// Overwrite the output file when it already exists.
    #[arg(long, conflicts_with = "patch")]
    pub force: bool,

    /// Append the run to the existing benchmark.json at the output path, asserting the same
    /// cluster.
    #[arg(long)]
    pub patch: bool,

    /// Compress the document, appending a .zstd suffix to the output path. The frontend loads the
    /// compressed document, the plain JSON serving only for debugging.
    #[arg(long)]
    pub zstd: bool,
}

impl ParseBenchmarkArgs {
    /// The path the document is written to, the output path with a .zstd suffix when compressing.
    /// The suffix is appended rather than replacing the .json one, so the file names the format it
    /// decompresses to and the sibling log tree still resolves from the same parent.
    pub fn output_path(&self) -> PathBuf {
        if self.zstd {
            self.output.with_added_extension("zstd")
        } else {
            self.output.clone()
        }
    }
}

/// Detects the zkVM, parses the logs, loads the run's sources, and assembles the document.
pub fn parse_to_benchmark(input: &Path) -> Result<Benchmark> {
    let logs_dir = input.join("logs");
    let backend = detect_backend(&logs_dir).ok_or(ParseError::UnknownZkvm(logs_dir.clone()))?;
    let parsed = backend.parse(&logs_dir)?;
    let sources = Sources::load(input)?;
    let meta = BenchMeta::load(input)?;
    let run_id = match run_id_of(input).as_str() {
        "" => "run".to_string(),
        id => id.to_string(),
    };
    let benchmark = assemble(parsed, sources, &run_id, meta.name, meta.description);
    assert_unique_block_names(&benchmark)?;
    Ok(benchmark)
}

/// Asserts every block name is unique within each run. Block names are the metric file names, the
/// key the views index on, so a duplicate would silently collide one block onto another.
fn assert_unique_block_names(benchmark: &Benchmark) -> Result<()> {
    for run in &benchmark.runs {
        let mut seen = HashSet::new();
        for block in &run.blocks {
            if !seen.insert(block.name.as_str()) {
                return Err(ParseError::DuplicateBlockName(block.name.clone()));
            }
        }
    }
    Ok(())
}

/// Parses every input run into one document at the output path, the runs taken in run-id order so
/// the earliest creates the document and each later run is patched onto it. The outcome matches
/// parsing the first run alone and patching each later run in turn. The returned count totals the
/// blocks every run contributed.
pub fn run(inputs: &[PathBuf], output: &Path, force: bool, patch: bool) -> Result<usize> {
    let mut ordered: Vec<&PathBuf> = inputs.iter().collect();
    ordered.sort_by_key(|a| run_id_of(a));
    let mut total = 0;
    for (i, input) in ordered.iter().enumerate() {
        // The earliest run creates the document unless patching onto an existing one, and only that
        // creating run honours force, with every later run always a patch.
        total += run_one(input, output, force && i == 0, patch || i > 0)?;
    }
    Ok(total)
}

/// The run id of an input directory, its basename, the key the runs are ordered by.
fn run_id_of(input: &Path) -> String {
    input
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

/// Parses the run at the input directory and persists it to the output path.
///
/// Without `patch` the run starts a fresh document, refusing to clobber an existing output unless
/// `force` is set. With `patch` the run is appended to the existing same-cluster document at the
/// output path. The returned count is the number of blocks the run contributed.
fn run_one(input: &Path, output: &Path, force: bool, patch: bool) -> Result<usize> {
    let parsed = parse_to_benchmark(input)?;
    // The per-block log files live in a log/{bench_id}/{run_id}/ tree beside the flat
    // benchmark.json.
    let base = output.parent().unwrap_or_else(|| Path::new("."));
    if patch {
        if !output.exists() {
            return Err(ParseError::PatchTargetMissing(output.to_path_buf()));
        }
        let mut existing = output::read(output)?;
        let added = merge_run(&mut existing, parsed)?;
        let bench_id = existing.id.clone();
        output::write(&existing, output)?;
        // Only the appended run carries logs in memory. The existing runs' log files are already on
        // disk, so just the new run's per-block logs are written.
        if let Some(appended) = existing.runs.last() {
            output::write_block_logs(appended, base, &bench_id)?;
        }
        return Ok(added);
    }
    if output.exists() && !force {
        return Err(ParseError::OutputExists(output.to_path_buf()));
    }
    let count = parsed.runs.iter().map(|r| r.blocks.len()).sum();
    output::write(&parsed, output)?;
    for run in &parsed.runs {
        output::write_block_logs(run, base, &parsed.id)?;
    }
    Ok(count)
}

/// Appends the parsed run to an existing document, returning the number of blocks it added.
///
/// The append is refused unless hardware and software match, so a document never mixes clusters. A
/// run id already present is suffixed with -patch-N so every run stays addressable, which is what a
/// re-run of the same run directory produces.
fn merge_run(existing: &mut Benchmark, mut parsed: Benchmark) -> Result<usize> {
    if existing.hardware != parsed.hardware {
        return Err(ParseError::PatchMismatch("hardware"));
    }
    // The pipeline and sub-phase templates are wider on a new-format run, which appends the
    // component kinds after the base kinds and carries the sub-phase template, so a patched-image
    // run and an unpatched one still describe the same cluster. The rest of the software must
    // match, and the two templates are reconciled below.
    let software_matches = existing.software.guest == parsed.software.guest
        && existing.software.zkvm.name == parsed.software.zkvm.name
        && existing.software.zkvm.version == parsed.software.zkvm.version
        && existing.software.zkvm.phases == parsed.software.zkvm.phases;
    if !software_matches {
        return Err(ParseError::PatchMismatch("software"));
    }
    // The component kinds append after the base kinds, so a base-only run and a
    // base-plus-components run share the base prefix and their rows stay index-aligned. Their
    // common prefix must match, and the merged document keeps the longer template so every
    // run's rows resolve their kinds.
    let existing_pipeline = &existing.software.zkvm.pipeline;
    let parsed_pipeline = &parsed.software.zkvm.pipeline;
    let prefix = existing_pipeline.len().min(parsed_pipeline.len());
    if existing_pipeline[..prefix] != parsed_pipeline[..prefix] {
        return Err(ParseError::PatchMismatch("pipeline"));
    }
    let adopt_pipeline = parsed_pipeline.len() > existing_pipeline.len();
    // At most one side may carry the sub-phase template, the merged document keeping the non-empty
    // one, since a template-less run's blocks carry no sub-phase rows to misalign. Two differing
    // non-empty templates would misalign the block row indices, so they are refused.
    let existing_template = &existing.software.zkvm.subphases;
    let parsed_template = &parsed.software.zkvm.subphases;
    if !existing_template.is_empty()
        && !parsed_template.is_empty()
        && existing_template != parsed_template
    {
        return Err(ParseError::PatchMismatch("subphases"));
    }
    let adopt_template = existing_template.is_empty() && !parsed_template.is_empty();
    if existing.name != parsed.name {
        return Err(ParseError::PatchMismatch("name"));
    }
    if adopt_pipeline {
        existing.software.zkvm.pipeline = std::mem::take(&mut parsed.software.zkvm.pipeline);
    }
    if adopt_template {
        existing.software.zkvm.subphases = std::mem::take(&mut parsed.software.zkvm.subphases);
    }
    let mut run = parsed
        .runs
        .into_iter()
        .next()
        .expect("a freshly parsed benchmark carries exactly one run");
    let added = run.blocks.len();
    let mut id = run.id.clone();
    let mut n = 1;
    while existing.runs.iter().any(|r| r.id == id) {
        id = format!("{}-patch-{n}", run.id);
        n += 1;
    }
    run.id = id;
    existing.runs.push(run);
    Ok(added)
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::fs::symlink,
        path::PathBuf,
        sync::atomic::{AtomicU32, Ordering},
    };

    use crate::parse_benchmark::{
        ParseBenchmarkArgs, ParseError, merge_run,
        output::{
            Benchmark,
            schema::{PipelineStep, SubPhase},
        },
        parse_to_benchmark,
    };

    /// The compression flag appends the suffix to the output path rather than replacing the .json
    /// one, so the file names the format it decompresses to.
    #[test]
    fn the_compression_flag_appends_the_suffix_to_the_output_path() {
        let args = |zstd| ParseBenchmarkArgs {
            input: vec![PathBuf::from("run")],
            output: PathBuf::from("data/bench.json"),
            force: false,
            patch: false,
            zstd,
        };
        assert_eq!(args(false).output_path(), PathBuf::from("data/bench.json"));
        assert_eq!(
            args(true).output_path(),
            PathBuf::from("data/bench.json.zstd")
        );
    }

    /// A one-entry sub-phase template naming the given sub-step under the segment owner.
    fn segment_template(name: &str) -> Vec<SubPhase> {
        vec![SubPhase {
            name: name.to_string(),
            label: name.to_string(),
            phase: "segment".to_string(),
        }]
    }

    /// The seven STARK sub-steps, in template order.
    const SUB_STEPS: [&str; 7] = [
        "execute_preflight",
        "trace_gen",
        "main_trace_commit",
        "logup_gkr",
        "round0",
        "mle_rounds",
        "openings",
    ];

    /// The full fourteen-entry sub-phase template, the seven sub-steps under the segment owner then
    /// the recursion owner.
    fn openvm_subphases() -> Vec<SubPhase> {
        ["segment", "recursion"]
            .iter()
            .flat_map(|phase| {
                SUB_STEPS.iter().map(move |name| SubPhase {
                    name: name.to_string(),
                    label: name.to_string(),
                    phase: phase.to_string(),
                })
            })
            .collect()
    }

    /// The openvm pipeline template, the six base kinds alone or those plus the fourteen component
    /// kinds a new-format run appends, mirroring the sub-phase template.
    fn openvm_pipeline(components: bool) -> Vec<PipelineStep> {
        let step = |name: &str, phase: &str| PipelineStep {
            name: name.to_string(),
            label: name.to_string(),
            phase: phase.to_string(),
            paired: false,
        };
        let mut kinds: Vec<PipelineStep> = [
            ("execution", "execution"),
            ("fastfwd", "execution"),
            ("app_segment", "segment"),
            ("leaf", "recursion"),
            ("internal", "recursion"),
            ("wrap", "wrap"),
        ]
        .iter()
        .map(|(name, phase)| step(name, phase))
        .collect();
        if components {
            kinds.extend(openvm_subphases().iter().map(|s| step(&s.name, &s.phase)));
        }
        kinds
    }

    /// The parsed benchmark reshaped to the openvm templates, a new-format run carrying the
    /// component pipeline kinds and the sub-phase template together as assemble emits them from one
    /// gate, an old-format run carrying the base pipeline alone with no sub-phase template.
    fn with_openvm_templates(mut b: Benchmark, new_format: bool) -> Benchmark {
        b.software.zkvm.pipeline = openvm_pipeline(new_format);
        b.software.zkvm.subphases = if new_format {
            openvm_subphases()
        } else {
            Vec::new()
        };
        b
    }

    /// The committed fixture run directory, the only run data a unit test may lean on.
    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixture")
    }

    /// A timestamped run directory that symlinks the fixture's logs and metrics yet carries no
    /// benchmark.json, the shape a deployment without an input identity produces. The run id is the
    /// basename, so the unique scratch part is the parent and the basename stays eest-60m-{stamp}.
    fn timestamped_run(stamp: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("merge-{}-{n}", std::process::id()))
            .join(format!("eest-60m-{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = fixture_dir();
        for entry in ["logs", "zkevm-metrics"] {
            let link = dir.join(entry);
            if !link.exists() {
                symlink(fixture.join(entry), link).unwrap();
            }
        }
        dir
    }

    #[test]
    fn two_timestamped_runs_without_benchmark_json_merge() {
        // Two runs of one benchmark, distinguished only by their -YYYYMMDD-HHMMSS suffix and
        // lacking a benchmark.json, share the derived name yet keep their own ids, so the
        // second merges onto the first into one document of two runs.
        let first = parse_to_benchmark(&timestamped_run("20260602-000001")).unwrap();
        let second = parse_to_benchmark(&timestamped_run("20260602-000002")).unwrap();
        assert_eq!(first.name, "eest-60m");
        assert_eq!(second.name, "eest-60m");
        assert_ne!(first.id, second.id, "the per-run id stays distinct");

        let mut existing = first;
        let added = merge_run(&mut existing, second).expect("same-benchmark runs merge");
        assert_eq!(added, existing.runs[0].blocks.len());
        assert_eq!(existing.runs.len(), 2, "the document holds both runs");
    }

    #[test]
    fn patch_refuses_a_conflicting_pipeline_prefix() {
        // Two runs whose pipeline templates differ within the shared base prefix must not merge,
        // since the per-block kind indices of one run would point at the other's template. A pure
        // extension is the version difference the prefix reconciliation allows, so the conflict
        // must sit in the prefix itself. Both runs are reshaped to the openvm base template because
        // the fixture's own backend withholds its pipeline, leaving nothing to conflict over.
        let mut existing = with_openvm_templates(
            parse_to_benchmark(&timestamped_run("20260602-000003")).unwrap(),
            false,
        );
        let mut incoming = with_openvm_templates(
            parse_to_benchmark(&timestamped_run("20260602-000004")).unwrap(),
            false,
        );
        incoming.software.zkvm.pipeline[0].name = "different".to_string();
        let err = merge_run(&mut existing, incoming).unwrap_err();
        assert!(matches!(err, ParseError::PatchMismatch("pipeline")));
    }

    #[test]
    fn patch_adopts_a_sub_phase_template_from_the_run_that_carries_it() {
        // The seeded run's logs carried no per-item spans and the incoming run's did, so the
        // incoming run carries the component pipeline kinds and the sub-phase template together,
        // the paired fields assemble emits from one gate. The shared base prefix matches,
        // so the merge succeeds and the merged document adopts the incoming run's longer
        // templates.
        let mut existing = with_openvm_templates(
            parse_to_benchmark(&timestamped_run("20260602-000010")).unwrap(),
            false,
        );
        let incoming = with_openvm_templates(
            parse_to_benchmark(&timestamped_run("20260602-000011")).unwrap(),
            true,
        );
        assert!(existing.software.zkvm.subphases.is_empty());
        assert_eq!(existing.software.zkvm.pipeline.len(), 6);
        merge_run(&mut existing, incoming).expect("the templates reconcile");
        assert_eq!(existing.software.zkvm.pipeline.len(), 20);
        assert_eq!(existing.software.zkvm.subphases.len(), 14);
        assert_eq!(existing.runs.len(), 2);
    }

    #[test]
    fn patch_reconciles_an_old_format_run_onto_a_new_format_document() {
        // The seeded document already carries the component pipeline and the sub-phase template,
        // and the incoming unpatched-image run carries the base template alone. Their
        // shared base prefix matches, so the merge succeeds and the merged document keeps
        // its own longer templates.
        let mut existing = with_openvm_templates(
            parse_to_benchmark(&timestamped_run("20260602-000016")).unwrap(),
            true,
        );
        let incoming = with_openvm_templates(
            parse_to_benchmark(&timestamped_run("20260602-000017")).unwrap(),
            false,
        );
        merge_run(&mut existing, incoming).expect("the templates reconcile");
        assert_eq!(existing.software.zkvm.pipeline.len(), 20);
        assert_eq!(existing.software.zkvm.subphases.len(), 14);
        assert_eq!(existing.runs.len(), 2);
    }

    #[test]
    fn patch_merges_two_runs_without_a_sub_phase_template() {
        // Two spans-less runs merge as before, the merged document carrying no template.
        let mut existing = parse_to_benchmark(&timestamped_run("20260602-000012")).unwrap();
        let incoming = parse_to_benchmark(&timestamped_run("20260602-000013")).unwrap();
        merge_run(&mut existing, incoming).expect("report-less runs merge");
        assert!(existing.software.zkvm.subphases.is_empty());
        assert_eq!(existing.runs.len(), 2);
    }

    #[test]
    fn patch_refuses_two_conflicting_sub_phase_templates() {
        // Two runs whose non-empty templates differ would misalign the per-block row indices, so
        // the merge is refused with a subphases mismatch rather than a misleading software
        // one.
        let mut existing = {
            let mut b = parse_to_benchmark(&timestamped_run("20260602-000014")).unwrap();
            b.software.zkvm.subphases = segment_template("trace_gen");
            b
        };
        let incoming = {
            let mut b = parse_to_benchmark(&timestamped_run("20260602-000015")).unwrap();
            b.software.zkvm.subphases = segment_template("openings");
            b
        };
        let err = merge_run(&mut existing, incoming).unwrap_err();
        assert!(matches!(err, ParseError::PatchMismatch("subphases")));
    }
}
