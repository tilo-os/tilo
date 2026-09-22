//! Unicode line-break opportunities shared by shaping-adjacent layout code.
//!
//! `RichTextLayout` receives UAX #14 behavior from cosmic-text. Components
//! that must build matching row metadata or return individually clamped lines
//! use this module instead of guessing at ASCII spaces.

use std::sync::LazyLock;

/// Whether a UAX #14 opportunity permits or requires a break.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineBreakKind {
    Allowed,
    Mandatory,
}

/// Iterate byte offsets where a line may or must end according to Unicode
/// Standard Annex #14. Every offset is a UTF-8 boundary and the final text
/// boundary is always returned as mandatory.
pub fn line_break_opportunities(text: &str) -> std::vec::IntoIter<(usize, LineBreakKind)> {
    use std::collections::BTreeMap;

    use icu_segmenter::{LineSegmenter, LineSegmenterBorrowed};
    // The auto model adds dictionary/LSTM boundaries for Khmer, Lao, Myanmar,
    // and Thai. The borrowed segmenter only references baked static data.
    static SEGMENTER: LazyLock<LineSegmenterBorrowed<'static>> =
        LazyLock::new(|| LineSegmenter::new_auto(Default::default()));

    let mut breaks = BTreeMap::new();
    for (byte, kind) in unicode_linebreak::linebreaks(text) {
        let kind = match kind {
            unicode_linebreak::BreakOpportunity::Allowed => LineBreakKind::Allowed,
            unicode_linebreak::BreakOpportunity::Mandatory => LineBreakKind::Mandatory,
        };
        breaks.insert(byte, kind);
    }
    for byte in SEGMENTER.segment_str(text).filter(|&byte| byte > 0) {
        breaks.entry(byte).or_insert(LineBreakKind::Allowed);
    }
    breaks.into_iter().collect::<Vec<_>>().into_iter()
}

/// UTF-8 byte offsets at which TeX/Liang patterns permit a discretionary
/// hyphen for `language`. The source text is never modified; callers decide
/// whether a chosen line break should paint the hyphen glyph.
pub fn discretionary_hyphen_breaks(text: &str, language: &str) -> Vec<usize> {
    let Ok(parsed) = language.parse::<unic_langid::LanguageIdentifier>() else {
        return Vec::new();
    };
    let code = parsed.language.as_str().as_bytes();
    let Some(lang) = (match code {
        [a, b] => hypher::Lang::from_iso([a.to_ascii_lowercase(), b.to_ascii_lowercase()]),
        _ => None,
    }) else {
        return Vec::new();
    };
    let mut breaks = Vec::new();
    for range in unicode_word_ranges(text) {
        let Some(word) = text.get(range.clone()) else {
            continue;
        };
        let mut source = range.start;
        let mut syllables = hypher::hyphenate(word, lang).peekable();
        while let Some(syllable) = syllables.next() {
            source = source.saturating_add(syllable.len());
            if syllables.peek().is_some() {
                breaks.push(source);
            }
        }
    }
    breaks
}

pub(crate) const VIRTUAL_BREAK: char = '\u{200b}';

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VirtualInsertion {
    pub(crate) source: usize,
    pub(crate) ch: char,
}

/// Maps the zero-width break hints used by cosmic-text back onto the
/// application's unchanged UTF-8 byte space.
#[derive(Clone, Debug, Default)]
pub(crate) struct OffsetMap {
    /// Virtual shaping characters inserted before source byte positions.
    pub(crate) insertions: Vec<VirtualInsertion>,
}

impl OffsetMap {
    pub(crate) fn for_text_with_hyphenation(text: &str, lang: Option<hypher::Lang>) -> Self {
        Self::for_text_with_breaks(text, lang, &[])
    }

    pub(crate) fn for_text_with_breaks(
        text: &str,
        lang: Option<hypher::Lang>,
        discretionary_breaks: &[usize],
    ) -> Self {
        use std::collections::HashSet;

        // cosmic-text already consumes unicode-linebreak's UAX #14 table. Only
        // add the ICU dictionary/LSTM opportunities which that table cannot
        // express for complex-context scripts.
        let native: HashSet<usize> = unicode_linebreak::linebreaks(text)
            .map(|(byte, _)| byte)
            .collect();
        let mut insertions: Vec<_> = line_break_opportunities(text)
            .map(|(byte, _)| byte)
            .filter(|&byte| byte > 0 && byte < text.len() && !native.contains(&byte))
            .map(|source| VirtualInsertion {
                source,
                ch: VIRTUAL_BREAK,
            })
            .collect();
        if let Some(lang) = lang {
            let hyphen = lang.hyphenation_character().unwrap_or(VIRTUAL_BREAK);
            for range in unicode_word_ranges(text) {
                let Some(word) = text.get(range.clone()) else {
                    continue;
                };
                let mut source = range.start;
                let mut syllables = hypher::hyphenate(word, lang).peekable();
                while let Some(syllable) = syllables.next() {
                    source = source.saturating_add(syllable.len());
                    if syllables.peek().is_some() {
                        insertions.push(VirtualInsertion { source, ch: hyphen });
                    }
                }
            }
        }
        insertions.extend(
            discretionary_breaks
                .iter()
                .copied()
                .filter(|byte| *byte > 0 && *byte < text.len() && text.is_char_boundary(*byte))
                .map(|source| VirtualInsertion {
                    source,
                    ch: '\u{ad}',
                }),
        );
        insertions.sort_unstable_by_key(|insertion| insertion.source);
        insertions.dedup_by(|a, b| {
            if a.source != b.source {
                return false;
            }
            // Prefer a visible discretionary hyphen over a plain zero-width
            // dictionary break when both models identify the same boundary.
            if a.ch == VIRTUAL_BREAK && b.ch != VIRTUAL_BREAK {
                a.ch = b.ch;
            }
            true
        });
        Self { insertions }
    }

    pub(crate) fn is_identity(&self) -> bool {
        self.insertions.is_empty()
    }

    pub(crate) fn has_discretionary_hyphens(&self) -> bool {
        self.insertions
            .iter()
            .any(|insertion| insertion.ch == '\u{ad}')
    }

    pub(crate) fn source_to_shaped(&self, byte: usize) -> usize {
        let inserted: usize = self
            .insertions
            .iter()
            .take_while(|insertion| insertion.source <= byte)
            .map(|insertion| insertion.ch.len_utf8())
            .sum();
        byte.saturating_add(inserted)
    }

    pub(crate) fn shaped_to_source(&self, byte: usize) -> usize {
        let mut added = 0;
        for insertion in &self.insertions {
            let source = insertion.source;
            let len = insertion.ch.len_utf8();
            let shaped = source.saturating_add(added);
            if byte <= shaped {
                return byte.saturating_sub(added);
            }
            if byte <= shaped.saturating_add(len) {
                return source;
            }
            added = added.saturating_add(len);
        }
        byte.saturating_sub(added)
    }

    pub(crate) fn shape_text<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        if self.is_identity() {
            return std::borrow::Cow::Borrowed(text);
        }
        let extra = self
            .insertions
            .iter()
            .map(|insertion| insertion.ch.len_utf8())
            .sum::<usize>();
        let mut shaped = String::with_capacity(text.len().saturating_add(extra));
        let mut start = 0;
        for insertion in &self.insertions {
            let at = insertion.source;
            if let Some(chunk) = text.get(start..at) {
                shaped.push_str(chunk);
            }
            shaped.push(insertion.ch);
            start = at;
        }
        if let Some(chunk) = text.get(start..) {
            shaped.push_str(chunk);
        }
        std::borrow::Cow::Owned(shaped)
    }

    pub(crate) fn shape_fragment<'a>(
        &self,
        text: &'a str,
        source_start: usize,
    ) -> std::borrow::Cow<'a, str> {
        let source_end = source_start.saturating_add(text.len());
        if self
            .insertions
            .iter()
            .all(|insertion| insertion.source <= source_start || insertion.source > source_end)
        {
            return std::borrow::Cow::Borrowed(text);
        }
        let mut shaped = String::with_capacity(text.len());
        let mut local_start = 0;
        for insertion in &self.insertions {
            let at = insertion.source;
            // A boundary shared by two attributed spans belongs to the span on
            // its left. This inserts exactly one hint without changing either
            // neighbouring run's visible styling.
            if at <= source_start || at > source_end {
                continue;
            }
            let local = at.saturating_sub(source_start);
            if let Some(chunk) = text.get(local_start..local) {
                shaped.push_str(chunk);
            }
            shaped.push(insertion.ch);
            local_start = local;
        }
        if let Some(chunk) = text.get(local_start..) {
            shaped.push_str(chunk);
        }
        std::borrow::Cow::Owned(shaped)
    }
}

/// Extended grapheme clusters (UAX #29) paired with their starting byte
/// offset, so combining marks, emoji modifiers, flags and ZWJ sequences stay
/// whole.
pub(crate) fn grapheme_indices(text: &str) -> impl Iterator<Item = (usize, &str)> {
    use icu_segmenter::{GraphemeClusterSegmenter, GraphemeClusterSegmenterBorrowed};
    const SEGMENTER: GraphemeClusterSegmenterBorrowed<'static> = GraphemeClusterSegmenter::new();
    let mut start = 0;
    SEGMENTER
        .segment_str(text)
        .filter(|&end| end > 0)
        .map(move |end| {
            let cluster = (start, text.get(start..end).unwrap_or_default());
            start = end;
            cluster
        })
}

/// Extended grapheme clusters (UAX #29) of `text`, in order.
pub(crate) fn graphemes(text: &str) -> impl Iterator<Item = &str> {
    grapheme_indices(text).map(|(_, cluster)| cluster)
}

/// Return Unicode word-like spans in original UTF-8 byte coordinates.
/// Uses the same ICU segmentation as toolkit word selection and movement.
pub fn unicode_word_ranges(text: &str) -> std::vec::IntoIter<std::ops::Range<usize>> {
    use icu_segmenter::{WordSegmenter, WordSegmenterBorrowed};
    static SEGMENTER: LazyLock<WordSegmenterBorrowed<'static>> =
        LazyLock::new(|| WordSegmenter::new_auto(Default::default()));
    let boundaries: Vec<_> = SEGMENTER.segment_str(text).iter_with_word_type().collect();
    boundaries
        .windows(2)
        .filter_map(|window| {
            let [first, second] = window else {
                return None;
            };
            let range = first.0..second.0;
            // ICU can tag a word ending in an Extend scalar as None before
            // whitespace despite finding the correct span. Retain that
            // complete segment when it contains a letter or number,
            // preserving ICU's boundaries and attached marks.
            let word_like = second.1.is_word_like()
                || text
                    .get(range.clone())
                    .is_some_and(|word| word.chars().any(char::is_alphanumeric));
            word_like.then_some(range)
        })
        .collect::<Vec<_>>()
        .into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_words_retain_trailing_combining_marks() {
        for (text, expected) in [
            ("test\u{301}", vec!["test\u{301}"]),
            ("test\u{301} ", vec!["test\u{301}"]),
            (
                "e\u{301}test test\u{301} 123\u{301}",
                vec!["e\u{301}test", "test\u{301}", "123\u{301}"],
            ),
        ] {
            let words: Vec<_> = unicode_word_ranges(text)
                .map(|range| text.get(range).expect("UTF-8 word range"))
                .collect();
            assert_eq!(
                words, expected,
                "the complete source word remains word-like"
            );
        }
        assert!(unicode_word_ranges("\u{301} , .").next().is_none());
    }

    #[test]
    fn grapheme_indices_keep_extended_clusters_whole() {
        let text = "e\u{301}👨‍👩‍👧🇳🇱a";
        let clusters: Vec<_> = grapheme_indices(text).collect();
        assert_eq!(
            clusters,
            vec![(0, "e\u{301}"), (3, "👨‍👩‍👧"), (21, "🇳🇱"), (29, "a"),]
        );
        assert_eq!(graphemes("").count(), 0);
    }

    #[test]
    fn cjk_has_opportunities_without_ascii_spaces() {
        let text = "東京京都";
        let breaks: Vec<_> = line_break_opportunities(text).collect();
        assert_eq!(breaks.last(), Some(&(text.len(), LineBreakKind::Mandatory)));
        assert!(breaks.iter().any(|(byte, _)| *byte < text.len()));
    }

    #[test]
    fn non_breaking_space_does_not_create_an_opportunity() {
        let text = "10\u{a0}km away";
        let protected_end = "10\u{a0}".len();
        assert!(
            line_break_opportunities(text).all(|(byte, _)| byte != protected_end),
            "NBSP keeps its adjacent words together"
        );
    }

    #[test]
    fn emoji_zwj_sequence_is_not_split() {
        let family = "👨‍👩‍👧‍👦";
        assert_eq!(
            line_break_opportunities(family).collect::<Vec<_>>(),
            vec![(family.len(), LineBreakKind::Mandatory)]
        );
    }

    #[test]
    fn dictionary_line_breaks_segment_thai_without_spaces() {
        let text = "ทุกสองสัปดาห์";
        let breaks: Vec<_> = line_break_opportunities(text)
            .map(|(byte, _)| byte)
            .collect();
        assert_eq!(breaks, vec![9, 18, text.len()]);
    }

    #[test]
    fn english_hyphenation_inserts_soft_hyphens_at_tex_pattern_boundaries() {
        let text = "extensive";
        let map = OffsetMap::for_text_with_hyphenation(text, Some(hypher::Lang::English));
        let insertions: Vec<_> = map
            .insertions
            .iter()
            .map(|insertion| (insertion.source, insertion.ch))
            .collect();
        assert_eq!(insertions, vec![(2, '\u{ad}'), (5, '\u{ad}')]);
        assert_eq!(map.shape_text(text), "ex\u{ad}ten\u{ad}sive");
        for source in 0..=text.len() {
            assert_eq!(map.shaped_to_source(map.source_to_shaped(source)), source);
        }
    }

    #[test]
    fn hyphenation_only_touches_word_like_segments() {
        let text = "extensive, 123";
        let map = OffsetMap::for_text_with_hyphenation(text, Some(hypher::Lang::English));
        assert!(map.insertions.iter().all(|insertion| insertion.source < 9));
        assert_eq!(
            map.shaped_to_source(map.source_to_shaped(text.len())),
            text.len()
        );
    }

    #[test]
    fn custom_breaks_ignore_endpoints_and_non_utf8_boundaries() {
        let text = "éclair";
        let map = OffsetMap::for_text_with_breaks(text, None, &[0, 1, 2, text.len(), usize::MAX]);
        assert_eq!(map.shape_text(text), "é\u{ad}clair");
        assert_eq!(map.insertions.len(), 1);
        assert_eq!(map.insertions[0].source, 2);
    }

    #[test]
    fn public_hyphen_breaks_are_source_offsets() {
        let text = "typography";
        let breaks = discretionary_hyphen_breaks(text, "en-US");
        assert!(!breaks.is_empty());
        assert!(breaks.iter().all(|&byte| text.is_char_boundary(byte)));
        assert!(discretionary_hyphen_breaks(text, "und").is_empty());
    }
}
