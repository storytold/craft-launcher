use std::io;
use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("invalid manifest {}: {source}", path.display())]
    Manifest { path: PathBuf, source: serde_json::Error },
    #[error("manifest schema version {found} is not supported: this build reads version {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("unsupported font format: {0}")]
    UnsupportedFormat(String),
    #[error("invalid font: {0}")]
    InvalidFont(String),
    #[error("no font matches id '{0}'")]
    NotFound(String),
    #[error("id '{0}' is ambiguous: it matches {1} fonts")]
    Ambiguous(String, usize),
    #[error("invalid app id '{0}': expected lowercase reverse-DNS such as ai.storytold.photocraft")]
    InvalidAppId(String),
    #[error("invalid timestamp '{0}': expected RFC 3339 such as 2026-10-22T02:30:00Z")]
    InvalidTimestamp(String),
    #[error("font {id} has no face with index {index}")]
    NoSuchFace { id: String, index: u32 },
    #[error("no vault at {} (run `fontcore init`)", .0.display())]
    NotInitialized(PathBuf),
    #[error("timed out waiting for vault lock {}", .0.display())]
    Locked(PathBuf),
    #[error("could not determine a default vault location; set FONTCORE_HOME")]
    NoHome,
}

pub(crate) trait IoContext<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for io::Result<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|source| Error::Io { path: path.into(), source })
    }
}
