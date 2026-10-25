//! The library's full-text index: one segment per book, built off the UI
//! thread by the device that first has the book's text and kept with the
//! book's row in the library, so that every device finds a passage in every
//! book at once, before the book's content is on it. Opening a hit is what
//! fetches the book, at the hit's place.
//!
//! A segment lists the book's words, each with the paragraphs it is in:
//!
//! - words are folded as the search folds them ([`ui::search::normalize_search_text`]);
//! - a word in more than [`COMMON_SHARE`] of a long book's paragraphs is kept
//!   only by name, as found everywhere: such words cost the most and narrow
//!   nothing;
//! - the dictionary is sorted and front-coded in runs of [`RUN`] words, the
//!   first of each run written whole, so a word or a prefix is found by a
//!   binary search over the runs and a scan of one;
//! - each word's paragraphs are counted through the book (every chapter's
//!   paragraphs after the last's), delta-coded as varints.
//!
//! A segment travels deflated ([`Segment::encode`]) and is searched as it is
//! read back ([`Segment::decode`]), without expanding its dictionary.

use std::collections::{HashMap, HashSet};
use std::io::{Read as _, Write as _};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::model::{Book, LibraryEntry};
use ui::arriving::{Arrival, Arriving, Source};

/// What a segment starts with, and the version of its layout.
const MAGIC: &[u8; 4] = b"TBX1";

/// The share of a book's paragraphs above which a word is common.
const COMMON_SHARE: f64 = 0.2;

/// The fewest paragraphs a book has for any word to be common in it: in a
/// short book every word is listed.
const COMMON_FLOOR: usize = 200;

/// How many dictionary words share one run.
const RUN: usize = 16;

/// The most words one typed prefix stands for; beyond them it narrows too
/// little to be worth reading.
const MAX_PREFIX_WORDS: usize = 512;

/// The largest segment read back, deflated or not: far above any book's.
const MAX_SEGMENT_BYTES: usize = 32 * 1024 * 1024;

/// Where in a book a word was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Place {
    pub chapter: usize,
    pub block: usize,
}

/// One book's index, as it is searched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Segment {
    /// Where each chapter's paragraphs start in the count through the book.
    chapters: Vec<u32>,
    /// What each chapter is called, for a hit's row.
    titles: Vec<String>,
    /// Words found everywhere.
    common: Vec<String>,
    /// How many words the dictionary holds.
    words: usize,
    /// Each run's start in `dictionary` and in `postings`.
    runs: Vec<(u32, u32)>,
    dictionary: Vec<u8>,
    postings: Vec<u8>,
}

/// The words of `text`, folded, in order.
pub(crate) fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(ui::search::normalize_search_text)
        .filter(|word| !word.is_empty())
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Reads varints and byte strings from a slice, failing on anything short.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn varint(&mut self) -> Option<u64> {
        let mut value = 0_u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.bytes.get(self.at)?;
            self.at += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    fn count(&mut self) -> Option<usize> {
        usize::try_from(self.varint()?).ok()
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(len)?;
        let taken = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(taken)
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes: [u8; 4] = self.take(4)?.try_into().ok()?;
        Some(u32::from_le_bytes(bytes))
    }
}

/// One dictionary entry as a run is scanned.
struct Entry {
    word: Vec<u8>,
    /// Where its paragraphs start in the postings, and how many there are.
    postings: usize,
    count: usize,
}

impl Segment {
    /// How many bytes the segment holds as it is searched.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.chapters.len() * 4
            + self.titles.iter().map(String::len).sum::<usize>()
            + self.common.iter().map(String::len).sum::<usize>()
            + self.runs.len() * 8
            + self.dictionary.len()
            + self.postings.len()
    }

    /// The index of `book`'s text.
    pub(crate) fn build(book: &Book) -> Self {
        let mut chapters = Vec::with_capacity(book.chapters.len());
        let titles = book
            .chapters
            .iter()
            .enumerate()
            .map(|(index, chapter)| crate::search_provider::chapter_label(chapter, index))
            .collect();
        let mut found: std::collections::BTreeMap<String, Vec<u32>> =
            std::collections::BTreeMap::new();
        let mut paragraph = 0_u32;
        for chapter in &book.chapters {
            chapters.push(paragraph);
            for block in &chapter.blocks {
                for word in words(&block.plain_text()) {
                    let places = found.entry(word).or_default();
                    if places.last() != Some(&paragraph) {
                        places.push(paragraph);
                    }
                }
                paragraph = paragraph.saturating_add(1);
            }
        }
        let paragraphs = paragraph as usize;
        let common_above =
            (paragraphs >= COMMON_FLOOR).then_some((paragraphs as f64 * COMMON_SHARE) as usize);
        let mut common = Vec::new();
        let mut listed = Vec::new();
        for (word, places) in found {
            if common_above.is_some_and(|above| places.len() > above) {
                common.push(word);
            } else {
                listed.push((word, places));
            }
        }
        let mut dictionary = Vec::new();
        let mut postings = Vec::new();
        let mut runs = Vec::new();
        let mut previous: &[u8] = &[];
        for (index, (word, places)) in listed.iter().enumerate() {
            let bytes = word.as_bytes();
            let shared = if index % RUN == 0 {
                runs.push((
                    u32::try_from(dictionary.len()).unwrap_or(u32::MAX),
                    u32::try_from(postings.len()).unwrap_or(u32::MAX),
                ));
                0
            } else {
                bytes
                    .iter()
                    .zip(previous)
                    .take_while(|(a, b)| a == b)
                    .count()
            };
            let suffix = bytes.get(shared..).unwrap_or_default();
            put_varint(&mut dictionary, shared as u64);
            put_varint(&mut dictionary, suffix.len() as u64);
            dictionary.extend_from_slice(suffix);
            put_varint(&mut dictionary, places.len() as u64);
            let mut last = None;
            for &place in places {
                let delta = last.map_or(place, |last: u32| place - last - 1);
                put_varint(&mut postings, u64::from(delta));
                last = Some(place);
            }
            previous = bytes;
        }
        Self {
            chapters,
            titles,
            common,
            words: listed.len(),
            runs,
            dictionary,
            postings,
        }
    }

    /// The segment as it is kept and synced: its layout, deflated.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut raw = Vec::with_capacity(self.dictionary.len() + self.postings.len() + 64);
        raw.extend_from_slice(MAGIC);
        put_varint(&mut raw, self.chapters.len() as u64);
        for (start, title) in self.chapters.iter().zip(&self.titles) {
            put_varint(&mut raw, u64::from(*start));
            put_varint(&mut raw, title.len() as u64);
            raw.extend_from_slice(title.as_bytes());
        }
        put_varint(&mut raw, self.common.len() as u64);
        for word in &self.common {
            put_varint(&mut raw, word.len() as u64);
            raw.extend_from_slice(word.as_bytes());
        }
        put_varint(&mut raw, self.words as u64);
        put_varint(&mut raw, self.runs.len() as u64);
        for (dictionary, postings) in &self.runs {
            raw.extend_from_slice(&dictionary.to_le_bytes());
            raw.extend_from_slice(&postings.to_le_bytes());
        }
        put_varint(&mut raw, self.dictionary.len() as u64);
        raw.extend_from_slice(&self.dictionary);
        put_varint(&mut raw, self.postings.len() as u64);
        raw.extend_from_slice(&self.postings);
        let mut deflated =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        // Writing into a Vec cannot fail.
        let _ = deflated.write_all(&raw);
        deflated.finish().unwrap_or_default()
    }

    /// A segment read back from what [`Self::encode`] made; `None` for
    /// anything else.
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let mut raw = Vec::new();
        flate2::read::DeflateDecoder::new(bytes)
            .take(MAX_SEGMENT_BYTES as u64)
            .read_to_end(&mut raw)
            .ok()?;
        let mut reader = Reader::new(&raw);
        if reader.take(MAGIC.len())? != MAGIC {
            return None;
        }
        let chapter_count = reader.count()?;
        let mut chapters = Vec::with_capacity(chapter_count.min(4096));
        let mut titles = Vec::with_capacity(chapter_count.min(4096));
        for _ in 0..chapter_count {
            chapters.push(u32::try_from(reader.varint()?).ok()?);
            let len = reader.count()?;
            titles.push(String::from_utf8(reader.take(len)?.to_vec()).ok()?);
        }
        let common_count = reader.count()?;
        let mut common = Vec::with_capacity(common_count.min(4096));
        for _ in 0..common_count {
            let len = reader.count()?;
            common.push(String::from_utf8(reader.take(len)?.to_vec()).ok()?);
        }
        let words = reader.count()?;
        let run_count = reader.count()?;
        if run_count != words.div_ceil(RUN) {
            return None;
        }
        let mut runs = Vec::with_capacity(run_count);
        for _ in 0..run_count {
            runs.push((reader.u32()?, reader.u32()?));
        }
        let len = reader.count()?;
        let dictionary = reader.take(len)?.to_vec();
        let len = reader.count()?;
        let postings = reader.take(len)?.to_vec();
        Some(Self {
            chapters,
            titles,
            common,
            words,
            runs,
            dictionary,
            postings,
        })
    }

    /// The entries of run `run`, in order, until `stop` says no more.
    fn scan_run(&self, run: usize, stop: &mut dyn FnMut(&Entry) -> bool) -> bool {
        let Some(&(dictionary, postings)) = self.runs.get(run) else {
            return true;
        };
        let mut reader = Reader::new(self.dictionary.get(dictionary as usize..).unwrap_or(&[]));
        let mut postings = postings as usize;
        let mut word: Vec<u8> = Vec::new();
        let in_run = self.words.saturating_sub(run * RUN).min(RUN);
        for _ in 0..in_run {
            let (Some(shared), Some(len)) = (reader.count(), reader.count()) else {
                return true;
            };
            word.truncate(shared);
            let Some(suffix) = reader.take(len) else {
                return true;
            };
            word.extend_from_slice(suffix);
            let Some(count) = reader.count() else {
                return true;
            };
            let entry = Entry {
                word: word.clone(),
                postings,
                count,
            };
            if stop(&entry) {
                return true;
            }
            // Skip the entry's paragraphs to reach the next one's.
            let mut skip = Reader::new(self.postings.get(postings..).unwrap_or(&[]));
            for _ in 0..count {
                if skip.varint().is_none() {
                    return true;
                }
            }
            postings += skip.at;
        }
        false
    }

    /// The first word of run `run`, written whole.
    fn run_head(&self, run: usize) -> Option<&[u8]> {
        let &(dictionary, _) = self.runs.get(run)?;
        let mut reader = Reader::new(self.dictionary.get(dictionary as usize..)?);
        let _shared = reader.count()?;
        let len = reader.count()?;
        reader.take(len)
    }

    /// The run a word at or after `key` starts in.
    fn run_for(&self, key: &[u8]) -> usize {
        // The last run whose head is not after the key.
        let mut low = 0;
        let mut high = self.runs.len();
        while low < high {
            let middle = usize::midpoint(low, high);
            if self.run_head(middle).is_some_and(|head| head <= key) {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        low.saturating_sub(1)
    }

    /// The paragraphs of entries whose word `matches` says, from the run
    /// `key` would be in on, while `goes_on` holds.
    fn gather(
        &self,
        key: &[u8],
        matches: &dyn Fn(&[u8]) -> bool,
        goes_on: &dyn Fn(&[u8]) -> bool,
    ) -> Option<Vec<u32>> {
        let mut entries = Vec::new();
        let mut run = self.run_for(key);
        let mut over = false;
        while !over && run < self.runs.len() {
            over = self.scan_run(run, &mut |entry| {
                if entry.word.as_slice() < key {
                    return false;
                }
                if !goes_on(&entry.word) {
                    return true;
                }
                if matches(&entry.word) {
                    entries.push((entry.postings, entry.count));
                }
                false
            });
            run += 1;
            if entries.len() > MAX_PREFIX_WORDS {
                return None;
            }
        }
        let mut places = Vec::new();
        for (start, count) in entries {
            let mut reader = Reader::new(self.postings.get(start..).unwrap_or(&[]));
            let mut last: Option<u32> = None;
            for _ in 0..count {
                let delta = u32::try_from(reader.varint()?).ok()?;
                let place = last.map_or(delta, |last| last.saturating_add(delta).saturating_add(1));
                places.push(place);
                last = Some(place);
            }
        }
        places.sort_unstable();
        places.dedup();
        Some(places)
    }

    /// The paragraphs that hold every word of `query`, the last as typed so
    /// far (a prefix), at most `limit`, in reading order. Common words narrow
    /// nothing; a query of common words alone finds nothing here.
    pub(crate) fn find(&self, query: &str, limit: usize) -> Vec<Place> {
        let typed: Vec<String> = words(query).collect();
        let mut narrowed: Option<Vec<u32>> = None;
        for (index, word) in typed.iter().enumerate() {
            let prefix = index + 1 == typed.len();
            let common = if prefix {
                self.common
                    .iter()
                    .any(|common| common.starts_with(word.as_str()))
            } else {
                self.common.iter().any(|common| common == word)
            };
            if common {
                continue;
            }
            let key = word.as_bytes();
            let places = if prefix {
                self.gather(key, &|_| true, &|found| found.starts_with(key))
            } else {
                self.gather(key, &|found| found == key, &|found| found == key)
            };
            let Some(places) = places else {
                continue;
            };
            narrowed = Some(match narrowed {
                None => places,
                Some(before) => intersect(&before, &places),
            });
            if narrowed.as_ref().is_some_and(Vec::is_empty) {
                return Vec::new();
            }
        }
        narrowed
            .unwrap_or_default()
            .into_iter()
            .take(limit)
            .filter_map(|paragraph| self.place(paragraph))
            .collect()
    }

    /// What chapter `chapter` is called.
    pub(crate) fn chapter_title(&self, chapter: usize) -> Option<&str> {
        self.titles.get(chapter).map(String::as_str)
    }

    /// The chapter and block of `paragraph`, counted through the book.
    fn place(&self, paragraph: u32) -> Option<Place> {
        let chapter = self
            .chapters
            .partition_point(|start| *start <= paragraph)
            .checked_sub(1)?;
        let start = *self.chapters.get(chapter)?;
        Some(Place {
            chapter,
            block: paragraph.saturating_sub(start) as usize,
        })
    }
}

/// What reading content from a content store came to.
pub(crate) enum Read<T> {
    /// It was here, and this is what it read as.
    Here(T),
    /// It is not here yet, and is on its way from a device that has it.
    OnItsWay,
    /// It is not here, and no device that could hand it over is connected
    /// right now; it comes once one is.
    Waiting,
    /// Nothing this device can reach has it.
    Unavailable,
    /// It is here, and does not read as what its row says.
    Unreadable,
}

impl<T> Read<T> {
    /// What reading `hash` from `store` came to, `parse` making it a `T`.
    fn from(
        store: &tilo_content::ContentStore,
        hash: &str,
        parse: impl FnOnce(&[u8]) -> Option<T>,
    ) -> Self {
        match store.read(hash) {
            Some(bytes) => parse(&bytes).map_or(Self::Unreadable, Self::Here),
            None => match store.state(hash) {
                tilo_content::ContentState::Waiting => Self::Waiting,
                tilo_content::ContentState::Unavailable => Self::Unavailable,
                tilo_content::ContentState::Pending | tilo_content::ContentState::Resolved => {
                    Self::OnItsWay
                }
            },
        }
    }
}

/// A book being fetched for a hit opened in it while its text was elsewhere.
struct Fetch {
    entry: LibraryEntry,
    /// Its text, as a read ([`ui::arriving`]): on its way from the devices
    /// that could hand it over since it was last found on its way (where
    /// its card's loading bar starts), or why it cannot come now. It
    /// follows those devices as they come and go.
    text: Arriving<(), BookUnreachable>,
    /// Whether a read of it is under way.
    reading: bool,
}

/// Why the text of a book being fetched cannot come now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BookUnreachable {
    /// No device that could hand it over is connected right now.
    Waiting,
    /// Every connected device that could hold it said it does not.
    Unavailable,
}

/// Where a book's text comes from while it is not here: the workspace's
/// other devices, which no one names here.
fn elsewhere() -> Source {
    Source::peer("")
}

/// The library's index as this device holds it: segments built here,
/// waiting to be kept with their rows, and the segments of books whose text
/// is elsewhere, read back and ready to search.
pub(crate) struct LibraryIndex {
    background: ui::background::Background,
    wake: ui::WakeHandle,
    /// The content store segments and books are read from.
    store: Option<tilo_content::ContentStore>,
    /// How many times content had reached this process when what was not
    /// here was last read again; see
    /// [`ui::collaboration::record_content::arrivals`].
    looked: u64,
    /// Books whose segment is being built here.
    building: HashSet<String>,
    /// Books whose segment was built here and handed on to be kept: not
    /// built again while their row still names none, which is what a row
    /// shows while the room has not taken the index, or after it refused it.
    built_here: HashSet<String>,
    built_send: Sender<(String, Vec<u8>)>,
    built: Receiver<(String, Vec<u8>)>,
    /// Segments being read back, by the hash their row names.
    loading: HashSet<String>,
    /// Segments not on this device yet, read again as content arrives.
    waiting: HashSet<String>,
    loaded_send: Sender<(String, Read<Arc<Segment>>)>,
    loaded: Receiver<(String, Read<Arc<Segment>>)>,
    /// Segments read back, by the hash their row names; `None` for one that
    /// could not be read.
    segments: HashMap<String, Option<Arc<Segment>>>,
    fetching: Option<Fetch>,
    fetched_send: Sender<(String, Read<crate::content::LoadedBook>)>,
    fetched: Receiver<(String, Read<crate::content::LoadedBook>)>,
}

impl Default for LibraryIndex {
    fn default() -> Self {
        let (built_send, built) = channel();
        let (loaded_send, loaded) = channel();
        let (fetched_send, fetched) = channel();
        Self {
            background: ui::background::Background::current(),
            wake: ui::WakeHandle::new(),
            store: None,
            looked: ui::collaboration::record_content::arrivals(),
            building: HashSet::new(),
            built_here: HashSet::new(),
            built_send,
            built,
            loading: HashSet::new(),
            waiting: HashSet::new(),
            loaded_send,
            loaded,
            segments: HashMap::new(),
            fetching: None,
            fetched_send,
            fetched,
        }
    }
}

impl LibraryIndex {
    /// Run the work off the UI thread on `background` (a test's pumped
    /// executor) instead.
    #[must_use]
    pub(crate) fn with_background(mut self, background: ui::background::Background) -> Self {
        self.background = background;
        self
    }

    /// Wake `wake`'s window when a segment is built or read back.
    pub(crate) fn set_waker(&mut self, wake: ui::WakeHandle) {
        self.wake = wake;
    }

    /// Build, off the UI thread, the segment of every book here whose row
    /// has none: the book a person just added, one added before books were
    /// indexed, or one whose text has just arrived, by the first device
    /// that has its text.
    pub(crate) fn build_missing(&mut self, books: &[Arc<Book>], entries: &[LibraryEntry]) {
        let unindexed: HashSet<&str> = entries
            .iter()
            .filter(|entry| entry.index.is_empty())
            .map(|entry| entry.id.as_str())
            .collect();
        for book in books {
            if !unindexed.contains(book.id.as_str())
                || self.built_here.contains(&book.id)
                || !self.building.insert(book.id.clone())
            {
                continue;
            }
            let book = Arc::clone(book);
            let send = self.built_send.clone();
            let wake = self.wake.clone();
            let spawned = self.background.spawn("books-index-build", move || {
                let segment = Segment::build(&book).encode();
                let _ = send.send((book.id.clone(), segment));
                wake.wake();
            });
            // Where nothing runs work in the background the book stays
            // marked, so it is not tried again every frame.
            if spawned.is_err() {
                tracing::debug!(
                    "books: nothing runs work in the background; a book stays unindexed"
                );
            }
        }
    }

    /// The segments built since asked, each its book's id and its bytes, to
    /// keep with the book's row.
    pub(crate) fn take_built(&mut self) -> Vec<(String, Vec<u8>)> {
        let built: Vec<(String, Vec<u8>)> = self.built.try_iter().collect();
        for (book, _) in &built {
            self.building.remove(book);
            self.built_here.insert(book.clone());
        }
        built
    }

    /// Read back, off the UI thread, the segment of every row whose book is
    /// not here (`local` says which are), from `store`; one not on this
    /// device yet is asked for, and read again once content arrives.
    pub(crate) fn load_missing(
        &mut self,
        entries: &[LibraryEntry],
        local: &dyn Fn(&str) -> bool,
        store: Option<&tilo_content::ContentStore>,
    ) {
        self.store = store.cloned();
        for entry in entries {
            let Some(hash) = entry.index_hash() else {
                continue;
            };
            if local(&entry.id) || self.segments.contains_key(&hash) || self.waiting.contains(&hash)
            {
                continue;
            }
            self.read_segment(hash);
        }
    }

    fn read_segment(&mut self, hash: String) {
        let Some(store) = self.store.clone() else {
            return;
        };
        if !self.loading.insert(hash.clone()) {
            return;
        }
        let send = self.loaded_send.clone();
        let wake = self.wake.clone();
        let spawned = self.background.spawn("books-index-read", move || {
            let read = Read::from(&store, &hash, |bytes| Segment::decode(bytes).map(Arc::new));
            let _ = send.send((hash, read));
            wake.wake();
        });
        if spawned.is_err() {
            tracing::debug!("books: nothing runs work in the background; a segment stays unread");
        }
    }

    /// Read again what was not here, once content has reached this process
    /// since it was last looked for: never on a timer.
    pub(crate) fn read_arrived(&mut self) {
        let arrivals = ui::collaboration::record_content::arrivals();
        if arrivals == self.looked {
            return;
        }
        self.looked = arrivals;
        for hash in std::mem::take(&mut self.waiting) {
            self.read_segment(hash);
        }
        if self.fetching.as_ref().is_some_and(|fetch| !fetch.reading) {
            self.read_book();
        }
    }

    /// Whether a segment is being built here.
    #[cfg(test)]
    pub(crate) fn building(&self) -> bool {
        !self.building.is_empty()
    }

    /// Whether segments are still being read back.
    pub(crate) fn reading(&self) -> bool {
        !self.loading.is_empty()
    }

    /// Take in the segments read back since asked; whether any arrived.
    pub(crate) fn take_loaded(&mut self) -> bool {
        let mut arrived = false;
        for (hash, read) in self.loaded.try_iter() {
            self.loading.remove(&hash);
            match read {
                Read::Here(segment) => {
                    self.segments.insert(hash, Some(segment));
                    arrived = true;
                }
                Read::Unreadable => {
                    tracing::debug!(%hash, "books: a segment does not read back");
                    self.segments.insert(hash, None);
                }
                Read::OnItsWay | Read::Waiting | Read::Unavailable => {
                    self.waiting.insert(hash);
                }
            }
        }
        arrived
    }

    /// Fetch, off the UI thread, the text of `entry`'s book from the content
    /// store, for a book opened while its text was elsewhere. Whether it is
    /// on its way.
    pub(crate) fn fetch(
        &mut self,
        entry: &LibraryEntry,
        store: Option<&tilo_content::ContentStore>,
    ) -> bool {
        if store.is_none() || entry.content.is_empty() {
            return false;
        }
        self.store = store.cloned();
        if self
            .fetching
            .as_ref()
            .is_some_and(|fetch| fetch.entry.id == entry.id)
        {
            return true;
        }
        self.fetching = Some(Fetch {
            entry: entry.clone(),
            text: Arriving::pending(elsewhere()),
            reading: false,
        });
        self.read_book();
        true
    }

    fn read_book(&mut self) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let Some(fetch) = self.fetching.as_mut() else {
            return;
        };
        let entry = fetch.entry.clone();
        let send = self.fetched_send.clone();
        let wake = self.wake.clone();
        let spawned = self.background.spawn("books-fetch", move || {
            let read = match entry.content_hash() {
                Some(hash) => Read::from(&store, &hash, |_| {
                    let paths: HashMap<String, std::path::PathBuf> = HashMap::new();
                    crate::content::hydrate_library(
                        std::slice::from_ref(&entry),
                        &paths,
                        Some(&store),
                    )
                    .into_iter()
                    .next()
                }),
                // A row naming no content names nothing to fetch.
                None => Read::Unreadable,
            };
            let _ = send.send((entry.id, read));
            wake.wake();
        });
        fetch.reading = spawned.is_ok();
    }

    /// Since when the text of `book` has been on its way here from a
    /// connected device, while it is.
    pub(crate) fn arriving(&self, book: &str) -> Option<ui::time::Instant> {
        self.fetching
            .as_ref()
            .filter(|fetch| fetch.entry.id == book && fetch.text.with(Arrival::is_pending))
            .map(|fetch| fetch.text.began())
    }

    /// The book being fetched, and where its text stands.
    pub(crate) fn fetching(&self) -> Option<(&str, Arrival<(), BookUnreachable>)> {
        self.fetching
            .as_ref()
            .map(|fetch| (fetch.entry.id.as_str(), fetch.text.get()))
    }

    /// What the fetch of a book came to since asked: its id and the book,
    /// or why it cannot be read. Nothing while it is still to come: on its
    /// way, or waiting for a device that has it ([`Self::fetching`] says
    /// which).
    pub(crate) fn take_fetched(&mut self) -> Option<(String, Read<crate::content::LoadedBook>)> {
        let (id, read) = self.fetched.try_iter().last()?;
        let fetch = self.fetching.as_mut().filter(|fetch| fetch.entry.id == id);
        let standing = match read {
            Read::OnItsWay => Arrival::Pending { from: elsewhere() },
            Read::Waiting => Arrival::Failed(BookUnreachable::Waiting),
            Read::Unavailable => Arrival::Failed(BookUnreachable::Unavailable),
            Read::Here(_) | Read::Unreadable => {
                if fetch.is_some() {
                    self.fetching = None;
                }
                return Some((id, read));
            }
        };
        if let Some(fetch) = fetch {
            fetch.reading = false;
            // Found on its way again after it could not come, it is asked
            // anew: its bar starts over.
            if fetch.text.with(|now| *now != standing) {
                fetch.text.stand(standing);
            }
        }
        None
    }

    /// What searches the books of `entries` that `local` says are not here,
    /// through their segments.
    pub(crate) fn source(
        &self,
        entries: &[LibraryEntry],
        local: &dyn Fn(&str) -> bool,
    ) -> Arc<dyn ui::framework::views::LocalSource> {
        elsewhere_books(
            entries
                .iter()
                .filter(|entry| !local(&entry.id))
                .filter_map(|entry| {
                    let segment = self.segments.get(&entry.index_hash()?)?.clone()?;
                    let title = if entry.title.trim().is_empty() {
                        "Untitled".to_owned()
                    } else {
                        entry.title.clone()
                    };
                    Some((entry.id.clone(), title, entry.author.clone(), segment))
                }),
        )
    }
}

/// What searches `books` (each its id, title, author and segment) as books
/// whose text is elsewhere.
pub(crate) fn elsewhere_books(
    books: impl IntoIterator<Item = (String, String, String, Arc<Segment>)>,
) -> Arc<dyn ui::framework::views::LocalSource> {
    Arc::new(ElsewhereBooks(
        books
            .into_iter()
            .map(|(id, title, author, segment)| Elsewhere {
                id,
                title,
                author,
                segment,
            })
            .collect(),
    ))
}

/// A book whose text is elsewhere, searched through its segment.
struct Elsewhere {
    id: String,
    title: String,
    author: String,
    segment: Arc<Segment>,
}

/// The books whose text is elsewhere, as the query view's own index: each
/// hit a passage, named by its chapter and its book.
struct ElsewhereBooks(Vec<Elsewhere>);

/// The most passages one book adds to a query's results.
const PASSAGES_PER_BOOK: usize = 8;

impl ui::framework::views::LocalSource for ElsewhereBooks {
    fn search(
        &self,
        query: &ui::framework::views::QuerySpec,
        limit: usize,
    ) -> Vec<ui::framework::views::Item> {
        use crate::search_provider::{AUTHOR_FACET, BOOK_FACET};
        let mut found = Vec::new();
        for book in &self.0 {
            // A chosen book or author leaves the others out before their
            // segments are read.
            let left_out = query.chips.iter().any(|chip| {
                chip.value.as_deref().is_some_and(|value| {
                    (chip.key == BOOK_FACET && value != book.title)
                        || (chip.key == AUTHOR_FACET && value != book.author)
                })
            });
            if left_out {
                continue;
            }
            let room = limit.saturating_sub(found.len()).min(PASSAGES_PER_BOOK);
            if room == 0 {
                break;
            }
            for place in book.segment.find(&query.text, room) {
                let chapter = book
                    .segment
                    .chapter_title(place.chapter)
                    .unwrap_or_default()
                    .to_owned();
                let mut item = ui::framework::views::Item::new(
                    crate::app::PASSAGE,
                    crate::search_provider::BookSearch::location_anchor(
                        &book.id,
                        place.chapter,
                        place.block,
                    ),
                )
                .with("title", chapter)
                .with("place", book.title.clone())
                .with(BOOK_FACET, book.title.clone());
                if !book.author.is_empty() {
                    item = item.with(AUTHOR_FACET, book.author.clone());
                }
                found.push(item);
            }
        }
        found
    }
}

fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while let (Some(x), Some(y)) = (a.get(i), b.get(j)) {
        match x.cmp(y) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(*x);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Block, Chapter, Span};

    fn paragraph(text: &str) -> Block {
        Block::Paragraph {
            spans: vec![Span::plain(text)],
        }
    }

    fn book(chapters: Vec<Vec<&str>>) -> Book {
        Book {
            id: "book:test".into(),
            chapters: chapters
                .into_iter()
                .enumerate()
                .map(|(index, blocks)| Chapter {
                    title: format!("Chapter {}", index + 1),
                    blocks: blocks.into_iter().map(paragraph).collect(),
                })
                .collect(),
            ..Book::default()
        }
    }

    #[test]
    fn a_book_whose_index_the_room_did_not_take_is_not_built_again() {
        let pumped = ui::background::Pumped::new();
        let mut index = LibraryIndex::default().with_background(pumped.background());
        let books = [Arc::new(book(vec![vec!["Call me Ishmael."]]))];
        let entries = [LibraryEntry {
            id: "book:test".into(),
            ..LibraryEntry::default()
        }];
        index.build_missing(&books, &entries);
        pumped.run_pending();
        assert_eq!(index.take_built().len(), 1, "built once");
        // The row still names no index: the room refused it, or has not
        // taken it yet. Building it again would only send it again.
        index.build_missing(&books, &entries);
        pumped.run_pending();
        assert!(index.take_built().is_empty(), "not built again");
    }

    #[test]
    fn a_segment_finds_the_paragraph_holding_every_word_the_last_as_a_prefix() {
        let book = book(vec![
            vec!["Call me Ishmael.", "Some years ago, never mind how long."],
            vec![
                "The whale surfaced beside the boat.",
                "Ishmael watched the whale go down.",
            ],
        ]);
        let segment = Segment::decode(&Segment::build(&book).encode()).expect("it reads back");
        assert_eq!(
            segment.find("ishmael whale", 10),
            [Place {
                chapter: 1,
                block: 1
            }]
        );
        assert_eq!(
            segment.find("ishm", 10),
            [
                Place {
                    chapter: 0,
                    block: 0
                },
                Place {
                    chapter: 1,
                    block: 1
                }
            ]
        );
        // Folded as the search folds: case does not matter.
        assert_eq!(segment.find("SURFACED", 10).len(), 1);
        assert!(segment.find("kraken", 10).is_empty());
        assert_eq!(segment.find("whale", 1).len(), 1, "at most the limit");
    }

    #[test]
    fn words_in_most_paragraphs_of_a_long_book_are_kept_by_name_alone() {
        let mut blocks = vec!["the tide came in"; COMMON_FLOOR];
        blocks.push("the lighthouse keeper");
        let book = book(vec![blocks]);
        let segment = Segment::build(&book);
        assert!(segment.common.iter().any(|word| word == "the"));
        // A common word narrows nothing; the rarer one finds the paragraph.
        assert_eq!(
            segment.find("the lighthouse", 10),
            [Place {
                chapter: 0,
                block: COMMON_FLOOR
            }]
        );
        assert!(segment.find("the", 10).is_empty(), "common words alone");
    }

    #[test]
    fn a_name_in_a_tenth_of_a_long_books_paragraphs_still_finds_them() {
        let mut blocks = vec!["the tide came in"; 270];
        blocks.extend(vec!["the prince and andrew spoke"; 30]);
        blocks.push("prince andrew looked away");
        let segment = Segment::build(&book(vec![blocks]));
        assert_eq!(segment.find("prince andrew", 100).len(), 31);
        assert_eq!(
            segment.find("prince andrew looked", 100),
            [Place {
                chapter: 0,
                block: 300
            }]
        );
    }

    #[test]
    fn many_words_and_runs_read_back_alike() {
        let texts: Vec<String> = (0..500)
            .map(|n| format!("word{n:04} filler{}", n % 7))
            .collect();
        let book = book(vec![texts.iter().map(String::as_str).collect()]);
        let built = Segment::build(&book);
        let segment = Segment::decode(&built.encode()).expect("it reads back");
        assert_eq!(segment, built);
        for n in [0, 15, 16, 17, 250, 499] {
            assert_eq!(
                segment.find(&format!("word{n:04}"), 10),
                [Place {
                    chapter: 0,
                    block: n
                }],
                "word{n:04}"
            );
        }
        assert_eq!(
            segment.find("word01", 1000).len(),
            100,
            "a prefix over runs"
        );
        assert!(Segment::decode(b"not a segment").is_none());
    }
}
