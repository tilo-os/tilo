//! A list as the store's open writes leave it: a list of the index with the few entries those
//! writes added and removed, read page by page as the list it stands for.

use std::rc::Rc;

use tilolang_runtime::store::PagedItems;

use super::Snapshot;
use super::lists::{Entry, Pages};

/// A list of entries in order: read from the index page by page, or held.
pub enum Base {
    Pages(Pages<Snapshot>),
    /// The entries `start..end` of a list read page by page: a run of a column's values, last
    /// first when `reversed`.
    Slice {
        pages: Pages<Snapshot>,
        start: usize,
        end: usize,
        reversed: bool,
    },
    Held(Vec<(String, Rc<str>)>),
}

impl Base {
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Pages(pages) => pages.len(),
            Self::Slice { start, end, .. } => end.saturating_sub(*start),
            Self::Held(held) => held.len(),
        }
    }

    pub fn get(&self, index: usize) -> Option<Rc<str>> {
        match self {
            Self::Pages(pages) => pages.get(index),
            Self::Slice {
                pages,
                start,
                end,
                reversed,
            } => {
                if index >= end.saturating_sub(*start) {
                    return None;
                }
                let at = if *reversed {
                    end.checked_sub(1)?.checked_sub(index)?
                } else {
                    start.checked_add(index)?
                };
                pages.get(at)
            }
            Self::Held(held) => held.get(index).map(|(_, id)| Rc::clone(id)),
        }
    }

    /// Whether its entries run from the last: a reversed slice.
    pub fn reversed(&self) -> bool {
        matches!(self, Self::Slice { reversed: true, .. })
    }

    /// Where `(key, id)` is or would go, and whether it is there. In a reversed slice the
    /// entries after it come before it.
    pub fn rank(&self, key: &str, id: &str) -> (usize, bool) {
        match self {
            Self::Pages(pages) => pages.rank(key, id),
            Self::Slice {
                pages,
                start,
                end,
                reversed,
            } => {
                let (at, found) = pages.rank(key, id);
                let found = found && at >= *start && at < *end;
                let before = at.clamp(*start, *end).saturating_sub(*start);
                if !*reversed {
                    return (before, found);
                }
                // Those after it, in the slice: it then is the one before them.
                let len = end.saturating_sub(*start);
                (
                    len.saturating_sub(before)
                        .saturating_sub(usize::from(found)),
                    found,
                )
            }
            Self::Held(held) => {
                let at = held.partition_point(|(k, i)| (k.as_str(), &**i) < (key, id));
                let found = held
                    .get(at)
                    .is_some_and(|(k, i)| k.as_str() == key && &**i == id);
                (at, found)
            }
        }
    }
}

/// The entries an item has in a list as one state of the index holds it (an item has one
/// per value of a column of a set): what finds where the item is ([`Merged::position`]).
/// `None` when they cannot be read.
pub type Locate = Box<dyn Fn(&str) -> Option<Vec<Entry>>>;

/// `base` with the entries `added` put in and those `removed` taken out.
pub struct Merged {
    base: Base,
    /// In the base's order, none of them in `base`.
    added: Vec<(String, Rc<str>)>,
    /// Where each of `added` is in the merged list, ascending.
    added_at: Vec<usize>,
    /// The indexes in `base` of the entries taken out, ascending.
    removed: Vec<usize>,
    /// Finds an item's entries, when the list was given the means.
    locate: Option<Locate>,
}

impl std::fmt::Debug for Merged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Merged")
            .field("len", &self.len())
            .field("added", &self.added.len())
            .field("removed", &self.removed.len())
            .finish_non_exhaustive()
    }
}

impl Merged {
    pub fn new(base: Base, mut added: Vec<Entry>, removed: &[Entry]) -> Self {
        let mut gone: Vec<usize> = removed
            .iter()
            .filter_map(|(key, id)| match base.rank(key, id) {
                (at, true) => Some(at),
                _ => None,
            })
            .collect();
        gone.sort_unstable();
        gone.dedup();
        added.sort();
        added.dedup();
        if base.reversed() {
            added.reverse();
        }
        let mut kept = Vec::with_capacity(added.len());
        let mut added_at = Vec::with_capacity(added.len());
        for (key, id) in added {
            let (at, found) = base.rank(&key, &id);
            if found && gone.binary_search(&at).is_err() {
                // Already there.
                continue;
            }
            let before_gone = gone.partition_point(|g| *g < at);
            let position = at.saturating_sub(before_gone).saturating_add(kept.len());
            added_at.push(position);
            kept.push((key, Rc::<str>::from(id)));
        }
        Self {
            base,
            added: kept,
            added_at,
            removed: gone,
            locate: None,
        }
    }

    /// The list, finding where an item is by its entries as `locate` reads them.
    #[must_use]
    pub fn locating(mut self, locate: Locate) -> Self {
        self.locate = Some(locate);
        self
    }

    /// Where the item `id` is, found by its entries: `Some(None)` when it is not in the list,
    /// `None` when the list cannot find it so.
    pub fn position(&self, id: &str) -> Option<Option<usize>> {
        let entries = (self.locate.as_ref()?)(id)?;
        Some(
            entries
                .iter()
                .find_map(|(key, id)| match self.rank(key, id) {
                    (at, true) => Some(at),
                    _ => None,
                }),
        )
    }

    pub fn len(&self) -> usize {
        self.base
            .len()
            .saturating_sub(self.removed.len())
            .saturating_add(self.added.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, index: usize) -> Option<Rc<str>> {
        if index >= self.len() {
            return None;
        }
        if let Ok(k) = self.added_at.binary_search(&index) {
            return self.added.get(k).map(|(_, id)| Rc::clone(id));
        }
        let before = self.added_at.partition_point(|at| *at < index);
        // The `n`th entry of the base that stays.
        let mut at = index.saturating_sub(before);
        for gone in &self.removed {
            if *gone <= at {
                at = at.saturating_add(1);
            } else {
                break;
            }
        }
        self.base.get(at)
    }

    /// Where `(key, id)` is or would go, and whether it is there.
    pub fn rank(&self, key: &str, id: &str) -> (usize, bool) {
        let (at, found) = self.base.rank(key, id);
        let gone_before = self.removed.partition_point(|g| *g < at);
        let found_in_base = found && self.removed.binary_search(&at).is_err();
        let reversed = self.base.reversed();
        let added_before = self.added.partition_point(|(k, i)| {
            let entry = (k.as_str(), &**i);
            if reversed {
                entry > (key, id)
            } else {
                entry < (key, id)
            }
        });
        let found_added = self
            .added
            .get(added_before)
            .is_some_and(|(k, i)| k.as_str() == key && &**i == id);
        (
            at.saturating_sub(gone_before).saturating_add(added_before),
            found_in_base || found_added,
        )
    }
}

impl PagedItems for Merged {
    fn len(&self) -> usize {
        Self::len(self)
    }

    fn get(&self, index: usize) -> Option<Rc<str>> {
        Self::get(self, index)
    }

    fn position(&self, id: &str) -> Option<Option<usize>> {
        Self::position(self, id)
    }
}

impl std::fmt::Debug for Base {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Base").field("len", &self.len()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(entries: &[(&str, &str)]) -> Base {
        Base::Held(
            entries
                .iter()
                .map(|(k, i)| ((*k).to_owned(), Rc::<str>::from(*i)))
                .collect(),
        )
    }

    fn entry(k: &str, i: &str) -> Entry {
        (k.to_owned(), i.to_owned())
    }

    fn all(merged: &Merged) -> Vec<String> {
        (0..merged.len())
            .map(|i| merged.get(i).unwrap().to_string())
            .collect()
    }

    #[test]
    fn a_merged_list_reads_as_the_list_it_stands_for() {
        let base = held(&[("b", "1"), ("d", "2"), ("f", "3"), ("h", "4")]);
        let merged = Merged::new(
            base,
            vec![entry("a", "x"), entry("e", "y"), entry("z", "w")],
            &[entry("d", "2"), entry("h", "4"), entry("q", "missing")],
        );
        assert_eq!(all(&merged), ["x", "1", "y", "3", "w"]);
        assert_eq!(merged.rank("e", "y"), (2, true));
        assert_eq!(merged.rank("d", "2"), (2, false));
        assert_eq!(merged.rank("f", "3"), (3, true));
        assert_eq!(merged.rank("zz", "0"), (5, false));
        assert_eq!(merged.get(5), None);
    }
}
