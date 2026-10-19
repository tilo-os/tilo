//! Compiles a typed query into one tantivy query that finds and ranks.
//!
//! Each field and [`MatchTier`] is one gram phrase: the query as a
//! substring, after a word-start marker, between word markers, after the
//! value-start marker, or between the value markers. A record document
//! scores [`FIELD_BASE`] plus its rank, the best tier of its fields, through
//! a `DisjunctionMaxQuery` of `ConstScoreQuery`s. Body text matches below
//! [`FIELD_BASE`], ordered by BM25 over its words.

use tantivy::query::{
    BooleanQuery, BoostQuery, ConstScoreQuery, DisjunctionMaxQuery, Occur, PhrasePrefixQuery,
    PhraseQuery, Query, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption};
use tantivy::{Score, Term};

use crate::engine::Fields;
use crate::index::RECORD_SEGMENT;
use crate::tokenizer::{GRAM_WIDTH, VALUE_END, VALUE_START, WORD_END, WORD_START, marked, words};
use crate::typo::{Typos, Vocabulary};
use crate::weight::{FieldWeight, INDEXED_WEIGHTS, KEYWORD_SLOT, MatchTier, Ranks};

/// Score of a record whose fields match at rank 0; body-only matches score
/// below it.
pub(crate) const FIELD_BASE: Score = 1000.0;

/// Scale applied to body BM25, keeping it well below one rank step.
const BM25_SCALE: Score = 0.001;

/// Shortest word matched with one typo, and with two. Four letters take one:
/// "nots" is "notes" mistyped, and a fast typist drops a letter as often in a
/// short word as in a long one.
const ONE_TYPO_SCALARS: usize = 4;
const TWO_TYPOS_SCALARS: usize = 9;

type BoxQuery = Box<dyn Query>;

/// A query matching `sequence` as consecutive scalars of a gram field.
fn sequence_query(field: Field, sequence: &[char]) -> Option<BoxQuery> {
    let gram = |at: usize| -> Option<Term> {
        let window = sequence.get(at..at.checked_add(GRAM_WIDTH)?)?;
        Some(Term::from_field_text(
            field,
            &window.iter().collect::<String>(),
        ))
    };
    match sequence.len() {
        0 => None,
        // Every indexed scalar starts a full gram, so a short sequence is
        // the prefix of the gram at its position.
        length if length < GRAM_WIDTH => Some(prefix_query(Term::from_field_text(
            field,
            &sequence.iter().collect::<String>(),
        ))),
        GRAM_WIDTH => Some(Box::new(TermQuery::new(gram(0)?, IndexRecordOption::Basic))),
        length => {
            // Abutting grams, the last one overlapping, cover the sequence.
            let last = length.saturating_sub(GRAM_WIDTH);
            let mut terms = Vec::new();
            let mut at = 0;
            loop {
                terms.push((at, gram(at)?));
                if at == last {
                    break;
                }
                at = at.saturating_add(GRAM_WIDTH).min(last);
            }
            Some(Box::new(PhraseQuery::new_with_offset(terms)))
        }
    }
}

/// Every term starting with `prefix`, however many there are.
fn prefix_query(prefix: Term) -> BoxQuery {
    let mut query = PhrasePrefixQuery::new_with_offset(vec![(0, prefix)]);
    query.set_max_expansions(u32::MAX);
    Box::new(query)
}

/// Fewest letters a query needs before it matches inside a word. "not" is
/// not looking for "another", and "s" is not looking for every word with an
/// "s" in it; a word start is where a person's query begins until it is long
/// enough to be a fragment they remember.
pub const MIN_INFIX_SCALARS: usize = 4;

/// Whether `scalar` belongs to a script written without spaces between words
/// (Han, kana, Hangul, Thai and the like). There a word start is not marked
/// in the text, so a short query must still match inside a run.
fn unspaced_script(scalar: char) -> bool {
    matches!(u32::from(scalar),
        0x0E00..=0x0EFF // Thai, Lao
        | 0x1000..=0x109F // Myanmar
        | 0x1780..=0x17FF // Khmer
        | 0x2E80..=0x9FFF // CJK radicals, kana, Han
        | 0xA960..=0xA97F
        | 0xAC00..=0xD7FF // Hangul
        | 0xF900..=0xFAFF
        | 0xFF66..=0xFF9F // half-width kana
        | 0x20000..=0x3FFFF)
}

/// Whether the folded `query` may match inside a word: it is long enough to
/// be a remembered fragment, or written in a script without word spaces.
pub(crate) fn infix_allowed(query: &[char]) -> bool {
    let letters = query
        .iter()
        .filter(|scalar| scalar.is_alphanumeric())
        .count();
    letters >= MIN_INFIX_SCALARS || query.iter().copied().any(unspaced_script)
}

/// [`infix_allowed`] for typed text: whether `query` may match inside a word
/// ([`MatchTier::Contains`]) rather than only at word starts.
#[must_use]
pub fn matches_inside_words(query: &str) -> bool {
    let scalars: Vec<char> = query.chars().collect();
    infix_allowed(&scalars)
}

/// The marked sequence a field tier looks for, if the query can match it.
pub(crate) fn tier_sequence(tier: MatchTier, query: &[char]) -> Option<Vec<char>> {
    if tier == MatchTier::Contains && !infix_allowed(query) {
        return None;
    }
    let is_word = |scalar: Option<&char>| scalar.is_some_and(|scalar| scalar.is_alphanumeric());
    let starts_word = is_word(query.first());
    let ends_word = is_word(query.last());
    let (value_start, word_start, word_end, value_end) = match tier {
        MatchTier::Contains => (false, false, false, false),
        MatchTier::WordPrefix => (false, true, false, false),
        MatchTier::Word => (false, true, true, false),
        MatchTier::Prefix => (true, starts_word, false, false),
        MatchTier::PrefixWord => (true, starts_word, true, false),
        MatchTier::Exact => (true, starts_word, ends_word, true),
        MatchTier::Spanning | MatchTier::Typo => return None,
    };
    // A query opening with punctuation (".rs") carries its own word start:
    // the marked query holds one before its first word, so it matches from
    // that word's start without one in front of the punctuation.
    let opens_word = starts_word || query.iter().any(|scalar| scalar.is_alphanumeric());
    if (word_start && !opens_word) || (word_end && !ends_word) {
        return None;
    }
    let word_start = word_start && starts_word;
    let mut sequence = Vec::with_capacity(query.len().saturating_add(4));
    sequence.extend(value_start.then_some(VALUE_START));
    sequence.extend(word_start.then_some(WORD_START));
    sequence.extend_from_slice(query);
    sequence.extend(word_end.then_some(WORD_END));
    sequence.extend(value_end.then_some(VALUE_END));
    Some(sequence)
}

fn any_of(mut queries: Vec<BoxQuery>) -> Option<BoxQuery> {
    match queries.len() {
        0 => None,
        1 => queries.pop(),
        _ => Some(Box::new(BooleanQuery::new(
            queries
                .into_iter()
                .map(|query| (Occur::Should, query))
                .collect(),
        ))),
    }
}

fn all_of(queries: Vec<BoxQuery>) -> BoxQuery {
    Box::new(BooleanQuery::new(
        queries
            .into_iter()
            .map(|query| (Occur::Must, query))
            .collect(),
    ))
}

fn constant(query: BoxQuery, rank: i32) -> BoxQuery {
    #[allow(
        clippy::cast_precision_loss,
        reason = "ranks are small integers, exact in f32"
    )]
    let score = FIELD_BASE + rank as Score;
    Box::new(ConstScoreQuery::new(Box::new(Unscored(query)), score))
}

/// A query matched without scoring: its rank is the constant around it, so
/// the BM25 statistics its weight would gather are never read.
#[derive(Debug)]
struct Unscored(BoxQuery);

impl Clone for Unscored {
    fn clone(&self) -> Self {
        Self(self.0.box_clone())
    }
}

impl Query for Unscored {
    fn weight(
        &self,
        enable_scoring: tantivy::query::EnableScoring<'_>,
    ) -> tantivy::Result<Box<dyn tantivy::query::Weight>> {
        let unscored = match enable_scoring.searcher() {
            Some(searcher) => tantivy::query::EnableScoring::disabled_from_searcher(searcher),
            None => enable_scoring,
        };
        self.0.weight(unscored)
    }

    fn query_terms<'a>(&'a self, visitor: &mut dyn FnMut(&'a Term, bool)) {
        self.0.query_terms(visitor);
    }
}

/// A title word within typing distance of `word`; the last word typed may
/// still be incomplete, so it matches as a prefix.
/// `word` in the words of `field`: exactly (or, for the last word, as a
/// prefix) when it is short, otherwise within a typo or two of the words
/// `vocabulary` holds. `None` when no word of the field is that close.
fn fuzzy_word(field: Field, vocabulary: &Vocabulary, word: &str, last: bool) -> Option<BoxQuery> {
    let term = Term::from_field_text(field, word);
    let scalars: Vec<char> = word.chars().collect();
    match (typo_distance(&scalars), last) {
        (0, false) => Some(Box::new(TermQuery::new(term, IndexRecordOption::Basic))),
        (0, true) => Some(prefix_query(term)),
        (distance, prefix) => any_of(
            vocabulary
                .near(&scalars, distance, prefix)
                .map(|near| -> BoxQuery {
                    Box::new(TermQuery::new(
                        Term::from_field_text(field, near),
                        IndexRecordOption::Basic,
                    ))
                })
                .collect(),
        ),
    }
}

/// The typos a typed word of `scalars` may carry: none while it is short,
/// then one, then two. A number carries none: "2999" is not "2998" mistyped,
/// and every neighbouring number would match it.
pub(crate) fn typo_distance(scalars: &[char]) -> usize {
    if scalars.iter().any(|scalar| scalar.is_numeric()) {
        return 0;
    }
    usize::from(scalars.len() >= ONE_TYPO_SCALARS)
        .saturating_add(usize::from(scalars.len() >= TWO_TYPOS_SCALARS))
}

/// Which parts of the schema hold any text, so a query skips the rest.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Used {
    pub slots: [bool; INDEXED_WEIGHTS.len()],
    pub body: bool,
}

/// What besides the whole query as a substring counts as a match.
///
/// By default a hit holds the typed text, folded, in one field or its body.
/// Surfaces that rank a person's loosely remembered names (applications,
/// actions, windows) opt into the looser tiers, which rank below every
/// substring match.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Matching {
    /// [`MatchTier::Spanning`]: every whitespace-separated word of the query
    /// occurs in some field.
    pub spanning: bool,
    /// [`MatchTier::Typo`]: every word of the query matches a title word
    /// within a small edit distance, the last one as a prefix.
    pub typos: bool,
}

impl Matching {
    /// Substrings, words spread over fields, and typos in titles.
    pub const LOOSE: Self = Self {
        spanning: true,
        typos: true,
    };
}

/// Every word of the query within a small edit distance of a title's words,
/// and of a keyword's one rank lower: a typo in a name outranks the same typo
/// in a keyword, so "terminl" is the Terminal before it is a calendar's
/// "termin".
fn typo_queries(
    fields: &Fields,
    used: Used,
    ranks: Ranks,
    typos: &Typos,
    query_words: &[String],
) -> Vec<BoxQuery> {
    let Some(last) = query_words.len().checked_sub(1) else {
        return Vec::new();
    };
    [
        (0, fields.title_words, FieldWeight::TITLE, &typos.title),
        (
            KEYWORD_SLOT,
            fields.keyword_words,
            FieldWeight::KEYWORD,
            &typos.keyword,
        ),
    ]
    .into_iter()
    .filter(|(slot, ..)| used.slots.get(*slot).copied().unwrap_or(false))
    .filter_map(|(_, field, weight, vocabulary)| {
        let rank = ranks(MatchTier::Typo, weight)?;
        let every = query_words
            .iter()
            .enumerate()
            .map(|(at, word)| fuzzy_word(field, vocabulary, word, at == last))
            .collect::<Option<Vec<_>>>()?;
        Some(constant(all_of(every), rank))
    })
    .collect()
}

/// Every chunk of the query in some field, as a substring or by the typos of
/// its words in a title or keyword: spread words with a typo among them.
/// `None` when a chunk can match neither way.
fn spanning_typo_query(
    fields: &Fields,
    slots: &[usize],
    used: Used,
    typos: &Typos,
    chunks: &[&str],
    query_words: &[String],
) -> Option<BoxQuery> {
    let last = query_words.len().checked_sub(1)?;
    let vocabularies: Vec<(Field, &Vocabulary)> = [
        (0, fields.title_words, &typos.title),
        (KEYWORD_SLOT, fields.keyword_words, &typos.keyword),
    ]
    .into_iter()
    .filter(|(slot, ..)| used.slots.get(*slot).copied().unwrap_or(false))
    .map(|(_, field, vocabulary)| (field, vocabulary))
    .collect();
    let mut at = 0usize;
    let mut every = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let marked_chunk = marked(chunk, false);
        let mut either: Vec<BoxQuery> = slots
            .iter()
            .filter_map(|slot| sequence_query(*fields.grams.get(*slot)?, &marked_chunk))
            .collect();
        let held = words(chunk);
        let range = at..at.saturating_add(held.len());
        at = range.end;
        if !held.is_empty() {
            let by_typo: Option<Vec<BoxQuery>> = range
                .map(|word_at| {
                    let word = query_words.get(word_at)?;
                    any_of(
                        vocabularies
                            .iter()
                            .filter_map(|(field, vocabulary)| {
                                fuzzy_word(*field, vocabulary, word, word_at == last)
                            })
                            .collect(),
                    )
                })
                .collect();
            if let Some(by_typo) = by_typo {
                either.push(all_of(by_typo));
            }
        }
        every.push(any_of(either)?);
    }
    (at == query_words.len()).then(|| all_of(every))
}

/// The query for typed `text`, or `None` when nothing can match. An empty
/// query browses every record.
pub(crate) fn compile(
    fields: &Fields,
    used: Used,
    matching: Matching,
    ranks: Ranks,
    typos: &Typos,
    text: &str,
) -> Option<BoxQuery> {
    let query = marked(text, false);
    if query.is_empty() {
        return Some(constant(
            Box::new(TermQuery::new(
                Term::from_field_u64(fields.segment, RECORD_SEGMENT),
                IndexRecordOption::Basic,
            )),
            0,
        ));
    }
    let slots: Vec<usize> = used
        .slots
        .iter()
        .enumerate()
        .filter_map(|(slot, used)| used.then_some(slot))
        .collect();
    let mut ranked = Vec::new();
    for &slot in &slots {
        let (Some(field), Some(weight)) = (fields.grams.get(slot), INDEXED_WEIGHTS.get(slot))
        else {
            continue;
        };
        for tier in MatchTier::FIELD {
            let Some(rank) = ranks(tier, *weight) else {
                continue;
            };
            if let Some(found) =
                tier_sequence(tier, &query).and_then(|sequence| sequence_query(*field, &sequence))
            {
                ranked.push(constant(found, rank));
            }
        }
    }
    let chunks: Vec<&str> = text.split_whitespace().collect();
    if matching.spanning
        && chunks.len() > 1
        && let Some(rank) = ranks(MatchTier::Spanning, FieldWeight::UNRANKED)
    {
        let every: Option<Vec<BoxQuery>> = chunks
            .iter()
            .map(|chunk| {
                let chunk = marked(chunk, false);
                any_of(
                    slots
                        .iter()
                        .filter_map(|slot| sequence_query(*fields.grams.get(*slot)?, &chunk))
                        .collect(),
                )
            })
            .collect();
        if let Some(every) = every {
            ranked.push(constant(all_of(every), rank));
        }
    }
    let query_words = words(text);
    if matching.typos {
        ranked.extend(typo_queries(fields, used, ranks, typos, &query_words));
        if matching.spanning
            && chunks.len() > 1
            && let Some(found) =
                spanning_typo_query(fields, &slots, used, typos, &chunks, &query_words)
            && let Some(rank) = ranks(MatchTier::Typo, FieldWeight::KEYWORD)
        {
            ranked.push(constant(found, rank));
        }
    }
    let mut parts = Vec::new();
    if !ranked.is_empty() {
        parts.push(Box::new(DisjunctionMaxQuery::new(ranked)) as BoxQuery);
    }
    if used.body
        && let Some(found) = sequence_query(fields.body_grams, &query)
    {
        let found: BoxQuery = Box::new(ConstScoreQuery::new(found, 0.0));
        let order = any_of(
            query_words
                .iter()
                .map(|word| -> BoxQuery {
                    Box::new(TermQuery::new(
                        Term::from_field_text(fields.body_words, word),
                        IndexRecordOption::WithFreqs,
                    ))
                })
                .collect(),
        );
        parts.push(match order {
            Some(order) => Box::new(BooleanQuery::new(vec![
                (Occur::Must, found),
                (
                    Occur::Should,
                    Box::new(BoostQuery::new(order, BM25_SCALE)) as BoxQuery,
                ),
            ])),
            None => found,
        });
    }
    any_of(parts)
}
