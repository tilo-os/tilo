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

use tilo_search_model::query::{Matching, Used, tier_sequence, typo_distance};
use tilo_search_model::record::RECORD_SEGMENT;
use tilo_search_model::tokenizer::{GRAM_WIDTH, marked, words};
use tilo_search_model::typo::{Typos, Vocabulary};
use tilo_search_model::weight::{FieldWeight, INDEXED_WEIGHTS, KEYWORD_SLOT, MatchTier, Ranks};

use crate::engine::Fields;

/// Score of a record whose fields match at rank 0; body-only matches score
/// below it.
pub const FIELD_BASE: Score = 1000.0;

/// Scale applied to body BM25, keeping it well below one rank step.
const BM25_SCALE: Score = 0.001;

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
    match (typo_distance(scalars.len()), last) {
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
pub fn compile(
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
