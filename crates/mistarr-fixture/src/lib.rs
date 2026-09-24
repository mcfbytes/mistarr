//! Synthetic test inputs: DATs, `.torrent` files, the synthetic set and a
//! local tracker. A development tool, never shipped; see `docs/TESTING.md`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]

use std::path::{Path, PathBuf};

pub mod dat;
pub mod rng;
pub mod set;
pub mod torrent;
pub mod tracker;

/// Failures of the fixture builders.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Reading or writing a file failed.
    #[error("{path}: {source}")]
    Io {
        /// The file or directory involved.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A zip could not be read or written.
    #[error("{path}: {source}")]
    Zip {
        /// The archive.
        path: PathBuf,
        /// The underlying error.
        source: zip::result::ZipError,
    },
    /// The tracker could not listen.
    #[error("listen on {addr}: {source}")]
    Listen {
        /// The address asked for.
        addr: String,
        /// The underlying error.
        source: std::io::Error,
    },
    /// No row of the platform table has this id.
    #[error("unknown platform {0:?}")]
    UnknownPlatform(String),
    /// The directory holds no files to describe.
    #[error("{0}: no files")]
    Empty(PathBuf),
    /// A path is not valid UTF-8 or has no usable name.
    #[error("{0}: unusable file name")]
    BadName(PathBuf),
}

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Wraps an I/O error with the path it concerns.
pub(crate) fn io_at(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Every regular file under `dir`, as paths relative to it, sorted by their
/// `/`-joined form so output is the same on every filesystem.
///
/// # Errors
///
/// [`Error::Io`] when a directory cannot be listed, [`Error::BadName`] for a
/// name that is not UTF-8.
pub fn walk(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let abs = dir.join(&rel);
        for entry in std::fs::read_dir(&abs).map_err(io_at(&abs))? {
            let entry = entry.map_err(io_at(&abs))?;
            let name = entry.file_name();
            if name.to_str().is_none() {
                return Err(Error::BadName(entry.path()));
            }
            let kind = entry.file_type().map_err(io_at(&entry.path()))?;
            if kind.is_dir() {
                stack.push(rel.join(name));
            } else if kind.is_file() {
                out.push(rel.join(name));
            }
        }
    }
    out.sort_by_key(|p| slash_path(p));
    Ok(out)
}

/// A relative path with `/` separators.
#[must_use]
pub fn slash_path(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_lists_nested_files_sorted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("b/c")).unwrap();
        std::fs::write(dir.path().join("b/c/z.bin"), b"z").unwrap();
        std::fs::write(dir.path().join("a.bin"), b"a").unwrap();
        std::fs::write(dir.path().join("b/y.bin"), b"y").unwrap();
        let got: Vec<String> = walk(dir.path())
            .unwrap()
            .iter()
            .map(|p| slash_path(p))
            .collect();
        assert_eq!(got, ["a.bin", "b/c/z.bin", "b/y.bin"]);
    }

    #[test]
    fn slash_path_joins_components() {
        assert_eq!(slash_path(Path::new("a").join("b").as_path()), "a/b");
    }

    #[test]
    fn io_errors_name_the_path() {
        let err = walk(Path::new("/nonexistent/fixture/dir")).unwrap_err();
        assert!(err.to_string().starts_with("/nonexistent/fixture/dir"));
    }
}
