//! The version this binary reports; see `docs/DEPLOYMENT.md` "Releasing".

use std::sync::LazyLock;

/// Characters of the commit id kept in a development version.
const SHORT: usize = 7;

static VERSION: LazyLock<String> = LazyLock::new(|| {
    format(
        option_env!("MISTARR_VERSION"),
        env!("CARGO_PKG_VERSION"),
        raw_commit(),
    )
});

fn raw_commit() -> Option<&'static str> {
    option_env!("MISTARR_COMMIT")
        .filter(|c| !c.is_empty())
        .or(option_env!("GITHUB_SHA"))
}

/// The version string: a release tag's version, or `<crate version>+dev`, with
/// `.<short commit>` when the build knew its commit.
///
/// ```
/// assert!(!mistarr_server::version::version().is_empty());
/// ```
#[must_use]
pub fn version() -> &'static str {
    VERSION.as_str()
}

/// Whether the build came from a release tag.
///
/// ```
/// let v = mistarr_server::version::version();
/// assert_eq!(mistarr_server::version::is_release(), !v.contains("+dev"));
/// ```
#[must_use]
pub fn is_release() -> bool {
    option_env!("MISTARR_VERSION").is_some_and(|v| !v.is_empty())
}

/// The commit the binary was built from, shortened, when the build knew it.
///
/// ```
/// assert!(mistarr_server::version::commit().is_none_or(|c| c.len() <= 7));
/// ```
#[must_use]
pub fn commit() -> Option<&'static str> {
    raw_commit().and_then(short)
}

/// Formats the version from a release tag's version, the crate version and a commit.
///
/// ```
/// use mistarr_server::version::format;
/// assert_eq!(format(Some("0.3.0"), "0.3.0", Some("0123456789abcdef")), "0.3.0");
/// assert_eq!(format(None, "0.3.0", Some("0123456789abcdef")), "0.3.0+dev.0123456");
/// assert_eq!(format(None, "0.3.0", None), "0.3.0+dev");
/// ```
#[must_use]
pub fn format(release: Option<&str>, crate_version: &str, commit: Option<&str>) -> String {
    if let Some(tag) = release.map(str::trim).filter(|t| !t.is_empty()) {
        return tag.trim_start_matches('v').to_owned();
    }
    match commit.and_then(short) {
        Some(c) => format!("{crate_version}+dev.{c}"),
        None => format!("{crate_version}+dev"),
    }
}

/// The first characters of a hexadecimal commit id; `None` for anything else.
fn short(commit: &str) -> Option<&str> {
    let c = commit.trim();
    let hex = !c.is_empty() && c.bytes().all(|b| b.is_ascii_hexdigit());
    hex.then(|| &c[..c.len().min(SHORT)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_builds_report_the_tag() {
        assert_eq!(format(Some("1.2.3"), "0.3.0", None), "1.2.3");
        assert_eq!(
            format(Some("v1.2.3-rc.1"), "0.3.0", Some("abc")),
            "1.2.3-rc.1"
        );
    }

    #[test]
    fn other_builds_are_marked_dev() {
        assert_eq!(format(Some(""), "0.3.0", None), "0.3.0+dev");
        assert_eq!(
            format(None, "0.3.0", Some("ABCDEF1234")),
            "0.3.0+dev.ABCDEF1"
        );
        assert_eq!(format(None, "0.3.0", Some("abc")), "0.3.0+dev.abc");
        assert_eq!(format(None, "0.3.0", Some("not a sha")), "0.3.0+dev");
        assert_eq!(format(None, "0.3.0", Some(" ")), "0.3.0+dev");
    }

    #[test]
    fn short_commit_is_hex_only() {
        assert_eq!(short("0123456789"), Some("0123456"));
        assert_eq!(short("g123"), None);
        assert_eq!(short(""), None);
    }

    #[test]
    fn this_build_is_consistent() {
        let v = version();
        assert!(v.starts_with(env!("CARGO_PKG_VERSION")) || is_release());
        if let (false, Some(c)) = (is_release(), commit()) {
            assert!(v.ends_with(&format!("+dev.{c}")));
        }
    }
}
