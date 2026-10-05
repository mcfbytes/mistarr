//! The set of file indices an operation selects in the client.

use std::collections::BTreeSet;

use crate::{Error, Result};

/// Wanted file indices, deduplicated and in ascending order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Wanted(BTreeSet<u32>);

impl Wanted {
    /// The indices in `wanted`, in any order and with repeats.
    pub(crate) fn from_slice(wanted: &[u32]) -> Self {
        Self(wanted.iter().copied().collect())
    }

    /// Refuses an index past the end of a torrent of `file_count` files.
    pub(crate) fn check(&self, file_count: usize) -> Result<()> {
        match self.0.last() {
            Some(&index) if usize::try_from(index).unwrap_or(usize::MAX) >= file_count => {
                Err(Error::FileIndex { index, file_count })
            }
            _ => Ok(()),
        }
    }

    /// Whether file `index` is wanted.
    pub(crate) fn contains(&self, index: u32) -> bool {
        self.0.contains(&index)
    }

    /// Whether no file is wanted.
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The wanted indices in ascending order.
    pub(crate) fn indices(&self) -> &BTreeSet<u32> {
        &self.0
    }

    /// The indices of the first `file_count` files that are not wanted.
    pub(crate) fn complement(&self, file_count: usize) -> Vec<u32> {
        (0u32..)
            .take(file_count)
            .filter(|i| !self.0.contains(i))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_deduplicates_and_sorts() {
        let w = Wanted::from_slice(&[3, 1, 3]);
        assert_eq!(w.indices().iter().copied().collect::<Vec<_>>(), [1, 3]);
        assert!(w.contains(1) && !w.contains(2));
        assert!(!w.is_empty() && Wanted::from_slice(&[]).is_empty());
    }

    #[test]
    fn checks_the_highest_index_against_the_count() {
        let w = Wanted::from_slice(&[1, 3]);
        assert!(w.check(4).is_ok());
        assert!(matches!(
            w.check(3),
            Err(Error::FileIndex {
                index: 3,
                file_count: 3
            })
        ));
        assert!(Wanted::default().check(0).is_ok());
    }

    #[test]
    fn complement_lists_the_unwanted_files() {
        let w = Wanted::from_slice(&[1, 3]);
        assert_eq!(w.complement(5), vec![0, 2, 4]);
        assert_eq!(w.complement(0), Vec::<u32>::new());
        assert!(Wanted::from_slice(&[0, 1]).complement(2).is_empty());
    }
}
