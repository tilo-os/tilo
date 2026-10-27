//! Lists on disk: a collection's items in order, or those of its items whose filter column
//! holds one value, as pages of at most [`SPLIT`] entries and a directory of the pages.
//!
//! An entry is an item's order key and id, and a list is ordered by them as the collection
//! is. The directory says how many entries each page holds and which comes first, so the
//! entry at an index, and where an entry is or would go, are found by reading the directory
//! and one page: a view showing the first rows of 100,000 items reads one page, and an item
//! added to the list rewrites one page and the directory.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use super::codec::{Corrupt, Reader, Writer};

/// A page is split in two once it holds more entries than this.
pub const SPLIT: usize = 512;

/// One entry: an item's order key and id.
pub type Entry = (String, String);

/// One page as the directory describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRef {
    /// The page's number in its list (pages are numbered as they are made, not in order).
    pub seq: u32,
    pub count: u32,
    /// Its first entry.
    pub first: Entry,
}

/// A list's directory: its pages in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dir {
    pub pages: Vec<PageRef>,
    /// The number the next page made gets.
    pub next: u32,
}

/// A page to write (`None`: it is gone), by list and number.
pub type PageWrite = ((String, u32), Option<Vec<Entry>>);

impl Dir {
    pub fn is_empty(&self) -> bool {
        self.pages.iter().all(|page| page.count == 0)
    }

    pub fn len(&self) -> usize {
        self.pages
            .iter()
            .map(|page| page.count as usize)
            .fold(0, usize::saturating_add)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(self.next);
        w.u32(u32::try_from(self.pages.len()).unwrap_or(u32::MAX));
        for page in &self.pages {
            w.u32(page.seq);
            w.u32(page.count);
            w.str(&page.first.0);
            w.str(&page.first.1);
        }
        w.sealed()
    }

    /// # Errors
    /// The bytes are not a directory the index wrote.
    pub fn decode(sealed: &[u8]) -> Result<Self, Corrupt> {
        let mut r = Reader::new(super::codec::open(sealed)?);
        let next = r.u32()?;
        let n = r.u32()?;
        let mut pages = Vec::new();
        for _ in 0..n {
            let seq = r.u32()?;
            let count = r.u32()?;
            let key = r.str()?.to_owned();
            let id = r.str()?.to_owned();
            pages.push(PageRef {
                seq,
                count,
                first: (key, id),
            });
        }
        if r.is_empty() {
            Ok(Self { pages, next })
        } else {
            Err(Corrupt)
        }
    }

    /// The page an entry `(key, id)` is in or goes in: the last whose first entry is not after
    /// it, or the first.
    pub fn page_for(&self, key: &str, id: &str) -> usize {
        self.pages
            .partition_point(|page| (page.first.0.as_str(), page.first.1.as_str()) <= (key, id))
            .saturating_sub(1)
    }
}

pub fn encode_page(entries: &[Entry]) -> Vec<u8> {
    let mut w = Writer::default();
    w.u32(u32::try_from(entries.len()).unwrap_or(u32::MAX));
    for (key, id) in entries {
        w.str(key);
        w.str(id);
    }
    w.sealed()
}

/// # Errors
/// The bytes are not a page the index wrote.
pub fn decode_page(sealed: &[u8]) -> Result<Vec<Entry>, Corrupt> {
    let mut r = Reader::new(super::codec::open(sealed)?);
    let n = r.u32()?;
    let mut entries = Vec::new();
    for _ in 0..n {
        let key = r.str()?.to_owned();
        let id = r.str()?.to_owned();
        entries.push((key, id));
    }
    if r.is_empty() {
        Ok(entries)
    } else {
        Err(Corrupt)
    }
}

/// Where a list's directory and pages are read from: an index as it was at one commit, or
/// the batch being written.
pub trait ListSource {
    /// The sealed directory of `list`, if it has one.
    ///
    /// # Errors
    /// The index cannot be read.
    fn dir_bytes(&self, list: &str) -> Result<Option<Vec<u8>>, super::IndexError>;

    /// The sealed page `seq` of `list`.
    ///
    /// # Errors
    /// The index cannot be read.
    fn page_bytes(&self, list: &str, seq: u32) -> Result<Option<Vec<u8>>, super::IndexError>;
}

/// Read the directory of `list`: empty when it has none.
///
/// # Errors
/// The index cannot be read, or what it holds is corrupt.
pub fn read_dir(source: &impl ListSource, list: &str) -> Result<Dir, super::IndexError> {
    match source.dir_bytes(list)? {
        Some(bytes) => Ok(Dir::decode(&bytes)?),
        None => Ok(Dir::default()),
    }
}

/// Read the page `seq` of `list`.
///
/// # Errors
/// The index cannot be read, or the page is missing or corrupt.
pub fn read_page(
    source: &impl ListSource,
    list: &str,
    seq: u32,
) -> Result<Vec<Entry>, super::IndexError> {
    let bytes = source.page_bytes(list, seq)?.ok_or(Corrupt)?;
    Ok(decode_page(&bytes)?)
}

/// One page decoded, as a paged listing keeps it.
struct Held {
    index: usize,
    ids: Vec<Rc<str>>,
    keys: Vec<Box<str>>,
}

/// A list as one snapshot of the index has it, read page by page: the entry at an index, and
/// where an entry is. Only the directory and the page read last are held.
pub struct Pages<S: ListSource> {
    source: Arc<S>,
    list: String,
    dir: Dir,
    /// Per page, how many entries the pages up to and with it hold.
    ends: Vec<usize>,
    held: RefCell<Option<Held>>,
    stats: Arc<super::IndexStats>,
}

impl<S: ListSource> std::fmt::Debug for Pages<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pages")
            .field("list", &self.list)
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

impl<S: ListSource> Pages<S> {
    /// The list `list` of `source`.
    ///
    /// # Errors
    /// The index cannot be read, or its directory is corrupt.
    pub fn open(
        source: Arc<S>,
        list: &str,
        stats: Arc<super::IndexStats>,
    ) -> Result<Self, super::IndexError> {
        let dir = read_dir(&*source, list)?;
        stats.count_dir();
        let mut ends = Vec::with_capacity(dir.pages.len());
        let mut total = 0usize;
        for page in &dir.pages {
            total = total.saturating_add(page.count as usize);
            ends.push(total);
        }
        Ok(Self {
            source,
            list: list.to_owned(),
            dir,
            ends,
            held: RefCell::new(None),
            stats,
        })
    }

    pub fn len(&self) -> usize {
        self.ends.last().copied().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Run `f` over the page `index`, read if it is not the one held.
    fn with_page<T>(&self, index: usize, f: impl FnOnce(&Held) -> T) -> Option<T> {
        let mut held = self.held.borrow_mut();
        if held.as_ref().is_none_or(|page| page.index != index) {
            let page = self.dir.pages.get(index)?;
            self.stats.count_page();
            let Ok(entries) = read_page(&*self.source, &self.list, page.seq) else {
                self.stats.corrupt();
                return None;
            };
            let (keys, ids) = entries
                .into_iter()
                .map(|(key, id)| (key.into_boxed_str(), Rc::<str>::from(id)))
                .unzip();
            *held = Some(Held { index, ids, keys });
        }
        held.as_ref().map(f)
    }

    /// The page holding the entry at `index`, and where in it the entry is.
    fn locate(&self, index: usize) -> Option<(usize, usize)> {
        let page = self.ends.partition_point(|end| *end <= index);
        let before = page
            .checked_sub(1)
            .and_then(|prev| self.ends.get(prev))
            .copied()
            .unwrap_or(0);
        (page < self.ends.len()).then(|| (page, index.saturating_sub(before)))
    }

    /// The id of the entry at `index`.
    pub fn get(&self, index: usize) -> Option<Rc<str>> {
        let (page, at) = self.locate(index)?;
        self.with_page(page, |held| held.ids.get(at).cloned())
            .flatten()
    }

    /// The entry at `index`.
    pub fn entry(&self, index: usize) -> Option<(String, Rc<str>)> {
        let (page, at) = self.locate(index)?;
        self.with_page(page, |held| {
            Some((held.keys.get(at)?.to_string(), held.ids.get(at)?.clone()))
        })
        .flatten()
    }

    /// Where the entry `(key, id)` is in the list, or would go, and whether it is there.
    pub fn rank(&self, key: &str, id: &str) -> (usize, bool) {
        if self.dir.pages.is_empty() {
            return (0, false);
        }
        let page = self.dir.page_for(key, id);
        let before = page
            .checked_sub(1)
            .and_then(|prev| self.ends.get(prev))
            .copied()
            .unwrap_or(0);
        self.with_page(page, |held| {
            let (mut lo, mut hi) = (0usize, held.ids.len().min(held.keys.len()));
            while lo < hi {
                let mid = lo.midpoint(hi);
                let before = held
                    .keys
                    .get(mid)
                    .zip(held.ids.get(mid))
                    .is_some_and(|(k, i)| (&**k, &**i) < (key, id));
                if before {
                    lo = mid.saturating_add(1);
                } else {
                    hi = mid;
                }
            }
            let at = lo;
            let found = held
                .keys
                .get(at)
                .zip(held.ids.get(at))
                .is_some_and(|(k, i)| &**k == key && &**i == id);
            (before.saturating_add(at), found)
        })
        .unwrap_or((before, false))
    }
}

/// Changes to lists in one batch: directories and pages read once, changed in memory and
/// written when the batch ends.
#[derive(Default)]
pub struct ListEdits {
    dirs: std::collections::HashMap<String, Dir>,
    /// The lists whose directory changed.
    dirty: std::collections::HashSet<String>,
    /// Pages changed (`None`: dropped).
    pages: std::collections::HashMap<(String, u32), Option<Vec<Entry>>>,
}

impl ListEdits {
    fn dir<'a>(
        &'a mut self,
        source: &impl ListSource,
        list: &str,
    ) -> Result<&'a mut Dir, super::IndexError> {
        if !self.dirs.contains_key(list) {
            let dir = read_dir(source, list)?;
            self.dirs.insert(list.to_owned(), dir);
        }
        self.dirs.get_mut(list).ok_or_else(|| Corrupt.into())
    }

    fn page(
        &self,
        source: &impl ListSource,
        list: &str,
        seq: u32,
    ) -> Result<Vec<Entry>, super::IndexError> {
        match self.pages.get(&(list.to_owned(), seq)) {
            Some(Some(entries)) => Ok(entries.clone()),
            Some(None) => Err(Corrupt.into()),
            None => read_page(source, list, seq),
        }
    }

    /// The directory of `list` as this batch leaves it.
    ///
    /// # Errors
    /// The index cannot be read, or its directory is corrupt.
    pub fn read(&mut self, source: &impl ListSource, list: &str) -> Result<Dir, super::IndexError> {
        self.dir(source, list).cloned()
    }

    /// Every entry of `list`, as this batch leaves it.
    ///
    /// # Errors
    /// The index cannot be read, or what it holds is corrupt.
    pub fn entries(
        &mut self,
        source: &impl ListSource,
        list: &str,
    ) -> Result<Vec<Entry>, super::IndexError> {
        let dir = self.dir(source, list)?.clone();
        let mut out = Vec::with_capacity(dir.len());
        for page in &dir.pages {
            out.extend(self.page(source, list, page.seq)?);
        }
        Ok(out)
    }

    /// Put `(key, id)` in `list` where its order puts it; nothing when it is there.
    ///
    /// # Errors
    /// The index cannot be read, or what it holds is corrupt.
    pub fn insert(
        &mut self,
        source: &impl ListSource,
        list: &str,
        entry: Entry,
    ) -> Result<(), super::IndexError> {
        let dir = self.dir(source, list)?.clone();
        if dir.pages.is_empty() {
            let mut dir = dir;
            let seq = dir.next;
            dir.next = dir.next.saturating_add(1);
            dir.pages.push(PageRef {
                seq,
                count: 1,
                first: entry.clone(),
            });
            self.pages.insert((list.to_owned(), seq), Some(vec![entry]));
            self.dirs.insert(list.to_owned(), dir);
            self.dirty.insert(list.to_owned());
            return Ok(());
        }
        let at = dir.page_for(&entry.0, &entry.1);
        let Some(page) = dir.pages.get(at).cloned() else {
            return Err(Corrupt.into());
        };
        let mut entries = self.page(source, list, page.seq)?;
        let position = entries.partition_point(|held| *held < entry);
        if entries.get(position) == Some(&entry) {
            return Ok(());
        }
        entries.insert(position, entry);
        let mut dir = dir;
        if entries.len() > SPLIT {
            // Appended past the end of the list (a message posted, a list built in order): the
            // full page stays full and the entry starts the next, so a list kept by appending
            // is held in full pages. Anywhere else the page splits in half, room on both sides.
            let appended = at.saturating_add(1) == dir.pages.len()
                && position.saturating_add(1) == entries.len();
            let keep = if appended { SPLIT } else { entries.len() / 2 };
            let rest = entries.split_off(keep);
            let seq = dir.next;
            dir.next = dir.next.saturating_add(1);
            let Some(first) = rest.first().cloned() else {
                return Err(Corrupt.into());
            };
            let made = PageRef {
                seq,
                count: u32::try_from(rest.len()).unwrap_or(u32::MAX),
                first,
            };
            self.pages.insert((list.to_owned(), seq), Some(rest));
            dir.pages.insert(at.saturating_add(1), made);
        }
        if let (Some(slot), Some(first)) = (dir.pages.get_mut(at), entries.first()) {
            slot.count = u32::try_from(entries.len()).unwrap_or(u32::MAX);
            slot.first = first.clone();
        }
        self.pages
            .insert((list.to_owned(), page.seq), Some(entries));
        self.dirs.insert(list.to_owned(), dir);
        self.dirty.insert(list.to_owned());
        Ok(())
    }

    /// Take `(key, id)` out of `list`; nothing when it is not there.
    ///
    /// # Errors
    /// The index cannot be read, or what it holds is corrupt.
    pub fn remove(
        &mut self,
        source: &impl ListSource,
        list: &str,
        entry: &Entry,
    ) -> Result<(), super::IndexError> {
        let mut dir = self.dir(source, list)?.clone();
        if dir.pages.is_empty() {
            return Ok(());
        }
        let at = dir.page_for(&entry.0, &entry.1);
        let Some(page) = dir.pages.get(at).cloned() else {
            return Err(Corrupt.into());
        };
        let mut entries = self.page(source, list, page.seq)?;
        let Ok(position) = entries.binary_search(entry) else {
            return Ok(());
        };
        entries.remove(position);
        if let Some(first) = entries.first().cloned() {
            if let Some(slot) = dir.pages.get_mut(at) {
                slot.count = u32::try_from(entries.len()).unwrap_or(u32::MAX);
                slot.first = first;
            }
            self.pages
                .insert((list.to_owned(), page.seq), Some(entries));
        } else {
            dir.pages.remove(at);
            self.pages.insert((list.to_owned(), page.seq), None);
        }
        self.dirs.insert(list.to_owned(), dir);
        self.dirty.insert(list.to_owned());
        Ok(())
    }

    /// Make `entries` (in order) the whole of `list`, dropping what it held: in full pages, as
    /// appending them one by one leaves them ([`Self::insert`]).
    ///
    /// # Errors
    /// The index cannot be read, or what it holds is corrupt.
    pub fn replace(
        &mut self,
        source: &impl ListSource,
        list: &str,
        entries: Vec<Entry>,
    ) -> Result<(), super::IndexError> {
        self.drop_list(source, list)?;
        let mut dir = Dir::default();
        let mut rest = entries;
        while !rest.is_empty() {
            let tail = rest.split_off(rest.len().min(SPLIT));
            let seq = dir.next;
            dir.next = dir.next.saturating_add(1);
            let Some(first) = rest.first().cloned() else {
                break;
            };
            dir.pages.push(PageRef {
                seq,
                count: u32::try_from(rest.len()).unwrap_or(u32::MAX),
                first,
            });
            self.pages.insert((list.to_owned(), seq), Some(rest));
            rest = tail;
        }
        self.dirs.insert(list.to_owned(), dir);
        self.dirty.insert(list.to_owned());
        Ok(())
    }

    /// Drop `list` and its pages.
    ///
    /// # Errors
    /// The index cannot be read, or its directory is corrupt.
    pub fn drop_list(
        &mut self,
        source: &impl ListSource,
        list: &str,
    ) -> Result<(), super::IndexError> {
        let dir = self.dir(source, list)?.clone();
        for page in dir.pages {
            self.pages.insert((list.to_owned(), page.seq), None);
        }
        // An empty directory is written as none.
        self.dirs.insert(list.to_owned(), Dir::default());
        self.dirty.insert(list.to_owned());
        Ok(())
    }

    /// The lists this batch holds the directory of whose names start with `prefix`, and
    /// whether each is empty.
    pub fn held_from(&self, prefix: &str) -> Vec<(String, bool)> {
        self.dirs
            .iter()
            .filter(|(name, _)| name.starts_with(prefix))
            .map(|(name, dir)| (name.clone(), dir.pages.is_empty()))
            .collect()
    }

    /// What to write: each directory changed (`None`: the list is gone) and each page.
    pub fn finish(self) -> (Vec<(String, Option<Dir>)>, Vec<PageWrite>) {
        let dirty = self.dirty;
        let dirs = self
            .dirs
            .into_iter()
            .filter(|(list, _)| dirty.contains(list))
            .map(|(list, dir)| {
                let dir = (!dir.pages.is_empty()).then_some(dir);
                (list, dir)
            })
            .collect();
        (dirs, self.pages.into_iter().collect())
    }
}
