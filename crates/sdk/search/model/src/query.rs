//! What a typed query matches: the tiers a field's marked value is searched
//! for ([`tier_sequence`]), the typos a word may carry ([`typo_distance`]),
//! and which looser matches an index opts into ([`Matching`]). The engine
//! compiles queries from these, and the search layer answers small indexes
//! with them directly, so both rank alike.

use crate::tokenizer::{VALUE_END, VALUE_START, WORD_END, WORD_START};
use crate::weight::{INDEXED_WEIGHTS, MatchTier};

/// Shortest word matched with one typo, and with two. Four letters take one:
/// "nots" is "notes" mistyped, and a fast typist drops a letter as often in a
/// short word as in a long one.
const ONE_TYPO_SCALARS: usize = 4;
const TWO_TYPOS_SCALARS: usize = 9;

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
pub fn infix_allowed(query: &[char]) -> bool {
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
pub fn tier_sequence(tier: MatchTier, query: &[char]) -> Option<Vec<char>> {
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

/// The typos a typed word of `scalars` scalars may carry: none while it is
/// short, then one, then two.
pub fn typo_distance(scalars: usize) -> usize {
    usize::from(scalars >= ONE_TYPO_SCALARS)
        .saturating_add(usize::from(scalars >= TWO_TYPOS_SCALARS))
}

/// Which parts of the schema hold any text, so a query skips the rest.
#[derive(Clone, Copy, Debug, Default)]
pub struct Used {
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
