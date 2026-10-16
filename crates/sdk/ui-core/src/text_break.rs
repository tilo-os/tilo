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
    // and Thai. The borrowed segmenter only references baked static data. In
    // the browser those models (about 2 MB) would be downloaded before
    // anything runs, so it uses the rules alone there.
    #[cfg(not(target_arch = "wasm32"))]
    static SEGMENTER: LazyLock<LineSegmenterBorrowed<'static>> =
        LazyLock::new(|| LineSegmenter::new_auto(Default::default()));
    #[cfg(target_arch = "wasm32")]
    static SEGMENTER: LazyLock<LineSegmenterBorrowed<'static>> =
        LazyLock::new(|| LineSegmenter::new_for_non_complex_scripts(Default::default()));

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
        hyphenate(word, lang, &mut |end| {
            breaks.push(range.start.saturating_add(end));
        });
    }
    breaks
}

/// Calls `out` with the byte offset in `word` after each of its syllables
/// but the last, by `lang`'s patterns.
pub type Hyphenate = fn(word: &str, lang: hypher::Lang, out: &mut dyn FnMut(usize));

/// Hands over the patterns' [`Hyphenate`], for [`install_hyphenation`].
pub type MakeHyphenation = fn() -> Hyphenate;

/// TeX/Liang hyphenation by the patterns hypher embeds.
#[must_use]
pub fn builtin_hyphenation() -> Hyphenate {
    fn by_patterns(word: &str, lang: hypher::Lang, out: &mut dyn FnMut(usize)) {
        let mut end = 0_usize;
        let mut syllables = hypher::hyphenate(word, lang).peekable();
        while let Some(syllable) = syllables.next() {
            end = end.saturating_add(syllable.len());
            if syllables.peek().is_some() {
                out(end);
            }
        }
    }
    by_patterns
}

/// [`Hyphenate`] with the patterns in reach; no breaks while their code is
/// still to come.
fn hyphenate(word: &str, lang: hypher::Lang, out: &mut dyn FnMut(usize)) {
    if let Some(hyphenate) = hyphenation::patterns() {
        hyphenate(word, lang, out);
    }
}

#[cfg(not(feature = "lazy-hyphenation"))]
mod hyphenation {
    use super::Hyphenate;

    #[expect(
        clippy::unnecessary_wraps,
        reason = "the lazy-hyphenation build has none until they are fetched"
    )]
    pub(super) fn patterns() -> Option<Hyphenate> {
        Some(super::builtin_hyphenation())
    }

    /// How many times the patterns arrived: never, as they were always
    /// there.
    #[must_use]
    pub fn hyphenation_generation() -> u64 {
        0
    }
}

/// The patterns are fetched the first time text is hyphenated: the program
/// hands over a loader ([`defer_hyphenation`]) that fetches their code and
/// passes back [`builtin_hyphenation`](super::builtin_hyphenation)
/// ([`install_hyphenation`]). Until then text breaks between words only;
/// layouts are made again once [`hyphenation_generation`] moved.
#[cfg(feature = "lazy-hyphenation")]
mod hyphenation {
    use super::{Hyphenate, MakeHyphenation};
    use crate::deferred::Deferred;

    static PATTERNS: Deferred<Hyphenate> = Deferred::new();

    /// The patterns, or `None` while their code is still to come (and then
    /// fetched).
    pub(super) fn patterns() -> Option<Hyphenate> {
        PATTERNS.get()
    }

    /// Bumped when the patterns arrive: hyphenated layouts are made again
    /// once this moved.
    #[must_use]
    pub fn hyphenation_generation() -> u64 {
        PATTERNS.generation()
    }

    /// How the program fetches the patterns' code: `load` starts the fetch
    /// and, once the code is in, calls [`install_hyphenation`] (or
    /// [`hyphenation_load_failed`]).
    pub fn defer_hyphenation(load: fn()) {
        PATTERNS.defer(load);
    }

    /// The patterns' code is in: text is hyphenated from now on.
    pub fn install_hyphenation(make: MakeHyphenation) {
        PATTERNS.install(make());
    }

    /// The fetch failed: the next layout that hyphenates fetches again.
    pub fn hyphenation_load_failed() {
        PATTERNS.load_failed();
    }
}

pub use hyphenation::hyphenation_generation;
#[cfg(feature = "lazy-hyphenation")]
pub use hyphenation::{defer_hyphenation, hyphenation_load_failed, install_hyphenation};

pub(crate) const VIRTUAL_BREAK: char = '\u{200b}';

/// Paragraphs longer than this are shaped in chunks (see
/// [`OffsetMap::add_paragraph_chunks`]). Ordinary documents never reach it.
pub(crate) const HUGE_PARAGRAPH_BYTES: usize = 16 * 1024;
const CHUNK_MIN_BYTES: usize = 2 * 1024;
const CHUNK_MAX_BYTES: usize = 8 * 1024;

/// Content-defined chunk boundaries (source byte offsets, each the start of a
/// chunk) for paragraphs longer than [`HUGE_PARAGRAPH_BYTES`]. A boundary
/// follows a space whose preceding bytes hash to an anchor value, at least
/// [`CHUNK_MIN_BYTES`] after the previous boundary; after
/// [`CHUNK_MAX_BYTES`] any space qualifies, and after twice that any
/// character boundary does (text without spaces). The hash depends only on
/// nearby bytes, so an edit shifts at most the boundaries around it.
pub(crate) fn paragraph_chunk_boundaries(text: &str) -> Vec<usize> {
    const WINDOW: usize = 16;
    let bytes = text.as_bytes();
    let mut boundaries = Vec::new();
    let mut paragraph_start = 0;
    while paragraph_start < bytes.len() {
        let paragraph_end = bytes[paragraph_start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |i| paragraph_start + i);
        if paragraph_end - paragraph_start > HUGE_PARAGRAPH_BYTES {
            let mut last = paragraph_start;
            let mut position = paragraph_start + CHUNK_MIN_BYTES;
            while position < paragraph_end.saturating_sub(CHUNK_MIN_BYTES) {
                let since = position - last;
                let anchor = if bytes[position - 1] == b' ' && text.is_char_boundary(position) {
                    let window = &bytes[position.saturating_sub(WINDOW).max(last)..position];
                    let hash = window.iter().fold(0x811c_9dc5_u32, |h, &b| {
                        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
                    });
                    since >= CHUNK_MAX_BYTES || hash % 512 == 0
                } else {
                    since >= 2 * CHUNK_MAX_BYTES && text.is_char_boundary(position)
                };
                if anchor {
                    boundaries.push(position);
                    last = position;
                    position += CHUNK_MIN_BYTES;
                } else {
                    position += 1;
                }
            }
        }
        paragraph_start = paragraph_end + 1;
    }
    boundaries
}

/// The whole lines (line-feed-delimited paragraphs) an edit touched: `old[start..old_end]`
/// became `new[start..new_end]`; both end before a line feed or at the end.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LineEdit {
    pub(crate) start: usize,
    pub(crate) old_end: usize,
    pub(crate) new_end: usize,
}

impl LineEdit {
    pub(crate) fn between(old: &str, new: &str) -> Self {
        let (old_bytes, new_bytes) = (old.as_bytes(), new.as_bytes());
        let prefix = old_bytes
            .iter()
            .zip(new_bytes)
            .take_while(|(a, b)| a == b)
            .count();
        let room = old.len().min(new.len()).saturating_sub(prefix);
        let suffix = old_bytes
            .iter()
            .rev()
            .zip(new_bytes.iter().rev())
            .take(room)
            .take_while(|(a, b)| a == b)
            .count();
        let start = old_bytes[..prefix]
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |at| at.saturating_add(1));
        // The first byte of the common suffix lies in the last changed
        // paragraph; it ends at the next line feed of that suffix.
        let changed_end = old.len().saturating_sub(suffix);
        let old_end = old_bytes[changed_end..]
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(old.len(), |at| changed_end.saturating_add(at));
        let new_end = old_end.saturating_add(new.len()).saturating_sub(old.len());
        Self {
            start,
            old_end,
            new_end,
        }
    }

    /// Index of the first touched line.
    pub(crate) fn first_line(&self, new: &str) -> usize {
        count_line_feeds(new.as_bytes().get(..self.start).unwrap_or_default())
    }

    /// Line feeds inside the old touched lines (their count less one).
    pub(crate) fn old_line_breaks(&self, old: &str) -> usize {
        count_line_feeds(
            old.as_bytes()
                .get(self.start..self.old_end)
                .unwrap_or_default(),
        )
    }
}

/// Line feeds in `bytes`.
pub(crate) fn count_line_feeds(bytes: &[u8]) -> usize {
    bytes.iter().map(|&byte| usize::from(byte == b'\n')).sum()
}

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
                hyphenate(word, lang, &mut |end| {
                    insertions.push(VirtualInsertion {
                        source: range.start.saturating_add(end),
                        ch: hyphen,
                    });
                });
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

    /// Add `extra` insertions; each goes after any already at its source
    /// position.
    pub(crate) fn add_insertions(&mut self, extra: &[VirtualInsertion]) {
        self.insertions.extend_from_slice(extra);
        self.insertions.sort_by_key(|insertion| insertion.source);
    }

    /// Whether the shaped byte `shaped` lies inside a virtual `ch`.
    pub(crate) fn is_virtual(&self, shaped: usize, ch: char) -> bool {
        let mut added = 0usize;
        for insertion in &self.insertions {
            let at = insertion.source.saturating_add(added);
            if shaped < at {
                return false;
            }
            let len = insertion.ch.len_utf8();
            if shaped < at.saturating_add(len) {
                return insertion.ch == ch;
            }
            added = added.saturating_add(len);
        }
        false
    }

    pub(crate) fn is_identity(&self) -> bool {
        self.insertions.is_empty()
    }

    /// Split paragraphs longer than [`HUGE_PARAGRAPH_BYTES`] into shaping
    /// chunks with virtual line feeds. cosmic-text shapes, wraps and caches
    /// one paragraph as a unit, so a pasted page of text without paragraph
    /// breaks was reshaped completely on every keystroke and every frame
    /// that relaid it (and grew without bound). Boundaries are content
    /// defined (see [`paragraph_chunk_boundaries`]): an edit moves only the
    /// boundaries next to it, so unchanged chunks keep their shaped lines.
    /// A chunk boundary ends a visual line early; that is the only visible
    /// difference, and only in paragraphs over the threshold.
    pub(crate) fn add_paragraph_chunks(&mut self, text: &str) {
        let boundaries = paragraph_chunk_boundaries(text);
        if boundaries.is_empty() {
            return;
        }
        let taken: std::collections::HashSet<usize> = self
            .insertions
            .iter()
            .map(|insertion| insertion.source)
            .collect();
        self.insertions.extend(
            boundaries
                .into_iter()
                .filter(|source| !taken.contains(source))
                .map(|source| VirtualInsertion { source, ch: '\n' }),
        );
        self.insertions.sort_by_key(|insertion| insertion.source);
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
/// whole. Offsets address the original UTF-8 string and each slice is a
/// complete UAX #29 cluster, using the same ICU rules as toolkit selection.
pub fn grapheme_indices(text: &str) -> impl Iterator<Item = (usize, &str)> {
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
    // As for lines: no dictionary/LSTM models in the browser.
    #[cfg(not(target_arch = "wasm32"))]
    static SEGMENTER: LazyLock<WordSegmenterBorrowed<'static>> =
        LazyLock::new(|| WordSegmenter::new_auto(Default::default()));
    #[cfg(target_arch = "wasm32")]
    static SEGMENTER: LazyLock<WordSegmenterBorrowed<'static>> =
        LazyLock::new(|| WordSegmenter::new_for_non_complex_scripts(Default::default()));
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
    fn huge_paragraphs_get_stable_content_defined_chunks() {
        let words: Vec<String> = (0..9000)
            .map(|i| format!("w{:x}{}", i * 7919 % 4093, "ab".repeat(i % 5)))
            .collect();
        let text = words.join(" ");
        assert!(text.len() > 3 * HUGE_PARAGRAPH_BYTES);
        assert!(paragraph_chunk_boundaries("short paragraph").is_empty());
        assert!(paragraph_chunk_boundaries(&"x ".repeat(HUGE_PARAGRAPH_BYTES / 2)).is_empty());
        let boundaries = paragraph_chunk_boundaries(&text);
        assert!(boundaries.len() >= text.len() / CHUNK_MAX_BYTES - 1);
        let mut last = 0;
        for &b in &boundaries {
            assert!(text.is_char_boundary(b));
            assert_eq!(text.as_bytes()[b - 1], b' ', "boundaries follow a space");
            assert!(b - last >= CHUNK_MIN_BYTES && b - last <= CHUNK_MAX_BYTES + 64);
            last = b;
        }
        // An edit near the start moves only nearby boundaries.
        let edited = format!("{}inserted {}", &text[..100], &text[100..]);
        let shift = "inserted ".len();
        let moved: Vec<usize> = paragraph_chunk_boundaries(&edited)
            .into_iter()
            .map(|b| b - shift)
            .collect();
        let unchanged = boundaries.iter().filter(|b| moved.contains(b)).count();
        assert!(
            unchanged + 2 >= boundaries.len(),
            "{unchanged} of {}",
            boundaries.len()
        );
        // Text without spaces is still bounded.
        let solid = "x".repeat(5 * CHUNK_MAX_BYTES + HUGE_PARAGRAPH_BYTES);
        assert!(!paragraph_chunk_boundaries(&solid).is_empty());
        // Line feeds split paragraphs first.
        let two = format!(
            "{}\n{}",
            &text[..HUGE_PARAGRAPH_BYTES - 10],
            &text[..HUGE_PARAGRAPH_BYTES - 10]
        );
        assert!(paragraph_chunk_boundaries(&two).is_empty());
    }

    #[test]
    fn paragraph_chunks_map_offsets_back_to_source() {
        let text = (0..6000)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut map = OffsetMap::default();
        map.add_paragraph_chunks(&text);
        assert!(!map.is_identity());
        let shaped = map.shape_text(&text);
        assert_eq!(shaped.replace('\n', ""), text);
        for byte in (0..=text.len())
            .step_by(97)
            .filter(|b| text.is_char_boundary(*b))
        {
            assert_eq!(map.shaped_to_source(map.source_to_shaped(byte)), byte);
        }
    }

    #[test]
    fn word_and_grapheme_segmentation_keeps_clusters_whole() {
        // Case `unicode_words_retain_trailing_combining_marks`.
        {
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

        // Case `grapheme_indices_keep_extended_clusters_whole`.
        {
            let text = "e\u{301}👨‍👩‍👧🇳🇱a";
            let clusters: Vec<_> = grapheme_indices(text).collect();
            assert_eq!(
                clusters,
                vec![(0, "e\u{301}"), (3, "👨‍👩‍👧"), (21, "🇳🇱"), (29, "a"),]
            );
            assert_eq!(graphemes("").count(), 0);
        }
    }

    #[test]
    fn line_break_opportunities_follow_script_rules() {
        // Case `cjk_has_opportunities_without_ascii_spaces`.
        {
            let text = "東京京都";
            let breaks: Vec<_> = line_break_opportunities(text).collect();
            assert_eq!(breaks.last(), Some(&(text.len(), LineBreakKind::Mandatory)));
            assert!(breaks.iter().any(|(byte, _)| *byte < text.len()));
        }

        // Case `non_breaking_space_does_not_create_an_opportunity`.
        {
            let text = "10\u{a0}km away";
            let protected_end = "10\u{a0}".len();
            assert!(
                line_break_opportunities(text).all(|(byte, _)| byte != protected_end),
                "NBSP keeps its adjacent words together"
            );
        }

        // Case `emoji_zwj_sequence_is_not_split`.
        {
            let family = "👨‍👩‍👧‍👦";
            assert_eq!(
                line_break_opportunities(family).collect::<Vec<_>>(),
                vec![(family.len(), LineBreakKind::Mandatory)]
            );
        }

        // Case `dictionary_line_breaks_segment_thai_without_spaces`.
        {
            let text = "ทุกสองสัปดาห์";
            let breaks: Vec<_> = line_break_opportunities(text)
                .map(|(byte, _)| byte)
                .collect();
            assert_eq!(breaks, vec![9, 18, text.len()]);
        }
    }

    #[test]
    fn discretionary_hyphens_map_to_word_like_source_offsets() {
        // Case `english_hyphenation_inserts_soft_hyphens_at_tex_pattern_boundaries`.
        {
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

        // Case `hyphenation_only_touches_word_like_segments`.
        {
            let text = "extensive, 123";
            let map = OffsetMap::for_text_with_hyphenation(text, Some(hypher::Lang::English));
            assert!(map.insertions.iter().all(|insertion| insertion.source < 9));
            assert_eq!(
                map.shaped_to_source(map.source_to_shaped(text.len())),
                text.len()
            );
        }

        // Case `custom_breaks_ignore_endpoints_and_non_utf8_boundaries`.
        {
            let text = "éclair";
            let map =
                OffsetMap::for_text_with_breaks(text, None, &[0, 1, 2, text.len(), usize::MAX]);
            assert_eq!(map.shape_text(text), "é\u{ad}clair");
            assert_eq!(map.insertions.len(), 1);
            assert_eq!(map.insertions[0].source, 2);
        }

        // Case `public_hyphen_breaks_are_source_offsets`.
        {
            let text = "typography";
            let breaks = discretionary_hyphen_breaks(text, "en-US");
            assert!(!breaks.is_empty());
            assert!(breaks.iter().all(|&byte| text.is_char_boundary(byte)));
            assert!(discretionary_hyphen_breaks(text, "und").is_empty());
        }
    }
}
