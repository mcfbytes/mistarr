//! This crate's errors for capped reads of MRA and romsets.xml; see docs/PLATFORMS.md.

use std::io;
use std::sync::Arc;

use mistarr_core::xml::ReadError;

use crate::Error;

/// `e` as an [`Error`], its position moved on by `offset` bytes the reader never saw;
/// `wrap` makes the error for malformed XML, and a kind of [`ReadError`] this crate does
/// not know becomes an I/O error through `wrap` at `offset`.
pub(super) fn read_error(
    e: ReadError,
    offset: u64,
    wrap: fn(quick_xml::Error, u64) -> Error,
) -> Error {
    match e {
        ReadError::Xml { position, source } => wrap(source, offset + position),
        ReadError::EventTooLarge { position } => Error::XmlEventTooLarge {
            position: offset + position,
        },
        ReadError::TooDeep { position } => Error::XmlTooDeep {
            position: offset + position,
        },
        other => wrap(
            quick_xml::Error::Io(Arc::new(io::Error::other(other))),
            offset,
        ),
    }
}

/// The document accumulates more `kind` than `limit` allows.
pub(super) fn output_too_large(kind: &'static str, limit: usize, position: u64) -> Error {
    Error::XmlOutputTooLarge {
        kind,
        limit,
        position,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(source: quick_xml::Error, position: u64) -> Error {
        Error::Romsets { position, source }
    }

    #[test]
    fn read_errors_keep_their_kind_and_shift_positions() {
        let deep = read_error(ReadError::TooDeep { position: 4 }, 3, wrap);
        assert!(matches!(deep, Error::XmlTooDeep { position: 7 }));
        let big = read_error(ReadError::EventTooLarge { position: 0 }, 3, wrap);
        assert!(matches!(big, Error::XmlEventTooLarge { position: 3 }));
        let source = quick_xml::Error::Io(Arc::new(io::Error::other("x")));
        let xml = read_error(
            ReadError::Xml {
                position: 1,
                source,
            },
            0,
            wrap,
        );
        assert!(matches!(xml, Error::Romsets { position: 1, .. }));
    }
}
