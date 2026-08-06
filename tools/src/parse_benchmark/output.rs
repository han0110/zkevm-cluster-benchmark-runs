//! The output stage. The schema holds the benchmark.json structs, assemble builds the document, and
//! this root serializes it to compact JSON on disk, framed with zstd when the path carries the
//! .zst suffix.

pub mod assemble;
pub mod schema;

use std::{
    fs::File,
    path::{Path, PathBuf},
};

use flate2::{Compression, write::GzEncoder};
pub use schema::Benchmark;
use tar::{Builder, Header};

use crate::parse_benchmark::{io_at, json_at, read_to_string_at};

/// Serializes the benchmark to compact JSON, keeping struct field order.
pub fn to_json(benchmark: &Benchmark) -> crate::parse_benchmark::Result<String> {
    serde_json::to_string(benchmark).map_err(json_at(PathBuf::from("benchmark.json")))
}

/// Whether the path names a zstd-framed document, the suffix the --zstd flag appends. The framing
/// is a property of the path alone, so a patch reads back whatever the same path was written as.
fn compressed(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "zst")
}

/// Reads the benchmark at the path, the existing document a patch appends a run to.
pub fn read(path: &Path) -> crate::parse_benchmark::Result<Benchmark> {
    if compressed(path) {
        let file = File::open(path).map_err(io_at(path))?;
        let bytes = zstd::decode_all(file).map_err(io_at(path))?;
        return serde_json::from_slice(&bytes).map_err(json_at(path));
    }
    let text = read_to_string_at(path)?;
    serde_json::from_str(&text).map_err(json_at(path))
}

/// Writes the benchmark to the output path, creating parent directories as needed. A compressed
/// path is written as one zstd frame declaring the uncompressed size, which lets the browser reader
/// allocate the whole document up front instead of growing it block by block.
pub fn write(benchmark: &Benchmark, output: &Path) -> crate::parse_benchmark::Result<()> {
    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(io_at(parent))?;
    }
    let text = to_json(benchmark)?;
    let bytes = if compressed(output) {
        zstd::bulk::compress(text.as_bytes(), zstd::DEFAULT_COMPRESSION_LEVEL)
            .map_err(io_at(output))?
    } else {
        text.into_bytes()
    };
    std::fs::write(output, bytes).map_err(io_at(output))
}

/// Writes each block's logs to a per-block tar.json under log/{bench_id}/{run_id}/ beside
/// benchmark.json, so the lean document stays small and a block's log loads only when its trace is
/// opened. The file is a gzipped tar carrying a .tar.json suffix rather than .tar.gz so a static
/// host serves it without a gzip Content-Encoding the browser would transparently inflate, leaving
/// the in-browser reader to decompress the bytes itself. The benchmark id namespaces the tree. A
/// block with no logs writes no file, and the frontend's build-time index skips fetching the absent
/// ones. The directory is gitignored and uploaded to Cloudflare R2 for production.
pub fn write_block_logs(
    run: &schema::Run,
    base_dir: &Path,
    bench_id: &str,
) -> crate::parse_benchmark::Result<()> {
    let dir = base_dir.join("log").join(bench_id).join(&run.id);
    let mut created = false;
    for block in &run.blocks {
        if block.logs.is_empty() {
            continue;
        }
        if !created {
            std::fs::create_dir_all(&dir).map_err(io_at(&dir))?;
            created = true;
        }
        let path = dir.join(format!("{}.tar.json", archive_stem(&block.name)));
        let json = serde_json::to_string(&block.logs).map_err(json_at(&path))?;
        write_log_archive(&path, &json)?;
    }
    Ok(())
}

/// Maps a block name to its flat archive file stem, replacing the `::` of an EEST test id with a
/// double underscore and the `/` of its fixture path with a single underscore so every block's
/// archive sits directly under log/{bench_id}/{run_id}/ as a single file rather than nesting under
/// the fixture path's directories. The colon is replaced rather than left in place because a colon
/// in a served path is not matched by the dev server or a static origin, which fall through to the
/// SPA HTML fallback. The slash is replaced because the frontend looks the archive up under one
/// percent-encoded path segment, which a nested tree never matches; brackets and spaces serve fine
/// percent-encoded. This mapping must stay in sync with blockArchivePath in
/// frontend/src/utils/archivePath.ts, which applies the same replacements so both address the same
/// file.
fn archive_stem(name: &str) -> String {
    name.replace("::", "__").replace('/', "_")
}

/// Writes the block's log JSON as a single-member gzipped tar at the path. The member is named
/// log.json, a short fixed name independent of the block name, so the frontend reads the one member
/// by position and a long block name never lands in a tar header.
fn write_log_archive(path: &Path, json: &str) -> crate::parse_benchmark::Result<()> {
    let file = File::create(path).map_err(io_at(path))?;
    let mut tar = Builder::new(GzEncoder::new(file, Compression::default()));
    let mut header = Header::new_ustar();
    header.set_size(json.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, "log.json", json.as_bytes())
        .map_err(io_at(path))?;
    tar.into_inner()
        .map_err(io_at(path))?
        .finish()
        .map_err(io_at(path))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::parse_benchmark::output::archive_stem;

    #[test]
    fn archive_stem_flattens_an_eest_id() {
        // An EEST test id carries a `::` between its file and test, which the stem replaces with a
        // double underscore so the archive is one flat file under the run directory, leaving the
        // bracketed parameters intact and never introducing a path separator.
        let name = "test_account_query.py::test_codecopy_benchmark[fork_Osaka-blockchain_test-code_size_0-mem_size_0-benchmark-gas-value_60M]";
        let stem = archive_stem(name);
        assert_eq!(
            stem,
            "test_account_query.py__test_codecopy_benchmark[fork_Osaka-blockchain_test-code_size_0-mem_size_0-benchmark-gas-value_60M]"
        );
        assert!(!stem.contains("::"), "the colon pair must not survive");
        assert!(
            !stem.contains('/'),
            "the stem stays a single flat file name"
        );
        assert!(stem.contains('['), "the bracketed parameters are preserved");
        assert!(stem.contains(']'), "the bracketed parameters are preserved");
    }

    #[test]
    fn archive_stem_flattens_a_path_carrying_eest_id() {
        // An EEST v0.6.2 test id carries the fixture path before the file name, and the stem
        // replaces the path's separators with single underscores so the archive is one flat file
        // under the run directory.
        let name = "tests/benchmark/compute/instruction/test_stack.py::test_swap[fork_Amsterdam-blockchain_test-opcode_SWAP15-benchmark-gas-value_60M]";
        let stem = archive_stem(name);
        assert_eq!(
            stem,
            "tests_benchmark_compute_instruction_test_stack.py__test_swap[fork_Amsterdam-blockchain_test-opcode_SWAP15-benchmark-gas-value_60M]"
        );
        assert!(
            !stem.contains('/'),
            "the stem stays a single flat file name"
        );
    }
}
