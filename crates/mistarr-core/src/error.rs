//! The crate-wide error, one variant per module error.

use crate::bencode::BencodeError;
use crate::chd::ChdError;
use crate::dat::{DatError, RewriteError};
use crate::hash::{HashError, UnknownHeaderRule};
use crate::magnet::MagnetError;
use crate::xml::ReadError;
use crate::ParseDigestError;

/// Any failure from this crate. Each module returns its own error; this
/// wraps them for callers that handle several modules alike.
///
/// ```
/// let e = mistarr_core::Error::from("zz".parse::<mistarr_core::Md5>().unwrap_err());
/// assert!(matches!(e, mistarr_core::Error::Digest(_)));
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A DAT could not be parsed.
    #[error(transparent)]
    Dat(#[from] DatError),
    /// A DAT could not be rewritten.
    #[error(transparent)]
    Rewrite(#[from] RewriteError),
    /// XML stopped at a cap or a parse error.
    #[error(transparent)]
    Xml(#[from] ReadError),
    /// A stream could not be hashed.
    #[error(transparent)]
    Hash(#[from] HashError),
    /// A name is not a header rule.
    #[error(transparent)]
    HeaderRule(#[from] UnknownHeaderRule),
    /// A CHD image could not be read.
    #[error(transparent)]
    Chd(#[from] ChdError),
    /// Bencoded data is malformed.
    #[error(transparent)]
    Bencode(#[from] BencodeError),
    /// A magnet link is malformed.
    #[error(transparent)]
    Magnet(#[from] MagnetError),
    /// Text is not a digest of the expected length.
    #[error(transparent)]
    Digest(#[from] ParseDigestError),
}

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_module_error_converts_and_keeps_its_message() {
        let digest = "zz".parse::<crate::Sha1>().unwrap_err();
        let text = digest.to_string();
        assert_eq!(Error::from(digest).to_string(), text);
        let rule = "nope".parse::<crate::hash::HeaderRule>().unwrap_err();
        assert!(matches!(Error::from(rule), Error::HeaderRule(_)));
        let magnet = crate::magnet::parse_magnet("http://x").unwrap_err();
        assert!(matches!(Error::from(magnet), Error::Magnet(_)));
        let ok: Result<u8> = Ok(1);
        assert_eq!(ok.ok(), Some(1));
    }
}
