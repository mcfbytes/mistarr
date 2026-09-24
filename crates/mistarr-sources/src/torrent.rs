//! Parsing `.torrent` files into a plain file list. See
//! `docs/ARCHITECTURE.md` "Source import" step 1.

use sha1::{Digest, Sha1};

use crate::bencode::Raw;
use crate::error::SourceError;

/// One file inside a torrent, as declared by the torrent itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentFile {
    /// Position in the torrent's file list, stable for the client's API.
    pub index: u32,
    /// Path segments joined with `/`, relative to the torrent's name.
    pub path: String,
    /// Declared size in bytes.
    pub size: u64,
}

/// The parsed contents of a `.torrent` file needed to bind and populate
/// `torrent_files` (`docs/DATA-MODEL.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentMeta {
    /// SHA1 of the bencoded `info` dict, the `BitTorrent` v1 infohash.
    pub infohash: [u8; 20],
    /// The torrent's `name` field.
    pub name: String,
    /// The declared files, in torrent order.
    pub files: Vec<TorrentFile>,
    /// Sum of every file's size.
    pub total_size: u64,
    /// The `info.private` flag, when the torrent is not meant to be shared
    /// outside its tracker.
    pub is_private: bool,
}

/// Parses a `.torrent` file's bytes into [`TorrentMeta`].
///
/// Supports single-file and multi-file `BitTorrent` v1 layouts, and hybrid
/// v1+v2 torrents by reading the v1 `files` list and ignoring `file tree`.
/// A v2-only torrent (no `files` or `length`) is rejected. The encoded `info` dict is
/// read in place, so memory beyond `data` is the returned file list.
///
/// # Errors
///
/// Returns [`SourceError`] when `data` is not valid bencode, has no `info`
/// dict, is missing a required field, or is a v2-only layout.
///
/// ```
/// use mistarr_sources::torrent::parse_torrent;
/// // See the crate's tests for a full synthetic torrent built with a tiny
/// // bencode encoder; a bare top-level dict without `info` is rejected here.
/// assert!(parse_torrent(b"d4:infoi1ee").is_err());
/// ```
pub fn parse_torrent(data: &[u8]) -> Result<TorrentMeta, SourceError> {
    let (top, len) = Raw::parse(data)?;
    if !matches!(top, Raw::Dict(_)) {
        return Err(SourceError::MalformedBencode(0));
    }
    if len != data.len() {
        return Err(SourceError::TrailingData);
    }
    let (info, info_bytes) = top
        .entries()
        .find(|(key, _, _)| *key == b"info")
        .map(|(_, value, bytes)| (value, bytes))
        .ok_or(SourceError::MissingInfoDict)?;
    if !matches!(info, Raw::Dict(_)) {
        return Err(SourceError::MissingInfoDict);
    }

    let name = info
        .get("name")
        .and_then(Raw::as_str)
        .ok_or(SourceError::BadField("info.name"))?
        .to_owned();
    let is_private = info.get("private").and_then(Raw::as_int) == Some(1);

    let files = match (info.get("files"), info.get("length")) {
        (Some(list @ Raw::List(_)), _) => parse_multi_file(list)?,
        (_, Some(Raw::Int(length))) => parse_single_file(&name, length)?,
        _ if info.get("file tree").is_some() => return Err(SourceError::V2Only),
        _ => return Err(SourceError::BadField("info.files/length")),
    };

    let total_size = files.iter().map(|f| f.size).sum();
    let infohash: [u8; 20] = Sha1::digest(info_bytes).into();

    Ok(TorrentMeta {
        infohash,
        name,
        files,
        total_size,
        is_private,
    })
}

fn parse_single_file(name: &str, length: i64) -> Result<Vec<TorrentFile>, SourceError> {
    let size = u64::try_from(length).map_err(|_| SourceError::BadField("info.length"))?;
    Ok(vec![TorrentFile {
        index: 0,
        path: name.to_owned(),
        size,
    }])
}

/// Reads `info.files` straight from the encoded list, one [`TorrentFile`] per entry.
fn parse_multi_file(list: Raw<'_>) -> Result<Vec<TorrentFile>, SourceError> {
    let mut files = Vec::new();
    for (i, entry) in list.items().enumerate() {
        let index = u32::try_from(i).map_err(|_| SourceError::BadField("info.files"))?;
        let length = entry
            .get("length")
            .and_then(Raw::as_int)
            .ok_or(SourceError::BadField("info.files[].length"))?;
        let size =
            u64::try_from(length).map_err(|_| SourceError::BadField("info.files[].length"))?;
        let Some(segments @ Raw::List(_)) = entry.get("path") else {
            return Err(SourceError::BadField("info.files[].path"));
        };
        let mut path = String::new();
        for (n, segment) in segments.items().enumerate() {
            if n > 0 {
                path.push('/');
            }
            path.push_str(
                segment
                    .as_str()
                    .ok_or(SourceError::BadField("info.files[].path[]"))?,
            );
        }
        files.push(TorrentFile { index, path, size });
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal hand-rolled bencode encoder for building synthetic torrents in
    // tests, independent of the crate's own encoder.
    fn benc_str(s: &str) -> Vec<u8> {
        let mut out = format!("{}:", s.len()).into_bytes();
        out.extend_from_slice(s.as_bytes());
        out
    }

    fn benc_int(v: i64) -> Vec<u8> {
        format!("i{v}e").into_bytes()
    }

    struct DictBuilder(Vec<(String, Vec<u8>)>);

    impl DictBuilder {
        fn new() -> Self {
            Self(Vec::new())
        }

        fn field(mut self, key: &str, value: Vec<u8>) -> Self {
            self.0.push((key.to_owned(), value));
            self
        }

        fn build(mut self) -> Vec<u8> {
            self.0.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            self.build_unsorted()
        }

        // Encodes fields in insertion order, for testing non-canonical input.
        fn build_unsorted(mut self) -> Vec<u8> {
            let mut out = vec![b'd'];
            for (k, v) in self.0.drain(..) {
                out.extend(benc_str(&k));
                out.extend(v);
            }
            out.push(b'e');
            out
        }
    }

    fn single_file_torrent(name: &str, size: i64) -> Vec<u8> {
        let info = DictBuilder::new()
            .field("name", benc_str(name))
            .field("length", benc_int(size))
            .field("piece length", benc_int(16384))
            .field("pieces", benc_str(""))
            .build();
        DictBuilder::new()
            .field("announce", benc_str("http://tracker.invalid/announce"))
            .field("info", info)
            .build()
    }

    fn file_entry(path_segments: &[&str], size: i64) -> Vec<u8> {
        let mut path_list = vec![b'l'];
        for seg in path_segments {
            path_list.extend(benc_str(seg));
        }
        path_list.push(b'e');
        DictBuilder::new()
            .field("length", benc_int(size))
            .field("path", path_list)
            .build()
    }

    fn multi_file_torrent(name: &str, files: &[(Vec<&str>, i64)]) -> Vec<u8> {
        let mut list = vec![b'l'];
        for (segs, size) in files {
            list.extend(file_entry(segs, *size));
        }
        list.push(b'e');
        let info = DictBuilder::new()
            .field("name", benc_str(name))
            .field("files", list)
            .field("piece length", benc_int(16384))
            .field("pieces", benc_str(""))
            .build();
        DictBuilder::new().field("info", info).build()
    }

    #[test]
    fn parses_single_file_torrent() {
        let data = single_file_torrent("game.rom", 1024);
        let meta = parse_torrent(&data).unwrap();
        assert_eq!(meta.name, "game.rom");
        assert_eq!(
            meta.files,
            vec![TorrentFile {
                index: 0,
                path: "game.rom".into(),
                size: 1024
            }]
        );
        assert_eq!(meta.total_size, 1024);
        assert!(!meta.is_private);
    }

    #[test]
    fn parses_multi_file_torrent() {
        let data = multi_file_torrent(
            "collection",
            &[(vec!["disc1.bin"], 700), (vec!["sub", "disc2.bin"], 650)],
        );
        let meta = parse_torrent(&data).unwrap();
        assert_eq!(meta.name, "collection");
        assert_eq!(meta.files.len(), 2);
        assert_eq!(meta.files[0].path, "disc1.bin");
        assert_eq!(meta.files[1].path, "sub/disc2.bin");
        assert_eq!(meta.total_size, 1350);
    }

    #[test]
    fn hybrid_torrent_reads_v1_files_and_ignores_file_tree() {
        let list = file_entry(&["game.rom"], 512);
        let info = DictBuilder::new()
            .field("name", benc_str("hybrid"))
            .field("files", {
                let mut l = vec![b'l'];
                l.extend(list);
                l.push(b'e');
                l
            })
            .field("meta version", benc_int(2))
            .field("file tree", DictBuilder::new().build())
            .build();
        let data = DictBuilder::new().field("info", info).build();
        let meta = parse_torrent(&data).unwrap();
        assert_eq!(meta.files[0].path, "game.rom");
        assert_eq!(meta.total_size, 512);
    }

    #[test]
    fn rejects_v2_only_torrent() {
        let info = DictBuilder::new()
            .field("name", benc_str("v2only"))
            .field("meta version", benc_int(2))
            .field("file tree", DictBuilder::new().build())
            .build();
        let data = DictBuilder::new().field("info", info).build();
        assert!(matches!(parse_torrent(&data), Err(SourceError::V2Only)));
    }

    #[test]
    fn rejects_missing_info() {
        assert!(matches!(
            parse_torrent(b"de"),
            Err(SourceError::MissingInfoDict)
        ));
    }

    #[test]
    fn rejects_malformed_bytes() {
        assert!(parse_torrent(b"not bencode at all").is_err());
    }

    #[test]
    fn infohash_matches_hand_computed_sha1() {
        // A fixed synthetic info dict; infohash is SHA1 over its exact bencoding.
        let info = DictBuilder::new()
            .field("name", benc_str("fixture"))
            .field("length", benc_int(4))
            .field("piece length", benc_int(16384))
            .field("pieces", benc_str(""))
            .build();
        let expected: [u8; 20] = {
            let mut hasher = Sha1::new();
            hasher.update(&info);
            hasher.finalize().into()
        };
        let data = DictBuilder::new().field("info", info).build();
        let meta = parse_torrent(&data).unwrap();
        assert_eq!(meta.infohash, expected);
    }

    #[test]
    fn infohash_hashes_raw_span_with_unsorted_keys() {
        // Canonical bencode sorts dict keys; this info dict deliberately does
        // not, matching a non-canonical encoder in the wild.
        let info = DictBuilder::new()
            .field("name", benc_str("unsorted"))
            .field("length", benc_int(4))
            .field("piece length", benc_int(16384))
            .field("pieces", benc_str(""))
            .build_unsorted();
        let expected: [u8; 20] = {
            let mut hasher = Sha1::new();
            hasher.update(&info);
            hasher.finalize().into()
        };
        let data = DictBuilder::new().field("info", info).build();
        let meta = parse_torrent(&data).unwrap();
        assert_eq!(meta.infohash, expected);
    }

    proptest::proptest! {
        #[test]
        fn never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let _ = parse_torrent(&bytes);
        }
    }
}
