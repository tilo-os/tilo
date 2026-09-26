//! Bounded full-text bodies for [`SearchCatalog`](super::SearchCatalog).
//!
//! A body is prose the catalogue must find but must not copy: an EPUB
//! chapter, a long note, a message. Its owner keeps the text and exposes it
//! through a [`SearchBodySource`]; the catalogue retains only the body's
//! distinct short grams. A query's candidates are the records whose grams
//! contain every gram of the query, and each candidate is then verified by
//! streaming its text a bounded chunk at a time, so one page of a search
//! never reads more than its work budget of prose, however long a body is.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use super::provider::SearchResult;
use super::tokenizer::{SearchTokenizer, UnicodeTokenizer};

/// Scalars verified per unit of search work. Deliberately well below the
/// longest interactive query, so a body made of one enormous paragraph still
/// yields between pages.
pub const BODY_CHUNK_SCALARS: usize = 1_024;

/// Up to [`GRAM_WIDTH`] folded scalars packed into one key. Each scalar is
/// stored as `scalar + 1` in its own 21-bit lane, so a gram never collides
/// with a shorter one that shares its prefix.
pub(crate) type Gram = u64;
const GRAM_WIDTH: usize = 3;
const GRAM_LANE_SHIFTS: [u32; GRAM_WIDTH] = [0, 21, 42];

fn pack_gram(chars: &[char]) -> Gram {
    chars
        .iter()
        .zip(GRAM_LANE_SHIFTS)
        .fold(0, |key, (character, shift)| {
            key | u64::from(u32::from(*character))
                .wrapping_add(1)
                .wrapping_shl(shift)
        })
}

/// The distinct grams a folded query needs a body to contain: the whole
/// query when it is short, otherwise every [`GRAM_WIDTH`]-scalar window.
pub(crate) fn query_grams(folded_query: &str) -> Vec<Gram> {
    let needle: Vec<char> = folded_query.chars().collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut grams: Vec<Gram> = if needle.len() <= GRAM_WIDTH {
        vec![pack_gram(&needle)]
    } else {
        needle.windows(GRAM_WIDTH).map(pack_gram).collect()
    };
    grams.sort_unstable();
    grams.dedup();
    grams
}

/// One contiguous run of body text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyPiece<'a> {
    pub text: &'a str,
    /// Whether a word break separates this piece from the text before it.
    /// Search treats the break as a single space, so a phrase can span it.
    pub space_before: bool,
}

/// Prose a catalogue record can be found by, owned by the app.
///
/// A body is a sequence of *segments* (the unit a hit points at: a paragraph,
/// a block, a line) and each segment a sequence of [`BodyPiece`]s (runs of
/// styled text, say). The source must be immutable: the catalogue indexes it
/// once and later verifies candidates against the same text.
pub trait SearchBodySource: Send + Sync {
    fn segment_count(&self) -> usize;

    fn piece_count(&self, segment: usize) -> usize;

    fn piece(&self, segment: usize, piece: usize) -> Option<BodyPiece<'_>>;

    /// The hit to report for a match that ends in `segment`. `None` reports
    /// the record's own result instead, once, and only when its fields did
    /// not already match the query.
    fn location(&self, _segment: usize) -> Option<SearchResult> {
        None
    }
}

/// A single-segment body of plain text.
impl SearchBodySource for String {
    fn segment_count(&self) -> usize {
        1
    }

    fn piece_count(&self, segment: usize) -> usize {
        usize::from(segment == 0)
    }

    fn piece(&self, segment: usize, piece: usize) -> Option<BodyPiece<'_>> {
        (segment == 0 && piece == 0).then_some(BodyPiece {
            text: self,
            space_before: false,
        })
    }
}

/// One segment per string, separated by word breaks.
impl SearchBodySource for Vec<String> {
    fn segment_count(&self) -> usize {
        self.len()
    }

    fn piece_count(&self, segment: usize) -> usize {
        usize::from(segment < self.len())
    }

    fn piece(&self, segment: usize, piece: usize) -> Option<BodyPiece<'_>> {
        let text = self.get(segment)?;
        (piece == 0).then_some(BodyPiece {
            text,
            space_before: segment > 0,
        })
    }
}

/// An indexed body: the app's source plus the distinct grams it contains.
///
/// Building one reads the whole source once; cloning one is O(1). Apps that
/// rebuild a catalogue over mostly unchanged content keep their
/// `SearchBody` values and hand the same ones back, so unchanged prose is
/// never re-read.
#[derive(Clone)]
pub struct SearchBody {
    source: Arc<dyn SearchBodySource>,
    grams: Arc<[Gram]>,
    tokenizer: &'static str,
}

impl fmt::Debug for SearchBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SearchBody")
            .field("grams", &self.grams.len())
            .field("tokenizer", &self.tokenizer)
            .finish_non_exhaustive()
    }
}

impl PartialEq for SearchBody {
    fn eq(&self, other: &Self) -> bool {
        self.shares_index_with(other)
    }
}

impl SearchBody {
    /// Index `source` with the default tokenizer.
    #[must_use]
    pub fn new(source: Arc<dyn SearchBodySource>) -> Self {
        Self::with_tokenizer(source, &UnicodeTokenizer)
    }

    /// Index `source` with the tokenizer of the catalogue it will join.
    #[must_use]
    pub fn with_tokenizer(
        source: Arc<dyn SearchBodySource>,
        tokenizer: &dyn SearchTokenizer,
    ) -> Self {
        let mut grams = HashSet::new();
        let mut window = ['\0'; GRAM_WIDTH];
        let mut filled = 0usize;
        let mut push = |character: char| {
            window.rotate_left(1);
            if let Some(last) = window.last_mut() {
                *last = character;
            }
            filled = filled.saturating_add(1).min(GRAM_WIDTH);
            for width in 1..=filled {
                if let Some(gram) = window.get(GRAM_WIDTH - width..) {
                    grams.insert(pack_gram(gram));
                }
            }
        };
        for segment in 0..source.segment_count() {
            for piece in 0..source.piece_count(segment) {
                let Some(piece) = source.piece(segment, piece) else {
                    continue;
                };
                if piece.space_before {
                    push(' ');
                }
                for scalar in piece.text.chars() {
                    tokenizer.fold_char(scalar, &mut push);
                }
            }
        }
        let mut grams: Vec<Gram> = grams.into_iter().collect();
        grams.sort_unstable();
        Self {
            source,
            grams: grams.into(),
            tokenizer: tokenizer.id(),
        }
    }

    /// Whether both handles share one index, i.e. neither re-read the prose.
    #[must_use]
    pub fn shares_index_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.grams, &other.grams)
    }

    /// Distinct grams retained for this body.
    #[must_use]
    pub fn gram_count(&self) -> usize {
        self.grams.len()
    }

    #[must_use]
    pub fn source(&self) -> &Arc<dyn SearchBodySource> {
        &self.source
    }

    pub(crate) fn tokenizer_id(&self) -> &'static str {
        self.tokenizer
    }

    pub(crate) fn grams(&self) -> &[Gram] {
        &self.grams
    }

    pub(crate) fn contains_all(&self, grams: &[Gram]) -> bool {
        grams
            .iter()
            .all(|gram| self.grams.binary_search(gram).is_ok())
    }

    /// Verify one bounded chunk starting at `position`.
    pub(crate) fn step(
        &self,
        tokenizer: &dyn SearchTokenizer,
        folded_query: &str,
        position: BodyPosition,
    ) -> BodyStep {
        let source = self.source.as_ref();
        let segments = source.segment_count();
        if position.segment >= segments {
            return BodyStep::Invalid;
        }
        let pieces = source.piece_count(position.segment);
        let Some(mut matcher) = FoldMatcher::with_state(folded_query, position.matcher) else {
            return BodyStep::Invalid;
        };
        let next_segment = |matcher: usize| {
            if position.segment.saturating_add(1) >= segments {
                BodyStep::Exhausted
            } else {
                BodyStep::Continue(BodyPosition {
                    segment: position.segment.saturating_add(1),
                    piece: 0,
                    byte: 0,
                    matcher,
                })
            }
        };
        if pieces == 0 {
            return if position.piece == 0 && position.byte == 0 {
                next_segment(matcher.state())
            } else {
                BodyStep::Invalid
            };
        }
        let Some(piece) = source.piece(position.segment, position.piece) else {
            return BodyStep::Invalid;
        };
        let text = piece.text;
        if position.byte > text.len() || !text.is_char_boundary(position.byte) {
            return BodyStep::Invalid;
        }
        if position.byte == 0 && piece.space_before && matcher.push(tokenizer, ' ') {
            return BodyStep::Matched {
                segment: position.segment,
            };
        }
        let rest = text.get(position.byte..).unwrap_or_default();
        let mut end = position.byte;
        for scalar in rest.chars().take(BODY_CHUNK_SCALARS) {
            end = end.saturating_add(scalar.len_utf8());
            if matcher.push(tokenizer, scalar) {
                return BodyStep::Matched {
                    segment: position.segment,
                };
            }
        }
        if end < text.len() {
            BodyStep::Continue(BodyPosition {
                byte: end,
                matcher: matcher.state(),
                ..position
            })
        } else if position.piece.saturating_add(1) < pieces {
            BodyStep::Continue(BodyPosition {
                piece: position.piece.saturating_add(1),
                byte: 0,
                matcher: matcher.state(),
                ..position
            })
        } else {
            next_segment(matcher.state())
        }
    }
}

/// Where verification of one body resumes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BodyPosition {
    pub(crate) segment: usize,
    pub(crate) piece: usize,
    pub(crate) byte: usize,
    /// Length of the query prefix matched so far.
    pub(crate) matcher: usize,
}

pub(crate) enum BodyStep {
    Matched {
        segment: usize,
    },
    Continue(BodyPosition),
    Exhausted,
    /// The position does not name text in this body.
    Invalid,
}

/// Streaming KMP matcher over folded text, so verification never builds a
/// folded copy of the prose it reads.
struct FoldMatcher {
    needle: Vec<char>,
    failure: Vec<usize>,
    matched: usize,
}

impl FoldMatcher {
    fn with_state(folded_query: &str, state: usize) -> Option<Self> {
        let needle: Vec<char> = folded_query.chars().collect();
        if state >= needle.len() {
            return None;
        }
        let mut failure = vec![0usize; needle.len()];
        let mut prefix = 0usize;
        for index in 1..needle.len() {
            while prefix > 0 && needle.get(index) != needle.get(prefix) {
                prefix = failure.get(prefix - 1).copied().unwrap_or_default();
            }
            if needle.get(index) == needle.get(prefix) {
                prefix += 1;
            }
            if let Some(slot) = failure.get_mut(index) {
                *slot = prefix;
            }
        }
        Some(Self {
            needle,
            failure,
            matched: state,
        })
    }

    const fn state(&self) -> usize {
        self.matched
    }

    fn push(&mut self, tokenizer: &dyn SearchTokenizer, scalar: char) -> bool {
        let mut found = false;
        tokenizer.fold_char(scalar, &mut |folded| {
            if found {
                return;
            }
            while self.matched > 0 && self.needle.get(self.matched) != Some(&folded) {
                self.matched = self
                    .failure
                    .get(self.matched - 1)
                    .copied()
                    .unwrap_or_default();
            }
            if self.needle.get(self.matched) == Some(&folded) {
                self.matched += 1;
                if self.matched == self.needle.len() {
                    found = true;
                }
            }
        });
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verify(body: &SearchBody, query: &str) -> (Option<usize>, usize) {
        let tokenizer = UnicodeTokenizer;
        let query = tokenizer.fold(query);
        let mut position = BodyPosition::default();
        let mut steps = 0;
        loop {
            steps += 1;
            match body.step(&tokenizer, &query, position) {
                BodyStep::Matched { segment } => return (Some(segment), steps),
                BodyStep::Continue(next) => position = next,
                BodyStep::Exhausted => return (None, steps),
                BodyStep::Invalid => panic!("a scanner position is always valid"),
            }
        }
    }

    #[test]
    fn phrases_cross_segment_breaks_and_fold_like_the_index() {
        let body = SearchBody::new(Arc::new(vec![
            "It was the best".to_string(),
            "of times, CAFÉ".to_string(),
        ]));
        assert_eq!(verify(&body, "best of").0, Some(1));
        assert_eq!(verify(&body, "cafe").0, Some(1));
        assert_eq!(verify(&body, "bestof").0, None);
        assert!(body.contains_all(&query_grams("best of")));
        assert!(!body.contains_all(&query_grams("worst")));
    }

    #[test]
    fn a_huge_piece_is_verified_in_bounded_chunks() {
        let mut text = "lorem ".repeat(20_000);
        text.push_str("cobalt");
        let body = SearchBody::new(Arc::new(text.clone()));
        let (segment, steps) = verify(&body, "cobalt");
        assert_eq!(segment, Some(0));
        let chunks = text.chars().count().div_ceil(BODY_CHUNK_SCALARS);
        assert_eq!(steps, chunks, "one step per chunk, none skipped");
        assert!(body.gram_count() < 64, "grams, not prose, are retained");
    }

    #[test]
    fn positions_outside_the_body_are_invalid() {
        let body = SearchBody::new(Arc::new("short".to_string()));
        let tokenizer = UnicodeTokenizer;
        for position in [
            BodyPosition {
                segment: 1,
                ..BodyPosition::default()
            },
            BodyPosition {
                byte: 99,
                ..BodyPosition::default()
            },
            BodyPosition {
                matcher: 9,
                ..BodyPosition::default()
            },
        ] {
            assert!(matches!(
                body.step(&tokenizer, "sh", position),
                BodyStep::Invalid
            ));
        }
    }
}
