//! DAT family keys; the rule is `docs/VERIFICATION.md` "DAT families".

use std::fmt;

use serde::{Deserialize, Serialize};

/// Bracketed groups that name a form of a DAT's list rather than another list, compared
/// lowercase with `-` and `_` read as spaces; a date or version may follow the marker.
pub const FORMAT_MARKERS: &[&str] = &[
    "db export",
    "headered",
    "headerless",
    "parent clone",
    "retool",
    "bigendian",
    "byteswapped",
    "littleendian",
];

/// The list a DAT describes, whatever its form or version: a new load supersedes the
/// current version of the same family on the same platform.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DatFamily(pub String);

impl DatFamily {
    /// The key as stored.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DatFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The family of a DAT name: lowercased, without [`FORMAT_MARKERS`] groups anywhere or
/// version groups at the end, whitespace collapsed.
///
/// ```
/// use mistarr_core::dat::family_key;
/// let logiqx = family_key("Example Vendor - Example System (Headered)");
/// let export = family_key("Example Vendor - Example System (DB Export) (20260101-000000)");
/// assert_eq!(logiqx, export);
/// assert_eq!(logiqx.as_str(), "example vendor - example system");
/// assert_ne!(logiqx, family_key("Example Samples - Example System (Headered)"));
/// ```
#[must_use]
pub fn family_key(name: &str) -> DatFamily {
    let mut kept = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(open) = rest.find(['(', '[']) {
        let close = if rest[open..].starts_with('(') {
            ')'
        } else {
            ']'
        };
        let Some(len) = rest[open..].find(close) else {
            break;
        };
        let inner = &rest[open + 1..open + len];
        kept.push_str(&rest[..open]);
        if !is_marker(inner) {
            kept.push_str(&rest[open..=open + len]);
        }
        rest = &rest[open + len + 1..];
    }
    kept.push_str(rest);
    let mut key = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    while let Some(start) = trailing_version(&key) {
        key.truncate(start);
        key.truncate(key.trim_end().len());
    }
    DatFamily(key.to_lowercase())
}

/// Splits a final `(…)` group made of a date or version off a name: the name before it,
/// trimmed, and the group's text, empty when there is none.
///
/// ```
/// use mistarr_core::dat::split_version;
/// assert_eq!(split_version("Example System (20260101-000000)"), ("Example System", "20260101-000000"));
/// assert_eq!(split_version("Example System (Europe)"), ("Example System (Europe)", ""));
/// ```
#[must_use]
pub fn split_version(name: &str) -> (&str, &str) {
    let trimmed = name.trim();
    match trailing_version(trimmed) {
        Some(open) => (
            trimmed[..open].trim_end(),
            trimmed[open + 1..trimmed.len() - 1].trim(),
        ),
        None => (trimmed, ""),
    }
}

fn is_marker(group: &str) -> bool {
    let spaced: String = group
        .to_lowercase()
        .chars()
        .map(|c| if c == '-' || c == '_' { ' ' } else { c })
        .collect();
    let norm = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    FORMAT_MARKERS.iter().any(|m| {
        norm.strip_prefix(m).is_some_and(|after| {
            after.is_empty()
                || after.starts_with(' ')
                    && after
                        .chars()
                        .all(|c| c.is_ascii_digit() || ". :".contains(c))
        })
    })
}

/// Where a final `(…)` group made of a version or date starts.
fn trailing_version(key: &str) -> Option<usize> {
    let body = key.strip_suffix(')')?;
    let open = body.rfind('(')?;
    let inner = body[open + 1..].trim();
    let digits = inner.strip_prefix(['v', 'V']).unwrap_or(inner);
    let version = digits.chars().any(|c| c.is_ascii_digit())
        && digits
            .chars()
            .all(|c| c.is_ascii_digit() || ".-_: ".contains(c));
    version.then_some(open)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn forms_and_versions_of_one_list_share_a_family() {
        let one = [
            "Example Vendor - Example System (Headered)",
            "Example Vendor - Example System (DB Export)",
            "Example Vendor - Example System (Parent-Clone) (20260101-000000)",
            "example vendor -  Example System (Headerless) (v1.2)",
            "Example Vendor - Example System [Retool 2026-01-01]",
            "Example Vendor - Example System (BigEndian)",
        ];
        for name in one {
            assert_eq!(
                family_key(name).as_str(),
                "example vendor - example system",
                "{name}"
            );
        }
        let others = [
            "Example Samples - Example System (Headered)",
            "Example Vendor - Example System (Private)",
            "Example Vendor - Example System (Headered Extra)",
        ];
        for name in others {
            assert_ne!(family_key(name), family_key(one[0]), "{name}");
        }
        assert_eq!(family_key("Example (2) (Headered)").as_str(), "example");
        assert_eq!(
            family_key("Example (Unclosed").as_str(),
            "example (unclosed"
        );
        assert_eq!(family_key("Example").to_string(), "example");
    }

    proptest! {
        #[test]
        fn a_family_key_is_stable(name in "[A-Za-z0-9 ()\\[\\]._-]{0,40}") {
            let key = family_key(&name);
            prop_assert_eq!(family_key(key.as_str()), key);
        }

        #[test]
        fn a_marker_never_changes_the_family(name in "[A-Za-z0-9 ._-]{0,30}", v in 0u32..99_999) {
            let export = format!("{name} (DB Export) ({v})");
            prop_assert_eq!(family_key(&export), family_key(&format!("{name} (Headered)")));
        }
    }
}
