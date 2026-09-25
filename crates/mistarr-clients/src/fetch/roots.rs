//! Trust anchors for https fetches: the system bundle when present, else the built-in set.

use std::path::{Path, PathBuf};

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::CertificateDer;
use rustls::RootCertStore;

/// Where the usual Linux distributions, Buildroot included, keep their CA bundle.
pub const SYSTEM_BUNDLES: [&str; 6] = [
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/ssl/cert.pem",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/ca-bundle.pem",
    "/etc/ssl/certs/cacert.pem",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
];

/// The environment variable naming a PEM bundle to use in place of the system's.
pub const CERT_FILE_ENV: &str = "SSL_CERT_FILE";

/// Where a [`Roots`] came from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RootsOrigin {
    /// A PEM bundle on disk.
    File(PathBuf),
    /// The Mozilla set compiled into the binary.
    Bundled,
}

/// The certificate authorities an https fetch trusts.
#[derive(Debug, Clone)]
pub struct Roots {
    pub(super) store: RootCertStore,
    origin: RootsOrigin,
    skipped: Option<(PathBuf, String)>,
}

impl Roots {
    /// The bundle named by `SSL_CERT_FILE`, else the first of [`SYSTEM_BUNDLES`] that holds
    /// a certificate, else [`Roots::bundled`]. A named bundle that cannot be used is
    /// reported by [`Roots::skipped`].
    ///
    /// ```
    /// let roots = mistarr_clients::fetch::Roots::system_or_bundled();
    /// assert!(roots.len() > 0);
    /// ```
    #[must_use]
    pub fn system_or_bundled() -> Self {
        let named = std::env::var_os(CERT_FILE_ENV).map(PathBuf::from);
        Self::first_usable(named, &SYSTEM_BUNDLES)
    }

    fn first_usable(named: Option<PathBuf>, system: &[&str]) -> Self {
        let skipped = named.as_ref().and_then(|p| match Self::from_pem_file(p) {
            Ok(_) => None,
            Err(e) => Some((p.clone(), e.to_string())),
        });
        let mut roots = named
            .into_iter()
            .chain(system.iter().map(PathBuf::from))
            .find_map(|p| Self::from_pem_file(&p).ok())
            .unwrap_or_else(Self::bundled);
        roots.skipped = skipped;
        roots
    }

    /// The bundle `SSL_CERT_FILE` named and why it was passed over, when it was.
    #[must_use]
    pub fn skipped(&self) -> Option<(&Path, &str)> {
        self.skipped
            .as_ref()
            .map(|(p, why)| (p.as_path(), why.as_str()))
    }

    /// The Mozilla root set from the `webpki-roots` crate, for boards without a bundle.
    ///
    /// ```
    /// use mistarr_clients::fetch::{Roots, RootsOrigin};
    /// assert_eq!(Roots::bundled().origin(), &RootsOrigin::Bundled);
    /// ```
    #[must_use]
    pub fn bundled() -> Self {
        let store = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        Self {
            store,
            origin: RootsOrigin::Bundled,
            skipped: None,
        }
    }

    /// The certificates of the PEM bundle at `path`.
    ///
    /// # Errors
    ///
    /// `NotFound` when the file cannot be read, `InvalidData` when it holds no usable certificate.
    ///
    /// ```
    /// let missing = std::path::Path::new("/nonexistent/bundle.pem");
    /// assert!(mistarr_clients::fetch::Roots::from_pem_file(missing).is_err());
    /// ```
    pub fn from_pem_file(path: &Path) -> std::io::Result<Self> {
        let certs = CertificateDer::pem_file_iter(path)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()))?;
        let mut store = RootCertStore::empty();
        let (added, _) = store.add_parsable_certificates(certs.flatten());
        if added == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "no certificate in the bundle",
            ));
        }
        Ok(Self {
            store,
            origin: RootsOrigin::File(path.to_path_buf()),
            skipped: None,
        })
    }

    /// Where these roots came from.
    #[must_use]
    pub fn origin(&self) -> &RootsOrigin {
        &self.origin
    }

    /// How many authorities are trusted.
    #[must_use]
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// Whether none is.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_without_certificates_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("empty.pem");
        std::fs::write(&path, "not a certificate\n").expect("write");
        let e = Roots::from_pem_file(&path).expect_err("empty");
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
        assert!(!Roots::bundled().is_empty());
    }

    #[test]
    fn a_pem_bundle_is_read() {
        let key = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).expect("cert");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ca.pem");
        std::fs::write(&path, key.cert.pem()).expect("write");
        let roots = Roots::from_pem_file(&path).expect("read");
        assert_eq!(roots.len(), 1);
        assert_eq!(roots.origin(), &RootsOrigin::File(path.clone()));
        assert!(roots.skipped().is_none());

        let bad = dir.path().join("bad.pem");
        std::fs::write(&bad, "junk\n").expect("write");
        let system = [path.to_str().expect("utf-8")];
        let roots = Roots::first_usable(Some(bad.clone()), &system);
        assert_eq!(roots.origin(), &RootsOrigin::File(path.clone()));
        assert_eq!(roots.skipped().map(|(p, _)| p), Some(bad.as_path()));
        let roots = Roots::first_usable(None, &[]);
        assert_eq!(roots.origin(), &RootsOrigin::Bundled);
        assert!(roots.skipped().is_none());
    }
}
