//! Minimal bencode walk to count the files of a `.torrent` before adding it.

const MAX_DEPTH: usize = 64;

/// Number of entries in the v1 file list of `metainfo`: the length of
/// `info.files`, or 1 for a single-file torrent. `None` if it cannot tell.
pub(crate) fn file_count(metainfo: &[u8]) -> Option<usize> {
    let info = dict_get(metainfo, 0, b"info")?;
    if let Some(files) = dict_get(metainfo, info, b"files") {
        return list_len(metainfo, files);
    }
    dict_get(metainfo, info, b"length").map(|_| 1)
}

/// Start offset of the value stored under `key` in the dict at `pos`.
fn dict_get(buf: &[u8], pos: usize, key: &[u8]) -> Option<usize> {
    if *buf.get(pos)? != b'd' {
        return None;
    }
    let mut at = pos + 1;
    while *buf.get(at)? != b'e' {
        let (k, value_at) = byte_string(buf, at)?;
        if k == key {
            return Some(value_at);
        }
        at = skip(buf, value_at, 0)?;
    }
    None
}

fn list_len(buf: &[u8], pos: usize) -> Option<usize> {
    if *buf.get(pos)? != b'l' {
        return None;
    }
    let mut at = pos + 1;
    let mut n = 0;
    while *buf.get(at)? != b'e' {
        at = skip(buf, at, 0)?;
        n += 1;
    }
    Some(n)
}

/// Parses `<len>:<bytes>` at `pos`, returning the bytes and the next offset.
fn byte_string(buf: &[u8], pos: usize) -> Option<(&[u8], usize)> {
    let colon = pos + buf.get(pos..)?.iter().position(|&b| b == b':')?;
    let digits = buf.get(pos..colon)?;
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let len: usize = std::str::from_utf8(digits).ok()?.parse().ok()?;
    let end = colon.checked_add(1)?.checked_add(len)?;
    Some((buf.get(colon + 1..end)?, end))
}

/// Offset just past the value starting at `pos`.
fn skip(buf: &[u8], pos: usize, depth: usize) -> Option<usize> {
    if depth > MAX_DEPTH {
        return None;
    }
    match *buf.get(pos)? {
        b'i' => Some(pos + buf.get(pos..)?.iter().position(|&b| b == b'e')? + 1),
        b'l' | b'd' => {
            let is_dict = buf[pos] == b'd';
            let mut at = pos + 1;
            while *buf.get(at)? != b'e' {
                if is_dict {
                    at = byte_string(buf, at)?.1;
                }
                at = skip(buf, at, depth + 1)?;
            }
            Some(at + 1)
        }
        b'0'..=b'9' => byte_string(buf, pos).map(|(_, end)| end),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fmt::Write as _;

    use super::file_count;

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

    #[test]
    fn counts_multi_and_single_file() {
        assert_eq!(file_count(&synthetic_metainfo(3)), Some(3));
        assert_eq!(file_count(&synthetic_metainfo(0)), Some(1));
        assert_eq!(file_count(&synthetic_metainfo(2001)), Some(2001));
    }

    #[test]
    fn rejects_malformed_input() {
        assert_eq!(file_count(b""), None);
        assert_eq!(file_count(b"d4:infod5:filesl"), None);
        assert_eq!(file_count(b"d4:info99:x"), None);
        assert_eq!(file_count(b"le"), None);
        assert_eq!(file_count(b"d4:infodee"), None);
        let deep = format!(
            "d4:infod1:x{}{}e5:filesleee",
            "l".repeat(100),
            "e".repeat(100)
        );
        assert_eq!(file_count(deep.as_bytes()), None);
    }
}
