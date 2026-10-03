//! Compiles a typed query into one tantivy query that finds and ranks.
//!
//! Each field and [`MatchTier`] is one gram phrase: the query as a
//! substring, after a word-start marker, between word markers, after the
//! value-start marker, or between the value markers. A record document
//! scores [`FIELD_BASE`] plus its rank, the best tier of its fields, through
//! a `DisjunctionMaxQuery` of `ConstScoreQuery`s. Body text matches below
//! [`FIELD_BASE`], ordered by BM25 over its words.

use tantivy::query::{
    BooleanQuery, BoostQuery, ConstScoreQuery, DisjunctionMaxQuery, FuzzyTermQuery, Occur,
    PhrasePrefixQuery, PhraseQuery, Query, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption};
use tantivy::{Score, Term};

use crate::index::{Fields, RECORD_SEGMENT};
use crate::tokenizer::{GRAM_WIDTH, VALUE_END, VALUE_START, WORD_END, WORD_START, marked, words};
use crate::weight::{FieldWeight, INDEXED_WEIGHTS, MatchTier, Ranks};

/// Score of a record whose fields match at rank 0; body-only matches score
/// below it.
pub(crate) const FIELD_BASE: Score = 1000.0;

/// Scale applied to body BM25, keeping it well below one rank step.
const BM25_SCALE: Score = 0.001;

/// Shortest word matched with one typo, and with two.
const ONE_TYPO_SCALARS: usize = 5;
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

/// The marked sequence a field tier looks for, if the query can match it.
fn tier_sequence(tier: MatchTier, query: &[char]) -> Option<Vec<char>> {
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
    if (word_start && !starts_word) || (word_end && !ends_word) {
        return None;
    }
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
    Box::new(ConstScoreQuery::new(query, score))
}

/// A title word within typing distance of `word`; the last word typed may
/// still be incomplete, so it matches as a prefix.
fn fuzzy_word(field: Field, word: &str, last: bool) -> BoxQuery {
    let term = Term::from_field_text(field, word);
    let scalars = word.chars().count();
    let distance = u8::from(scalars >= ONE_TYPO_SCALARS)
        .saturating_add(u8::from(scalars >= TWO_TYPOS_SCALARS));
    match (distance, last) {
        (0, false) => Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
        (0, true) => prefix_query(term),
        (distance, false) => Box::new(FuzzyTermQuery::new(term, distance, true)),
        (distance, true) => Box::new(FuzzyTermQuery::new_prefix(term, distance, true)),
    }
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

/// The query for typed `text`, or `None` when nothing can match. An empty
/// query browses every record.
pub(crate) fn compile(
    fields: &Fields,
    used: Used,
    matching: Matching,
    ranks: Ranks,
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
    if matching.typos
        && used.slots.first().copied().unwrap_or(false)
        && let Some(rank) = ranks(MatchTier::Typo, FieldWeight::TITLE)
        && let Some(last) = query_words.len().checked_sub(1)
    {
        let every = query_words
            .iter()
            .enumerate()
            .map(|(at, word)| fuzzy_word(fields.title_words, word, at == last))
            .collect();
        ranked.push(constant(all_of(every), rank));
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
