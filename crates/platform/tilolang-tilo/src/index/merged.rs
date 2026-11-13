//! A list as the store's open writes leave it: a list of the index with the few entries those
//! writes added and removed, read page by page as the list it stands for.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
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

/// What a listing read: the run and the checks it was made of, named. A listing read the same
/// way again after a change keeps the checks it made ([`Filtered::spliced`]).
pub type Shape = Rc<str>;

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
    /// Where each item checked so far has its first entry among them: what finds a row the
    /// listing already read without reading where its item is.
    first: RefCell<HashMap<Rc<str>, usize>>,
    /// How many it keeps, when the index's counts answer it before the run is read: a run
    /// narrowed to the items open, whose count the column's counts keep.
    known: Option<usize>,
    /// How many of the run's entries are checked.
    at: Cell<usize>,
    /// How many of `scanned` are kept.
    kept: Cell<usize>,
    /// Finds an item's entries in the run, when the run was given the means.
    locate: Option<Locate>,
    /// The read this listing is of, when it can be read again the same way: what a change
    /// leaves of it is kept ([`Self::spliced`]).
    shape: Option<Shape>,
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
            known: None,
            scanned: RefCell::new(Vec::new()),
            first: RefCell::new(HashMap::new()),
            at: Cell::new(0),
            kept: Cell::new(0),
            locate: None,
            shape: None,
        }
    }

    /// The listing is of the read `shape`, so a listing read the same way again after a change
    /// may keep what this one checked ([`Self::spliced`]).
    #[must_use]
    pub fn shaped(mut self, shape: Shape) -> Self {
        self.shape = Some(shape);
        self
    }

    /// The listing `old` was, over the run `base` as the change left it: the items `touched`
    /// checked again as they stand now, and every other entry of the run keeping what `old`
    /// found of it and where it stood.
    ///
    /// `Err` with the run and its check when `old` is of another read, or the run moved in a
    /// way the change did not name before the entries it checked: the caller reads it anew.
    pub fn spliced(
        old: &Self,
        base: Merged,
        keep: Keep,
        shape: Shape,
        touched: &BTreeSet<String>,
    ) -> Result<Self, Box<(Merged, Keep)>> {
        let walked = Self::kept_over(old, &base, &shape, &keep, touched);
        let Some((out, held)) = walked else {
            return Err(Box::new((base, keep)));
        };
        let most = base.len();
        let at = out.len();
        let mut first = HashMap::with_capacity(out.len());
        for (place, entry) in out.iter().enumerate() {
            first.entry(Rc::clone(&entry.id)).or_insert(place);
        }
        Ok(Self {
            most,
            base,
            keep,
            known: None,
            scanned: RefCell::new(out),
            first: RefCell::new(first),
            at: Cell::new(at),
            kept: Cell::new(held),
            locate: None,
            shape: Some(shape),
        })
    }

    /// What [`Self::spliced`] keeps of `old` over the run `base`: each entry of the run and
    /// whether it is kept, and how many are. `None` when `old` says nothing of the run.
    ///
    /// A run `old` checked whole keeps all of its checks; one it was still reading keeps those
    /// up to where the run moved, and reads the rest as it fills its pages again.
    fn kept_over(
        old: &Self,
        base: &Merged,
        shape: &Shape,
        keep: &Keep,
        touched: &BTreeSet<String>,
    ) -> Option<(Vec<Scanned>, usize)> {
        if old.shape.as_ref() != Some(shape) {
            return None;
        }
        let scanned = old.scanned.borrow();
        if !old.exact() {
            let mut out: Vec<Scanned> = Vec::new();
            let mut kept = 0usize;
            for at in 0..scanned.len() {
                let old_id = old.base.get(at)?;
                let new_id = base.get(at)?;
                // Where the run moved, what `old` found of the entries before it still holds;
                // from there the entries are read anew.
                if old_id != new_id || touched.contains(&*old_id) {
                    break;
                }
                let holds = scanned.get(at)?.kept;
                if holds {
                    kept = kept.saturating_add(1);
                }
                out.push(Scanned {
                    id: new_id,
                    kept: holds,
                });
            }
            return Some((out, kept));
        }
        let mut out: Vec<Scanned> = Vec::with_capacity(base.len());
        let mut kept = 0usize;
        let (mut before, mut now) = (0usize, 0usize);
        loop {
            let old_id = old.base.get(before);
            let new_id = base.get(now);
            match (old_id, new_id) {
                (None, None) => break,
                (Some(old_id), Some(new_id)) if old_id == new_id && !touched.contains(&*old_id) => {
                    let holds = scanned.get(before)?.kept;
                    if holds {
                        kept = kept.saturating_add(1);
                    }
                    out.push(Scanned {
                        id: new_id,
                        kept: holds,
                    });
                    before = before.saturating_add(1);
                    now = now.saturating_add(1);
                }
                // Gone from the run, or moved: where it is now is checked there.
                (Some(old_id), _) if touched.contains(&*old_id) => {
                    before = before.saturating_add(1);
                }
                (_, Some(new_id)) if touched.contains(&*new_id) => {
                    let holds = keep(&new_id);
                    if holds {
                        kept = kept.saturating_add(1);
                    }
                    out.push(Scanned {
                        id: new_id,
                        kept: holds,
                    });
                    now = now.saturating_add(1);
                }
                _ => return None,
            }
        }
        Some((out, kept))
    }

    /// The listing whose kept count is `count`, answered from the index's counts rather than
    /// from the run: how many of a place's items are open. Once the run is read to its end its
    /// own count stands.
    #[must_use]
    pub fn known(mut self, count: usize) -> Self {
        self.known = Some(count.min(self.most));
        self
    }

    /// The listing, finding where an item is by its entries as `locate` reads them.
    #[must_use]
    pub fn locating(mut self, locate: Locate) -> Self {
        self.locate = Some(locate);
        self
    }

    /// The listing at most `most` items: the count of another column's run as the open writes
    /// leave it, one that names no more than this run does, for the items it answers for.
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
            self.first.borrow_mut().entry(Rc::clone(&id)).or_insert(at);
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
        // A row already read is where its first entry was checked kept: no entry of the item
        // stands before the first one.
        if let Some(at) = self.first.borrow().get(id).copied() {
            let scanned = self.scanned.borrow();
            if scanned.get(at).is_some_and(|entry| entry.kept) {
                return Some(Some(
                    scanned.iter().take(at).filter(|entry| entry.kept).count(),
                ));
            }
        }
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
            // The run's last entry stands after every other one it keeps, so the count the
            // index answers of the listing says where it is: its own check and no read of the
            // run before it.
            if at.saturating_add(1) == self.base.len()
                && let Some(total) = self.known
            {
                if (self.keep)(&id) {
                    let held = total.saturating_sub(1);
                    best = Some(best.map_or(held, |now| now.min(held)));
                }
                continue;
            }
            let Some(before) = self.place_among_kept(at, &id) else {
                continue;
            };
            best = Some(best.map_or(before, |held| held.min(before)));
        }
        Some(best)
    }

    /// Where the entry at `at` of the run, of the item `id`, stands among the kept entries:
    /// `None` when it is not kept.
    ///
    /// An entry already checked keeps its check, and its place is the kept entries before it.
    /// One within a page past the checked ones is checked with those before it. One further on
    /// is checked alone, and stands where it would if every unchecked entry before it were kept,
    /// as the listing's length counts them ([`PagedItems::len`]) until the run is read that far:
    /// an item a change put at the end of a long run is placed by its own check, not by checking
    /// the whole run before it, and where it stands is read once a list's pages reach it.
    fn place_among_kept(&self, at: usize, id: &str) -> Option<usize> {
        let checked = self.at.get();
        if at >= checked.saturating_add(PAGE) && self.known.is_none() {
            if !(self.keep)(id) {
                return None;
            }
            let estimate = self.kept.get().saturating_add(at.saturating_sub(checked));
            return Some(estimate.min(self.most.saturating_sub(1)));
        }
        // An entry the item is not kept at has no place, which its own check says without
        // reading the run before it.
        if at >= checked && !(self.keep)(id) {
            return None;
        }
        self.scan_to(at.saturating_add(1));
        let scanned = self.scanned.borrow();
        let entry = scanned.get(at)?;
        if !entry.kept {
            return None;
        }
        Some(scanned.iter().take(at).filter(|e| e.kept).count())
    }

    /// Whether it keeps the item `id`: the check of its own entries of the run, never the ones
    /// before them. `None` when its checks cannot say which item an entry is
    /// ([`Filtered::position_of`]).
    fn keeps_item(&self, id: &str) -> Option<bool> {
        let locate = self.locate.as_ref()?;
        let entries = locate(id)?;
        if entries.is_empty() {
            return Some(false);
        }
        for (key, id) in entries {
            if self.base.place(&key, &id).is_some() && (self.keep)(&id) {
                return Some(true);
            }
        }
        Some(false)
    }
}

/// The entries one read of a [`Filtered`] checks at a time.
pub(super) const PAGE: usize = 64;

impl PagedItems for Filtered {
    fn len(&self) -> usize {
        // The kept entries once the whole run is checked, or as the index's counts answer them
        // when it knows; the most it can have before then, so a list sized before a page is
        // read costs the run's count and not the workspace.
        if self.at.get() >= self.base.len() {
            return self.kept.get();
        }
        self.known.unwrap_or(self.most)
    }

    fn len_is_exact(&self) -> bool {
        self.known.is_some() || self.at.get() >= self.base.len()
    }

    fn get(&self, index: usize) -> Option<Rc<str>> {
        self.kept_at(index)
    }

    fn position(&self, id: &str) -> Option<Option<usize>> {
        self.position_of(id)
    }

    fn holds(&self, id: &str) -> Option<bool> {
        self.keeps_item(id)
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

    /// Whether any of its parts keeps the item: each answers from the item's own entries, so
    /// none of them reads the entries before it.
    fn holds(&self, id: &str) -> Option<bool> {
        let mut say = Some(false);
        for part in &self.parts {
            match part.keeps_item(id) {
                Some(true) => return Some(true),
                Some(false) => {}
                None => say = None,
            }
        }
        say
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
    fn an_entry_far_past_the_checked_ones_is_placed_by_its_own_check() {
        let locate: Locate = Box::new(|id: &str| Some(vec![(id.to_owned(), id.to_owned())]));
        let checks = Rc::new(std::cell::Cell::new(0_usize));
        let counted = Rc::clone(&checks);
        let keep: Keep = Box::new(move |id: &str| {
            counted.set(counted.get().saturating_add(1));
            even(id)
        });
        let filtered = Filtered::new(numbered(1000), keep).locating(locate);
        assert_eq!(
            filtered.position("0998"),
            Some(Some(998)),
            "it stands where it would if every entry before it were kept, as the length counts them"
        );
        assert_eq!(checks.get(), 1, "only its own entry is checked");
        assert_eq!(
            filtered.position("0999"),
            Some(None),
            "a dropped one has no place"
        );
        assert!(!filtered.len_is_exact());
        assert_eq!(filtered.get(1).as_deref(), Some("0002"));
        assert_eq!(
            filtered.position("0002"),
            Some(Some(1)),
            "a checked one keeps its check and its place"
        );
    }

    #[test]
    fn counting_a_filtered_run_reads_it_to_its_end() {
        let filtered = Filtered::new(numbered(200), Box::new(even));
        assert_eq!(filtered_ids(&filtered, 200).len(), 100);
        assert_eq!(filtered.len(), 100, "the count once every entry is checked");
    }

    /// A run of the items `ids`, in the order given.
    fn run(ids: &[&str]) -> Merged {
        let held: Vec<(String, Rc<str>)> = ids
            .iter()
            .map(|id| ((*id).to_owned(), Rc::<str>::from(*id)))
            .collect();
        Merged::new(Base::Held(held), Vec::new(), &[])
    }

    /// Keeps the items `ids`: the run's items with a field of their own.
    fn keeps(ids: &[&str]) -> Keep {
        let kept: BTreeSet<String> = ids.iter().map(|id| (*id).to_owned()).collect();
        Box::new(move |id: &str| kept.contains(id))
    }

    fn touched(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    fn read_all(filtered: &Filtered) -> Vec<String> {
        filtered_ids(filtered, filtered.len())
    }

    #[test]
    fn a_spliced_listing_reads_as_the_one_read_anew() {
        let shape: Shape = Rc::from("the run of a column");
        let old =
            Filtered::new(run(&["a", "b", "c", "d"]), keeps(&["a", "c"])).shaped(shape.clone());
        assert_eq!(read_all(&old), ["a", "c"]);
        let after = [&["a", "b", "c", "d", "e"][..], &["a", "b", "c", "d"][..]];
        // An item put in: checked, the rest as they were.
        let spliced = Filtered::spliced(
            &old,
            run(after[0]),
            keeps(&["a", "c", "e"]),
            shape.clone(),
            &touched(&["e"]),
        )
        .ok()
        .expect("the run only gained an item");
        assert_eq!(
            read_all(&spliced),
            read_all(&Filtered::new(run(after[0]), keeps(&["a", "c", "e"])))
        );
        // An item made to be of the listing: checked, the rest as they were.
        let spliced = Filtered::spliced(
            &old,
            run(after[1]),
            keeps(&["a", "b", "c"]),
            shape.clone(),
            &touched(&["b"]),
        )
        .ok()
        .expect("the run is as it was");
        assert_eq!(
            read_all(&spliced),
            read_all(&Filtered::new(run(after[1]), keeps(&["a", "b", "c"])))
        );
        // An item made not to be of it.
        let spliced = Filtered::spliced(
            &old,
            run(after[1]),
            keeps(&["c"]),
            shape.clone(),
            &touched(&["a"]),
        )
        .ok()
        .expect("the run is as it was");
        assert_eq!(
            read_all(&spliced),
            read_all(&Filtered::new(run(after[1]), keeps(&["c"])))
        );
        // An item taken out: it is not checked again, and the rest stay.
        let spliced = Filtered::spliced(
            &old,
            run(&["b", "c", "d"]),
            keeps(&["a", "c"]),
            shape,
            &touched(&["a"]),
        )
        .ok()
        .expect("the run only lost an item");
        assert_eq!(
            read_all(&spliced),
            read_all(&Filtered::new(run(&["b", "c", "d"]), keeps(&["a", "c"])))
        );
    }

    #[test]
    fn a_spliced_listing_checks_the_items_the_change_wrote() {
        let shape: Shape = Rc::from("the run of a column");
        let old = Filtered::new(numbered(200), Box::new(even)).shaped(shape.clone());
        assert_eq!(read_all(&old).len(), 100, "the run read whole");
        let checks = Rc::new(Cell::new(0usize));
        let counted = {
            let checks = Rc::clone(&checks);
            Box::new(move |id: &str| {
                checks.set(checks.get().saturating_add(1));
                even(id)
            })
        };
        let spliced = Filtered::spliced(
            &old,
            numbered(200),
            counted,
            shape.clone(),
            &BTreeSet::new(),
        )
        .ok()
        .expect("the run is as it was");
        assert_eq!(checks.get(), 0, "nothing was written: nothing is read");
        assert_eq!(read_all(&spliced), read_all(&old));
        let checks = Rc::new(Cell::new(0usize));
        let counted = {
            let checks = Rc::clone(&checks);
            Box::new(move |id: &str| {
                checks.set(checks.get().saturating_add(1));
                even(id)
            })
        };
        let spliced = Filtered::spliced(
            &old,
            numbered(200),
            counted,
            shape,
            &touched(&["0003", "0100"]),
        )
        .ok()
        .expect("the run is as it was");
        assert_eq!(checks.get(), 2, "only the items the change wrote are read");
        assert_eq!(read_all(&spliced), read_all(&old));
    }

    #[test]
    fn a_listing_is_read_anew_when_the_change_does_not_explain_the_run() {
        let shape: Shape = Rc::from("the run of a column");
        let old = Filtered::new(run(&["a", "b", "c"]), keeps(&["a"])).shaped(shape.clone());
        assert_eq!(read_all(&old), ["a"]);
        assert!(
            Filtered::spliced(
                &old,
                run(&["a", "b", "c"]),
                keeps(&["a"]),
                Rc::from("another run"),
                &BTreeSet::new()
            )
            .is_err(),
            "a listing of another read is not the same listing"
        );
        assert!(
            Filtered::spliced(
                &old,
                run(&["a", "b", "d"]),
                keeps(&["a"]),
                shape,
                &BTreeSet::new()
            )
            .is_err(),
            "an entry no change names went"
        );
    }

    #[test]
    fn a_spliced_listing_keeps_the_entries_it_checked_before_the_run_moved() {
        let shape: Shape = Rc::from("the run of a column");
        let checks = Rc::new(Cell::new(0usize));
        let counting = {
            let checks = Rc::clone(&checks);
            Box::new(move |id: &str| {
                checks.set(checks.get().saturating_add(1));
                even(id)
            })
        };
        // A listing still reading its run: its first page is checked, the rest is not.
        let partial = Filtered::new(numbered(200), counting).shaped(shape.clone());
        assert_eq!(partial.get(0).as_deref(), Some("0000"));
        let checked = checks.get();
        assert!(checked > 0 && checked < 200, "a page of it is checked");
        let touched = touched(&["0150"]);
        let spliced = Filtered::spliced(&partial, numbered(200), Box::new(even), shape, &touched)
            .ok()
            .expect("the run is as it was");
        assert_eq!(
            checks.get(),
            checked,
            "the entries it checked are not read again"
        );
        assert_eq!(
            read_all(&spliced),
            read_all(&Filtered::new(numbered(200), Box::new(even)))
        );
    }
}
