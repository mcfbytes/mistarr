//! Reasons an incoming file is refused before it is parsed; `docs/DATS.md` "Rejections".

use std::fs::File;
use std::io::Read as _;
use std::path::Path;

/// Bytes of a file [`obvious_reason`] looks at.
pub const HEAD_BYTES: usize = 512;

/// Reason an empty file is refused with.
pub const EMPTY_REASON: &str = "The file is empty.";

/// Reason a web page saved under a data extension is refused with.
pub const WEB_PAGE_REASON: &str =
    "This is a web page, not a DAT or torrent file. The download probably failed; download it again.";

/// Reason a zip whose container cannot be read is refused with.
pub const DAMAGED_ZIP_REASON: &str = "This zip file is damaged or incomplete. Download it again.";

/// Why `head`, the first [`HEAD_BYTES`] bytes of a `len`-byte file, is not a DAT or
/// torrent before any parsing; `None` when it may be one.
///
/// ```
/// use mistarr_server::incoming::reject::{obvious_reason, EMPTY_REASON, WEB_PAGE_REASON};
/// assert_eq!(obvious_reason(b"", 0), Some(EMPTY_REASON));
/// assert_eq!(obvious_reason(b"<!DOCTYPE html>", 15), Some(WEB_PAGE_REASON));
/// assert_eq!(obvious_reason(b"d8:announce", 11), None);
/// ```
#[must_use]
pub fn obvious_reason(head: &[u8], len: u64) -> Option<&'static str> {
    if len == 0 {
        return Some(EMPTY_REASON);
    }
    let text = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    let start = text
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(text.len());
    let lower = text[start..].to_ascii_lowercase();
    let html = lower.starts_with(b"<!doctype html")
        || lower.starts_with(b"<html")
        || (lower.starts_with(b"<?xml") && lower.windows(5).any(|w| w == b"<html".as_slice()));
    html.then_some(WEB_PAGE_REASON)
}

/// [`obvious_reason`] for the file at `path`, read in one open; `None` when it cannot be
/// read, so a missing file is never reported as empty.
#[must_use]
pub fn obvious_reason_at(path: &Path) -> Option<&'static str> {
    let file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut head = Vec::with_capacity(HEAD_BYTES);
    file.take(HEAD_BYTES as u64).read_to_end(&mut head).ok()?;
    obvious_reason(&head, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reason for `head` treated as a whole file.
    fn reason(head: &[u8]) -> Option<&'static str> {
        obvious_reason(head, head.len() as u64)
    }

    #[test]
    fn an_empty_file_is_refused_as_empty() {
        assert_eq!(obvious_reason(b"", 0), Some(EMPTY_REASON));
    }

    #[test]
    fn an_html_document_is_refused_as_a_web_page() {
        assert_eq!(reason(b"<!DOCTYPE html>\n<html>"), Some(WEB_PAGE_REASON));
        assert_eq!(reason(b"  \n\t<HTML lang=\"en\">"), Some(WEB_PAGE_REASON));
        assert_eq!(
            reason(b"\xEF\xBB\xBF \r\n<html>"),
            Some(WEB_PAGE_REASON),
            "a byte order mark and whitespace are skipped"
        );
        assert_eq!(
            reason(b"<?xml version=\"1.0\"?><!DOCTYPE html><html><body>"),
            Some(WEB_PAGE_REASON)
        );
    }

    #[test]
    fn heads_that_may_be_a_dat_or_torrent_are_left_to_the_parsers() {
        assert_eq!(reason(b"<?xml version=\"1.0\"?><!DOCTYPE datafile>"), None);
        assert_eq!(reason(b"d8:announce"), None);
        assert_eq!(reason(b"PK\x03\x04"), None);
    }

    #[test]
    fn a_file_is_read_once_for_its_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        let empty = dir.path().join("a.dat");
        std::fs::write(&empty, b"").expect("write");
        assert_eq!(obvious_reason_at(&empty), Some(EMPTY_REASON));
        let page = dir.path().join("b.torrent");
        std::fs::write(&page, b"<!doctype html><html>").expect("write");
        assert_eq!(obvious_reason_at(&page), Some(WEB_PAGE_REASON));
        assert_eq!(obvious_reason_at(&dir.path().join("missing")), None);
    }
}
