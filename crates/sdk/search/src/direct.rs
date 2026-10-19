//! Answering a small index without tantivy.
//!
//! A palette list (the shell's actions, its session rows, Settings' pages,
//! an app list) holds tens to hundreds of short records and is searched on
//! every keystroke. tantivy answers it correctly but spends some fifty
//! microseconds per search before it reads a posting: dozens of tier
//! queries to weigh, term dictionaries to look up, columns to open. Five
//! such lists are a quarter of a keystroke's budget.
//!
//! [`Direct`] answers the same query over the same records by reading
//! them: the marked value of each field (what [`GramTokenizer`] indexes the
//! grams of) holds a tier's sequence exactly when tantivy's phrase of grams
//! matches it, and the words of titles and keywords are compared with the
//! typed words as the typo tier does. It ranks and orders as the tantivy
//! path does; `direct_and_tantivy_answer_alike` in the tests holds the two
//! to that.
//!
//! [`GramTokenizer`]: crate::GramTokenizer

use std::collections::{BTreeMap, HashMap};

use crate::index::{Document, Hit, Hits, Page, Signals};
use crate::query::{Matching, tier_sequence, typo_distance};
use crate::tokenizer::{marked, words};
use crate::typo::within;
use crate::weight::{FieldWeight, INDEXED_WEIGHTS, KEYWORD_SLOT, MatchTier, Ranks};

/// Most records an index answers directly; a larger one is searched through
/// tantivy, whose postings outpace a scan.
pub(crate) const DIRECT_LIMIT: usize = 1024;

/// One record as [`Direct`] reads it.
#[derive(Debug)]
struct Record {
    key: u64,
    signals: (i32, u32),
    /// Every field's marked value, joined: a query none of them holds is
    /// not in any of them.
    joined: String,
    /// Each field's slot and marked value.
    fields: Vec<(usize, String)>,
    /// The words of its title fields and of its keyword fields, as ids into
    /// [`Direct::words`], sorted.
    title_words: Vec<u32>,
    keyword_words: Vec<u32>,
}

/// The records of an index, read directly; see the module.
#[derive(Debug, Default)]
pub(crate) struct Direct {
    records: Vec<Record>,
    /// Every word of a title or keyword field, by id.
    words: Vec<(String, Box<[char]>)>,
    ids: HashMap<String, u32>,
}

impl Direct {
    /// The records of `documents`, in key order. Typo words are kept only
    /// when `typos` are matched.
    pub(crate) fn new(documents: &BTreeMap<u64, Document>, typos: bool) -> Self {
        let mut direct = Self::default();
        for document in documents.values() {
            let mut fields = Vec::with_capacity(document.fields().len());
            let mut joined = String::new();
            let mut title_words = Vec::new();
            let mut keyword_words = Vec::new();
            for (text, weight) in document.fields() {
                let slot = weight.slot();
                let value: String = marked(text, true).into_iter().collect();
                joined.push_str(&value);
                joined.push('\u{0}');
                fields.push((slot, value));
                if typos {
                    let list = match slot {
                        0 => &mut title_words,
                        KEYWORD_SLOT => &mut keyword_words,
                        _ => continue,
                    };
                    for word in words(text) {
                        list.push(direct.id(word));
                    }
                }
            }
            for list in [&mut title_words, &mut keyword_words] {
                list.sort_unstable();
                list.dedup();
            }
            direct.records.push(Record {
                key: document.key(),
                signals: document.signals_pair(),
                joined,
                fields,
                title_words,
                keyword_words,
            });
        }
        direct
    }

    fn id(&mut self, word: String) -> u32 {
        if let Some(&id) = self.ids.get(&word) {
            return id;
        }
        let id = u32::try_from(self.words.len()).unwrap_or(u32::MAX);
        let scalars = word.chars().collect();
        self.words.push((word.clone(), scalars));
        self.ids.insert(word, id);
        id
    }

    /// For each typed word, which words of the index it matches as the
    /// typo tier matches a word: exactly, or as a prefix when it is the last
    /// one, while it is too short for typos; otherwise within its typos.
    fn typo_words(&self, typed: &[String]) -> Vec<Vec<bool>> {
        let last = typed.len().saturating_sub(1);
        typed
            .iter()
            .enumerate()
            .map(|(at, word)| {
                let scalars: Vec<char> = word.chars().collect();
                let distance = typo_distance(scalars.len());
                let prefix = at == last;
                self.words
                    .iter()
                    .map(|(text, held)| match (distance, prefix) {
                        (0, false) => text == word,
                        (0, true) => text.starts_with(word.as_str()),
                        (distance, prefix) => within(&scalars, held, distance, prefix),
                    })
                    .collect()
            })
            .collect()
    }

    /// The page of `query`'s hits, ordered by `score` of their
    /// [`Signals`], then rank, then key: what the tantivy path answers.
    pub(crate) fn search(
        &self,
        matching: Matching,
        ranks: Ranks,
        query: &str,
        page: Page,
        score: &dyn Fn(Signals) -> i32,
    ) -> Hits {
        let limit = page
            .limit
            .min(self.records.len().saturating_sub(page.offset));
        if limit == 0 {
            return Hits::default();
        }
        let mut found: Vec<(i32, i32, u64)> = match Typed::new(self, matching, ranks, query) {
            // An empty query browses every record.
            None => self
                .records
                .iter()
                .map(|record| ordered(record, 0, score))
                .collect(),
            Some(typed) => self
                .records
                .iter()
                .filter_map(|record| Some(ordered(record, typed.rank(record)?, score)))
                .collect(),
        };
        // Best first: the score, then the rank, then the lower key.
        found.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
        let more = found.len() > page.offset.saturating_add(limit);
        let hits = found
            .into_iter()
            .skip(page.offset)
            .take(limit)
            .map(|(score, rank, key)| Hit {
                key,
                segment: None,
                rank: Some(rank),
                score,
            })
            .collect();
        Hits { hits, more }
    }
}

/// One whitespace-separated chunk of a query, marked, and the range of the
/// query's words it holds.
type Chunk = (String, std::ops::Range<usize>);

/// A non-empty query, prepared once for every record: the tiers' marked
/// sequences, the words spread over fields, and which index words each typed
/// word matches, each with the rank it earns.
struct Typed<'a> {
    ranks: Ranks,
    needle: String,
    tiers: Vec<(MatchTier, String)>,
    /// The marked chunks every one of which some field must hold, and their
    /// rank; only for a query of several chunks.
    spanning: Option<(Vec<String>, i32)>,
    /// The query's chunks again, each with the range of typed words it
    /// holds, and the rank of every chunk matching either as a substring or
    /// by the typos of its words: "nots neu" is still the new-note action.
    spanning_typos: Option<(Vec<Chunk>, i32)>,
    /// For each typed word, the index words it matches.
    typo_words: Vec<Vec<bool>>,
    title_typo: Option<i32>,
    keyword_typo: Option<i32>,
    _direct: std::marker::PhantomData<&'a Direct>,
}

impl<'a> Typed<'a> {
    /// `None` for a query with nothing to find, which browses every record.
    fn new(direct: &'a Direct, matching: Matching, ranks: Ranks, query: &str) -> Option<Self> {
        let marked_query: Vec<char> = marked(query, false);
        if marked_query.is_empty() {
            return None;
        }
        let tiers = MatchTier::FIELD
            .into_iter()
            .filter_map(|tier| {
                let sequence = tier_sequence(tier, &marked_query)?;
                Some((tier, sequence.into_iter().collect()))
            })
            .collect();
        let chunks: Vec<&str> = query.split_whitespace().collect();
        let spanning = (matching.spanning && chunks.len() > 1)
            .then(|| {
                chunks
                    .iter()
                    .map(|chunk| {
                        let chunk: String = marked(chunk, false).into_iter().collect();
                        (!chunk.is_empty()).then_some(chunk)
                    })
                    .collect::<Option<Vec<_>>>()
            })
            .flatten()
            .and_then(|chunks| Some((chunks, ranks(MatchTier::Spanning, FieldWeight::UNRANKED)?)));
        let typed = if matching.typos {
            words(query)
        } else {
            Vec::new()
        };
        let spanning_typos = (matching.spanning && matching.typos && chunks.len() > 1)
            .then(|| {
                let mut at = 0usize;
                chunks
                    .iter()
                    .map(|chunk| {
                        let held = words(chunk).len();
                        let range = at..at.saturating_add(held);
                        at = range.end;
                        let marked: String = marked(chunk, false).into_iter().collect();
                        (!marked.is_empty()).then_some((marked, range))
                    })
                    .collect::<Option<Vec<_>>>()
                    // Chunks and words split alike, or the ranges are wrong.
                    .filter(|_| at == typed.len())
            })
            .flatten()
            .and_then(|chunks| Some((chunks, ranks(MatchTier::Typo, FieldWeight::KEYWORD)?)));
        Some(Self {
            ranks,
            needle: marked_query.into_iter().collect(),
            tiers,
            spanning,
            spanning_typos,
            typo_words: if typed.is_empty() {
                Vec::new()
            } else {
                direct.typo_words(&typed)
            },
            title_typo: ranks(MatchTier::Typo, FieldWeight::TITLE),
            keyword_typo: ranks(MatchTier::Typo, FieldWeight::KEYWORD),
            _direct: std::marker::PhantomData,
        })
    }

    /// The best rank `record` earns, if it matches.
    fn rank(&self, record: &Record) -> Option<i32> {
        let mut best: Option<i32> = None;
        let mut better = |rank: i32| best = Some(best.map_or(rank, |held| held.max(rank)));
        // Every tier's sequence holds the query, so a record without it
        // matches no tier.
        if record.joined.contains(self.needle.as_str()) {
            for (slot, value) in &record.fields {
                let Some(weight) = INDEXED_WEIGHTS.get(*slot) else {
                    continue;
                };
                if !value.contains(self.needle.as_str()) {
                    continue;
                }
                for (tier, sequence) in &self.tiers {
                    if let Some(rank) = (self.ranks)(*tier, *weight)
                        && value.contains(sequence.as_str())
                    {
                        better(rank);
                    }
                }
            }
        }
        if let Some((chunks, rank)) = &self.spanning
            && chunks.iter().all(|chunk| {
                record
                    .fields
                    .iter()
                    .any(|(_, value)| value.contains(chunk.as_str()))
            })
        {
            better(*rank);
        }
        if let Some((chunks, rank)) = &self.spanning_typos
            && chunks.iter().all(|(chunk, typed)| {
                record
                    .fields
                    .iter()
                    .any(|(_, value)| value.contains(chunk.as_str()))
                    || (!typed.is_empty()
                        && typed.clone().all(|word| {
                            self.typo_words.get(word).is_some_and(|matches| {
                                [&record.title_words, &record.keyword_words]
                                    .into_iter()
                                    .flatten()
                                    .any(|&id| word_matches(matches, id))
                            })
                        }))
            })
        {
            better(*rank);
        }
        if !self.typo_words.is_empty() {
            for (held, rank) in [
                (&record.title_words, self.title_typo),
                (&record.keyword_words, self.keyword_typo),
            ] {
                if let Some(rank) = rank
                    && self
                        .typo_words
                        .iter()
                        .all(|matches| held.iter().any(|&id| word_matches(matches, id)))
                {
                    better(rank);
                }
            }
        }
        best
    }
}

/// Whether the index word `id` is among a typed word's `matches`.
fn word_matches(matches: &[bool], id: u32) -> bool {
    usize::try_from(id)
        .ok()
        .and_then(|id| matches.get(id))
        .copied()
        .unwrap_or(false)
}

/// The ordering of `record` hit at `rank`.
fn ordered(record: &Record, rank: i32, score: &dyn Fn(Signals) -> i32) -> (i32, i32, u64) {
    let signals = Signals {
        rank: Some(rank),
        boost: record.signals.0,
        length: record.signals.1,
    };
    (score(signals), rank, record.key)
}
