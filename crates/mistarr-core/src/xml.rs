//! Reading XML that may hold invalid UTF-8; the contract is `docs/VERIFICATION.md` "Text encoding".

use std::borrow::Cow;
use std::io::{self, BufRead, Read};
use std::str::Utf8Error;

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

/// First code point of the escapes: byte `b` becomes `ESCAPE_BASE + b`, all in plane 16.
const ESCAPE_BASE: u32 = 0x10_FF00;
/// UTF-8 length of one escape.
const ESCAPE_LEN: usize = 4;

/// A [`BufRead`] that passes valid UTF-8 through unchanged and replaces each byte that
/// is not part of a valid UTF-8 sequence with an escape, the code point U+10FF00 plus
/// the byte, so an XML reader never fails on encoding. Callers check the values they
/// use with [`check_utf8`]; bytes in markup they skip pass unexamined.
///
/// ```
/// use std::io::Read as _;
/// let mut s = String::new();
/// mistarr_core::xml::EscapeInvalid::new(&b"ok \xe9"[..]).read_to_string(&mut s)?;
/// assert!(s.starts_with("ok "));
/// assert!(mistarr_core::xml::check_utf8(&s).is_err());
/// # Ok::<(), std::io::Error>(())
/// ```
pub struct EscapeInvalid<R> {
    inner: R,
    /// Valid bytes at the front of the inner buffer being passed through.
    pass: usize,
    /// Output of one run decided byte by byte, and how much of it was read.
    out: Vec<u8>,
    at: usize,
    /// Whether `out` holds escapes rather than a copy.
    escaped: bool,
    /// Bytes taken from `inner` and not yet decided.
    carry: Vec<u8>,
    /// Inner bytes behind everything read, excluding the current `out`.
    position: u64,
}

impl<R: BufRead> EscapeInvalid<R> {
    /// Wraps `inner`.
    ///
    /// ```
    /// let r = mistarr_core::xml::EscapeInvalid::new(&b"<a/>"[..]);
    /// assert_eq!(r.position(), 0);
    /// ```
    #[must_use]
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            pass: 0,
            out: Vec::new(),
            at: 0,
            escaped: false,
            carry: Vec::new(),
            position: 0,
        }
    }

    /// Bytes of `inner` behind everything consumed so far, which is fewer than the
    /// bytes consumed once an escape was read.
    ///
    /// ```
    /// use std::io::BufRead as _;
    /// let mut r = mistarr_core::xml::EscapeInvalid::new(&b"\xe9<a/>"[..]);
    /// let n = r.fill_buf()?.len();
    /// r.consume(n);
    /// assert_eq!((n, r.position()), (4, 1));
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[must_use]
    pub fn position(&self) -> u64 {
        let at = if self.escaped {
            self.at / ESCAPE_LEN
        } else {
            self.at
        };
        self.position + at as u64
    }

    /// Sets up the next run: a pass-through length, or `out`; neither at the end of input.
    fn refill(&mut self) -> io::Result<()> {
        self.position = self.position();
        self.out.clear();
        self.at = 0;
        loop {
            if !self.carry.is_empty() {
                match split(&self.carry) {
                    (0, Some(bad)) => self.set_out(bad, true),
                    (0, None) => {
                        let Some(&b) = self.inner.fill_buf()?.first() else {
                            let n = self.carry.len();
                            self.set_out(n, true);
                            return Ok(());
                        };
                        self.carry.push(b);
                        self.inner.consume(1);
                        continue;
                    }
                    (valid, _) => self.set_out(valid, false),
                }
                return Ok(());
            }
            let buf = self.inner.fill_buf()?;
            match split(buf) {
                (0, None) if !buf.is_empty() => {
                    self.carry.extend_from_slice(buf);
                    let n = buf.len();
                    self.inner.consume(n);
                }
                (0, Some(bad)) => {
                    self.carry.extend_from_slice(&buf[..bad]);
                    self.inner.consume(bad);
                    self.set_out(bad, true);
                    return Ok(());
                }
                (valid, _) => {
                    self.pass = valid;
                    return Ok(());
                }
            }
        }
    }

    /// Moves the first `n` carried bytes to `out`, escaped or copied.
    fn set_out(&mut self, n: usize, escape: bool) {
        self.escaped = escape;
        for &b in &self.carry[..n] {
            if escape {
                let c = char::from_u32(ESCAPE_BASE + u32::from(b))
                    .unwrap_or(char::REPLACEMENT_CHARACTER);
                self.out
                    .extend_from_slice(c.encode_utf8(&mut [0; ESCAPE_LEN]).as_bytes());
            } else {
                self.out.push(b);
            }
        }
        self.carry.drain(..n);
    }
}

/// The length of the valid UTF-8 prefix of `buf` and, when that is empty, the length of
/// the invalid sequence starting `buf`; `(0, None)` for a truncated sequence or no input.
fn split(buf: &[u8]) -> (usize, Option<usize>) {
    match std::str::from_utf8(buf) {
        Ok(_) => (buf.len(), None),
        Err(e) => (e.valid_up_to(), e.error_len()),
    }
}

impl<R: BufRead> BufRead for EscapeInvalid<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.pass == 0 && self.at == self.out.len() {
            self.refill()?;
        }
        if self.pass == 0 {
            return Ok(&self.out[self.at..]);
        }
        let buf = self.inner.fill_buf()?;
        Ok(&buf[..self.pass.min(buf.len())])
    }

    fn consume(&mut self, amt: usize) {
        if self.pass > 0 {
            let amt = amt.min(self.pass);
            self.inner.consume(amt);
            self.pass -= amt;
            self.position += amt as u64;
        } else {
            self.at = (self.at + amt).min(self.out.len());
        }
    }
}

impl<R: BufRead> Read for EscapeInvalid<R> {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        let buf = self.fill_buf()?;
        let n = buf.len().min(into.len());
        into[..n].copy_from_slice(&buf[..n]);
        self.consume(n);
        Ok(n)
    }
}

/// The byte an escape stands for.
fn escaped_byte(c: char) -> Option<u8> {
    u32::from(c)
        .checked_sub(ESCAPE_BASE)
        .and_then(|b| u8::try_from(b).ok())
        .filter(|b| *b >= 0x80)
}

/// `value` with each escape turned back into its byte.
fn restore(value: &str) -> Cow<'_, [u8]> {
    if !value.chars().any(|c| escaped_byte(c).is_some()) {
        return Cow::Borrowed(value.as_bytes());
    }
    let mut bytes = Vec::with_capacity(value.len());
    for c in value.chars() {
        match escaped_byte(c) {
            Some(b) => bytes.push(b),
            None => bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
        }
    }
    Cow::Owned(bytes)
}

/// Fails as the original bytes of `value` fail UTF-8 validation when it holds an
/// escape from [`EscapeInvalid`].
///
/// ```
/// assert!(mistarr_core::xml::check_utf8("café").is_ok());
/// assert!(mistarr_core::xml::check_utf8("caf\u{10ffe9}").is_err());
/// ```
///
/// # Errors
/// The [`Utf8Error`] of the original bytes.
pub fn check_utf8(value: &str) -> Result<(), Utf8Error> {
    std::str::from_utf8(&restore(value)).map(|_| ())
}

/// `value` as [`String::from_utf8_lossy`] reads its original bytes.
///
/// ```
/// assert_eq!(mistarr_core::xml::lossy("caf\u{10ffe9}"), "caf\u{fffd}");
/// ```
#[must_use]
pub fn lossy(value: &str) -> String {
    String::from_utf8_lossy(&restore(value)).into_owned()
}

/// A [`BufRead`] that hands out at most a set number of bytes after each [`Capped::arm`],
/// then fails with [`io::ErrorKind::OutOfMemory`], so one XML event can never grow past
/// its cap in memory. Unarmed it passes everything through.
///
/// ```
/// use std::io::Read as _;
/// let mut r = mistarr_core::xml::Capped::new(&b"abcdef"[..]);
/// r.arm(4);
/// let mut out = Vec::new();
/// assert!(r.read_to_end(&mut out).is_err());
/// assert_eq!(out, b"abcd");
/// assert!(r.over());
/// ```
pub struct Capped<R> {
    inner: R,
    consumed: u64,
    limit: u64,
}

impl<R: BufRead> Capped<R> {
    /// Wraps `inner`, unarmed.
    #[must_use]
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            consumed: 0,
            limit: u64::MAX,
        }
    }

    /// Allows `cap` more bytes from here.
    pub fn arm(&mut self, cap: u64) {
        self.limit = self.consumed.saturating_add(cap);
    }

    /// Whether the cap was reached.
    #[must_use]
    pub fn over(&self) -> bool {
        self.consumed >= self.limit
    }

    /// The wrapped reader.
    #[must_use]
    pub fn get_ref(&self) -> &R {
        &self.inner
    }
}

impl<R: BufRead> BufRead for Capped<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        let left = self.limit.saturating_sub(self.consumed);
        let buf = self.inner.fill_buf()?;
        if left == 0 && !buf.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "an XML element is too large",
            ));
        }
        let n = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
        Ok(&buf[..n])
    }

    fn consume(&mut self, amt: usize) {
        self.consumed = self.consumed.saturating_add(amt as u64);
        self.inner.consume(amt);
    }
}

impl<R: BufRead> Read for Capped<R> {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        let buf = self.fill_buf()?;
        let n = buf.len().min(into.len());
        into[..n].copy_from_slice(&buf[..n]);
        self.consume(n);
        Ok(n)
    }
}

/// A character or predefined entity reference as text; an unknown entity is kept as written.
///
/// ```
/// use quick_xml::events::BytesRef;
/// use mistarr_core::xml::resolve_ref;
/// assert_eq!(resolve_ref(&BytesRef::new("amp"))?, "&");
/// assert_eq!(resolve_ref(&BytesRef::new("#x41"))?, "A");
/// assert_eq!(resolve_ref(&BytesRef::new("custom"))?, "&custom;");
/// # Ok::<(), quick_xml::Error>(())
/// ```
///
/// # Errors
/// A malformed character reference, or a name read from bytes that are not UTF-8.
pub fn resolve_ref(r: &BytesRef<'_>) -> Result<String, quick_xml::Error> {
    if let Some(c) = r.resolve_char_ref()? {
        return Ok(c.to_string());
    }
    let name: &str = r;
    check_utf8(name)?;
    Ok(resolve_predefined_entity(name).map_or_else(|| format!("&{name};"), str::to_owned))
}

/// The value of the attribute whose local name is `key`, with references resolved and
/// whitespace normalized as XML 1.0 says; `None` when the element has no such attribute.
///
/// ```
/// use quick_xml::events::BytesStart;
/// use mistarr_core::xml::attr_value;
/// let e = BytesStart::from_content(r#"rom name="a &amp; b"  size="4""#, 3);
/// assert_eq!(attr_value(&e, "name")?.as_deref(), Some("a & b"));
/// assert_eq!(attr_value(&e, "crc")?, None);
/// # Ok::<(), quick_xml::Error>(())
/// ```
///
/// # Errors
/// A malformed attribute, or a value read from bytes that are not UTF-8.
pub fn attr_value(e: &BytesStart<'_>, key: &str) -> Result<Option<String>, quick_xml::Error> {
    for attr in e.attributes() {
        let attr = attr?;
        if attr.key.local_name().as_ref() == key {
            let value = attr.normalized_value(XmlVersion::Implicit1_0)?;
            check_utf8(&value)?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

/// Why a [`CappedReader`] stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadError {
    /// The XML is malformed or could not be read.
    #[error("invalid XML at byte {position}: {source}")]
    Xml {
        /// Byte offset where the error was detected.
        position: u64,
        /// Underlying parser error.
        source: quick_xml::Error,
    },
    /// One event, a tag with its attributes, a text run or a comment, is over the event cap.
    #[error("an XML event at byte {position} is too large")]
    EventTooLarge {
        /// Byte offset where the event starts.
        position: u64,
    },
    /// Elements nest deeper than the depth cap.
    #[error("elements nest too deep at byte {position}")]
    TooDeep {
        /// Byte offset of the element that went too deep.
        position: u64,
    },
}

/// An XML reader over input that may hold invalid UTF-8 (see [`EscapeInvalid`]) which
/// never buffers one event past its event cap nor nests past its depth cap.
///
/// ```
/// use quick_xml::events::Event;
/// use mistarr_core::xml::{CappedReader, ReadError};
/// let mut r = CappedReader::new(&b"<a><b/></a>"[..], 64, 1);
/// assert!(matches!(r.read_event()?, Event::Start(_)));
/// assert!(matches!(r.read_event()?, Event::Empty(_)));
/// assert_eq!(r.position(), 7);
/// let mut deep = CappedReader::new(&b"<a><b>"[..], 64, 1);
/// deep.read_event()?;
/// assert!(matches!(deep.read_event(), Err(ReadError::TooDeep { position: 3 })));
/// # Ok::<(), ReadError>(())
/// ```
pub struct CappedReader<R: BufRead> {
    reader: Reader<Capped<EscapeInvalid<R>>>,
    buf: Vec<u8>,
    event_cap: u64,
    depth_cap: usize,
    depth: usize,
}

impl<R: BufRead> CappedReader<R> {
    /// Wraps `reader`, allowing each event at most `event_cap` bytes and elements at
    /// most `depth_cap` levels deep, the root at level 1.
    #[must_use]
    pub fn new(reader: R, event_cap: u64, depth_cap: usize) -> Self {
        Self {
            reader: Reader::from_reader(Capped::new(EscapeInvalid::new(reader))),
            buf: Vec::new(),
            event_cap,
            depth_cap,
            depth: 0,
        }
    }

    /// Bytes of the input behind everything read so far.
    #[must_use]
    pub fn position(&self) -> u64 {
        self.reader.get_ref().get_ref().position()
    }

    /// A parser error at the current position.
    #[must_use]
    pub fn error(&self, source: quick_xml::Error) -> ReadError {
        ReadError::Xml {
            position: self.position(),
            source,
        }
    }

    /// The next event.
    ///
    /// # Errors
    /// [`ReadError::EventTooLarge`] or [`ReadError::TooDeep`] past a cap, else
    /// [`ReadError::Xml`] for malformed XML or a failed read.
    pub fn read_event(&mut self) -> Result<Event<'static>, ReadError> {
        self.buf.clear();
        let start = self.position();
        self.reader.get_mut().arm(self.event_cap);
        let event = match self.reader.read_event_into(&mut self.buf) {
            Ok(event) => event.into_owned(),
            Err(_) if self.reader.get_ref().over() => {
                return Err(ReadError::EventTooLarge { position: start })
            }
            Err(err) => return Err(self.error(err)),
        };
        match event {
            Event::Start(_) => {
                self.depth += 1;
                if self.depth > self.depth_cap {
                    return Err(ReadError::TooDeep { position: start });
                }
            }
            Event::End(_) => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::BufReader;

    fn read_all(bytes: &[u8], capacity: usize) -> (String, u64) {
        let mut r = EscapeInvalid::new(BufReader::with_capacity(capacity, bytes));
        let mut s = String::new();
        r.read_to_string(&mut s).unwrap();
        (s, r.position())
    }

    #[test]
    fn utf8_passes_through_and_invalid_bytes_are_escaped() {
        assert_eq!(read_all("a é €".as_bytes(), 1).0, "a é €");
        assert_eq!(
            read_all(b"x\xe9\xff", 64),
            ("x\u{10ffe9}\u{10ffff}".to_owned(), 3)
        );
        assert_eq!(read_all(b"\xe2\x82", 2).0, "\u{10ffe2}\u{10ff82}");
        assert_eq!(read_all(b"\xe2A", 1).0, "\u{10ffe2}A");
    }

    #[test]
    fn position_counts_inner_bytes() {
        let mut r = EscapeInvalid::new(&b"\xe9\xe9<"[..]);
        for _ in 0..2 {
            assert_eq!(r.fill_buf().unwrap().len(), ESCAPE_LEN);
            r.consume(ESCAPE_LEN);
        }
        assert_eq!(r.position(), 2);
        assert_eq!(r.fill_buf().unwrap(), b"<");
        r.consume(1);
        assert_eq!(r.position(), 3);
        assert!(r.fill_buf().unwrap().is_empty());
    }

    #[test]
    fn check_and_lossy_see_the_original_bytes() {
        let (s, _) = read_all(b"ok \xc3(", 8);
        let err = check_utf8(&s).unwrap_err();
        assert_eq!(err.valid_up_to(), 3);
        assert_eq!(lossy(&s), "ok \u{fffd}(");
        assert_eq!(lossy("plain"), "plain");
        assert!(check_utf8("\u{10ff7f}").is_ok());
    }

    #[test]
    fn refs_resolve_and_bad_ones_fail() {
        assert_eq!(resolve_ref(&BytesRef::new("lt")).unwrap(), "<");
        assert_eq!(resolve_ref(&BytesRef::new("#233")).unwrap(), "é");
        assert_eq!(resolve_ref(&BytesRef::new("unknown")).unwrap(), "&unknown;");
        assert!(resolve_ref(&BytesRef::new("#xZZ")).is_err());
        assert!(resolve_ref(&BytesRef::new("a\u{10ffe9}")).is_err());
    }

    #[test]
    fn attr_values_match_local_names_and_check_utf8() {
        let e = BytesStart::from_content("rom x:crc=\" 0A \" name=\"a\u{10ffe9}\"", 3);
        assert_eq!(attr_value(&e, "crc").unwrap().as_deref(), Some(" 0A "));
        assert!(attr_value(&e, "name").is_err());
        assert_eq!(attr_value(&e, "size").unwrap(), None);
        let bad = BytesStart::from_content("rom name=unquoted", 3);
        assert!(attr_value(&bad, "name").is_err());
    }

    #[test]
    fn capped_reader_enforces_both_caps_and_reports_positions() {
        let mut r = CappedReader::new(&b"<a><b>text</b></a>"[..], 16, 2);
        let mut kinds = Vec::new();
        loop {
            match r.read_event().unwrap() {
                Event::Eof => break,
                e => kinds.push(std::mem::discriminant(&e)),
            }
        }
        assert_eq!(kinds.len(), 5);
        assert_eq!(r.position(), 18);

        let mut big = CappedReader::new(&b"<a>0123456789abcdef</a>"[..], 8, 4);
        big.read_event().unwrap();
        assert!(matches!(
            big.read_event(),
            Err(ReadError::EventTooLarge { position: 3 })
        ));

        let mut bad = CappedReader::new(&b"<a></b>"[..], 64, 4);
        bad.read_event().unwrap();
        let err = bad.read_event().unwrap_err();
        assert!(matches!(err, ReadError::Xml { .. }), "{err}");
        let made = bad.error(quick_xml::Error::from(
            check_utf8("\u{10ffe9}").unwrap_err(),
        ));
        assert!(matches!(made, ReadError::Xml { position, .. } if position == bad.position()));
    }

    proptest! {
        #[test]
        fn capped_reader_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..128), cap in 1u64..32) {
            let mut r = CappedReader::new(&bytes[..], cap, 3);
            for _ in 0..256 {
                match r.read_event() {
                    Ok(Event::Eof) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        }

        #[test]
        fn restores_input_at_any_capacity(
            bytes in prop::collection::vec(any::<u8>(), 0..64),
            capacity in 1usize..8,
        ) {
            let (s, position) = read_all(&bytes, capacity);
            prop_assert_eq!(restore(&s).into_owned(), bytes.clone());
            prop_assert_eq!(check_utf8(&s).is_ok(), std::str::from_utf8(&bytes).is_ok());
            prop_assert_eq!(lossy(&s), String::from_utf8_lossy(&bytes));
            prop_assert_eq!(position, bytes.len() as u64);
        }
    }
}
