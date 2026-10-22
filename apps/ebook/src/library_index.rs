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

use std::io::{Read as _, Write as _};

use crate::model::Book;

/// What a segment starts with, and the version of its layout.
const MAGIC: &[u8; 4] = b"TBX1";

/// The share of a book's paragraphs above which a word is common.
const COMMON_SHARE: f64 = 0.05;

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
            (paragraphs >= COMMON_FLOOR).then(|| (paragraphs as f64 * COMMON_SHARE) as usize);
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
            let middle = (low + high) / 2;
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
        let mut book = Book::default();
        book.id = "book:test".into();
        book.chapters = chapters
            .into_iter()
            .enumerate()
            .map(|(index, blocks)| Chapter {
                title: format!("Chapter {}", index + 1),
                blocks: blocks.into_iter().map(paragraph).collect(),
            })
            .collect();
        book
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
