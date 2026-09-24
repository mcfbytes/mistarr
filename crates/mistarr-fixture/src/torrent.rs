//! v1 multi-file `.torrent` files built from a directory, deterministic for a
//! given tree so the infohash is stable across runs.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use mistarr_sources::bencode::{encode, Value};
use sha1::{Digest, Sha1};

use crate::{io_at, walk, Error, Result};

/// Piece length of every torrent this module builds.
pub const PIECE_LENGTH: usize = 256 * 1024;

fn bytes(s: &str) -> Value {
    Value::Bytes(s.as_bytes().to_vec())
}

fn dict(entries: Vec<(&str, Value)>) -> Value {
    Value::Dict(
        entries
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect::<BTreeMap<_, _>>(),
    )
}

/// Builds a multi-file torrent of every file under `dir`, named after the
/// directory, announcing to `tracker` and listing `web_seed` as a `url-list`.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(dir.path().join("a.bin"), b"abc").unwrap();
/// let t = mistarr_fixture::torrent::build(dir.path(), "http://tracker.invalid/announce", None).unwrap();
/// let meta = mistarr_sources::torrent::parse_torrent(&t).unwrap();
/// assert_eq!(meta.files[0].size, 3);
/// ```
///
/// # Errors
///
/// [`Error::Io`] when a file cannot be read, [`Error::Empty`] when there are
/// none, [`Error::BadName`] when the directory has no UTF-8 name.
pub fn build(dir: &Path, tracker: &str, web_seed: Option<&str>) -> Result<Vec<u8>> {
    let name = dir
        .canonicalize()
        .map_err(io_at(dir))?
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .ok_or_else(|| Error::BadName(dir.to_path_buf()))?;
    let rels = walk(dir)?;
    if rels.is_empty() {
        return Err(Error::Empty(dir.to_path_buf()));
    }
    let mut files = Vec::with_capacity(rels.len());
    let mut pieces = Vec::new();
    let mut piece = Vec::with_capacity(PIECE_LENGTH);
    let mut buf = vec![0u8; PIECE_LENGTH];
    for rel in &rels {
        let abs = dir.join(rel);
        let mut f = File::open(&abs).map_err(io_at(&abs))?;
        let mut length: i64 = 0;
        loop {
            let want = PIECE_LENGTH - piece.len();
            let n = f.read(&mut buf[..want]).map_err(io_at(&abs))?;
            if n == 0 {
                break;
            }
            length += i64::try_from(n).unwrap_or(i64::MAX);
            piece.extend_from_slice(&buf[..n]);
            if piece.len() == PIECE_LENGTH {
                pieces.extend_from_slice(&Sha1::digest(&piece));
                piece.clear();
            }
        }
        let path = rel
            .components()
            .map(|c| bytes(&c.as_os_str().to_string_lossy()))
            .collect();
        files.push(dict(vec![
            ("length", Value::Int(length)),
            ("path", Value::List(path)),
        ]));
    }
    if !piece.is_empty() {
        pieces.extend_from_slice(&Sha1::digest(&piece));
    }
    let info = dict(vec![
        ("files", Value::List(files)),
        ("name", bytes(&name)),
        (
            "piece length",
            Value::Int(i64::try_from(PIECE_LENGTH).unwrap_or(i64::MAX)),
        ),
        ("pieces", Value::Bytes(pieces)),
    ]);
    let mut top = vec![
        ("announce", bytes(tracker)),
        ("created by", bytes("mistarr-fixture")),
        ("info", info),
    ];
    if let Some(seed) = web_seed {
        top.push(("url-list", Value::List(vec![bytes(seed)])));
    }
    Ok(encode(&dict(top)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mistarr_sources::bencode::decode;
    use mistarr_sources::torrent::parse_torrent;

    const TRACKER: &str = "http://tracker.invalid/announce";

    fn tree() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("Synthetic Set");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.bin"), vec![1u8; PIECE_LENGTH + 10]).unwrap();
        std::fs::write(dir.join("sub/b.bin"), vec![2u8; 100]).unwrap();
        root
    }

    #[test]
    fn files_pieces_and_name_are_as_on_disk() {
        let root = tree();
        let t = build(&root.path().join("Synthetic Set"), TRACKER, None).unwrap();
        let meta = parse_torrent(&t).unwrap();
        assert_eq!(meta.name, "Synthetic Set");
        let files: Vec<_> = meta
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.size))
            .collect();
        assert_eq!(files, [("a.bin", 262_154), ("sub/b.bin", 100)]);
        let v = decode(&t).unwrap();
        let info = v.get("info").unwrap();
        assert_eq!(info.get("pieces").unwrap().as_bytes().unwrap().len(), 40);
        assert_eq!(info.get("piece length").unwrap().as_int(), Some(262_144));
        assert_eq!(v.get("announce").unwrap().as_str(), Some(TRACKER));
        assert!(v.get("url-list").is_none());
    }

    #[test]
    fn second_piece_spans_the_file_boundary() {
        let root = tree();
        let t = build(&root.path().join("Synthetic Set"), TRACKER, None).unwrap();
        let v = decode(&t).unwrap();
        let pieces = v
            .get("info")
            .unwrap()
            .get("pieces")
            .unwrap()
            .as_bytes()
            .unwrap();
        let mut tail = vec![1u8; 10];
        tail.extend(vec![2u8; 100]);
        assert_eq!(&pieces[20..], &Sha1::digest(&tail)[..]);
    }

    #[test]
    fn web_seed_is_a_url_list() {
        let root = tree();
        let seed = "http://example.invalid/files/";
        let t = build(&root.path().join("Synthetic Set"), TRACKER, Some(seed)).unwrap();
        let v = decode(&t).unwrap();
        let list = v.get("url-list").unwrap().as_list().unwrap();
        assert_eq!(list[0].as_str(), Some(seed));
    }

    #[test]
    fn output_is_deterministic() {
        let root = tree();
        let dir = root.path().join("Synthetic Set");
        assert_eq!(
            build(&dir, TRACKER, None).unwrap(),
            build(&dir, TRACKER, None).unwrap()
        );
    }

    #[test]
    fn empty_directory_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        assert!(matches!(
            build(root.path(), TRACKER, None),
            Err(Error::Empty(_))
        ));
    }
}
