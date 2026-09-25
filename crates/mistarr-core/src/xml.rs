//! Reading XML that may hold invalid UTF-8; the contract is `docs/VERIFICATION.md` "Text encoding".

use std::borrow::Cow;
use std::io::{self, BufRead, Read};
use std::str::Utf8Error;

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

    proptest! {
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
