//! A list as the store's open writes leave it: a list of the index with the few entries those
//! writes added and removed, read page by page as the list it stands for.

use std::cell::{Cell, RefCell};
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
    /// A column's list with its first `absent` entries last: the items that have a value in
    /// their order, and then those without one.
    Presence {
        pages: Pages<Snapshot>,
        absent: usize,
    },
    Held(Vec<(String, Rc<str>)>),
}

impl Base {
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Pages(pages) | Self::Presence { pages, .. } => pages.len(),
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
            Self::Presence { pages, absent } => {
                let absent = (*absent).min(pages.len());
                if index < pages.len().saturating_sub(absent) {
                    pages.get(index.saturating_add(absent))
                } else {
                    pages.get(index.saturating_sub(pages.len().saturating_sub(absent)))
                }
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
            Self::Presence { pages, absent } => {
                let (at, found) = pages.rank(key, id);
                let values = pages.len().saturating_sub(*absent);
                // The entries without a value stand first in the list and last here.
                let at = if at >= *absent {
                    at.saturating_sub(*absent)
                } else {
                    at.saturating_add(values)
                };
                (at, found)
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
        // Each entry that is not in the base goes where the base would hold it, and the entries
        // put in before it stand before it.
        let mut placed: Vec<(usize, (String, Rc<str>))> = Vec::with_capacity(added.len());
        for (key, id) in added {
            let (at, found) = base.rank(&key, &id);
            if found && gone.binary_search(&at).is_err() {
                // Already there.
                continue;
            }
            let before_gone = gone.partition_point(|g| *g < at);
            placed.push((at.saturating_sub(before_gone), (key, Rc::<str>::from(id))));
        }
        placed.sort_by_key(|(at, _)| *at);
        let mut kept = Vec::with_capacity(placed.len());
        let mut added_at = Vec::with_capacity(placed.len());
        for (n, (at, entry)) in placed.into_iter().enumerate() {
            added_at.push(at.saturating_add(n));
            kept.push(entry);
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

    /// The place of the entry `(key, id)` when it is in the list: the same as
    /// [`Self::rank`], nothing when it is not.
    pub fn place(&self, key: &str, id: &str) -> Option<usize> {
        match self.rank(key, id) {
            (at, true) => Some(at),
            _ => None,
        }
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

/// Whether an entry of a run is kept: read as the listing fills its pages.
pub type Keep = Box<dyn Fn(&str) -> bool>;

/// One entry of a run as [`Filtered`] holds it, kept or not.
struct Scanned {
    id: Rc<str>,
    kept: bool,
}

/// A run read page by page and checked as it is read: the entries of the run whose items
/// `keep` holds of, in the run's order.
///
/// One conjunct of a filter chooses the run; the others are checked here, item by item, as the
/// listing fills its pages. Filling a page reads the run's pages until it holds a page's worth
/// of kept entries, so a view showing the first rows costs the entries it reads to fill them,
/// not the run's length.
///
/// Its length is the run's own, read from the column's counts: a list that has to size itself
/// before showing a row costs a directory read, not the workspace. Until the whole run is checked
/// the length is the most the listing can have, a kept entry per entry of the run, and
/// [`PagedItems::len_is_exact`] says so; once every entry is checked it is the count.
pub struct Filtered {
    base: Merged,
    keep: Keep,
    /// The most the listing can have: the run's length as the open writes leave it.
    most: usize,
    /// The entries checked so far, from the run's first in order, each kept or not.
    scanned: RefCell<Vec<Scanned>>,
    /// How many of the run's entries are checked.
    at: Cell<usize>,
    /// How many of `scanned` are kept.
    kept: Cell<usize>,
    /// Finds an item's entries in the run, when the run was given the means.
    locate: Option<Locate>,
}

impl std::fmt::Debug for Filtered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Filtered")
            .field("len", &self.len())
            .field("read", &self.at.get())
            .field("kept", &self.kept.get())
            .finish_non_exhaustive()
    }
}

impl Filtered {
    /// The run `base` (as the open writes leave it) of the entries `keep` holds of. The most
    /// it can hold is the run's own length, with the items the open writes added to it: a
    /// bound counted from the index alone left out what was just written, and a list sized
    /// by it showed none of that.
    pub fn new(base: Merged, keep: Keep) -> Self {
        Self {
            most: base.len(),
            base,
            keep,
            scanned: RefCell::new(Vec::new()),
            at: Cell::new(0),
            kept: Cell::new(0),
            locate: None,
        }
    }

    /// The listing, finding where an item is by its entries as `locate` reads them.
    #[must_use]
    pub fn locating(mut self, locate: Locate) -> Self {
        self.locate = Some(locate);
        self
    }

    /// The listing at most `most` items: the count of another run of the index, one that names
    /// no more than this run does, for the items it answers for.
    #[must_use]
    pub fn bounded(mut self, most: Option<usize>) -> Self {
        if let Some(most) = most {
            self.most = self.most.min(most);
        }
        self
    }

    /// Check the run's entries from where the last check stopped up to the place `upto`
    /// (exclusive), keeping those `keep` holds of.
    fn scan_to(&self, upto: usize) {
        let base = self.base.len();
        let upto = upto.min(base);
        let mut scanned = self.scanned.borrow_mut();
        while self.at.get() < upto {
            let at = self.at.get();
            let Some(id) = self.base.get(at) else {
                break;
            };
            let kept = (self.keep)(&id);
            if kept {
                self.kept.set(self.kept.get().saturating_add(1));
            }
            scanned.push(Scanned { id, kept });
            self.at.set(at.saturating_add(1));
        }
    }

    /// Whether every entry of the run is checked, so [`PagedItems::len`] is the count of the
    /// items kept.
    fn exact(&self) -> bool {
        self.at.get() >= self.base.len()
    }

    /// How many entries it keeps, checking the rest of the run.
    fn kept_all(&self) -> usize {
        self.scan_to(self.base.len());
        self.kept.get()
    }

    /// The kept entry at `index`, reading on until it is found or the run ends.
    fn kept_at(&self, index: usize) -> Option<Rc<str>> {
        loop {
            {
                let scanned = self.scanned.borrow();
                if let Some(id) = scanned.iter().filter(|entry| entry.kept).nth(index) {
                    return Some(Rc::clone(&id.id));
                }
            }
            if self.at.get() >= self.base.len() {
                return None;
            }
            // Read on in pages, so filling a page costs pages, not entries.
            self.scan_to(self.at.get().saturating_add(PAGE));
        }
    }

    /// Where the item `id` is among the kept entries: `Some(None)` when it is not one, or when
    /// it is not in the run at all.
    #[expect(
        clippy::option_option,
        reason = "Three states are the trait's: an index, not an item of the listing, and a listing that cannot say"
    )]
    fn position_of(&self, id: &str) -> Option<Option<usize>> {
        let locate = self.locate.as_ref()?;
        let entries = locate(id)?;
        if entries.is_empty() {
            return Some(None);
        }
        let mut best: Option<usize> = None;
        for (key, id) in entries {
            let Some(at) = self.base.place(&key, &id) else {
                // One of the item's entries is not in the run.
                continue;
            };
            // Check the run up to and with the entry, then count the ones kept before it.
            self.scan_to(at.saturating_add(1));
            let scanned = self.scanned.borrow();
            let Some(entry) = scanned.get(at) else {
                continue;
            };
            if entry.kept {
                let before = scanned.iter().take(at).filter(|e| e.kept).count();
                best = Some(best.map_or(before, |held| held.min(before)));
            }
        }
        Some(best)
    }
}

/// The entries one read of a [`Filtered`] checks at a time.
const PAGE: usize = 64;

impl PagedItems for Filtered {
    fn len(&self) -> usize {
        // The most it can have while the run is unread, so a list sized before a page is read
        // costs the run's count; the kept entries once the whole run is checked.
        if self.at.get() >= self.base.len() {
            return self.kept.get();
        }
        self.most
    }

    fn len_is_exact(&self) -> bool {
        self.at.get() >= self.base.len()
    }

    fn get(&self, index: usize) -> Option<Rc<str>> {
        self.kept_at(index)
    }

    fn position(&self, id: &str) -> Option<Option<usize>> {
        self.position_of(id)
    }
}

impl std::fmt::Debug for Base {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Base").field("len", &self.len()).finish()
    }
}

/// One run of items read page by page, one after another: what a key stands two items under
/// (the items done with, then the rest) read in the order of the key that tells them apart.
///
/// Each part checks its own run as it fills its pages ([`Filtered`]), so the first rows a view
/// shows cost the entries read to fill them. Where a part ends is learned by reading it to its
/// end: a row past the first part, or the length of them all, reads every part before it whole.
pub struct Partition {
    parts: Vec<Filtered>,
    /// The most the parts can hold together: the run they are each drawn from.
    most: usize,
}

impl Partition {
    /// The parts, read in the order given, each of the run `most` entries long.
    pub fn new(parts: Vec<Filtered>, most: usize) -> Self {
        Self { parts, most }
    }
}

impl std::fmt::Debug for Partition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Partition")
            .field("len", &self.len())
            .field("parts", &self.parts.len())
            .finish_non_exhaustive()
    }
}

impl PagedItems for Partition {
    fn len(&self) -> usize {
        // Every part checked: the entries kept. Until then the run they are drawn from is the
        // most they can hold, each entry standing in one part.
        if self.parts.iter().all(Filtered::exact) {
            return self.parts.iter().map(Filtered::kept_all).sum();
        }
        self.most
    }

    fn len_is_exact(&self) -> bool {
        self.parts.iter().all(Filtered::exact)
    }

    fn get(&self, index: usize) -> Option<Rc<str>> {
        let mut at = index;
        for part in &self.parts {
            if let Some(id) = part.kept_at(at) {
                return Some(id);
            }
            // This part ends before it: what it holds comes off the count.
            at = at.saturating_sub(part.kept_all());
        }
        None
    }

    fn position(&self, id: &str) -> Option<Option<usize>> {
        let mut before = 0usize;
        for part in &self.parts {
            match part.position_of(id) {
                Some(Some(at)) => return Some(Some(before.saturating_add(at))),
                Some(None) => before = before.saturating_add(part.kept_all()),
                None => return None,
            }
        }
        Some(None)
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

    fn filtered_ids(filtered: &Filtered, n: usize) -> Vec<String> {
        (0..n)
            .map_while(|i| filtered.get(i))
            .map(|id| id.to_string())
            .collect()
    }

    /// A run of `n` entries whose items are numbered, kept when their number is even.
    fn numbered(n: usize) -> Merged {
        let entries: Vec<(String, String)> = (0..n)
            .map(|i| (format!("{i:04}"), format!("{i:04}")))
            .collect();
        let held: Vec<(String, Rc<str>)> = entries
            .iter()
            .map(|(k, i)| (k.clone(), Rc::<str>::from(i.as_str())))
            .collect();
        Merged::new(Base::Held(held), Vec::new(), &[])
    }

    fn even(id: &str) -> bool {
        id.parse::<usize>().is_ok_and(|n| n % 2 == 0)
    }

    #[test]
    fn a_filtered_run_keeps_what_it_checks_and_reads_only_as_far_as_a_page_needs() {
        // Longer than a page of checks, so reading the first row does not read the run.
        let filtered = Filtered::new(numbered(200), Box::new(even));
        assert_eq!(
            filtered.len(),
            200,
            "sized before a page is read: the run's length, the most it can have"
        );
        assert_eq!(filtered.get(0).as_deref(), Some("0000"));
        assert_eq!(filtered.get(1).as_deref(), Some("0002"));
        assert!(!filtered.len_is_exact(), "the run is not all read yet");
        assert_eq!(filtered_ids(&filtered, 200).len(), 100);
        assert!(filtered.len_is_exact(), "the whole run is read now");
        assert_eq!(filtered.len(), 100);
    }

    #[test]
    fn a_filtered_run_finds_where_a_kept_item_is_and_that_a_dropped_one_is_not() {
        let locate: Locate = Box::new(|id: &str| Some(vec![(id.to_owned(), id.to_owned())]));
        let filtered = Filtered::new(numbered(200), Box::new(even)).locating(locate);
        assert_eq!(filtered.position("0004"), Some(Some(2)));
        assert!(
            !filtered.len_is_exact(),
            "finding 0004 checked only up to it"
        );
        assert_eq!(
            filtered.position("0001"),
            Some(None),
            "odd ones are dropped"
        );
        assert_eq!(filtered.position("0009"), Some(None));
    }

    #[test]
    fn counting_a_filtered_run_reads_it_to_its_end() {
        let filtered = Filtered::new(numbered(200), Box::new(even));
        assert_eq!(filtered_ids(&filtered, 200).len(), 100);
        assert_eq!(filtered.len(), 100, "the count once every entry is checked");
    }
}
