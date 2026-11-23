//! Unicode line-break opportunities and discretionary hyphens, shared by everything that
//! sets text into a measure: the reader's pages, the word processor's sheets and PDF export.
//! Measuring stays with the caller; what may break between belongs here, so no two setters
//! guess at ASCII spaces or carry their own hyphenation table.

use std::sync::LazyLock;

/// Whether a UAX #14 opportunity permits or requires a break.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineBreakKind {
    Allowed,
    Mandatory,
}

/// Iterate byte offsets where a line may or must end according to Unicode Standard Annex
/// #14.
pub fn line_break_opportunities(text: &str) -> std::vec::IntoIter<(usize, LineBreakKind)> {
    use std::collections::BTreeMap;

    use icu_segmenter::{LineSegmenter, LineSegmenterBorrowed};
    // The auto model adds dictionary/LSTM boundaries for Khmer, Lao, Myanmar, and Thai.
    #[cfg(not(target_arch = "wasm32"))]
    static SEGMENTER: LazyLock<LineSegmenterBorrowed<'static>> =
        LazyLock::new(|| LineSegmenter::new_auto(icu_segmenter::LineBreakOptions::default()));
    #[cfg(target_arch = "wasm32")]
    static SEGMENTER: LazyLock<LineSegmenterBorrowed<'static>> =
        LazyLock::new(|| LineSegmenter::new_for_non_complex_scripts(icu_segmenter::LineBreakOptions::default()));

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

/// UTF-8 byte offsets at which TeX/Liang patterns permit a discretionary hyphen for
/// `language`. The source text is never modified; callers decide whether a chosen line
/// break should paint the hyphen glyph.
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

/// Calls `out` with the byte offset in `word` after each of its syllables but the last, by
/// `lang`'s TeX/Liang patterns, as hypher embeds them.
pub fn hyphenate(word: &str, lang: hypher::Lang, out: &mut dyn FnMut(usize)) {
    let mut end = 0_usize;
    let mut syllables = hypher::hyphenate(word, lang).peekable();
    while let Some(syllable) = syllables.next() {
        end = end.saturating_add(syllable.len());
        if syllables.peek().is_some() {
            out(end);
        }
    }
}

/// Return Unicode word-like spans in original UTF-8 byte coordinates.
pub fn unicode_word_ranges(text: &str) -> std::vec::IntoIter<std::ops::Range<usize>> {
    use icu_segmenter::{WordSegmenter, WordSegmenterBorrowed};
    #[cfg(not(target_arch = "wasm32"))]
    static SEGMENTER: LazyLock<WordSegmenterBorrowed<'static>> =
        LazyLock::new(|| WordSegmenter::new_auto(icu_segmenter::WordBreakInvariantOptions::default()));
    #[cfg(target_arch = "wasm32")]
    static SEGMENTER: LazyLock<WordSegmenterBorrowed<'static>> =
        LazyLock::new(|| WordSegmenter::new_for_non_complex_scripts(icu_segmenter::WordBreakInvariantOptions::default()));
    let boundaries: Vec<_> = SEGMENTER.segment_str(text).iter_with_word_type().collect();
    boundaries
        .windows(2)
        .filter_map(|window| {
            let [first, second] = window else {
                return None;
            };
            let range = first.0..second.0;
            let word_like = second.1.is_word_like()
                || text
                    .get(range.clone())
                    .is_some_and(|word| word.chars().any(char::is_alphanumeric));
            word_like.then_some(range)
        })
        .collect::<Vec<_>>()
        .into_iter()
}
