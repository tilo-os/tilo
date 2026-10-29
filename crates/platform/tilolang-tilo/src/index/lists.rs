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
pub type PageWrite = ((String, u32), Option<PageBuf>);

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

/// A page being changed, as the bytes its entries are written in and where each starts: an
/// entry put in or taken out moves bytes, and costs the same few allocations however many
/// entries the page holds.
#[derive(Clone, Debug, Default)]
pub struct PageBuf {
    /// The entries, each its key and its id as [`Writer::str`] writes them.
    bytes: Vec<u8>,
    /// Where each entry starts in `bytes`.
    starts: Vec<usize>,
}

/// The text at `at` of `bytes` as [`Writer::str`] wrote it, and where it ends.
fn str_at(bytes: &[u8], at: usize) -> Option<(&str, usize)> {
    let len: [u8; 4] = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
    let len = usize::try_from(u32::from_le_bytes(len)).ok()?;
    let start = at.checked_add(4)?;
    let end = start.checked_add(len)?;
    // `PageBuf::decode` checks every entry before `get` reads one.
    // optional-decode: `None` is `Corrupt` to `PageBuf::decode`
    let text = std::str::from_utf8(bytes.get(start..end)?).ok()?;
    Some((text, end))
}

fn put_str(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&u32::try_from(text.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

impl PageBuf {
    /// The page the index wrote as `sealed`.
    ///
    /// # Errors
    /// The bytes are not a page the index wrote.
    pub fn decode(sealed: &[u8]) -> Result<Self, Corrupt> {
        let mut r = Reader::new(super::codec::open(sealed)?);
        let n = r.u32()?;
        let body = r.rest();
        let mut starts = Vec::with_capacity(usize::try_from(n).unwrap_or(0));
        let mut at = 0usize;
        for _ in 0..n {
            starts.push(at);
            let (_, key_end) = str_at(body, at).ok_or(Corrupt)?;
            let (_, id_end) = str_at(body, key_end).ok_or(Corrupt)?;
            at = id_end;
        }
        if at != body.len() {
            return Err(Corrupt);
        }
        Ok(Self {
            bytes: body.to_vec(),
            starts,
        })
    }

    /// A page of `entries`, in order.
    #[must_use]
    pub fn of(entries: &[Entry]) -> Self {
        let mut page = Self::default();
        for (key, id) in entries {
            page.starts.push(page.bytes.len());
            put_str(&mut page.bytes, key);
            put_str(&mut page.bytes, id);
        }
        page
    }

    /// The page as the index writes it.
    #[must_use]
    pub fn sealed(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bytes.len().saturating_add(8));
        out.extend_from_slice(
            &u32::try_from(self.starts.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(&self.bytes);
        super::codec::seal(out)
    }

    pub fn len(&self) -> usize {
        self.starts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.starts.is_empty()
    }

    /// The entry at `index`, borrowed.
    pub fn get(&self, index: usize) -> Option<(&str, &str)> {
        let at = *self.starts.get(index)?;
        let (key, key_end) = str_at(&self.bytes, at)?;
        let (id, _) = str_at(&self.bytes, key_end)?;
        Some((key, id))
    }

    /// The entry at `index`, owned.
    pub fn entry(&self, index: usize) -> Option<Entry> {
        self.get(index)
            .map(|(key, id)| (key.to_owned(), id.to_owned()))
    }

    /// Where `(key, id)` is, or where it would go.
    pub fn search(&self, key: &str, id: &str) -> Result<usize, usize> {
        let (mut lo, mut hi) = (0usize, self.len());
        while lo < hi {
            let mid = lo.midpoint(hi);
            match self.get(mid).map(|held| held.cmp(&(key, id))) {
                Some(std::cmp::Ordering::Less) => lo = mid.saturating_add(1),
                Some(std::cmp::Ordering::Equal) => return Ok(mid),
                _ => hi = mid,
            }
        }
        Err(lo)
    }

    /// Put `(key, id)` at `index`.
    pub fn insert(&mut self, index: usize, (key, id): &Entry) {
        let at = self.starts.get(index).copied().unwrap_or(self.bytes.len());
        let mut written = Vec::with_capacity(key.len().saturating_add(id.len()).saturating_add(8));
        put_str(&mut written, key);
        put_str(&mut written, id);
        let added = written.len();
        self.bytes.splice(at..at, written);
        for start in self.starts.iter_mut().skip(index) {
            *start = start.saturating_add(added);
        }
        self.starts.insert(index.min(self.starts.len()), at);
    }

    /// Take out the entry at `index`.
    pub fn remove(&mut self, index: usize) {
        let Some(&at) = self.starts.get(index) else {
            return;
        };
        let end = self
            .starts
            .get(index.saturating_add(1))
            .copied()
            .unwrap_or(self.bytes.len());
        self.bytes.drain(at..end);
        self.starts.remove(index);
        let removed = end.saturating_sub(at);
        for start in self.starts.iter_mut().skip(index) {
            *start = start.saturating_sub(removed);
        }
    }

    /// Split off the entries from `index` on, as a page of their own.
    #[must_use]
    pub fn split_off(&mut self, index: usize) -> Self {
        let Some(&at) = self.starts.get(index) else {
            return Self::default();
        };
        let bytes = self.bytes.split_off(at);
        let starts = self
            .starts
            .split_off(index)
            .into_iter()
            .map(|start| start.saturating_sub(at))
            .collect();
        Self { bytes, starts }
    }

    /// Its entries, owned.
    pub fn entries(&self) -> Vec<Entry> {
        (0..self.len())
            .filter_map(|index| self.entry(index))
            .collect()
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
/// where an entry is. Only the directory and the two pages read last are held: a list is read
/// where it ends as well as where its rows are shown, and holding one page alone would read
/// them in turn.
pub struct Pages<S: ListSource> {
    source: Arc<S>,
    list: String,
    dir: Dir,
    /// Per page, how many entries the pages up to and with it hold.
    ends: Vec<usize>,
    /// The page read last, then the one before.
    held: RefCell<[Option<Held>; 2]>,
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
            held: RefCell::new([None, None]),
            stats,
        })
    }

    pub fn len(&self) -> usize {
        self.ends.last().copied().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Run `f` over the page `index`, read if it is not one held.
    fn with_page<T>(&self, index: usize, f: impl FnOnce(&Held) -> T) -> Option<T> {
        let mut held = self.held.borrow_mut();
        let [last, before] = &mut *held;
        if before.as_ref().is_some_and(|page| page.index == index) {
            std::mem::swap(last, before);
        } else if last.as_ref().is_none_or(|page| page.index != index) {
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
            *before = last.replace(Held { index, ids, keys });
        }
        last.as_ref().map(f)
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
    pages: std::collections::HashMap<(String, u32), Option<PageBuf>>,
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

    /// The page `seq` of `list` as this batch leaves it, taken out of the batch to change.
    fn page(
        &mut self,
        source: &impl ListSource,
        list: &str,
        seq: u32,
    ) -> Result<PageBuf, super::IndexError> {
        match self.pages.remove(&(list.to_owned(), seq)) {
            Some(Some(page)) => Ok(page),
            Some(None) => Err(Corrupt.into()),
            None => {
                let bytes = source.page_bytes(list, seq)?.ok_or(Corrupt)?;
                Ok(PageBuf::decode(&bytes)?)
            }
        }
    }

    /// Every entry of the page `seq` of `list` as this batch leaves it.
    fn page_entries(
        &self,
        source: &impl ListSource,
        list: &str,
        seq: u32,
    ) -> Result<Vec<Entry>, super::IndexError> {
        match self.pages.get(&(list.to_owned(), seq)) {
            Some(Some(page)) => Ok(page.entries()),
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
            out.extend(self.page_entries(source, list, page.seq)?);
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
            self.pages
                .insert((list.to_owned(), seq), Some(PageBuf::of(&[entry])));
            self.dirs.insert(list.to_owned(), dir);
            self.dirty.insert(list.to_owned());
            return Ok(());
        }
        let at = dir.page_for(&entry.0, &entry.1);
        let Some(page) = dir.pages.get(at).cloned() else {
            return Err(Corrupt.into());
        };
        let mut entries = self.page(source, list, page.seq)?;
        let Err(position) = entries.search(&entry.0, &entry.1) else {
            self.pages
                .insert((list.to_owned(), page.seq), Some(entries));
            return Ok(());
        };
        entries.insert(position, &entry);
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
            let Some(first) = rest.entry(0) else {
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
        if let Some(slot) = dir.pages.get_mut(at) {
            slot.count = u32::try_from(entries.len()).unwrap_or(u32::MAX);
            if position == 0
                && let Some(first) = entries.entry(0)
            {
                slot.first = first;
            }
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
        let Ok(position) = entries.search(&entry.0, &entry.1) else {
            self.pages
                .insert((list.to_owned(), page.seq), Some(entries));
            return Ok(());
        };
        entries.remove(position);
        if entries.is_empty() {
            dir.pages.remove(at);
            self.pages.insert((list.to_owned(), page.seq), None);
        } else {
            if let Some(slot) = dir.pages.get_mut(at) {
                slot.count = u32::try_from(entries.len()).unwrap_or(u32::MAX);
                if position == 0
                    && let Some(first) = entries.entry(0)
                {
                    slot.first = first;
                }
            }
            self.pages
                .insert((list.to_owned(), page.seq), Some(entries));
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
            self.pages
                .insert((list.to_owned(), seq), Some(PageBuf::of(&rest)));
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
