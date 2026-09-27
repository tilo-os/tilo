//! Plans a typed query as tantivy queries, one per rank.
//!
//! Each field and [`MatchTier`] is one gram phrase: the query as a
//! substring, after a word-start marker, after the value-start marker, or
//! between the value markers. A hit's rank is the best `tier × weight` it
//! matches, so the plan lists one [`Stage`] per rank, best first; the index
//! walks them in order and stops once a page is full. Hits of one rank come
//! in index order. Multi-word queries spread over fields and typos in a
//! title are further, lower stages. Body text is last and ordered by BM25.

use tantivy::Term;
use tantivy::query::{
    BooleanQuery, FuzzyTermQuery, Occur, PhrasePrefixQuery, PhraseQuery, Query, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption};

use crate::index::{Fields, RECORD_SEGMENT};
use crate::tokenizer::{GRAM_WIDTH, VALUE_END, VALUE_START, WORD_START, marked, words};
use crate::weight::{FUZZY_RANK, INDEXED_WEIGHTS, MatchTier, SPANNING_RANK};

/// Shortest word matched with one typo, and with two.
const ONE_TYPO_SCALARS: usize = 5;
const TWO_TYPOS_SCALARS: usize = 9;

pub(crate) type BoxQuery = Box<dyn Query>;

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
        length if length < GRAM_WIDTH => {
            // Every indexed scalar starts a full gram, so a short sequence
            // is the prefix of the gram at its position.
            let prefix = Term::from_field_text(field, &sequence.iter().collect::<String>());
            Some(prefix_query(prefix))
        }
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

/// The marked sequence a tier looks for.
fn tier_sequence(tier: MatchTier, query: &[char]) -> Option<Vec<char>> {
    let starts_word = query.first().is_some_and(|first| first.is_alphanumeric());
    let mut sequence = Vec::with_capacity(query.len().saturating_add(3));
    match tier {
        MatchTier::Contains => {}
        MatchTier::WordPrefix => {
            if !starts_word {
                return None;
            }
            sequence.push(WORD_START);
        }
        MatchTier::Prefix | MatchTier::Exact => {
            sequence.push(VALUE_START);
            if starts_word {
                sequence.push(WORD_START);
            }
        }
    }
    sequence.extend_from_slice(query);
    if tier == MatchTier::Exact {
        sequence.push(VALUE_END);
    }
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
    /// Every whitespace-separated word of the query occurs in some field,
    /// though no field holds the whole query: [`SPANNING_RANK`].
    pub spanning: bool,
    /// Every word of the query matches a title word within a small edit
    /// distance, the last one as a prefix: [`FUZZY_RANK`].
    pub typos: bool,
}

impl Matching {
    /// Substrings, words spread over fields, and typos in titles.
    pub const LOOSE: Self = Self {
        spanning: true,
        typos: true,
    };
}

/// One way to match at a stage's rank, built only if the stage is reached.
enum Part<'a> {
    /// A marked sequence in a gram field.
    Sequence(Field, Vec<char>),
    Built(Box<dyn Fn() -> Option<BoxQuery> + 'a>),
}

/// Records matching at one rank.
pub(crate) struct Stage<'a> {
    pub rank: i32,
    parts: Vec<Part<'a>>,
}

impl Stage<'_> {
    /// The stage as one tantivy query, or `None` when nothing can match.
    pub fn query(&self) -> Option<BoxQuery> {
        any_of(
            self.parts
                .iter()
                .filter_map(|part| match part {
                    Part::Sequence(field, sequence) => sequence_query(*field, sequence),
                    Part::Built(build) => build(),
                })
                .collect(),
        )
    }
}

/// How to find and order the hits of one query.
pub(crate) struct Plan<'a> {
    /// Field matches, best rank first.
    pub stages: Vec<Stage<'a>>,
    /// Body matches, after every field match.
    pub body: Option<BoxQuery>,
    /// BM25 over the body's words, to order body matches.
    pub body_order: Option<BoxQuery>,
}

/// The plan for typed `text`. An empty query browses every record.
pub(crate) fn plan<'a>(
    fields: &'a Fields,
    used: Used,
    matching: Matching,
    text: &'a str,
) -> Plan<'a> {
    let query = marked(text, false);
    if query.is_empty() {
        let records = fields.segment;
        return Plan {
            stages: vec![Stage {
                rank: 0,
                parts: vec![Part::Built(Box::new(move || {
                    Some(Box::new(TermQuery::new(
                        Term::from_field_u64(records, RECORD_SEGMENT),
                        IndexRecordOption::Basic,
                    )))
                }))],
            }],
            body: None,
            body_order: None,
        };
    }
    let mut ranked = record_parts(fields, used, matching, text, &query);
    ranked.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));
    let mut stages: Vec<Stage<'a>> = Vec::new();
    for (rank, part) in ranked {
        match stages.last_mut() {
            Some(stage) if stage.rank == rank => stage.parts.push(part),
            _ => stages.push(Stage {
                rank,
                parts: vec![part],
            }),
        }
    }
    let body = used
        .body
        .then(|| sequence_query(fields.body_grams, &query))
        .flatten();
    let body_order = body.as_ref().and_then(|_| {
        any_of(
            words(text)
                .iter()
                .map(|word| -> BoxQuery {
                    Box::new(TermQuery::new(
                        Term::from_field_text(fields.body_words, word),
                        IndexRecordOption::WithFreqs,
                    ))
                })
                .collect(),
        )
    });
    Plan {
        stages,
        body,
        body_order,
    }
}

fn record_parts<'a>(
    fields: &'a Fields,
    used: Used,
    matching: Matching,
    text: &'a str,
    query: &[char],
) -> Vec<(i32, Part<'a>)> {
    let slots: Vec<usize> = used
        .slots
        .iter()
        .enumerate()
        .filter_map(|(slot, used)| used.then_some(slot))
        .collect();
    let mut parts = Vec::new();
    for &slot in &slots {
        let (Some(field), Some(weight)) = (fields.grams.get(slot), INDEXED_WEIGHTS.get(slot))
        else {
            continue;
        };
        let tiers: &[MatchTier] = if weight.percent() == 0 {
            &[MatchTier::Contains]
        } else {
            &MatchTier::ALL
        };
        for &tier in tiers {
            if let Some(sequence) = tier_sequence(tier, query) {
                parts.push((tier.rank_in(*weight), Part::Sequence(*field, sequence)));
            }
        }
    }

    // Every word of a multi-word query somewhere in the fields.
    if matching.spanning && text.split_whitespace().nth(1).is_some() {
        parts.push((
            SPANNING_RANK,
            Part::Built(Box::new(move || {
                let every: Option<Vec<BoxQuery>> = text
                    .split_whitespace()
                    .map(|chunk| {
                        let chunk = marked(chunk, false);
                        any_of(
                            slots
                                .iter()
                                .filter_map(|slot| {
                                    sequence_query(*fields.grams.get(*slot)?, &chunk)
                                })
                                .collect(),
                        )
                    })
                    .collect();
                Some(all_of(every?))
            })),
        ));
    }

    // A typo in the title's words.
    if matching.typos && used.slots.first().copied().unwrap_or(false) {
        parts.push((
            FUZZY_RANK,
            Part::Built(Box::new(move || {
                let query_words = words(text);
                let title_words = fields.title_words;
                let last = query_words.len().checked_sub(1)?;
                Some(all_of(
                    query_words
                        .iter()
                        .enumerate()
                        .map(|(at, word)| fuzzy_word(title_words, word, at == last))
                        .collect(),
                ))
            })),
        ));
    }
    parts
}
