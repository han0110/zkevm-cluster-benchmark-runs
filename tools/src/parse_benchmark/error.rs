//! Crate error type and Result alias.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Fatal errors the parser can return.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// A filesystem operation failed for the given path.
    #[error("io error at {path}: {source}")]
    Io { path: PathBuf, source: io::Error },

    /// A JSON document failed to deserialize.
    #[error("json error in {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },

    /// A timestamp string could not be parsed.
    #[error("failed to parse timestamp {value:?}: {reason}")]
    Timestamp { value: String, reason: String },

    /// The zkevm-metrics directory was absent.
    #[error("metrics directory not found: {0}")]
    MissingMetricsDir(PathBuf),

    /// No registered zkVM backend recognized the run logs.
    #[error("could not detect a known zkVM from logs in {0}")]
    UnknownZkvm(PathBuf),

    /// A coordinator INFO event line carried a data tag or new tag but matched no known pattern.
    #[error(
        "unrecognized coordinator line, the parser may need an update for a new zkVM version: {0}"
    )]
    UnrecognizedCoordinatorLine(String),

    /// A worker log line carried a known work-item anchor phrase but matched no known pattern.
    #[error("unrecognized worker line, the parser may need an update for a new zkVM version: {0}")]
    UnrecognizedWorkerLine(String),

    /// A dmon log was not in the supported columnar format.
    #[error("dmon file {0} is not in the supported columnar (#-header) format")]
    UnsupportedDmonFormat(PathBuf),

    /// The output path already holds a benchmark and neither overwrite nor patch was requested.
    #[error("output {0} already exists; pass --force to overwrite or --patch to add a run")]
    OutputExists(PathBuf),

    /// A patch was requested but the output path holds no benchmark to add the run to.
    #[error("cannot patch {0}: no existing benchmark.json at the output path")]
    PatchTargetMissing(PathBuf),

    /// A patch was requested but the run's identity field differs from the existing document, so it
    /// belongs to a different benchmark or cluster.
    #[error("cannot patch: the run's {0} differs from the existing benchmark document")]
    PatchMismatch(&'static str),

    /// Two blocks in a run shared a name. Block names are the metric file names that views index
    /// on as a unique id.
    #[error("duplicate block name {0:?}; block names must be unique within a run")]
    DuplicateBlockName(String),
}

/// Crate result alias over [`ParseError`].
pub type Result<T> = std::result::Result<T, ParseError>;

/// Tags an io error with the path it occurred at.
pub(crate) fn io_at(path: impl Into<PathBuf>) -> impl FnOnce(io::Error) -> ParseError {
    let path = path.into();
    move |source| ParseError::Io { path, source }
}

/// Tags a JSON error with the document path.
pub(crate) fn json_at(path: impl Into<PathBuf>) -> impl FnOnce(serde_json::Error) -> ParseError {
    let path = path.into();
    move |source| ParseError::Json { path, source }
}

/// Reads a file to a string, tagging io errors with its path.
pub(crate) fn read_to_string_at(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(io_at(path))
}

/// Reads a directory, tagging io errors with its path.
pub(crate) fn read_dir_at(path: &Path) -> Result<fs::ReadDir> {
    fs::read_dir(path).map_err(io_at(path))
}
