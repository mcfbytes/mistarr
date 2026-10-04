//! Synthetic torrents for this crate's tests.

use std::fmt::Write as _;

use mistarr_core::InfoHash;

use crate::TorrentSource;

/// Synthetic metainfo with `files` entries (0 means single-file).
pub(crate) fn synthetic_metainfo(files: usize) -> Vec<u8> {
    let mut info = String::from("d");
    if files == 0 {
        info.push_str("6:lengthi16e");
    } else {
        info.push_str("5:filesl");
        for i in 0..files {
            let name = format!("f{i}.bin");
            write!(info, "d6:lengthi16e4:pathl{}:{name}ee", name.len()).expect("write");
        }
        info.push('e');
    }
    info.push_str("4:name4:test12:piece lengthi16384e6:pieces0:e");
    format!("d8:announce31:http://tracker.invalid/announce4:info{info}e").into_bytes()
}

/// [`synthetic_metainfo`] as a source whose infohash is `byte` repeated.
pub(crate) fn metainfo(files: usize, byte: u8) -> TorrentSource {
    TorrentSource::Metainfo {
        bytes: synthetic_metainfo(files),
        infohash: InfoHash::from_bytes([byte; 20]),
        file_count: files.max(1),
    }
}

/// A magnet whose infohash is `byte` repeated.
pub(crate) fn magnet(byte: u8) -> TorrentSource {
    let infohash = InfoHash::from_bytes([byte; 20]);
    TorrentSource::Magnet {
        uri: format!("magnet:?xt=urn:btih:{infohash}"),
        infohash,
    }
}

#[test]
fn sources_carry_their_counts_and_hashes() {
    let TorrentSource::Metainfo { file_count, .. } = metainfo(0, 1) else {
        panic!("not metainfo");
    };
    assert_eq!(file_count, 1);
    assert_eq!(magnet(2).infohash(), InfoHash::from_bytes([2; 20]));
}
