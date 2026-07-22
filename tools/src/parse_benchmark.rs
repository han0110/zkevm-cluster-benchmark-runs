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
fn merge_run(existing: &mut Benchmark, parsed: Benchmark) -> Result<usize> {
    if existing.hardware != parsed.hardware {
        return Err(ParseError::PatchMismatch("hardware"));
    }
    if existing.software != parsed.software {
        return Err(ParseError::PatchMismatch("software"));
    }
    if existing.name != parsed.name {
        return Err(ParseError::PatchMismatch("name"));
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
        ParseError, merge_run, output::schema::PipelineStep, parse_to_benchmark,
    };

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
    fn patch_refuses_a_pipeline_template_mismatch() {
        // Two runs of one benchmark whose zkVM pipeline templates differ must not merge, since the
        // per-block kind indices of one run would point at the other's template.
        let mut existing = parse_to_benchmark(&timestamped_run("20260602-000003")).unwrap();
        let mut incoming = parse_to_benchmark(&timestamped_run("20260602-000004")).unwrap();
        incoming.software.zkvm.pipeline.push(PipelineStep {
            name: "extra".to_string(),
            label: "Extra".to_string(),
            phase: "prove".to_string(),
            paired: false,
        });
        let err = merge_run(&mut existing, incoming).unwrap_err();
        assert!(matches!(err, ParseError::PatchMismatch("software")));
    }
}
