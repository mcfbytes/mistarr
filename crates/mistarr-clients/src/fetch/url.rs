//! The one URL a fetch may follow: http or https, no user name or password.

use std::fmt::Write as _;

use hyper::Uri;

use super::FetchError;

/// An `http` or `https` URL checked for fetching. Its text is never logged or stored;
/// only [`FetchUrl::host`] may be logged, at debug level.
#[derive(Clone, PartialEq, Eq)]
pub struct FetchUrl {
    secure: bool,
    /// The host without IPv6 brackets, as dialled and as the TLS server name.
    host: String,
    port: u16,
    /// `host[:port]` as written, for the `Host` header.
    authority: String,
    /// The path and query sent in the request line.
    target: String,
}

impl std::fmt::Debug for FetchUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FetchUrl(..)")
    }
}

/// Characters a pasted URL may carry that a request line cannot; they are percent-encoded.
fn needs_escape(b: u8) -> bool {
    !b.is_ascii_graphic()
        || matches!(
            b,
            b'"' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'|' | b'}'
        )
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if needs_escape(b) {
            let _ = write!(out, "%{b:02X}");
        } else {
            out.push(char::from(b));
        }
    }
    out
}

impl FetchUrl {
    /// Checks `text` as an absolute `http` or `https` URL without userinfo; spaces and
    /// other characters a request line cannot carry are percent-encoded, and a fragment dropped.
    ///
    /// ```
    /// use mistarr_clients::fetch::FetchUrl;
    /// let url = FetchUrl::parse("https://example.invalid/dats/a b.dat#top")?;
    /// assert_eq!(url.host(), "example.invalid");
    /// assert_eq!(url.last_segment().as_deref(), Some("a b.dat"));
    /// assert!(FetchUrl::parse("ftp://example.invalid/a.dat").is_err());
    /// assert!(FetchUrl::parse("http://user:pass@example.invalid/a.dat").is_err());
    /// # Ok::<(), mistarr_clients::fetch::FetchError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`FetchError::Url`] saying what is wrong, never repeating the URL.
    pub fn parse(text: &str) -> Result<Self, FetchError> {
        let text = text.trim();
        let text = text.split_once('#').map_or(text, |(url, _)| url);
        let uri: Uri = escape(text)
            .parse()
            .map_err(|_| FetchError::Url("This is not a valid link."))?;
        let secure = match uri.scheme_str().map(str::to_ascii_lowercase).as_deref() {
            Some("https") => true,
            Some("http") => false,
            _ => {
                return Err(FetchError::Url(
                    "Only http, https and magnet links are accepted.",
                ))
            }
        };
        let authority = uri
            .authority()
            .ok_or(FetchError::Url("The link has no host."))?;
        if authority.as_str().contains('@') {
            return Err(FetchError::Url(
                "Links with a user name or password are not accepted.",
            ));
        }
        let host = authority.host();
        if host.is_empty() {
            return Err(FetchError::Url("The link has no host."));
        }
        let written = authority.as_str().get(host.len()..).unwrap_or("");
        let port = match written.strip_prefix(':') {
            None if secure => 443,
            None => 80,
            Some(text) => match text.parse::<u16>() {
                Ok(p) if p > 0 => p,
                _ => return Err(FetchError::Url("The link's port is not valid.")),
            },
        };
        let target = uri
            .path_and_query()
            .map_or_else(|| "/".to_owned(), |p| p.as_str().to_owned());
        let target = if target.starts_with('/') {
            target
        } else {
            format!("/{target}")
        };
        Ok(Self {
            secure,
            host: host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_owned(),
            port,
            authority: authority.as_str().to_owned(),
            target,
        })
    }

    /// The host name or address, the only part of the URL that may be logged, at debug.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port dialled.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Whether the URL is `https`.
    #[must_use]
    pub fn is_secure(&self) -> bool {
        self.secure
    }

    pub(super) fn authority(&self) -> &str {
        &self.authority
    }

    pub(super) fn target(&self) -> &str {
        &self.target
    }

    /// The last segment of the path, percent-decoded, or `None` when the path ends in `/`.
    ///
    /// ```
    /// use mistarr_clients::fetch::FetchUrl;
    /// let url = FetchUrl::parse("http://example.invalid/x/Pack%20A.zip?v=2")?;
    /// assert_eq!(url.last_segment().as_deref(), Some("Pack A.zip"));
    /// assert_eq!(FetchUrl::parse("http://example.invalid/")?.last_segment(), None);
    /// # Ok::<(), mistarr_clients::fetch::FetchError>(())
    /// ```
    #[must_use]
    pub fn last_segment(&self) -> Option<String> {
        let path = self.target.split(['?']).next().unwrap_or("");
        let last = path.rsplit('/').next().unwrap_or("");
        let decoded = percent_decode(last);
        (!decoded.is_empty()).then_some(decoded)
    }

    /// Resolves a redirect's `Location` against this URL.
    ///
    /// ```
    /// use mistarr_clients::fetch::FetchUrl;
    /// let base = FetchUrl::parse("https://example.invalid/a/b.dat?x=1")?;
    /// assert_eq!(base.join("c.dat")?.last_segment().as_deref(), Some("c.dat"));
    /// assert_eq!(base.join("//other.invalid/d.dat")?.host(), "other.invalid");
    /// assert!(base.join("http://other.invalid/e.dat").is_ok());
    /// # Ok::<(), mistarr_clients::fetch::FetchError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`FetchError::Url`] when the result is not a URL [`FetchUrl::parse`] accepts.
    pub fn join(&self, location: &str) -> Result<Self, FetchError> {
        let location = location.trim();
        let scheme = if self.secure { "https" } else { "http" };
        let has_scheme = location.split_once(':').is_some_and(|(s, _)| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        }) && !location.starts_with('/');
        let absolute = if has_scheme {
            location.to_owned()
        } else if location.starts_with("//") {
            format!("{scheme}:{location}")
        } else if location.starts_with('/') {
            format!("{scheme}://{}{location}", self.authority)
        } else {
            let path = self.target.split('?').next().unwrap_or("/");
            let dir = if location.starts_with('?') {
                path
            } else {
                path.rsplit_once('/').map_or("", |(d, _)| d)
            };
            let sep = if location.starts_with('?') { "" } else { "/" };
            format!("{scheme}://{}{dir}{sep}{location}", self.authority)
        };
        Self::parse(&absolute)
    }
}

/// Decodes `%XX` escapes, keeping malformed ones as written; invalid UTF-8 is replaced.
///
/// ```
/// assert_eq!(mistarr_clients::fetch::percent_decode("a%20b%zz"), "a b%zz");
/// ```
#[must_use]
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(u8::try_from(h * 16 + l).unwrap_or(b'?'));
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemes_hosts_and_ports_are_checked() {
        let u = FetchUrl::parse("  HTTPS://example.invalid:8443/a.dat ").expect("valid");
        assert!(u.is_secure());
        assert_eq!(
            (u.host(), u.port(), u.target()),
            ("example.invalid", 8443, "/a.dat")
        );
        let u = FetchUrl::parse("http://[::1]/p?q=1").expect("valid");
        assert_eq!((u.host(), u.port(), u.authority()), ("::1", 80, "[::1]"));
        assert_eq!(u.target(), "/p?q=1");
        assert_eq!(
            FetchUrl::parse("http://example.invalid")
                .expect("valid")
                .target(),
            "/"
        );
        for bad in [
            "",
            "example.invalid/a.dat",
            "file:///etc/passwd",
            "magnet:?xt=urn:btih:x",
            "http://u@example.invalid/",
            "http://example.invalid:0/",
            "http://example.invalid:/",
            "http://example.invalid:99999/",
            "http:///a.dat",
        ] {
            assert!(FetchUrl::parse(bad).is_err(), "{bad}");
        }
        let e = FetchUrl::parse("http://u:p@example.invalid/").expect_err("userinfo");
        assert!(!e.to_string().contains("example.invalid"));
        assert_eq!(format!("{u:?}"), "FetchUrl(..)");
    }

    #[test]
    fn pasted_characters_are_escaped() {
        let u = FetchUrl::parse("http://example.invalid/a b/é.dat").expect("valid");
        assert_eq!(u.target(), "/a%20b/%C3%A9.dat");
        assert_eq!(u.last_segment().as_deref(), Some("é.dat"));
    }

    #[test]
    fn redirects_resolve_against_the_base() {
        let base = FetchUrl::parse("http://example.invalid:81/a/b.dat?x=1").expect("valid");
        let j = |l: &str| base.join(l).expect(l);
        assert_eq!(j("/c.dat").target(), "/c.dat");
        assert_eq!(j("/c.dat").authority(), "example.invalid:81");
        assert_eq!(j("c.dat").target(), "/a/c.dat");
        assert_eq!(j("?y=2").target(), "/a/b.dat?y=2");
        assert_eq!(j("//other.invalid/d").host(), "other.invalid");
        assert!(!j("//other.invalid/d").is_secure());
        assert!(j("https://other.invalid/e").is_secure());
        assert!(base.join("ftp://other.invalid/").is_err());
        assert!(base.join("http://u:p@other.invalid/").is_err());
    }

    proptest::proptest! {
        #[test]
        fn parsing_any_text_never_panics(text in "\\PC{0,80}") {
            if let Ok(u) = FetchUrl::parse(&text) {
                proptest::prop_assert!(u.target().starts_with('/'));
                proptest::prop_assert!(u.port() > 0);
                let _ = u.last_segment();
                let _ = u.join(&text);
            }
            let _ = percent_decode(&text);
        }
    }

    #[test]
    fn percent_escapes_decode() {
        assert_eq!(percent_decode("%41%2f%"), "A/%");
        assert_eq!(percent_decode("%4"), "%4");
        assert_eq!(percent_decode("%ff"), "\u{fffd}");
    }
}
